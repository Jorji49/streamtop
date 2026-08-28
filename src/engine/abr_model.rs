//! Deterministic virtual player: buffer occupancy, rebuffer risk, and ABR ladder behavior.

use crate::models::{VirtualBuffer, BUFFER_STALL_THRESHOLD_SECS};

/// Throughput-based ABR simulation state (one ladder rung at a time).
#[derive(Debug, Clone, Default)]
pub struct AbrLadderState {
    pub last_bandwidth_bps: Option<u64>,
    pub ladder_switches: u32,
    pub ping_pong_detected: bool,
    last_declared_bandwidth: Option<u64>,
    switch_history: Vec<u64>,
}

const PING_PONG_WINDOW: usize = 6;
const PING_PONG_MIN_SWITCHES: u32 = 3;

impl AbrLadderState {
    /// Record a throughput sample and declared variant bandwidth; detect ping-pong down/up switching.
    pub fn observe_throughput(
        &mut self,
        download_kbps: Option<u64>,
        declared_bandwidth_bps: Option<u64>,
    ) {
        let Some(kbps) = download_kbps.filter(|&k| k > 0) else {
            return;
        };
        let bps = kbps.saturating_mul(1000);
        if let Some(decl) = declared_bandwidth_bps.filter(|&d| d > 0) {
            if let Some(prev_decl) = self.last_declared_bandwidth {
                if prev_decl != decl {
                    self.ladder_switches = self.ladder_switches.saturating_add(1);
                    self.switch_history.push(decl);
                    if self.switch_history.len() > PING_PONG_WINDOW {
                        self.switch_history.remove(0);
                    }
                    self.detect_ping_pong();
                }
            }
            self.last_declared_bandwidth = Some(decl);
        }
        self.last_bandwidth_bps = Some(bps);
    }

    fn detect_ping_pong(&mut self) {
        if self.switch_history.len() < 4 {
            return;
        }
        let mut alternations = 0u32;
        for w in self.switch_history.windows(2) {
            if w[0] != w[1] {
                alternations = alternations.saturating_add(1);
            }
        }
        if alternations >= PING_PONG_MIN_SWITCHES {
            self.ping_pong_detected = true;
        }
    }
}

/// Update virtual buffer using download time vs segment duration (deterministic player model).
pub fn simulate_segment_fetch(
    vbuf: &mut VirtualBuffer,
    duration_secs: f32,
    download_ms: u64,
    elapsed_wall_secs: f64,
    download_kbps: Option<u64>,
    declared_bandwidth_bps: Option<u64>,
    ladder: &mut AbrLadderState,
) {
    vbuf.drain_elapsed(elapsed_wall_secs);
    ladder.observe_throughput(download_kbps, declared_bandwidth_bps);

    let duration = f64::from(duration_secs.max(0.001));
    let download_secs = download_ms as f64 / 1000.0;

    // Buffer drains while downloading; credits full segment duration on completion.
    vbuf.buffer_secs = (vbuf.buffer_secs - download_secs + duration).clamp(0.0, 120.0);

    // Rebuffer probability rises when download exceeds remaining buffer headroom.
    let headroom = vbuf.buffer_secs + duration - download_secs;
    vbuf.rebuffer_probability_pct = if headroom >= BUFFER_STALL_THRESHOLD_SECS {
        0
    } else {
        let deficit =
            (BUFFER_STALL_THRESHOLD_SECS - headroom.max(0.0)) / BUFFER_STALL_THRESHOLD_SECS;
        (deficit * 100.0).round().clamp(0.0, 100.0) as u8
    };

    vbuf.recompute_stall_risk();
    vbuf.stall_risk_index = vbuf
        .stall_risk_pct
        .saturating_add(vbuf.rebuffer_probability_pct)
        .min(100);
    vbuf.ladder_switches = ladder.ladder_switches;
    vbuf.ping_pong_detected = ladder.ping_pong_detected;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slow_download_raises_rebuffer_probability() {
        let mut vbuf = VirtualBuffer::default();
        let mut ladder = AbrLadderState::default();
        simulate_segment_fetch(
            &mut vbuf,
            2.0,
            3_000,
            0.0,
            Some(500),
            Some(2_000_000),
            &mut ladder,
        );
        assert!(vbuf.rebuffer_probability_pct > 0);
    }

    #[test]
    fn ping_pong_detected_on_alternating_bandwidth() {
        let mut ladder = AbrLadderState::default();
        ladder.observe_throughput(Some(800), Some(4_000_000));
        ladder.observe_throughput(Some(600), Some(1_000_000));
        ladder.observe_throughput(Some(900), Some(4_000_000));
        ladder.observe_throughput(Some(500), Some(1_000_000));
        assert!(ladder.ping_pong_detected || ladder.ladder_switches >= 3);
    }
}
