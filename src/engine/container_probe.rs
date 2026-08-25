//! Codec / resolution / FPS probe for fMP4 and MPEG-TS.

use crate::models::{ContainerKind, WireProbeInfo};

/// Probe segment bytes for codec / resolution / FPS.
pub fn deep_wire_probe(bytes: &[u8]) -> WireProbeInfo {
    let kind = classify_container(bytes);
    let mut info = match kind {
        ContainerKind::Fmp4 => probe_fmp4(bytes),
        ContainerKind::Ts => probe_mpeg_ts(bytes),
        ContainerKind::Unknown => WireProbeInfo::default(),
    };
    info.container = kind;
    info
}

fn classify_container(bytes: &[u8]) -> ContainerKind {
    if bytes
        .windows(4)
        .any(|w| w == b"ftyp" || w == b"styp" || w == b"moof" || w == b"moov" || w == b"traf")
    {
        return ContainerKind::Fmp4;
    }
    if bytes.first() == Some(&0x47) {
        return ContainerKind::Ts;
    }
    if bytes.len() >= 188 && bytes.iter().step_by(188).take(3).all(|b| *b == 0x47) {
        return ContainerKind::Ts;
    }
    ContainerKind::Unknown
}

fn probe_fmp4(bytes: &[u8]) -> WireProbeInfo {
    let mut info = WireProbeInfo::default();
    walk_boxes(bytes, 0, bytes.len(), &mut |name, payload| {
        match name {
            b"avc1" | b"avc3" => {
                if info.codec.is_none() {
                    info.codec = Some("avc1".into());
                }
                if payload.len() >= 24 {
                    let w = u16::from_be_bytes([payload[16], payload[17]]);
                    let h = u16::from_be_bytes([payload[18], payload[19]]);
                    if w > 0 && h > 0 {
                        info.width = Some(u32::from(w));
                        info.height = Some(u32::from(h));
                    }
                }
                if let Some(avcc) = find_child_box(payload, b"avcC") {
                    parse_avcc(avcc, &mut info);
                }
            }
            b"hvc1" | b"hev1" => {
                if info.codec.is_none() {
                    info.codec = Some("hvc1".into());
                }
                if payload.len() >= 24 {
                    let w = u16::from_be_bytes([payload[16], payload[17]]);
                    let h = u16::from_be_bytes([payload[18], payload[19]]);
                    if w > 0 && h > 0 {
                        info.width = Some(u32::from(w));
                        info.height = Some(u32::from(h));
                    }
                }
                if let Some(hvcc) = find_child_box(payload, b"hvcC") {
                    parse_hvcc(hvcc, &mut info);
                }
            }
            b"mp4a" => {
                if info.audio_sample_rate.is_none() && payload.len() >= 28 {
                    let sr = u16::from_be_bytes([payload[24], payload[25]]);
                    let ch = payload.get(17).copied().unwrap_or(0);
                    if sr > 0 {
                        info.audio_sample_rate = Some(u32::from(sr));
                    }
                    if ch > 0 {
                        info.audio_channels = Some(ch);
                    }
                }
            }
            b"avcC" => parse_avcc(payload, &mut info),
            b"hvcC" => parse_hvcc(payload, &mut info),
            b"trun" if info.sync_sample.is_none() => {
                info.sync_sample = trun_first_sample_sync(payload);
            }
            _ => {}
        }
        true
    });
    info
}

fn walk_boxes(
    data: &[u8],
    start: usize,
    end: usize,
    visit: &mut dyn FnMut(&[u8; 4], &[u8]) -> bool,
) {
    let mut off = start;
    while off + 8 <= end && off + 8 <= data.len() {
        let size32 = u32::from_be_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]]);
        let mut name = [0u8; 4];
        name.copy_from_slice(&data[off + 4..off + 8]);
        let (header, box_size) = if size32 == 1 {
            if off + 16 > data.len() {
                break;
            }
            let size64 = u64::from_be_bytes([
                data[off + 8],
                data[off + 9],
                data[off + 10],
                data[off + 11],
                data[off + 12],
                data[off + 13],
                data[off + 14],
                data[off + 15],
            ]);
            (16usize, size64 as usize)
        } else if size32 == 0 {
            (8usize, end.saturating_sub(off))
        } else {
            (8usize, size32 as usize)
        };
        if box_size < header || off + box_size > data.len() || off + box_size > end {
            break;
        }
        let payload = &data[off + header..off + box_size];
        let descend = visit(&name, payload);
        if descend
            && matches!(
                &name,
                b"moov"
                    | b"trak"
                    | b"mdia"
                    | b"minf"
                    | b"stbl"
                    | b"stsd"
                    | b"moof"
                    | b"traf"
                    | b"mdat"
                    | b"edts"
                    | b"mvex"
            )
        {
            let nest_off = if &name == b"stsd" && payload.len() >= 8 {
                8
            } else {
                0
            };
            walk_boxes(data, off + header + nest_off, off + box_size, visit);
        } else if descend && matches!(&name, b"avc1" | b"avc3" | b"hvc1" | b"hev1" | b"mp4a") {
            let skip = if &name == b"mp4a" {
                28
            } else {
                78.min(payload.len())
            };
            if skip < payload.len() {
                walk_boxes(data, off + header + skip, off + box_size, visit);
            }
        }
        off += box_size;
    }
}

fn find_child_box<'a>(payload: &'a [u8], want: &[u8; 4]) -> Option<&'a [u8]> {
    let mut found = None;
    let start = if payload.len() > 86 { 78 } else { 0 };
    let slice = &payload[start.min(payload.len())..];
    let mut off = 0usize;
    while off + 8 <= slice.len() {
        let size = u32::from_be_bytes([slice[off], slice[off + 1], slice[off + 2], slice[off + 3]])
            as usize;
        if size < 8 || off + size > slice.len() {
            break;
        }
        if &slice[off + 4..off + 8] == want {
            found = Some(&slice[off + 8..off + size]);
            break;
        }
        off += size;
    }
    found
}

fn parse_avcc(avcc: &[u8], info: &mut WireProbeInfo) {
    if avcc.len() < 7 {
        return;
    }
    let profile = avcc[1];
    let level = avcc[3];
    info.profile_level = Some(format!("avc1.{profile:02x}{level:02x}"));
    if info.codec.is_none() {
        info.codec = Some("avc1".into());
    }
    let num_sps = (avcc[5] & 0x1f) as usize;
    let mut off = 6usize;
    for _ in 0..num_sps {
        if off + 2 > avcc.len() {
            break;
        }
        let len = u16::from_be_bytes([avcc[off], avcc[off + 1]]) as usize;
        off += 2;
        if off + len > avcc.len() {
            break;
        }
        apply_h264_sps(&avcc[off..off + len], info);
        off += len;
    }
}

fn parse_hvcc(hvcc: &[u8], info: &mut WireProbeInfo) {
    if hvcc.len() < 23 {
        return;
    }
    let general_profile = hvcc[1];
    let general_level = hvcc[12];
    info.profile_level = Some(format!("hvc1.{general_profile}.{general_level}"));
    if info.codec.is_none() {
        info.codec = Some("hvc1".into());
    }
    let mut off = 22usize;
    if off >= hvcc.len() {
        return;
    }
    let num_arrays = hvcc[off] as usize;
    off += 1;
    for _ in 0..num_arrays {
        if off + 3 > hvcc.len() {
            break;
        }
        let nal_type = hvcc[off] & 0x3f;
        let num_nalus = u16::from_be_bytes([hvcc[off + 1], hvcc[off + 2]]) as usize;
        off += 3;
        for _ in 0..num_nalus {
            if off + 2 > hvcc.len() {
                return;
            }
            let len = u16::from_be_bytes([hvcc[off], hvcc[off + 1]]) as usize;
            off += 2;
            if off + len > hvcc.len() {
                return;
            }
            if nal_type == 33 {
                apply_h265_sps(&hvcc[off..off + len], info);
            }
            off += len;
        }
    }
}

fn trun_first_sample_sync(trun: &[u8]) -> Option<bool> {
    if trun.len() < 8 {
        return None;
    }
    let version = trun[0];
    let flags = u32::from_be_bytes([0, trun[1], trun[2], trun[3]]);
    let mut off = 8usize;
    if flags & 0x000001 != 0 {
        off += 4;
    }
    if flags & 0x000004 != 0 {
        if off + 4 > trun.len() {
            return None;
        }
        let sample_flags =
            u32::from_be_bytes([trun[off], trun[off + 1], trun[off + 2], trun[off + 3]]);
        let non_sync = (sample_flags >> 16) & 1 == 1;
        return Some(!non_sync);
    }
    let _ = version;
    if flags & 0x000400 != 0 {
        let sample_count = u32::from_be_bytes([trun[4], trun[5], trun[6], trun[7]]) as usize;
        if sample_count == 0 {
            return None;
        }
        let mut so = off;
        if flags & 0x000100 != 0 {
            so += 4;
        }
        if flags & 0x000200 != 0 {
            so += 4;
        }
        if flags & 0x000400 != 0 {
            if so + 4 > trun.len() {
                return None;
            }
            let sample_flags =
                u32::from_be_bytes([trun[so], trun[so + 1], trun[so + 2], trun[so + 3]]);
            let non_sync = (sample_flags >> 16) & 1 == 1;
            return Some(!non_sync);
        }
    }
    None
}

fn probe_mpeg_ts(bytes: &[u8]) -> WireProbeInfo {
    let mut info = WireProbeInfo::default();
    let packets: Vec<&[u8]> = bytes
        .chunks(188)
        .filter(|p| p.len() == 188 && p[0] == 0x47)
        .collect();
    if packets.is_empty() {
        return info;
    }

    let mut pmt_pid: Option<u16> = None;
    let mut video_pid: Option<u16> = None;
    let mut audio_pid: Option<u16> = None;
    let mut video_stream_type: Option<u8> = None;

    for pkt in &packets {
        let pid = packet_pid(pkt);
        if pid == 0 {
            if let Some(pmt) = parse_pat_pmt_pid(pkt) {
                pmt_pid = Some(pmt);
            }
        }
    }

    if let Some(pmt) = pmt_pid {
        for pkt in &packets {
            if packet_pid(pkt) == pmt {
                if let Some((vpid, st, apid)) = parse_pmt(pkt) {
                    video_pid = Some(vpid);
                    video_stream_type = Some(st);
                    audio_pid = apid;
                    break;
                }
            }
        }
    }

    let mut video_payload = Vec::new();
    let mut audio_payload = Vec::new();
    for pkt in &packets {
        let pid = packet_pid(pkt);
        if video_pid.map(|v| v == pid).unwrap_or(false)
            || (video_pid.is_none() && has_pes_start(pkt))
        {
            if let Some(payload) = ts_payload(pkt) {
                if payload
                    .windows(4)
                    .any(|w| w == [0, 0, 1, 0xe0] || w == [0, 0, 0, 1])
                    || video_pid.is_some()
                {
                    video_payload.extend_from_slice(payload);
                }
            }
        }
        if audio_pid.map(|a| a == pid).unwrap_or(false) {
            if let Some(payload) = ts_payload(pkt) {
                audio_payload.extend_from_slice(payload);
            }
        }
    }

    if video_payload.is_empty() {
        for pkt in &packets {
            if let Some(payload) = ts_payload(pkt) {
                video_payload.extend_from_slice(payload);
            }
        }
    }

    match video_stream_type {
        Some(0x1b) | None => {
            if info.codec.is_none() {
                info.codec = Some("avc1".into());
            }
            extract_h264_from_annexb(&video_payload, &mut info);
        }
        Some(0x24) => {
            info.codec = Some("hvc1".into());
            extract_h265_from_annexb(&video_payload, &mut info);
        }
        _ => {
            extract_h264_from_annexb(&video_payload, &mut info);
            if info.width.is_none() {
                extract_h265_from_annexb(&video_payload, &mut info);
            }
        }
    }

    if !audio_payload.is_empty() {
        parse_adts_header(&audio_payload, &mut info);
    } else {
        parse_adts_header(&video_payload, &mut info);
    }

    info
}

fn packet_pid(pkt: &[u8]) -> u16 {
    (((pkt[1] as u16) & 0x1f) << 8) | pkt[2] as u16
}

fn has_pes_start(pkt: &[u8]) -> bool {
    pkt[1] & 0x40 != 0
}

fn ts_payload(pkt: &[u8]) -> Option<&[u8]> {
    let adaptation = (pkt[3] >> 4) & 0x3;
    let mut off = 4usize;
    if adaptation == 2 || adaptation == 3 {
        let len = pkt[4] as usize;
        off = 5 + len;
    }
    if adaptation == 0 || off >= 188 {
        return None;
    }
    Some(&pkt[off..188])
}

fn parse_pat_pmt_pid(pkt: &[u8]) -> Option<u16> {
    let payload = ts_payload(pkt)?;
    let pointer = payload.first().copied()? as usize;
    let section = payload.get(1 + pointer..)?;
    if section.len() < 12 || section[0] != 0x00 {
        return None;
    }
    let mut off = 8usize;
    while off + 4 <= section.len() {
        let program = u16::from_be_bytes([section[off], section[off + 1]]);
        let pid = (((section[off + 2] as u16) & 0x1f) << 8) | section[off + 3] as u16;
        if program != 0 {
            return Some(pid);
        }
        off += 4;
    }
    None
}

fn parse_pmt(pkt: &[u8]) -> Option<(u16, u8, Option<u16>)> {
    let payload = ts_payload(pkt)?;
    let pointer = payload.first().copied()? as usize;
    let section = payload.get(1 + pointer..)?;
    if section.len() < 12 || section[0] != 0x02 {
        return None;
    }
    let section_len = (((section[1] as usize) & 0x0f) << 8) | section[2] as usize;
    let program_info_len = (((section[10] as usize) & 0x0f) << 8) | section[11] as usize;
    let mut off = 12 + program_info_len;
    let end = (3 + section_len).min(section.len()).saturating_sub(4);
    let mut video = None;
    let mut audio = None;
    while off + 5 <= end {
        let stream_type = section[off];
        let es_pid = (((section[off + 1] as u16) & 0x1f) << 8) | section[off + 2] as u16;
        let es_info_len = (((section[off + 3] as usize) & 0x0f) << 8) | section[off + 4] as usize;
        match stream_type {
            0x1b | 0x24 | 0xea if video.is_none() => {
                video = Some((es_pid, stream_type));
            }
            0x0f | 0x03 | 0x04 | 0x11 if audio.is_none() => {
                audio = Some(es_pid);
            }
            _ => {}
        }
        off += 5 + es_info_len;
    }
    let (vpid, st) = video?;
    Some((vpid, st, audio))
}

fn extract_h264_from_annexb(data: &[u8], info: &mut WireProbeInfo) {
    for nal in split_annexb(data) {
        if nal.is_empty() {
            continue;
        }
        let nal_type = nal[0] & 0x1f;
        if nal_type == 7 {
            apply_h264_sps(nal, info);
        }
    }
}

fn extract_h265_from_annexb(data: &[u8], info: &mut WireProbeInfo) {
    for nal in split_annexb(data) {
        if nal.is_empty() {
            continue;
        }
        let nal_type = (nal[0] >> 1) & 0x3f;
        if nal_type == 33 {
            apply_h265_sps(nal, info);
        }
    }
}

fn split_annexb(data: &[u8]) -> Vec<&[u8]> {
    let mut nals = Vec::new();
    let mut i = 0usize;
    while i + 3 < data.len() {
        let start = if data[i..].starts_with(&[0, 0, 0, 1]) {
            i + 4
        } else if data[i..].starts_with(&[0, 0, 1]) {
            i + 3
        } else {
            i += 1;
            continue;
        };
        let mut end = start;
        while end + 3 < data.len() {
            if data[end..].starts_with(&[0, 0, 0, 1]) || data[end..].starts_with(&[0, 0, 1]) {
                break;
            }
            end += 1;
        }
        if end + 3 >= data.len() {
            end = data.len();
        }
        if end > start {
            nals.push(&data[start..end]);
        }
        i = end;
    }
    nals
}

fn parse_adts_header(data: &[u8], info: &mut WireProbeInfo) {
    if data.len() < 7 {
        return;
    }
    for i in 0..=data.len() - 7 {
        if data[i] == 0xff && data[i + 1] & 0xf0 == 0xf0 {
            let sr_idx = ((data[i + 2] & 0x3c) >> 2) as usize;
            let rates = [
                96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000,
                7350,
            ];
            let channels = ((data[i + 2] & 1) << 2) | ((data[i + 3] & 0xc0) >> 6);
            if sr_idx < rates.len() {
                info.audio_sample_rate = Some(rates[sr_idx]);
            }
            if channels > 0 {
                info.audio_channels = Some(channels);
            }
            break;
        }
    }
}

struct BitReader<'a> {
    data: &'a [u8],
    bit: usize,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, bit: 0 }
    }

    fn read_bit(&mut self) -> Option<u32> {
        if self.bit / 8 >= self.data.len() {
            return None;
        }
        let b = self.data[self.bit / 8];
        let v = ((b >> (7 - (self.bit % 8))) & 1) as u32;
        self.bit += 1;
        Some(v)
    }

    fn read_bits(&mut self, n: usize) -> Option<u32> {
        let mut v = 0u32;
        for _ in 0..n {
            v = (v << 1) | self.read_bit()?;
        }
        Some(v)
    }

    fn read_ue(&mut self) -> Option<u32> {
        let mut zeros = 0usize;
        while self.read_bit()? == 0 {
            zeros += 1;
            if zeros > 31 {
                return None;
            }
        }
        let info = self.read_bits(zeros).unwrap_or(0);
        Some(((1u32 << zeros) - 1).saturating_add(info))
    }

    fn read_se(&mut self) -> Option<i32> {
        let v = self.read_ue()? as i32;
        Some(if v & 1 != 0 { (v + 1) / 2 } else { -(v / 2) })
    }
}

/// Remove emulation prevention bytes (0x000003 → 0x0000).
fn rbsp_from_nal(nal: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(nal.len());
    let mut i = 0usize;
    while i < nal.len() {
        if i + 2 < nal.len() && nal[i] == 0 && nal[i + 1] == 0 && nal[i + 2] == 3 {
            out.push(0);
            out.push(0);
            i += 3;
        } else {
            out.push(nal[i]);
            i += 1;
        }
    }
    out
}

fn apply_h264_sps(nal: &[u8], info: &mut WireProbeInfo) {
    let body = if !nal.is_empty() && nal[0] & 0x1f == 7 {
        &nal[1..]
    } else {
        nal
    };
    let rbsp = rbsp_from_nal(body);
    let mut br = BitReader::new(&rbsp);
    let profile_idc = match br.read_bits(8) {
        Some(v) => v as u8,
        None => return,
    };
    let _constraint = br.read_bits(8);
    let level_idc = match br.read_bits(8) {
        Some(v) => v as u8,
        None => return,
    };
    info.profile_level = Some(format!("avc1.{profile_idc:02x}{level_idc:02x}"));
    if info.codec.is_none() {
        info.codec = Some("avc1".into());
    }
    let _sps_id = br.read_ue();
    let mut chroma_format_idc = 1u32;
    if matches!(
        profile_idc,
        100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134
    ) {
        chroma_format_idc = br.read_ue().unwrap_or(1);
        if chroma_format_idc == 3 {
            let _ = br.read_bit();
        }
        let _ = br.read_ue();
        let _ = br.read_ue();
        let _ = br.read_bit();
        if br.read_bit().unwrap_or(0) == 1 {
            let count = if chroma_format_idc != 3 { 8 } else { 12 };
            for i in 0..count {
                if br.read_bit().unwrap_or(0) == 1 {
                    let size = if i < 6 { 16 } else { 64 };
                    let mut next = 8i32;
                    for _ in 0..size {
                        let delta = br.read_se().unwrap_or(0);
                        next = (next + delta + 256) % 256;
                    }
                }
            }
        }
    }
    let _log2_max_frame_num = br.read_ue();
    let poc_type = br.read_ue().unwrap_or(0);
    if poc_type == 0 {
        let _ = br.read_ue();
    } else if poc_type == 1 {
        let _ = br.read_bit();
        let _ = br.read_se();
        let _ = br.read_se();
        let n = br.read_ue().unwrap_or(0) as usize;
        for _ in 0..n {
            let _ = br.read_se();
        }
    }
    let _ = br.read_ue();
    let _ = br.read_bit();
    let w_mbs = br.read_ue().unwrap_or(0);
    let h_map = br.read_ue().unwrap_or(0);
    let frame_mbs_only = br.read_bit().unwrap_or(1);
    if frame_mbs_only == 0 {
        let _ = br.read_bit();
    }
    let _ = br.read_bit();
    let mut crop_l = 0u32;
    let mut crop_r = 0u32;
    let mut crop_t = 0u32;
    let mut crop_b = 0u32;
    if br.read_bit().unwrap_or(0) == 1 {
        crop_l = br.read_ue().unwrap_or(0);
        crop_r = br.read_ue().unwrap_or(0);
        crop_t = br.read_ue().unwrap_or(0);
        crop_b = br.read_ue().unwrap_or(0);
    }
    let width = ((w_mbs + 1) * 16).saturating_sub((crop_l + crop_r) * 2);
    let height = ((2 - frame_mbs_only) * (h_map + 1) * 16).saturating_sub((crop_t + crop_b) * 2);
    if width > 0 && height > 0 {
        info.width = Some(width);
        info.height = Some(height);
    }
    if br.read_bit().unwrap_or(0) == 1 {
        if br.read_bit().unwrap_or(0) == 1 {
            let aspect = br.read_bits(8).unwrap_or(0);
            if aspect == 255 {
                let _ = br.read_bits(16);
                let _ = br.read_bits(16);
            }
        }
        if br.read_bit().unwrap_or(0) == 1 {
            let _ = br.read_bit();
        }
        if br.read_bit().unwrap_or(0) == 1 {
            let _ = br.read_bits(3);
            let _ = br.read_bit();
            if br.read_bit().unwrap_or(0) == 1 {
                let _ = br.read_bits(8);
                let _ = br.read_bits(8);
                let _ = br.read_bits(8);
            }
        }
        if br.read_bit().unwrap_or(0) == 1 {
            let _ = br.read_ue();
            let _ = br.read_ue();
        }
        if br.read_bit().unwrap_or(0) == 1 {
            let num_units = br.read_bits(32).unwrap_or(0);
            let time_scale = br.read_bits(32).unwrap_or(0);
            let fixed = br.read_bit().unwrap_or(0);
            if num_units > 0 && time_scale > 0 {
                let mut fps = time_scale as f64 / (2.0 * num_units as f64);
                if fixed == 0 {
                    fps = time_scale as f64 / num_units as f64;
                }
                if fps > 0.0 && fps < 120.0 {
                    info.frame_rate = Some(fps);
                }
            }
        }
    }
    let _ = chroma_format_idc;
}

fn apply_h265_sps(nal: &[u8], info: &mut WireProbeInfo) {
    if nal.len() <= 2 {
        return;
    }
    let body = &nal[2..];
    let rbsp = rbsp_from_nal(body);
    let mut br = BitReader::new(&rbsp);
    let _vps_id = br.read_bits(4);
    let max_sub_layers = br.read_bits(3).unwrap_or(0);
    let _ = br.read_bit();
    let _ = br.read_bits(2);
    let _ = br.read_bit();
    let profile = br.read_bits(5).unwrap_or(0);
    let _ = br.read_bits(32);
    let _ = br.read_bits(48);
    let level = br.read_bits(8).unwrap_or(0);
    info.profile_level = Some(format!("hvc1.{profile}.{level}"));
    if info.codec.is_none() {
        info.codec = Some("hvc1".into());
    }
    for _ in 0..max_sub_layers {
        let _ = br.read_bit();
        let _ = br.read_bit();
    }
    let _sps_id = br.read_ue();
    let chroma = br.read_ue().unwrap_or(1);
    if chroma == 3 {
        let _ = br.read_bit();
    }
    let width = br.read_ue().unwrap_or(0);
    let height = br.read_ue().unwrap_or(0);
    if width > 0 && height > 0 && width < 8192 && height < 8192 {
        info.width = Some(width);
        info.height = Some(height);
    }
}

/// Compare manifest ABR fields against wire probe; returns human-readable mismatch lines.
pub fn manifest_wire_mismatches(
    manifest_res: Option<&str>,
    manifest_fps: Option<f64>,
    manifest_codecs: Option<&str>,
    wire: &WireProbeInfo,
) -> Vec<String> {
    let mut out = Vec::new();
    if let (Some(mres), Some(w), Some(h)) = (manifest_res, wire.width, wire.height) {
        let wire_res = format!("{w}x{h}");
        let norm = |s: &str| s.to_ascii_lowercase().replace('×', "x");
        if norm(mres) != norm(&wire_res) && (mres.contains('x') || mres.contains('×')) {
            out.push(format!(
                "[MISMATCH] Manifest declares {mres}, wire stream is {wire_res}"
            ));
        }
    }
    if let (Some(mfps), Some(wfps)) = (manifest_fps, wire.frame_rate) {
        if (mfps - wfps).abs() > 0.5 {
            out.push(format!(
                "[MISMATCH] Manifest declares {mfps:.2} FPS, wire stream is {wfps:.2} FPS"
            ));
        }
    }
    if let (Some(mc), Some(wc)) = (manifest_codecs, wire.codec.as_deref()) {
        let m = mc.to_ascii_lowercase();
        if !m.contains(wc) && !wc.contains(m.split('.').next().unwrap_or("")) {
            let m_avc = m.contains("avc") || m.contains("h264");
            let m_hevc = m.contains("hvc") || m.contains("hev") || m.contains("h265");
            let w_avc = wc.contains("avc");
            let w_hevc = wc.contains("hvc") || wc.contains("hev");
            if (m_avc && w_hevc) || (m_hevc && w_avc) {
                out.push(format!(
                    "[MISMATCH] Manifest declares codec {mc}, wire stream is {wc}"
                ));
            }
        }
    }
    out
}

/// Fill missing ABR display fields from wire; returns whether any field was wire-sourced.
pub fn fill_abr_from_wire(
    resolution: &mut Option<String>,
    frame_rate: &mut Option<f64>,
    codecs: &mut Option<String>,
    wire: &WireProbeInfo,
) -> bool {
    let mut filled = false;
    if resolution.is_none() {
        if let (Some(w), Some(h)) = (wire.width, wire.height) {
            *resolution = Some(format!("{w}x{h}"));
            filled = true;
        }
    }
    if frame_rate.is_none() {
        if let Some(fps) = wire.frame_rate {
            *frame_rate = Some(fps);
            filled = true;
        }
    }
    if codecs.is_none() {
        if let Some(c) = wire.profile_level.as_ref().or(wire.codec.as_ref()) {
            *codecs = Some(c.clone());
            filled = true;
        }
    }
    filled
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_ts_sync() {
        let mut buf = vec![0u8; 188];
        buf[0] = 0x47;
        assert_eq!(classify_container(&buf), ContainerKind::Ts);
    }

    #[test]
    fn classify_fmp4_ftyp() {
        let mut buf = vec![0u8; 32];
        buf[4..8].copy_from_slice(b"ftyp");
        assert_eq!(classify_container(&buf), ContainerKind::Fmp4);
    }

    #[test]
    fn mismatch_resolution() {
        let wire = WireProbeInfo {
            width: Some(1280),
            height: Some(720),
            ..Default::default()
        };
        let msgs = manifest_wire_mismatches(Some("1920x1080"), None, None, &wire);
        assert!(msgs[0].contains("MISMATCH"));
        assert!(msgs[0].contains("1280x720"));
    }

    #[test]
    fn fill_missing_fps() {
        let wire = WireProbeInfo {
            frame_rate: Some(25.0),
            width: Some(1920),
            height: Some(1080),
            ..Default::default()
        };
        let mut res = None;
        let mut fps = None;
        let mut codecs = None;
        assert!(fill_abr_from_wire(&mut res, &mut fps, &mut codecs, &wire));
        assert_eq!(res.as_deref(), Some("1920x1080"));
        assert_eq!(fps, Some(25.0));
    }

    #[test]
    fn adts_sample_rate() {
        let hdr = [0xff, 0xf1, 0x4c, 0x80, 0x01, 0xbf, 0xfc];
        let mut info = WireProbeInfo::default();
        parse_adts_header(&hdr, &mut info);
        assert_eq!(info.audio_sample_rate, Some(48000));
    }
}
