//! Clientless QoE prediction: TDR, virtual jitter buffers, and ABR ladder simulation.

use crate::models::SyntheticQoeSnapshot;
#[derive(Debug, Clone)]
pub struct SyntheticQoeEngine {
    throttle_kbps: Option<u64>,
    simulated_rtt_ms: Option<u64>,
    ttff_ms: Option<u64>,
    first_segment: bool,
    buffer_levels: [f64; 3],
}

const BUFFER_DEPTHS: [f64; 3] = [2.0, 4.0, 6.0];

impl Default for SyntheticQoeEngine {
    fn default() -> Self {
        Self::new(None, None)
    }
}

impl SyntheticQoeEngine {
    pub fn new(throttle_kbps: Option<u64>, simulated_rtt_ms: Option<u64>) -> Self {
        Self {
            throttle_kbps,
            simulated_rtt_ms,
            ttff_ms: None,
            first_segment: true,
            buffer_levels: [0.0; 3],
        }
    }

    pub fn observe_segment(
        &mut self,
        duration_secs: f32,
        download_ms: u64,
        throughput_kbps: Option<u64>,
        ladder_bps: &[u64],
    ) -> SyntheticQoeSnapshot {
        let duration = f64::from(duration_secs.max(0.001));
        let mut effective_download_ms = download_ms.max(1);

        if let Some(rtt) = self.simulated_rtt_ms {
            effective_download_ms = effective_download_ms.saturating_add(rtt);
        }

        let mut effective_kbps = throughput_kbps;
        if let Some(cap) = self.throttle_kbps {
            effective_kbps = Some(effective_kbps.map_or(cap, |k| k.min(cap)));
            let bytes = (duration * cap as f64 * 1000.0 / 8.0) as u64;
            let throttled_ms = bytes.saturating_mul(1000) / cap.saturating_mul(1000).max(1);
            effective_download_ms = effective_download_ms.max(throttled_ms);
        }

        let download_secs = effective_download_ms as f64 / 1000.0;
        let tdr = download_secs / duration;

        if self.first_segment {
            self.ttff_ms = Some(effective_download_ms);
            self.first_segment = false;
        }

        for (i, depth) in BUFFER_DEPTHS.iter().enumerate() {
            let headroom = self.buffer_levels[i] - download_secs + duration;
            self.buffer_levels[i] = headroom.clamp(0.0, 120.0);
            if headroom < 0.0 {
                self.buffer_levels[i] = 0.0;
            }
            let _ = depth;
        }

        let rebuffer_pcts = std::array::from_fn(|i| {
            let level = self.buffer_levels[i];
            let depth = BUFFER_DEPTHS[i];
            if level <= 0.0 {
                100
            } else {
                let deficit = (depth - level).max(0.0) / depth;
                (deficit * 100.0).round().clamp(0.0, 100.0) as u8
            }
        });

        let rebuffer_risk_score = compute_rebuffer_risk(tdr, &rebuffer_pcts);
        let selected = select_abr_ladder(effective_kbps, ladder_bps);

        SyntheticQoeSnapshot {
            tdr,
            rebuffer_risk_score,
            ttff_ms: self.ttff_ms,
            selected_bitrate_bps: selected,
            buffer_2s_rebuffer_pct: rebuffer_pcts[0],
            buffer_4s_rebuffer_pct: rebuffer_pcts[1],
            buffer_6s_rebuffer_pct: rebuffer_pcts[2],
            throttle_kbps: self.throttle_kbps,
            simulated_rtt_ms: self.simulated_rtt_ms,
        }
    }

    /// Simulate an abrupt bandwidth drop to the next lower ladder rung.
    pub fn simulate_bandwidth_drop(
        &mut self,
        duration_secs: f32,
        ladder_bps: &[u64],
        current_bps: u64,
    ) -> u8 {
        let mut sorted: Vec<u64> = ladder_bps.iter().copied().filter(|&b| b > 0).collect();
        sorted.sort_by_key(|b| std::cmp::Reverse(*b));
        let lower = sorted
            .iter()
            .copied()
            .find(|&b| b < current_bps)
            .unwrap_or(current_bps / 2);
        let cap_kbps = lower / 1000;
        let snap = self.observe_segment(duration_secs, 5000, Some(cap_kbps), ladder_bps);
        snap.rebuffer_risk_score
    }
}

#[allow(clippy::trivially_copy_pass_by_ref)] // QoE score helper; f64 copy not worth API churn
fn compute_rebuffer_risk(tdr: f64, rebuffer_pcts: &[u8; 3]) -> u8 {
    let tdr_component = if tdr <= 0.8 {
        0.0
    } else if tdr >= 1.5 {
        60.0
    } else {
        (tdr - 0.8) / 0.7 * 60.0
    };
    let avg_rebuf =
        (f64::from(rebuffer_pcts[0]) + f64::from(rebuffer_pcts[1]) + f64::from(rebuffer_pcts[2]))
            / 3.0;
    (tdr_component + avg_rebuf * 0.4).round().clamp(0.0, 100.0) as u8
}

/// Throughput-based ABR rung selection (BBA-style safety margin).
pub fn select_abr_ladder(throughput_kbps: Option<u64>, ladder_bps: &[u64]) -> Option<u64> {
    let kbps = throughput_kbps.filter(|&k| k > 0)?;
    let effective_bps = kbps.saturating_mul(850); // 85% safety factor
    ladder_bps
        .iter()
        .copied()
        .filter(|&bps| bps <= effective_bps)
        .max()
        .or_else(|| ladder_bps.first().copied())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slow_download_raises_risk() {
        let mut engine = SyntheticQoeEngine::new(None, None);
        let snap = engine.observe_segment(2.0, 4_000, Some(400), &[1_000_000, 2_000_000]);
        assert!(snap.tdr > 1.0);
        assert!(snap.rebuffer_risk_score > 0);
    }

    #[test]
    fn throttle_caps_throughput() {
        let mut engine = SyntheticQoeEngine::new(Some(500), None);
        let snap = engine.observe_segment(6.0, 500, Some(5000), &[1_000_000]);
        assert_eq!(snap.throttle_kbps, Some(500));
        assert!(snap.tdr > 0.0);
    }

    #[test]
    fn abr_selects_highest_safe_rung() {
        assert_eq!(
            select_abr_ladder(Some(3000), &[800_000, 2_000_000, 5_000_000]),
            Some(2_000_000)
        );
    }

    #[test]
    fn bandwidth_drop_raises_rebuffer_risk() {
        let mut engine = SyntheticQoeEngine::new(None, None);
        let risk = engine.simulate_bandwidth_drop(2.0, &[500_000, 2_000_000, 5_000_000], 5_000_000);
        assert!(risk > 0);
    }
}
