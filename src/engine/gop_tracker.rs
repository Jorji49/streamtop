//! Cross-segment GOP interval and cadence tracking from keyframe PTS samples.

use std::collections::VecDeque;

use crate::models::WireProbeInfo;

const MAX_SAMPLES: usize = 4;
const MIN_SAMPLES_FOR_CADENCE: usize = 3;
const CADENCE_TOLERANCE: f64 = 0.08;

#[derive(Debug, Clone, Default)]
pub struct GopCadenceTracker {
    keyframe_pts: VecDeque<f64>,
}

impl GopCadenceTracker {
    pub fn observe_keyframe(&mut self, pts_sec: Option<f64>) {
        let Some(pts) = pts_sec.filter(|p| p.is_finite() && *p >= 0.0) else {
            return;
        };
        if self
            .keyframe_pts
            .back()
            .is_some_and(|last| (last - pts).abs() < f64::EPSILON)
        {
            return;
        }
        self.keyframe_pts.push_back(pts);
        while self.keyframe_pts.len() > MAX_SAMPLES {
            self.keyframe_pts.pop_front();
        }
    }

    pub fn apply(&self, wire: &mut WireProbeInfo) {
        if self.keyframe_pts.len() < 2 {
            return;
        }
        let intervals: Vec<f64> = self
            .keyframe_pts
            .iter()
            .zip(self.keyframe_pts.iter().skip(1))
            .map(|(a, b)| b - a)
            .filter(|d| *d > 0.0 && d.is_finite())
            .collect();
        if intervals.is_empty() {
            return;
        }
        let avg = intervals.iter().sum::<f64>() / intervals.len() as f64;
        wire.gop_duration_sec = Some(avg);
        wire.is_fixed_cadence = self.keyframe_pts.len() >= MIN_SAMPLES_FOR_CADENCE
            && intervals.len() >= 2
            && intervals
                .iter()
                .all(|d| (d - avg).abs() <= avg * CADENCE_TOLERANCE);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_cadence_after_three_keyframes() {
        let mut tracker = GopCadenceTracker::default();
        tracker.observe_keyframe(Some(0.0));
        tracker.observe_keyframe(Some(2.0));
        tracker.observe_keyframe(Some(4.0));
        let mut wire = WireProbeInfo::default();
        tracker.apply(&mut wire);
        assert_eq!(wire.gop_duration_sec, Some(2.0));
        assert!(wire.is_fixed_cadence);
    }

    #[test]
    fn variable_cadence_detected() {
        let mut tracker = GopCadenceTracker::default();
        tracker.observe_keyframe(Some(0.0));
        tracker.observe_keyframe(Some(2.0));
        tracker.observe_keyframe(Some(5.0));
        let mut wire = WireProbeInfo::default();
        tracker.apply(&mut wire);
        assert_eq!(wire.gop_duration_sec, Some(2.5));
        assert!(!wire.is_fixed_cadence);
    }

    #[test]
    fn needs_two_samples_for_duration() {
        let mut tracker = GopCadenceTracker::default();
        tracker.observe_keyframe(Some(1.0));
        let mut wire = WireProbeInfo::default();
        tracker.apply(&mut wire);
        assert!(wire.gop_duration_sec.is_none());
        tracker.observe_keyframe(Some(3.0));
        tracker.apply(&mut wire);
        assert_eq!(wire.gop_duration_sec, Some(2.0));
        assert!(!wire.is_fixed_cadence);
    }
}
