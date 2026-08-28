//! Cross-segment wire timing continuity and manifest duration checks.

use crate::models::WireTimingInfo;

const PTS33_MOD: u64 = 1 << 33;
const ROLLOVER_THRESHOLD_TICKS: u64 = PTS33_MOD / 2;

#[derive(Debug, Clone, Default)]
pub struct WireTimingTracker {
    last_decode_ticks: Option<u64>,
    last_timescale: Option<u32>,
    last_keyframe_pts_sec: Option<f64>,
}

impl WireTimingTracker {
    pub fn observe_segment(&mut self, timing: &WireTimingInfo, keyframe_pts_sec: Option<f64>) {
        if let (Some(base), Some(scale)) = (timing.moof_base_decode_time, timing.moof_timescale) {
            if scale > 0 {
                self.last_decode_ticks = Some(base);
                self.last_timescale = Some(scale);
            }
        }
        if let Some(pts) = keyframe_pts_sec.filter(|p| p.is_finite() && *p >= 0.0) {
            self.last_keyframe_pts_sec = Some(pts);
        }
    }

    pub fn apply(&mut self, timing: &mut WireTimingInfo, target_duration_secs: Option<f32>) {
        self.apply_cross_segment(timing);
        self.apply_target(timing, target_duration_secs);
    }

    pub fn apply_target(&mut self, timing: &mut WireTimingInfo, target_duration_secs: Option<f32>) {
        if let (Some(wire_dur), Some(target)) = (timing.wire_duration_sec, target_duration_secs) {
            if target > 0.0 && wire_dur.is_finite() && wire_dur > 0.0 {
                let pct = ((wire_dur - f64::from(target)) / f64::from(target)) * 100.0;
                if pct.is_finite() {
                    timing.target_duration_deviation_pct = Some(pct);
                }
            }
        }
    }

    fn apply_cross_segment(&mut self, timing: &mut WireTimingInfo) {
        if let (Some(base), Some(prev)) = (timing.moof_base_decode_time, self.last_decode_ticks) {
            if self.last_timescale == timing.moof_timescale {
                let delta = base.wrapping_sub(prev);
                if delta > ROLLOVER_THRESHOLD_TICKS {
                    timing.pts_rollover_suspect = true;
                } else if delta > 0 {
                    let scale = timing.moof_timescale.unwrap_or(1).max(1) as f64;
                    let gap_sec = delta as f64 / scale;
                    if gap_sec > 30.0 {
                        timing.pts_discontinuity = true;
                        timing.pts_gap_ms = Some(gap_sec * 1000.0);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_duration_deviation_computed() {
        let mut tracker = WireTimingTracker::default();
        let mut timing = WireTimingInfo {
            wire_duration_sec: Some(3.0),
            ..Default::default()
        };
        tracker.apply(&mut timing, Some(2.0));
        assert_eq!(timing.target_duration_deviation_pct, Some(50.0));
    }

    #[test]
    fn decode_time_gap_flags_discontinuity() {
        let mut tracker = WireTimingTracker {
            last_decode_ticks: Some(0),
            last_timescale: Some(90000),
            ..Default::default()
        };
        let mut timing = WireTimingInfo {
            moof_base_decode_time: Some(90000 * 45),
            moof_timescale: Some(90000),
            ..Default::default()
        };
        tracker.apply(&mut timing, None);
        assert!(timing.pts_discontinuity);
        assert!(timing.pts_gap_ms.unwrap() > 40_000.0);
    }

    #[test]
    fn rollover_suspect_on_33bit_wrap() {
        let mut tracker = WireTimingTracker {
            last_decode_ticks: Some(PTS33_MOD - 90000),
            last_timescale: Some(90000),
            ..Default::default()
        };
        let mut timing = WireTimingInfo {
            moof_base_decode_time: Some(90000),
            moof_timescale: Some(90000),
            ..Default::default()
        };
        tracker.apply(&mut timing, None);
        assert!(timing.pts_rollover_suspect);
    }
}
