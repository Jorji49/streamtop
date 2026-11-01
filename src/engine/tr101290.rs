//! ETSI TR 101 290 Priority 1 and 2 MPEG-TS checks on segment probe windows.
//!
//! Each call to [`Tr101290Engine::ingest`] is one probe window: the first bytes
//! of one media segment. Windows are not contiguous (only a prefix of each
//! segment is read and segments may be skipped), so continuity counter, PCR,
//! and PTS state restarts at every window. Repetition checks for PAT and PMT
//! use the stream clock (PCR) inside the window, not wall-clock time.

use std::collections::{HashMap, HashSet};

use crate::models::{ContainerKind, Tr101290Check, Tr101290Report};

pub const TS_PACKET_SIZE: usize = 188;
pub const SYNC_BYTE: u8 = 0x47;
pub const PAT_PID: u16 = 0x0000;
pub const PCR_MAX_GAP_MS: f64 = 40.0;
pub const PCR_JITTER_ALARM_MS: f64 = 100.0;
pub const PAT_PMT_TIMEOUT_MS: u64 = 500;
pub const MAX_VIOLATIONS: usize = 32;
const PTS_JUMP_MS: f64 = 5_000.0;
/// 33-bit PCR base and PTS wrap at 2^33 ticks of 90 kHz.
const WRAP_33: u64 = 1 << 33;

#[derive(Debug, Default)]
struct PidCcState {
    last_cc: Option<u8>,
    last_payload_sig: Option<u64>,
}

/// Per-window state. Reset at every ingest.
#[derive(Debug, Default)]
struct WindowState {
    cc: HashMap<u16, PidCcState>,
    last_pcr: Option<(u16, u64)>,
    first_pcr: Option<u64>,
    pcr_intervals_ms: Vec<f64>,
    pcr_gaps: Vec<(u16, f64)>,
    last_pts: HashMap<u16, u64>,
    pat_at: Option<u64>,
    pmt_at: Option<u64>,
    pat_seen: bool,
    pmt_seen: bool,
}

#[derive(Debug, Default)]
pub struct Tr101290Engine {
    win: WindowState,
    pmt_pids: HashSet<u16>,
    referenced_pids: HashSet<u16>,
    reported_orphans: HashSet<u16>,
    seen_pids: HashSet<u16>,
    new_checks: Vec<Tr101290Check>,
    report: Tr101290Report,
}

impl Tr101290Engine {
    pub fn new() -> Self {
        Self::default()
    }

    /// Analyze one probe window. `_wall_ms` is kept for API compatibility;
    /// timing checks use the PCR clock inside the window.
    pub fn ingest(&mut self, bytes: &[u8], _wall_ms: u64) -> Tr101290Report {
        if bytes.is_empty() {
            return self.report.clone();
        }
        self.win = WindowState::default();
        self.report.pat_timeout = false;
        self.report.pmt_timeout = false;
        let mut offset = 0usize;
        let mut in_sync = true;
        while offset + TS_PACKET_SIZE <= bytes.len() {
            if bytes[offset] == SYNC_BYTE {
                in_sync = true;
                self.process_packet(&bytes[offset..offset + TS_PACKET_SIZE]);
                offset += TS_PACKET_SIZE;
                continue;
            }
            if in_sync {
                self.report.sync_errors = self.report.sync_errors.saturating_add(1);
                self.add_p1(
                    "P1_SYNC",
                    format!("TS sync byte missing at offset {offset} (expected 0x47)"),
                );
                in_sync = false;
            }
            offset = resync(bytes, offset + 1);
        }
        self.finish_window();
        self.report.clone()
    }

    pub fn snapshot(&self) -> Tr101290Report {
        self.report.clone()
    }

    /// Checks raised since the last call. Use this to emit each finding once.
    pub fn take_new_checks(&mut self) -> Vec<Tr101290Check> {
        std::mem::take(&mut self.new_checks)
    }

    fn process_packet(&mut self, pkt: &[u8]) {
        let pid = packet_pid(pkt);
        if pid == 0x1FFF {
            return;
        }
        self.seen_pids.insert(pid);
        if pkt[1] & 0x80 != 0 {
            self.add_p2(
                "P2_TEI",
                format!("PID 0x{pid:04X}: transport error indicator set"),
            );
        }
        let adaptation = (pkt[3] >> 4) & 0x3;
        let discontinuity = adaptation >= 2 && pkt[4] > 0 && pkt[5] & 0x80 != 0;
        if let Some(pcr) = parse_pcr(pkt) {
            self.track_pcr(pid, pcr, discontinuity);
        }
        self.track_cc(pid, pkt, adaptation, discontinuity);
        let pusi = pkt[1] & 0x40 != 0;
        if let Some(payload) = ts_payload(pkt) {
            if pusi {
                self.parse_tables(pid, payload);
                self.track_pts(pid, payload, discontinuity);
            }
        }
    }

    fn track_cc(&mut self, pid: u16, pkt: &[u8], adaptation: u8, discontinuity: bool) {
        // CC only increments on packets with payload.
        if adaptation == 0 || adaptation == 2 {
            return;
        }
        let cc = pkt[3] & 0x0f;
        let sig = ts_payload(pkt).map(payload_signature);
        let entry = self.win.cc.entry(pid).or_default();
        let mut violation: Option<(u8, String)> = None;
        if let (Some(prev), false) = (entry.last_cc, discontinuity) {
            let expected = (prev + 1) & 0x0f;
            if cc == prev {
                // One duplicate packet is allowed. A repeated CC with a
                // different payload is an error.
                if sig.is_some() && entry.last_payload_sig != sig {
                    violation = Some((
                        1,
                        format!("PID 0x{pid:04X}: CC repeated with new payload (cc={cc})"),
                    ));
                }
            } else if cc != expected {
                violation = Some((
                    1,
                    format!(
                        "PID 0x{pid:04X}: CC discontinuity (prev={prev}, got={cc}, expected={expected})"
                    ),
                ));
            }
        }
        entry.last_payload_sig = sig;
        entry.last_cc = Some(cc);
        if let Some((_, msg)) = violation {
            self.report.cc_errors = self.report.cc_errors.saturating_add(1);
            self.add_p1("P1_CC", msg);
        }
    }

    fn parse_tables(&mut self, pid: u16, payload: &[u8]) {
        let pointer = usize::from(payload[0]);
        let Some(section) = payload.get(1 + pointer..) else {
            return;
        };
        let Some(&table_id) = section.first() else {
            return;
        };
        let now = self.win.last_pcr.map(|(_, p)| p);
        if pid == PAT_PID {
            if table_id != 0x00 {
                self.add_p1(
                    "P1_PAT",
                    format!("PID 0x0000 carries table_id 0x{table_id:02X}, expected 0x00"),
                );
                return;
            }
            self.check_repetition(true, now);
            self.win.pat_seen = true;
            self.win.pat_at = now;
            for pmt_pid in parse_pat_pmt_pids(section) {
                self.pmt_pids.insert(pmt_pid);
            }
        } else if self.pmt_pids.contains(&pid) && table_id == 0x02 {
            self.check_repetition(false, now);
            self.win.pmt_seen = true;
            self.win.pmt_at = now;
            if section.len() >= 10 {
                let pcr_pid = u16::from_be_bytes([section[8] & 0x1f, section[9]]);
                self.referenced_pids.insert(pcr_pid);
            }
            for es_pid in parse_pmt_es_pids(section) {
                self.referenced_pids.insert(es_pid);
            }
        }
    }

    /// PAT and PMT must repeat at least every 500 ms of stream time.
    fn check_repetition(&mut self, pat: bool, now: Option<u64>) {
        let last = if pat {
            self.win.pat_at
        } else {
            self.win.pmt_at
        };
        if let (Some(last), Some(now)) = (last, now) {
            let gap_ms = ticks_ms(last, now);
            if gap_ms > PAT_PMT_TIMEOUT_MS as f64 {
                self.flag_table_gap(pat, gap_ms);
            }
        }
    }

    fn flag_table_gap(&mut self, pat: bool, gap_ms: f64) {
        let name = if pat { "PAT" } else { "PMT" };
        if pat {
            self.report.pat_timeout = true;
        } else {
            self.report.pmt_timeout = true;
        }
        self.add_p1(
            if pat { "P1_PAT" } else { "P1_PMT" },
            format!("{name} interval {gap_ms:.0}ms exceeds {PAT_PMT_TIMEOUT_MS}ms"),
        );
    }

    fn track_pcr(&mut self, pid: u16, pcr: u64, discontinuity: bool) {
        if self.win.first_pcr.is_none() {
            self.win.first_pcr = Some(pcr);
        }
        if let Some((prev_pid, prev)) = self.win.last_pcr {
            if prev_pid == pid && !discontinuity {
                let gap_ms = ticks_ms(prev, pcr);
                self.win.pcr_intervals_ms.push(gap_ms);
                if gap_ms > PCR_MAX_GAP_MS {
                    self.win.pcr_gaps.push((pid, gap_ms));
                }
            }
        }
        if discontinuity {
            // Table repetition is measured on the new timebase from here on.
            self.win.pat_at = self.win.pat_at.map(|_| pcr);
            self.win.pmt_at = self.win.pmt_at.map(|_| pcr);
        }
        self.win.last_pcr = Some((pid, pcr));
    }

    fn track_pts(&mut self, pid: u16, payload: &[u8], discontinuity: bool) {
        if payload.len() < 14 || payload[0] != 0 || payload[1] != 0 || payload[2] != 1 {
            return;
        }
        if !(0xE0..=0xEF).contains(&payload[3]) {
            return;
        }
        let Some(pts) = parse_pes_pts(payload) else {
            return;
        };
        if let (Some(prev), false) = (self.win.last_pts.insert(pid, pts), discontinuity) {
            let fwd = ticks_ms(prev, pts);
            let back = ticks_ms(pts, prev);
            let jump_ms = fwd.min(back);
            if jump_ms > PTS_JUMP_MS {
                self.report.pts_discontinuities = self.report.pts_discontinuities.saturating_add(1);
                self.add_p2(
                    "P2_PTS_DISC",
                    format!("PID 0x{pid:04X}: PTS jump {jump_ms:.0}ms inside one segment"),
                );
            }
        }
    }

    fn finish_window(&mut self) {
        // A window that spans more than 500 ms of PCR time must contain a PAT
        // and a PMT.
        if let (Some(first), Some((_, last))) = (self.win.first_pcr, self.win.last_pcr) {
            let span = ticks_ms(first, last);
            if span > PAT_PMT_TIMEOUT_MS as f64 {
                if !self.win.pat_seen {
                    self.flag_table_gap(true, span);
                } else if !self.win.pmt_seen && !self.pmt_pids.is_empty() {
                    self.flag_table_gap(false, span);
                }
            }
        }
        let gaps = std::mem::take(&mut self.win.pcr_gaps);
        if let Some(&(pid, _)) = gaps.first() {
            let max = gaps.iter().map(|g| g.1).fold(0.0_f64, f64::max);
            self.report.pcr_gap_ms = Some(self.report.pcr_gap_ms.map_or(max, |g| g.max(max)));
            // One finding per window, not one per PCR.
            self.add_p2(
                "P2_PCR_GAP",
                format!(
                    "PID 0x{pid:04X}: PCR interval above {PCR_MAX_GAP_MS}ms {} times in segment window (max {max:.1}ms)",
                    gaps.len()
                ),
            );
        }
        let pcr = std::mem::take(&mut self.win.pcr_intervals_ms);
        if pcr.len() >= 3 {
            let mean = pcr.iter().sum::<f64>() / pcr.len() as f64;
            let var = pcr.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / pcr.len() as f64;
            let jitter = var.sqrt();
            self.report.pcr_jitter_ms = Some(jitter);
            if jitter > PCR_JITTER_ALARM_MS {
                self.add_p2(
                    "P2_PCR_JITTER",
                    format!("PCR interval spread {jitter:.1}ms exceeds {PCR_JITTER_ALARM_MS}ms"),
                );
            }
        }
        self.check_unreferenced_pids();
    }

    fn check_unreferenced_pids(&mut self) {
        if self.referenced_pids.is_empty() {
            return;
        }
        let mut orphans: Vec<u16> = self
            .seen_pids
            .iter()
            .copied()
            .filter(|pid| {
                is_program_pid(*pid)
                    && !self.pmt_pids.contains(pid)
                    && !self.referenced_pids.contains(pid)
                    && !self.reported_orphans.contains(pid)
            })
            .collect();
        orphans.sort_unstable();
        for pid in orphans {
            self.reported_orphans.insert(pid);
            self.report.unreferenced_pids = self.report.unreferenced_pids.saturating_add(1);
            self.add_p1(
                "P1_PID",
                format!("PID 0x{pid:04X} present but not referenced in PMT"),
            );
        }
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
        let check = Tr101290Check {
            priority,
            code: code.into(),
            message,
        };
        if self.new_checks.len() < MAX_VIOLATIONS {
            self.new_checks.push(check.clone());
        }
        // Keep the most recent checks for the overlay and summaries.
        if self.report.checks.len() >= MAX_VIOLATIONS {
            self.report.checks.remove(0);
        }
        self.report.checks.push(check);
    }
}

/// Milliseconds from `a` to `b` on the 33-bit 90 kHz clock, handling wrap.
fn ticks_ms(a: u64, b: u64) -> f64 {
    let d = (b + WRAP_33 - (a % WRAP_33)) % WRAP_33;
    d as f64 / 90.0
}

/// Next offset that starts a run of two sync bytes 188 apart (or the last packet).
fn resync(bytes: &[u8], from: usize) -> usize {
    let mut o = from;
    while o + TS_PACKET_SIZE <= bytes.len() {
        if bytes[o] == SYNC_BYTE
            && (o + 2 * TS_PACKET_SIZE > bytes.len() || bytes[o + TS_PACKET_SIZE] == SYNC_BYTE)
        {
            return o;
        }
        o += 1;
    }
    bytes.len()
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

/// Three sync bytes at 188-byte spacing from the start of the window.
fn looks_like_ts(bytes: &[u8]) -> bool {
    bytes.len() >= 3 * TS_PACKET_SIZE && (0..3).all(|i| bytes[i * TS_PACKET_SIZE] == SYNC_BYTE)
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

fn parse_pat_pmt_pids(section: &[u8]) -> Vec<u16> {
    let mut out = Vec::new();
    if section.len() < 12 {
        return out;
    }
    let section_length = u16::from_be_bytes([section[1] & 0x0f, section[2]]) as usize;
    // Programs run to the CRC (last 4 bytes of the section).
    let end = (3 + section_length).saturating_sub(4).min(section.len());
    let mut i = 8usize;
    while i + 4 <= end {
        let program = u16::from_be_bytes([section[i], section[i + 1]]);
        let pmt_pid = u16::from_be_bytes([section[i + 2] & 0x1f, section[i + 3]]);
        if program != 0 {
            out.push(pmt_pid);
        }
        i += 4;
    }
    out
}

fn parse_pmt_es_pids(section: &[u8]) -> Vec<u16> {
    let mut out = Vec::new();
    if section.len() < 12 {
        return out;
    }
    let section_length = u16::from_be_bytes([section[1] & 0x0f, section[2]]) as usize;
    let program_info_len = u16::from_be_bytes([section[10] & 0x0f, section[11]]) as usize;
    let mut i = 12 + program_info_len;
    let end = (3 + section_length).saturating_sub(4).min(section.len());
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

/// PIDs 0x0000-0x001F are reserved for PSI/SI (NIT, SDT, EIT and others),
/// which a PMT never lists.
fn is_program_pid(pid: u16) -> bool {
    (0x0020..=0x1FFE).contains(&pid)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packet(pid: u16, cc: u8, pusi: bool, payload: &[u8]) -> Vec<u8> {
        let mut pkt = vec![0xffu8; TS_PACKET_SIZE];
        pkt[0] = SYNC_BYTE;
        pkt[1] = (((pid >> 8) as u8) & 0x1f) | if pusi { 0x40 } else { 0 };
        pkt[2] = (pid & 0xff) as u8;
        pkt[3] = 0x10 | (cc & 0x0f);
        pkt[4..4 + payload.len()].copy_from_slice(payload);
        pkt
    }

    /// Adaptation-field-only packet carrying a PCR (base in 90 kHz ticks).
    fn pcr_packet(pid: u16, cc: u8, base: u64) -> Vec<u8> {
        let mut pkt = vec![0xffu8; TS_PACKET_SIZE];
        pkt[0] = SYNC_BYTE;
        pkt[1] = ((pid >> 8) as u8) & 0x1f;
        pkt[2] = (pid & 0xff) as u8;
        pkt[3] = 0x20 | (cc & 0x0f);
        pkt[4] = 183;
        pkt[5] = 0x10;
        pkt[6] = (base >> 25) as u8;
        pkt[7] = (base >> 17) as u8;
        pkt[8] = (base >> 9) as u8;
        pkt[9] = (base >> 1) as u8;
        pkt[10] = ((base & 1) as u8) << 7 | 0x7e;
        pkt[11] = 0;
        pkt
    }

    fn pat(cc: u8) -> Vec<u8> {
        // pointer, table_id, length 13, tsid 1, version, sec 0, last 0,
        // program 1 -> PMT PID 0x1000, CRC (not checked).
        let sec = [
            0x00, 0x00, 0xB0, 0x0D, 0x00, 0x01, 0xC1, 0x00, 0x00, 0x00, 0x01, 0xF0, 0x00, 0, 0, 0,
            0,
        ];
        packet(PAT_PID, cc, true, &sec)
    }

    fn pmt(cc: u8) -> Vec<u8> {
        // program 1, PCR PID 0x100, one H.264 stream on 0x100, one AAC on 0x101.
        let sec = [
            0x00, 0x02, 0xB0, 0x17, 0x00, 0x01, 0xC1, 0x00, 0x00, 0xE1, 0x00, 0xF0, 0x00, 0x1B,
            0xE1, 0x00, 0xF0, 0x00, 0x0F, 0xE1, 0x01, 0xF0, 0x00, 0, 0, 0, 0,
        ];
        packet(0x1000, cc, true, &sec)
    }

    fn join(pkts: &[Vec<u8>]) -> Vec<u8> {
        pkts.concat()
    }

    #[test]
    fn clean_window_has_no_violations() {
        let mut e = Tr101290Engine::new();
        let mut pkts = vec![pat(0), pmt(0)];
        for i in 0..20u8 {
            pkts.push(pcr_packet(0x100, 0, u64::from(i) * 2700)); // 30 ms
            pkts.push(packet(0x100, i, false, &[1, 2, 3]));
        }
        let r = e.ingest(&join(&pkts), 0);
        assert_eq!(r.p1_violations, 0, "{:?}", r.checks);
        assert_eq!(r.p2_violations, 0, "{:?}", r.checks);
        assert!(!r.pmt_timeout);
    }

    #[test]
    fn cc_gap_is_p1_and_reported_once() {
        let mut e = Tr101290Engine::new();
        let buf = join(&[
            pat(0),
            pmt(0),
            packet(0x100, 0, false, &[1]),
            packet(0x100, 5, false, &[2]),
        ]);
        let r = e.ingest(&buf, 0);
        assert_eq!(r.cc_errors, 1);
        assert_eq!(e.take_new_checks().len(), 1);
        assert!(e.take_new_checks().is_empty());
    }

    #[test]
    fn cc_state_restarts_per_window() {
        let mut e = Tr101290Engine::new();
        e.ingest(&join(&[pat(0), pmt(0), packet(0x100, 3, false, &[1])]), 0);
        let r = e.ingest(
            &join(&[pat(7), pmt(9), packet(0x100, 12, false, &[1])]),
            6000,
        );
        assert_eq!(r.cc_errors, 0, "{:?}", r.checks);
    }

    #[test]
    fn duplicate_packet_is_allowed() {
        let mut e = Tr101290Engine::new();
        let p = packet(0x100, 4, false, &[9, 9]);
        let r = e.ingest(&join(&[pat(0), pmt(0), p.clone(), p]), 0);
        assert_eq!(r.cc_errors, 0);
    }

    #[test]
    fn sync_loss_counts_once_per_loss() {
        let mut e = Tr101290Engine::new();
        let mut buf = pat(0);
        buf.extend(vec![0u8; 500]);
        buf.extend(join(&[pat(1), pat(2)]));
        let r = e.ingest(&buf, 0);
        assert_eq!(r.sync_errors, 1);
    }

    #[test]
    fn pat_interval_uses_pcr_clock() {
        let mut e = Tr101290Engine::new();
        let buf = join(&[
            pcr_packet(0x100, 0, 0),
            pat(0),
            pmt(0),
            pcr_packet(0x100, 0, 90 * 600),
            pat(1),
        ]);
        let r = e.ingest(&buf, 0);
        assert!(r.pat_timeout);
        assert!(r.checks.iter().any(|c| c.code == "P1_PAT"));
        // PCR gap of 600 ms is also a P2 finding.
        assert!(r.checks.iter().any(|c| c.code == "P2_PCR_GAP"));
    }

    #[test]
    fn short_window_without_tables_is_not_flagged() {
        let mut e = Tr101290Engine::new();
        let buf = join(&[pcr_packet(0x100, 0, 0), pcr_packet(0x100, 0, 2700)]);
        let r = e.ingest(&buf, 0);
        assert!(!r.pat_timeout);
        assert_eq!(r.p1_violations, 0);
    }

    #[test]
    fn si_pids_are_not_orphans() {
        let mut e = Tr101290Engine::new();
        let buf = join(&[
            pat(0),
            pmt(0),
            packet(0x11, 0, false, &[0]),
            packet(0x200, 0, false, &[0]),
        ]);
        let r = e.ingest(&buf, 0);
        assert_eq!(r.unreferenced_pids, 1);
        e.ingest(&buf, 0);
        assert_eq!(e.snapshot().unreferenced_pids, 1);
    }

    #[test]
    fn pcr_wrap_is_not_a_gap() {
        assert!((ticks_ms(WRAP_33 - 900, 1800) - 30.0).abs() < 1e-9);
    }

    #[test]
    fn random_bytes_are_not_ts() {
        let mut buf = vec![0u8; 4096];
        buf[188] = SYNC_BYTE;
        assert!(!looks_like_ts(&buf));
    }
}
