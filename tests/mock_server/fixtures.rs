//! Synthetic MPEG-TS / fMP4 bytes for hermetic E2E and integration tests.

#![allow(clippy::cast_possible_truncation)] // fixed-width TS packet layout

const TS: usize = 188;
const SYNC: u8 = 0x47;

fn ts_packet(pid: u16, cc: u8, payload: &[u8], with_pcr: bool) -> [u8; TS] {
    let mut pkt = [0u8; TS];
    pkt[0] = SYNC;
    pkt[1] = ((pid >> 8) as u8) & 0x1f;
    pkt[2] = (pid & 0xff) as u8;
    if with_pcr {
        pkt[3] = 0x30 | (cc & 0x0f);
        pkt[4] = 183;
        pkt[5] = 0x10;
        let pcr_base = 1_000_000u64;
        pkt[6] = ((pcr_base >> 25) & 0xff) as u8;
        pkt[7] = ((pcr_base >> 17) & 0xff) as u8;
        pkt[8] = ((pcr_base >> 9) & 0xff) as u8;
        pkt[9] = ((pcr_base >> 1) & 0xff) as u8;
        pkt[10] = (((pcr_base & 1) << 7) | 0x7e) as u8;
        let start = 13usize;
        let copy = payload.len().min(TS - start);
        pkt[start..start + copy].copy_from_slice(&payload[..copy]);
    } else {
        pkt[3] = 0x10 | (cc & 0x0f);
        let copy = payload.len().min(TS - 4);
        pkt[4..4 + copy].copy_from_slice(&payload[..copy]);
    }
    pkt
}

const fn ts_packet_pcr(pid: u16, cc: u8, pcr_base: u64) -> [u8; TS] {
    let mut pkt = [0u8; TS];
    pkt[0] = SYNC;
    pkt[1] = ((pid >> 8) as u8) & 0x1f;
    pkt[2] = (pid & 0xff) as u8;
    pkt[3] = 0x30 | (cc & 0x0f);
    pkt[4] = 183;
    pkt[5] = 0x10;
    pkt[6] = ((pcr_base >> 25) & 0xff) as u8;
    pkt[7] = ((pcr_base >> 17) & 0xff) as u8;
    pkt[8] = ((pcr_base >> 9) & 0xff) as u8;
    pkt[9] = ((pcr_base >> 1) & 0xff) as u8;
    pkt[10] = (((pcr_base & 1) << 7) | 0x7e) as u8;
    pkt
}

fn pat_section() -> Vec<u8> {
    let mut s = vec![0u8; 16];
    s[0] = 0x00;
    s[1] = 0xB0;
    s[2] = 0x0D;
    s[3] = 0x00;
    s[4] = 0x01;
    s[5] = 0xC1;
    s[6] = 0x00;
    s[7] = 0x00;
    s[8] = 0x00;
    s[9] = 0x01;
    s[10] = 0xE0;
    s[11] = 0x10;
    s
}

fn pmt_section() -> Vec<u8> {
    let mut s = vec![0u8; 21];
    s[0] = 0x02;
    s[1] = 0xB0;
    s[2] = 0x12;
    s[3] = 0x00;
    s[4] = 0x01;
    s[5] = 0xC1;
    s[6] = 0x00;
    s[7] = 0x00;
    s[8] = 0xE1;
    s[9] = 0x00;
    s[10] = 0xF0;
    s[11] = 0x00;
    s[12] = 0x1B;
    s[13] = 0xE1;
    s[14] = 0x00;
    s[15] = 0xF0;
    s[16] = 0x00;
    s
}

fn pes_with_annex_b(nal_body: &[u8]) -> Vec<u8> {
    const PES_HDR_LEN: u8 = 5;
    let mut pes = vec![0u8; 9 + PES_HDR_LEN as usize + nal_body.len()];
    pes[0..4].copy_from_slice(&[0, 0, 0, 1]);
    pes[4] = 0xE0;
    pes[7] = 0x80;
    pes[8] = PES_HDR_LEN;
    pes[9..14].copy_from_slice(&[0x00, 0x00, 0x01, 0x09, 0x80]);
    pes[14..14 + nal_body.len()].copy_from_slice(nal_body);
    pes
}

/// MPEG-TS with sync loss, CC errors, orphan PID, and PCR gap > 40ms.
pub fn tr101290_broken_ts() -> Vec<u8> {
    let mut out = Vec::new();
    let mut bad = [0u8; TS];
    bad[0] = 0x00;
    out.extend_from_slice(&bad);

    out.extend_from_slice(&ts_packet(0x0000, 0, &pat_section(), false));
    out.extend_from_slice(&ts_packet(0x0010, 0, &pmt_section(), false));
    out.extend_from_slice(&ts_packet(0x0100, 0, &[0xFF; 8], false));
    out.extend_from_slice(&ts_packet(
        0x0101,
        0,
        &pes_with_annex_b(&[0x00, 0x00, 0x00, 0x01, 0x09, 0x10]),
        false,
    ));
    out.extend_from_slice(&ts_packet(
        0x0101,
        7,
        &pes_with_annex_b(&[0x00, 0x00, 0x00, 0x01, 0x09, 0x30]),
        false,
    ));
    out.extend_from_slice(&ts_packet_pcr(0x0101, 1, 1_000_000));
    out.extend_from_slice(&ts_packet_pcr(0x0101, 2, 1_005_000));
    out
}

fn sei_nal(payload_type: u8, payload: &[u8]) -> Vec<u8> {
    let mut body = vec![0x06, payload_type, payload.len() as u8];
    body.extend_from_slice(payload);
    body.push(0x80);
    let mut annex = vec![0x00, 0x00, 0x00, 0x01];
    annex.extend_from_slice(&body);
    annex
}

/// TS PES with ATSC captions + content light level SEI.
pub fn sei_caption_hdr_ts() -> Vec<u8> {
    let atsc = [0xB5u8, 0x00, 0x31, 0x81, 0x00, 0x00];
    let cll = [0x03u8, 0xE8, 0x01, 0xF4];
    let mut nals = sei_nal(4, &atsc);
    nals.extend_from_slice(&sei_nal(144, &cll));
    let pes = pes_with_annex_b(&nals);
    let mut out = Vec::new();
    out.extend_from_slice(&ts_packet(0x0000, 0, &pat_section(), false));
    out.extend_from_slice(&ts_packet(0x0010, 0, &pmt_section(), false));
    out.extend_from_slice(&ts_packet(0x0101, 0, &pes, false));
    out
}

/// Minimal fMP4 with `ftyp` + `mdat` holding length-prefixed SEI NAL.
pub fn sei_fmp4_m4s() -> Vec<u8> {
    let atsc = [0xB5u8, 0x00, 0x31, 0x81, 0x00, 0x00];
    let nal = sei_nal(4, &atsc);
    let mut mdat = Vec::new();
    let nlen = (nal.len() as u32).to_be_bytes();
    mdat.extend_from_slice(&nlen);
    mdat.extend_from_slice(&nal);
    let mut out = Vec::new();
    out.extend_from_slice(&[0, 0, 0, 32]);
    out.extend_from_slice(b"ftyp");
    out.extend_from_slice(b"isom");
    out.extend_from_slice(&[0, 0, 0, 0]);
    out.extend_from_slice(b"isom");
    out.extend_from_slice(b"iso2");
    let mdat_len = (8 + mdat.len()) as u32;
    out.extend_from_slice(&mdat_len.to_be_bytes());
    out.extend_from_slice(b"mdat");
    out.extend_from_slice(&mdat);
    out
}

pub fn minimal_ts_packet(seq: u64) -> Vec<u8> {
    ts_packet(0x0101, (seq % 16) as u8, &[0xFF; 8], false).to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tr101290_fixture_has_packets() {
        let b = tr101290_broken_ts();
        assert!(b.len() >= TS * 3);
        assert_eq!(b[0], 0x00);
        assert!(b[TS..].contains(&SYNC));
    }

    #[test]
    fn sei_fixture_has_annex_b() {
        let b = sei_caption_hdr_ts();
        assert!(b.windows(4).any(|w| w == [0, 0, 0, 1]));
    }
}
