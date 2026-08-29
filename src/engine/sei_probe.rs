//! H.264/H.265 SEI NAL probe: closed captions, HDR metadata, HLG hints.

use crate::models::{ContainerKind, SeiProbeResult};

pub const SEI_PROBE_LIMIT: usize = 65536;

#[derive(Debug, Clone, Default)]
pub struct SeiProbeAccumulator {
    result: SeiProbeResult,
}

impl SeiProbeAccumulator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn ingest(&mut self, bytes: &[u8], container: ContainerKind) -> SeiProbeResult {
        let slice = &bytes[..bytes.len().min(SEI_PROBE_LIMIT)];
        match container {
            ContainerKind::Ts => self.scan_ts_pes(slice),
            ContainerKind::Fmp4 => self.scan_length_prefixed(slice, 4),
            ContainerKind::Unknown if looks_like_ts(slice) => self.scan_ts_pes(slice),
            _ => self.scan_annex_b(slice),
        }
        self.result.clone()
    }

    pub fn snapshot(&self) -> SeiProbeResult {
        self.result.clone()
    }

    fn scan_ts_pes(&mut self, bytes: &[u8]) {
        for pkt in bytes.chunks(188).filter(|p| p.len() == 188 && p[0] == 0x47) {
            if let Some(payload) = ts_payload(pkt) {
                let mut scanned = false;
                if payload.len() >= 9
                    && payload[0] == 0
                    && payload[1] == 0
                    && payload[2] == 1
                    && (0xE0..=0xEF).contains(&payload[3])
                {
                    let pes_header_len = payload[8] as usize;
                    let start = 9 + pes_header_len;
                    if start < payload.len() {
                        self.scan_annex_b(&payload[start..]);
                        scanned = true;
                    }
                }
                if !scanned {
                    self.scan_annex_b(payload);
                }
            }
        }
    }

    fn scan_annex_b(&mut self, bytes: &[u8]) {
        let mut i = 0usize;
        while i + 3 < bytes.len() {
            let start = if bytes[i..].starts_with(&[0, 0, 0, 1]) {
                i + 4
            } else if bytes[i..].starts_with(&[0, 0, 1]) {
                i + 3
            } else {
                i += 1;
                continue;
            };
            let next = find_next_start_code(&bytes[start..])
                .map(|o| start + o)
                .unwrap_or(bytes.len());
            if start < next {
                self.parse_nal(&bytes[start..next]);
            }
            i = if next > i { next } else { i.saturating_add(1) };
        }
    }

    fn scan_length_prefixed(&mut self, bytes: &[u8], len_size: usize) {
        let mut i = 0usize;
        while i + len_size < bytes.len() {
            let (nal_len, hdr) = match len_size {
                4 => {
                    let n = u32::from_be_bytes([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]])
                        as usize;
                    (n, 4)
                }
                _ => break,
            };
            i += hdr;
            if nal_len == 0 || i + nal_len > bytes.len() {
                break;
            }
            self.parse_nal(&bytes[i..i + nal_len]);
            i += nal_len;
        }
    }

    fn parse_nal(&mut self, nal: &[u8]) {
        if nal.is_empty() {
            return;
        }
        self.result.nal_units_scanned = self.result.nal_units_scanned.saturating_add(1);
        let h264_type = nal[0] & 0x1f;
        let h265_type = (nal[0] >> 1) & 0x3f;
        if h264_type == 6 {
            parse_h264_sei(nal, &mut self.result);
        } else if h264_type == 7 {
            parse_h264_sps_vui(nal, &mut self.result);
        } else if h265_type == 39 || h265_type == 40 {
            parse_h265_sei(nal, &mut self.result);
        } else if h265_type == 33 {
            parse_h265_vui(nal, &mut self.result);
        }
    }
}

pub fn probe_sei(bytes: &[u8], container: ContainerKind) -> SeiProbeResult {
    let mut acc = SeiProbeAccumulator::new();
    acc.ingest(bytes, container)
}

fn ts_payload(pkt: &[u8]) -> Option<&[u8]> {
    let adaptation = (pkt[3] >> 4) & 0x3;
    let mut start = 4usize;
    if adaptation == 2 || adaptation == 3 {
        let afl = *pkt.get(4)? as usize;
        start = start.saturating_add(1 + afl);
    }
    if adaptation == 2 {
        return None;
    }
    pkt.get(start..).filter(|p| !p.is_empty())
}

fn find_next_start_code(bytes: &[u8]) -> Option<usize> {
    bytes
        .windows(3)
        .position(|w| w == [0, 0, 1] || (w[0] == 0 && w[1] == 0 && w[2] == 1))
}

fn looks_like_ts(bytes: &[u8]) -> bool {
    bytes.chunks(188).any(|p| p.len() == 188 && p[0] == 0x47)
}

fn parse_h264_sei(nal: &[u8], out: &mut SeiProbeResult) {
    let mut i = 1usize;
    while i + 1 < nal.len() {
        let (payload_type, consumed) = read_sei_var(nal, i);
        i += consumed;
        if i >= nal.len() {
            break;
        }
        let (payload_size, consumed) = read_sei_var(nal, i);
        i += consumed;
        if i + payload_size > nal.len() {
            break;
        }
        let payload = &nal[i..i + payload_size];
        classify_sei_payload(payload_type, payload, out);
        i += payload_size;
    }
}

fn parse_h265_sei(nal: &[u8], out: &mut SeiProbeResult) {
    if nal.len() < 2 {
        return;
    }
    parse_h264_sei(nal, out);
}

fn read_sei_var(data: &[u8], start: usize) -> (usize, usize) {
    let mut val = 0usize;
    let mut i = start;
    while i < data.len() {
        let b = data[i];
        val = val.saturating_mul(255).saturating_add(usize::from(b));
        i += 1;
        if b != 0xff {
            break;
        }
    }
    (val, i - start)
}

fn classify_sei_payload(payload_type: usize, payload: &[u8], out: &mut SeiProbeResult) {
    match payload_type {
        137 => parse_mastering_display(payload, out),
        144 => parse_content_light_level(payload, out),
        4 => parse_user_data_registered(payload, out),
        _ => {}
    }
}

fn parse_user_data_registered(payload: &[u8], out: &mut SeiProbeResult) {
    if payload.len() < 3 {
        return;
    }
    // itu_t_t35 country code + provider code
    if payload[0] != 0xB5 {
        return;
    }
    if payload.len() < 4 {
        return;
    }
    let provider = u16::from_be_bytes([payload[1], payload[2]]);
    let user_data = &payload[3..];
    // ATSC A/53 provider 0x0031
    if provider == 0x0031 {
        parse_atsc_a53(user_data, out);
    }
}

fn parse_atsc_a53(data: &[u8], out: &mut SeiProbeResult) {
    if data.len() < 2 {
        return;
    }
    let mut i = 0usize;
    while i + 1 < data.len() {
        let cc_count = data[i] & 0x1f;
        let cc_type = (data[i] >> 5) & 0x03;
        i += 1;
        match cc_type {
            0 => out.cea608_present = true,
            1 => out.cea708_present = true,
            _ => {}
        }
        let block_end = (i + cc_count as usize * 3).min(data.len());
        i = block_end;
        if cc_count == 0 {
            break;
        }
    }
    if out.caption_language.is_none() {
        out.caption_language = Some("und".into());
    }
}

fn parse_mastering_display(payload: &[u8], out: &mut SeiProbeResult) {
    if payload.len() >= 24 {
        out.hdr10_present = true;
    }
}

fn parse_content_light_level(payload: &[u8], out: &mut SeiProbeResult) {
    if payload.len() >= 4 {
        out.max_cll = Some(u16::from_be_bytes([payload[0], payload[1]]));
        out.max_fall = Some(u16::from_be_bytes([payload[2], payload[3]]));
        out.hdr10_present = true;
    }
}

fn parse_h264_sps_vui(nal: &[u8], out: &mut SeiProbeResult) {
    if nal.len() < 4 {
        return;
    }
    let profile = nal[1];
    if profile == 100 || profile == 110 || profile == 122 {
        // High profile - scan for transfer characteristics in rough VUI region
        if nal
            .windows(2)
            .any(|w| w == [0x12, 0x00] || w == [0x12, 0x01])
        {
            out.hlg_present = true;
        }
    }
}

fn parse_h265_vui(_nal: &[u8], out: &mut SeiProbeResult) {
    // Simplified HLG hint when HEVC SPS present in probe window
    out.hlg_present = out.hlg_present || out.hdr10_present;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_light_level_parsed() {
        let mut out = SeiProbeResult::default();
        parse_content_light_level(&[0x03, 0xE8, 0x01, 0xF4], &mut out);
        assert_eq!(out.max_cll, Some(1000));
        assert_eq!(out.max_fall, Some(500));
        assert!(out.hdr10_present);
    }

    #[test]
    fn atsc_caption_detected() {
        let mut out = SeiProbeResult::default();
        let payload = [0xB5, 0x00, 0x31, 0x01, 0x00, 0x00];
        parse_user_data_registered(&payload, &mut out);
        assert!(out.cea608_present || out.cea708_present);
    }
}
