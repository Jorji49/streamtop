//! Cross-layer DAI validation: manifest cues vs wire SCTE-35.

use crate::engine::scte35::parse_scte35_bytes;
use crate::models::{AdBreakInfo, AdMarkerMismatch, InbandAdEvent, WireProbeInfo};

const PTS_TOLERANCE_MS: f64 = 500.0;
const DURATION_TOLERANCE_SECS: f64 = 1.5;

/// Correlate manifest ad tag with binary splice markers in the probe window.
pub fn validate_ad_alignment(
    manifest: &AdBreakInfo,
    wire: &WireProbeInfo,
    scte35_summary: Option<&str>,
) -> Option<AdMarkerMismatch> {
    let binary = scte35_summary.or(manifest.scte35_binary.as_deref());
    let has_manifest_cue = manifest.kind.contains("CUE")
        || manifest.kind.contains("DATERANGE")
        || manifest.kind.contains("SCTE35");
    let has_binary = binary.is_some_and(|s| !s.is_empty());

    if has_manifest_cue && !has_binary {
        return Some(AdMarkerMismatch {
            rule: "DAI_MANIFEST_WITHOUT_BINARY".into(),
            message: format!(
                "Manifest {} at seq hint without SCTE-35 splice in wire probe",
                manifest.kind
            ),
            manifest_kind: manifest.kind.clone(),
            planned_duration_secs: manifest.planned_duration_secs,
            wire_pts_ms: wire_pts_ms(wire),
            drift_ms: None,
        });
    }

    if has_binary && !has_manifest_cue {
        return Some(AdMarkerMismatch {
            rule: "DAI_BINARY_WITHOUT_MANIFEST".into(),
            message: "Binary SCTE-35 in segment without matching manifest cue".into(),
            manifest_kind: manifest.kind.clone(),
            planned_duration_secs: manifest.planned_duration_secs,
            wire_pts_ms: wire_pts_ms(wire),
            drift_ms: None,
        });
    }

    if let (Some(planned), Some(wire_dur)) = (
        manifest.planned_duration_secs,
        wire.timing.wire_duration_sec,
    ) {
        if manifest.active && wire_dur + DURATION_TOLERANCE_SECS < planned * 0.5 {
            return Some(AdMarkerMismatch {
                rule: "DAI_DURATION_DRIFT".into(),
                message: format!(
                    "Planned ad {planned:.1}s but wire window only {wire_dur:.1}s before return"
                ),
                manifest_kind: manifest.kind.clone(),
                planned_duration_secs: Some(planned),
                wire_pts_ms: wire_pts_ms(wire),
                drift_ms: Some(((planned - wire_dur) * 1000.0).round() as i64),
            });
        }
    }

    if let Some(drift) = wire.timing.pcr_pts_drift_ms {
        if drift.abs() > PTS_TOLERANCE_MS {
            return Some(AdMarkerMismatch {
                rule: "DAI_PTS_DRIFT".into(),
                message: format!("Ad marker PTS drift {drift:.0}ms exceeds tolerance"),
                manifest_kind: manifest.kind.clone(),
                planned_duration_secs: manifest.planned_duration_secs,
                wire_pts_ms: Some(drift.round() as i64),
                drift_ms: Some(drift.round() as i64),
            });
        }
    }

    None
}

/// Build inband ad events from wire `emsg` boxes (SCTE schemes only).
pub fn inband_events_from_wire(wire: &WireProbeInfo) -> Vec<InbandAdEvent> {
    wire.inband_emsg
        .iter()
        .filter(|e| e.is_scte_related())
        .map(|emsg| InbandAdEvent {
            scte35_summary: decode_emsg_scte_summary_bytes(emsg.message_data.as_slice()),
            emsg: emsg.clone(),
        })
        .collect()
}

fn decode_emsg_scte_summary_bytes(data: &[u8]) -> Option<String> {
    if data.is_empty() {
        return None;
    }
    parse_scte35_bytes(data).map(|s| s.summary_line())
}

/// Correlate manifest ad state with inband `emsg` when both are present.
pub fn validate_inband_vs_manifest(
    manifest: &AdBreakInfo,
    inband: &InbandAdEvent,
) -> Option<AdMarkerMismatch> {
    if !manifest.active && inband.emsg.event_duration > 0 {
        return Some(AdMarkerMismatch {
            rule: "DAI_INBAND_WITHOUT_MANIFEST".into(),
            message: format!(
                "Inband emsg id={} without matching manifest cue",
                inband.emsg.id
            ),
            manifest_kind: manifest.kind.clone(),
            planned_duration_secs: manifest.planned_duration_secs,
            wire_pts_ms: None,
            drift_ms: None,
        });
    }
    if manifest.active && inband.scte35_summary.is_none() && inband.emsg.message_data.is_empty() {
        return Some(AdMarkerMismatch {
            rule: "DAI_MANIFEST_WITHOUT_INBAND".into(),
            message: "Manifest ad active but inband emsg carries no SCTE payload".into(),
            manifest_kind: manifest.kind.clone(),
            planned_duration_secs: manifest.planned_duration_secs,
            wire_pts_ms: None,
            drift_ms: None,
        });
    }
    None
}

fn wire_pts_ms(wire: &WireProbeInfo) -> Option<i64> {
    wire.timing
        .pcr_pts_drift_ms
        .map(|d| d.round() as i64)
        .or_else(|| wire.keyframe_pts_sec.map(|p| (p * 1000.0).round() as i64))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::WireTimingInfo;

    #[test]
    fn flags_manifest_without_binary() {
        let ad = AdBreakInfo {
            kind: "CUE-OUT".into(),
            id: None,
            planned_duration_secs: Some(30.0),
            elapsed_secs: None,
            remaining_secs: None,
            summary: "test".into(),
            active: true,
            scte35_binary: None,
        };
        let wire = WireProbeInfo::default();
        let m = validate_ad_alignment(&ad, &wire, None);
        assert!(m.is_some());
        assert_eq!(m.unwrap().rule, "DAI_MANIFEST_WITHOUT_BINARY");
    }

    #[test]
    fn passes_when_binary_present() {
        let ad = AdBreakInfo {
            kind: "CUE-OUT".into(),
            id: None,
            planned_duration_secs: Some(30.0),
            elapsed_secs: None,
            remaining_secs: None,
            summary: "test".into(),
            active: true,
            scte35_binary: Some("splice_out".into()),
        };
        let wire = WireProbeInfo::default();
        assert!(validate_ad_alignment(&ad, &wire, Some("splice_out")).is_none());
    }

    #[test]
    fn pts_drift_tolerance() {
        let ad = AdBreakInfo {
            kind: "DATERANGE".into(),
            id: Some("ad1".into()),
            planned_duration_secs: None,
            elapsed_secs: None,
            remaining_secs: None,
            summary: "test".into(),
            active: false,
            scte35_binary: Some("ok".into()),
        };
        let wire = WireProbeInfo {
            timing: WireTimingInfo {
                pcr_pts_drift_ms: Some(800.0),
                ..Default::default()
            },
            ..Default::default()
        };
        let m = validate_ad_alignment(&ad, &wire, Some("ok"));
        assert!(m.is_some());
        assert_eq!(m.unwrap().rule, "DAI_PTS_DRIFT");
    }
}
