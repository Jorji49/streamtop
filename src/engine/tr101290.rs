//! ETSI TR 101 290 Priority 1 and 2 MPEG-TS compliance engine.

use std::collections::{HashMap, HashSet, VecDeque};

use crate::models::{ContainerKind, Tr101290Check, Tr101290Report};

pub const TS_PACKET_SIZE: usize = 188;
pub const SYNC_BYTE: u8 = 0x47;
pub const PAT_PID: u16 = 0x0000;
pub const PCR_MAX_GAP_MS: f64 = 40.0;
pub const PAT_PMT_TIMEOUT_MS: u64 = 500;
pub const MAX_VIOLATIONS: usize = 32;

#[derive(Debug, Default)]
struct PidCcState {
    last_cc: Option<u8>,
    last_payload_sig: Option<u64>,
}

#[derive(Debug, Default)]
pub struct Tr101290Engine {
    cc: HashMap<u16, PidCcState>,
    pmt_pids: HashSet<u16>,
    stream_pids: HashSet<u16>,
    seen_pids: HashSet<u16>,
    last_pcr_base: Option<u64>,
    last_pcr_wall_ms: u64,
    pcr_intervals_ms: VecDeque<f64>,
    last_pts: HashMap<u16, u64>,
    last_pat_ms: Option<u64>,
    last_pmt_ms: Option<u64>,
    wall_ms: u64,
    report: Tr101290Report,
}

impl Tr101290Engine {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn ingest(&mut self, bytes: &[u8], wall_ms: u64) -> Tr101290Report {
        self.wall_ms = wall_ms;
        if bytes.is_empty() {
            return self.report.clone();
        }
        self.scan_sync(bytes);
        for pkt in bytes
            .chunks(TS_PACKET_SIZE)
            .filter(|p| p.len() == TS_PACKET_SIZE && p[0] == SYNC_BYTE)
        {
            self.process_packet(pkt);
        }
        self.check_pat_pmt_timeouts();
        self.check_unreferenced_pids();
        self.report.clone()
    }

    pub fn snapshot(&self) -> Tr101290Report {
        self.report.clone()
    }

    fn scan_sync(&mut self, bytes: &[u8]) {
        let mut offset = 0usize;
        while offset + TS_PACKET_SIZE <= bytes.len() {
            if bytes[offset] != SYNC_BYTE {
                self.bump_sync_error(offset);
                offset += 1;
                continue;
            }
            offset += TS_PACKET_SIZE;
        }
    }

    fn bump_sync_error(&mut self, offset: usize) {
        self.report.sync_errors = self.report.sync_errors.saturating_add(1);
        self.add_p1(
            "P1_SYNC",
            format!("TS sync byte missing at offset {offset} (expected 0x47)"),
        );
    }

    fn process_packet(&mut self, pkt: &[u8]) {
        if pkt.len() != TS_PACKET_SIZE || pkt[0] != SYNC_BYTE {
            return;
        }
        let pid = packet_pid(pkt);
        self.seen_pids.insert(pid);
        let adaptation = (pkt[3] >> 4) & 0x3;
        self.track_cc(pid, pkt, adaptation);
        if let Some(payload) = ts_payload(pkt) {
            self.parse_tables(pid, payload);
            self.track_pts(pid, payload);
        }
        if let Some(pcr) = parse_pcr(pkt) {
            self.track_pcr(pcr);
        }
    }

    fn track_cc(&mut self, pid: u16, pkt: &[u8], adaptation: u8) {
        if adaptation == 0 || adaptation == 2 {
            return;
        }
        let cc = pkt[3] & 0x0f;
        let mut violation: Option<(u8, String)> = None;
        {
            let entry = self.cc.entry(pid).or_default();
            if let Some(prev) = entry.last_cc {
                let expected = (prev + 1) & 0x0f;
                if cc != prev && cc != expected {
                    if let Some(payload) = ts_payload(pkt) {
                        let sig = payload_signature(payload);
                        if entry.last_payload_sig == Some(sig) && cc == prev {
                            violation = Some((
                                2,
                                format!("PID 0x{pid:04X}: duplicate CC with identical payload"),
                            ));
                        } else {
                            self.report.cc_errors = self.report.cc_errors.saturating_add(1);
                            violation = Some((
                                1,
                                format!(
                                    "PID 0x{pid:04X}: CC discontinuity (prev={prev}, got={cc}, expected={expected})"
                                ),
                            ));
                        }
                    } else {
                        self.report.cc_errors = self.report.cc_errors.saturating_add(1);
                        violation = Some((
                            1,
                            format!("PID 0x{pid:04X}: CC discontinuity (prev={prev}, got={cc})"),
                        ));
                    }
                }
            }
            if let Some(payload) = ts_payload(pkt) {
                entry.last_payload_sig = Some(payload_signature(payload));
            }
            entry.last_cc = Some(cc);
        }
        if let Some((pri, msg)) = violation {
            if pri == 1 {
                self.add_p1("P1_CC", msg);
            } else {
                self.add_p2("P2_DUP_CC", msg);
            }
        }
    }

    fn parse_tables(&mut self, pid: u16, payload: &[u8]) {
        if payload.len() < 4 || payload[0] != 0 {
            return;
        }
        let table_id = payload[0];
        if pid == PAT_PID && table_id == 0x00 {
            self.last_pat_ms = Some(self.wall_ms);
            if let Some(pmt_pid) = parse_pat_pmt_pid(payload) {
                self.pmt_pids.insert(pmt_pid);
            }
        }
        if self.pmt_pids.contains(&pid) && table_id == 0x02 {
            self.last_pmt_ms = Some(self.wall_ms);
            for es_pid in parse_pmt_es_pids(payload) {
                self.stream_pids.insert(es_pid);
            }
        }
    }

    fn track_pcr(&mut self, pcr_base: u64) {
        if let Some(prev) = self.last_pcr_base {
            let delta_ticks = pcr_base.saturating_sub(prev);
            let gap_ms = delta_ticks as f64 / 90.0;
            self.pcr_intervals_ms.push_back(gap_ms);
            while self.pcr_intervals_ms.len() > 64 {
                self.pcr_intervals_ms.pop_front();
            }
            if gap_ms > PCR_MAX_GAP_MS {
                self.report.pcr_gap_ms = Some(gap_ms);
                self.add_p2(
                    "P2_PCR_GAP",
                    format!("PCR gap {gap_ms:.2}ms exceeds {PCR_MAX_GAP_MS}ms"),
                );
            }
            if self.pcr_intervals_ms.len() >= 3 {
                let mean: f64 =
                    self.pcr_intervals_ms.iter().sum::<f64>() / self.pcr_intervals_ms.len() as f64;
                let var: f64 = self
                    .pcr_intervals_ms
                    .iter()
                    .map(|v| {
                        let d = *v - mean;
                        d * d
                    })
                    .sum::<f64>()
                    / self.pcr_intervals_ms.len() as f64;
                let jitter = var.sqrt();
                self.report.pcr_jitter_ms = Some(jitter);
                if jitter > 10.0 {
                    self.add_p2(
                        "P2_PCR_JITTER",
                        format!("PCR jitter {jitter:.2}ms against nominal clock"),
                    );
                }
            }
        }
        self.last_pcr_base = Some(pcr_base);
        self.last_pcr_wall_ms = self.wall_ms;
    }

    fn track_pts(&mut self, pid: u16, payload: &[u8]) {
        if payload.len() < 14 {
            return;
        }
        if payload[0] != 0 || payload[1] != 0 || payload[2] != 1 {
            return;
        }
        let stream_id = payload[3];
        if !(0xE0..=0xEF).contains(&stream_id) {
            return;
        }
        let Some(pts) = parse_pes_pts(payload) else {
            return;
        };
        if let Some(prev) = self.last_pts.insert(pid, pts) {
            let jump_ms = (pts.abs_diff(prev) as f64) / 90.0;
            if jump_ms > 5_000.0 {
                self.report.pts_discontinuities = self.report.pts_discontinuities.saturating_add(1);
                self.add_p2(
                    "P2_PTS_DISC",
                    format!("PID 0x{pid:04X}: PTS jump {jump_ms:.0}ms"),
                );
            }
        }
    }

    fn check_pat_pmt_timeouts(&mut self) {
        if self.last_pat_ms.is_none() {
            self.report.pat_timeout = true;
            self.add_p1("P1_PAT", "PAT not received within 500ms window".into());
        } else if let Some(last) = self.last_pat_ms {
            if self.wall_ms.saturating_sub(last) > PAT_PMT_TIMEOUT_MS {
                self.report.pat_timeout = true;
                self.add_p1("P1_PAT", "PAT table stale (>500ms)".into());
            }
        }
        if !self.pmt_pids.is_empty() && self.last_pmt_ms.is_none() {
            self.report.pmt_timeout = true;
            self.add_p1("P1_PMT", "PMT not received within 500ms window".into());
        } else if let Some(last) = self.last_pmt_ms {
            if self.wall_ms.saturating_sub(last) > PAT_PMT_TIMEOUT_MS {
                self.report.pmt_timeout = true;
                self.add_p1("P1_PMT", "PMT table stale (>500ms)".into());
            }
        }
    }

    fn check_unreferenced_pids(&mut self) {
        if self.stream_pids.is_empty() {
            return;
        }
        let mut missing = 0u32;
        let orphan_pids: Vec<u16> = self
            .seen_pids
            .iter()
            .copied()
            .filter(|pid| {
                *pid != PAT_PID
                    && !self.pmt_pids.contains(pid)
                    && !self.stream_pids.contains(pid)
                    && is_media_pid(*pid)
            })
            .collect();
        for pid in orphan_pids {
            missing = missing.saturating_add(1);
            self.add_p1(
                "P1_PID",
                format!("PID 0x{pid:04X} present but not referenced in PMT"),
            );
        }
        self.report.unreferenced_pids = missing;
    }

    fn add_p1(&mut self, code: &str, message: String) {
        self.report.p1_violations = self.report.p1_violations.saturating_add(1);
        self.push_check(1, code, message);
    }

    fn add_p2(&mut self, code: &str, message: String) {
        self.report.p2_violations = self.report.p2_violations.saturating_add(1);
        self.push_check(2, code, message);
    }

    fn push_check(&mut self, priority: u8, code: &str, message: String) {
        if self.report.checks.len() >= MAX_VIOLATIONS {
            return;
        }
        self.report.checks.push(Tr101290Check {
            priority,
            code: code.into(),
            message,
        });
    }
}

pub fn analyze_ts_chunk(engine: &mut Tr101290Engine, bytes: &[u8], wall_ms: u64) -> Tr101290Report {
    engine.ingest(bytes, wall_ms)
}

pub fn probe_container_tr101290(
    engine: &mut Tr101290Engine,
    bytes: &[u8],
    container: ContainerKind,
    wall_ms: u64,
) -> Option<Tr101290Report> {
    if matches!(container, ContainerKind::Ts) || looks_like_ts(bytes) {
        Some(analyze_ts_chunk(engine, bytes, wall_ms))
    } else {
        None
    }
}

fn looks_like_ts(bytes: &[u8]) -> bool {
    bytes
        .chunks(TS_PACKET_SIZE)
        .any(|p| p.len() == TS_PACKET_SIZE && p[0] == SYNC_BYTE)
}

fn packet_pid(pkt: &[u8]) -> u16 {
    u16::from_be_bytes([pkt[1] & 0x1f, pkt[2]])
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

fn parse_pcr(pkt: &[u8]) -> Option<u64> {
    let adaptation = (pkt[3] >> 4) & 0x3;
    if adaptation != 2 && adaptation != 3 {
        return None;
    }
    let afl = *pkt.get(4)? as usize;
    if afl < 7 || pkt.len() < 5 + afl {
        return None;
    }
    if pkt[5] & 0x10 == 0 {
        return None;
    }
    let base = u64::from(pkt[6]) << 25
        | u64::from(pkt[7]) << 17
        | u64::from(pkt[8]) << 9
        | u64::from(pkt[9]) << 1
        | u64::from(pkt[10] >> 7);
    Some(base)
}

fn parse_pes_pts(payload: &[u8]) -> Option<u64> {
    if payload.len() < 14 {
        return None;
    }
    let flags = payload[7];
    if flags & 0x80 == 0 {
        return None;
    }
    let pts_bytes = crate::engine::slice_util::subslice_len(payload, 9, 5)?;
    let pts = u64::from(pts_bytes[0] >> 1 & 0x07) << 30
        | u64::from(pts_bytes[1]) << 22
        | u64::from(pts_bytes[2] >> 1) << 15
        | u64::from(pts_bytes[3]) << 7
        | u64::from(pts_bytes[4] >> 1);
    Some(pts)
}

fn parse_pat_pmt_pid(section: &[u8]) -> Option<u16> {
    if section.len() < 12 {
        return None;
    }
    let section_length = u16::from_be_bytes([section[1] & 0x0f, section[2]]) as usize;
    let end = (3 + section_length).min(section.len());
    let mut i = 8usize;
    while i + 4 <= end {
        let program = u16::from_be_bytes([section[i], section[i + 1]]);
        let pmt_pid = u16::from_be_bytes([section[i + 2] & 0x1f, section[i + 3]]);
        if program != 0 {
            return Some(pmt_pid);
        }
        i += 4;
    }
    None
}

fn parse_pmt_es_pids(section: &[u8]) -> Vec<u16> {
    let mut out = Vec::new();
    if section.len() < 12 {
        return out;
    }
    let section_length = u16::from_be_bytes([section[1] & 0x0f, section[2]]) as usize;
    let program_info_len = u16::from_be_bytes([section[10] & 0x0f, section[11]]) as usize;
    let mut i = 12 + program_info_len;
    let end = (3 + section_length).min(section.len());
    while i + 5 <= end {
        let es_pid = u16::from_be_bytes([section[i + 1] & 0x1f, section[i + 2]]);
        out.push(es_pid);
        let desc_len = u16::from_be_bytes([section[i + 3] & 0x0f, section[i + 4]]) as usize;
        i += 5 + desc_len;
    }
    out
}

fn payload_signature(payload: &[u8]) -> u64 {
    let take = payload.len().min(16);
    let mut sig = 0u64;
    for (i, b) in payload[..take].iter().enumerate() {
        sig ^= u64::from(*b) << ((i % 8) * 8);
    }
    sig
}

fn is_media_pid(pid: u16) -> bool {
    (0x0010..=0x1FFE).contains(&pid)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts_packet(pid: u16, cc: u8, payload: &[u8]) -> Vec<u8> {
        let mut pkt = vec![0u8; TS_PACKET_SIZE];
        pkt[0] = SYNC_BYTE;
        pkt[1] = ((pid >> 8) as u8) & 0x1f;
        pkt[2] = (pid & 0xff) as u8;
        pkt[3] = 0x10 | (cc & 0x0f);
        let copy_len = payload.len().min(TS_PACKET_SIZE - 4);
        pkt[4..4 + copy_len].copy_from_slice(&payload[..copy_len]);
        pkt
    }

    fn pat_packet() -> Vec<u8> {
        let mut section = vec![0u8; 16];
        section[0] = 0x00;
        section[1] = 0xB0;
        section[2] = 0x0D;
        section[3] = 0x00;
        section[4] = 0x01;
        section[5] = 0xC1;
        section[6] = 0x00;
        section[7] = 0x00;
        section[8] = 0x00;
        section[9] = 0x01;
        section[10] = 0xE0;
        section[11] = 0x10;
        ts_packet(PAT_PID, 0, &section)
    }

    #[test]
    fn cc_discontinuity_is_p1() {
        let mut engine = Tr101290Engine::new();
        let p1 = pat_packet();
        let mut p2 = pat_packet();
        p2[3] = 0x15;
        let mut buf = p1;
        buf.extend_from_slice(&p2);
        let report = engine.ingest(&buf, 0);
        assert!(report.p1_violations > 0 || report.cc_errors > 0);
    }

    #[test]
    fn sync_error_detected() {
        let mut engine = Tr101290Engine::new();
        let mut buf = vec![0u8; 188];
        buf[0] = 0x00;
        let report = engine.ingest(&buf, 0);
        assert!(report.sync_errors > 0);
    }

    #[test]
    fn pat_seen_avoids_timeout() {
        let mut engine = Tr101290Engine::new();
        let report = engine.ingest(&pat_packet(), 100);
        assert!(!report.pat_timeout);
    }
}
