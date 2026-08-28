//! WebVTT / TTML subtitle payload inspection and PTS drift correlation.

use crate::models::SubtitleSyncInfo;

pub const SUBTITLE_DRIFT_WARN_MS: i64 = 200;

#[derive(Debug, Clone, Default)]
pub struct SubtitleProbeInfo {
    pub format: Option<String>,
    pub valid_syntax: bool,
    pub cue_count: u32,
    pub has_timing: bool,
    pub issues: Vec<String>,
    /// First cue start time in milliseconds (media timeline).
    pub first_cue_start_ms: Option<u64>,
}

pub fn probe_subtitle_payload(bytes: &[u8]) -> SubtitleProbeInfo {
    let text = String::from_utf8_lossy(bytes);
    let trimmed = text.trim_start();
    if trimmed.starts_with("WEBVTT") {
        return probe_webvtt(&text);
    }
    if trimmed.starts_with("<?xml") && trimmed.to_ascii_lowercase().contains("<tt") {
        return probe_ttml(&text);
    }
    SubtitleProbeInfo::default()
}

/// Correlate subtitle cue start against video segment PTS (milliseconds).
pub fn compute_subtitle_drift(
    probe: &SubtitleProbeInfo,
    video_pts_ms: Option<u64>,
) -> SubtitleSyncInfo {
    let mut info = SubtitleSyncInfo {
        format: probe.format.clone(),
        cue_count: probe.cue_count,
        ..Default::default()
    };
    let (Some(cue_ms), Some(vid_ms)) = (probe.first_cue_start_ms, video_pts_ms) else {
        return info;
    };
    let drift = cue_ms as i64 - vid_ms as i64;
    info.subtitle_drift_ms = Some(drift);
    info.desync_warning = drift.abs() > SUBTITLE_DRIFT_WARN_MS;
    info
}

fn probe_webvtt(raw: &str) -> SubtitleProbeInfo {
    let mut info = SubtitleProbeInfo {
        format: Some("WebVTT".into()),
        valid_syntax: true,
        ..Default::default()
    };
    if !raw.starts_with("WEBVTT") {
        info.valid_syntax = false;
        info.issues.push("missing WEBVTT header".into());
    }
    for line in raw.lines() {
        if line.contains("-->") {
            info.cue_count = info.cue_count.saturating_add(1);
            info.has_timing = true;
            if info.first_cue_start_ms.is_none() {
                if let Some((start, _)) = line.split_once("-->") {
                    info.first_cue_start_ms = parse_vtt_timestamp(start.trim());
                }
            }
        }
    }
    if info.cue_count == 0 {
        info.issues.push("no timed cues found".into());
    }
    info
}

fn probe_ttml(raw: &str) -> SubtitleProbeInfo {
    let lower = raw.to_ascii_lowercase();
    let mut info = SubtitleProbeInfo {
        format: Some("TTML".into()),
        valid_syntax: lower.contains("<tt") && lower.contains("</tt>"),
        ..Default::default()
    };
    if !info.valid_syntax {
        info.issues.push("malformed TTML root".into());
    }
    info.cue_count = lower.matches("<p").count() as u32;
    info.has_timing = lower.contains("begin=") || lower.contains("dur=");
    if let Some(begin) = extract_ttml_begin(&lower) {
        info.first_cue_start_ms = parse_ttml_time(&begin);
    }
    info
}

fn parse_vtt_timestamp(s: &str) -> Option<u64> {
    let s = s.trim();
    let (h, m, rest) = if s.matches(':').count() == 2 {
        let mut parts = s.split(':');
        let h: u64 = parts.next()?.parse().ok()?;
        let m: u64 = parts.next()?.parse().ok()?;
        (h, m, parts.next()?)
    } else if s.matches(':').count() == 1 {
        let mut parts = s.split(':');
        (0, parts.next()?.parse().ok()?, parts.next()?)
    } else {
        return None;
    };
    let (sec_str, ms_str) = if let Some((a, b)) = rest.split_once('.') {
        (a, b)
    } else {
        (rest, "0")
    };
    let sec: u64 = sec_str.parse().ok()?;
    let ms: u64 = ms_str.chars().take(3).collect::<String>().parse().ok()?;
    Some((h * 3600 + m * 60 + sec) * 1000 + ms)
}

fn extract_ttml_begin(lower: &str) -> Option<String> {
    let idx = lower.find("begin=\"")?;
    let rest = &lower[idx + 7..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn parse_ttml_time(s: &str) -> Option<u64> {
    if s.ends_with('s') {
        let num: f64 = s.trim_end_matches('s').parse().ok()?;
        return Some((num * 1000.0).round() as u64);
    }
    parse_vtt_timestamp(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn webvtt_detects_cues() {
        let vtt = "WEBVTT\n\n00:00:01.000 --> 00:00:04.000\nHello\n";
        let info = probe_webvtt(vtt);
        assert!(info.valid_syntax);
        assert_eq!(info.cue_count, 1);
        assert_eq!(info.first_cue_start_ms, Some(1000));
    }

    #[test]
    fn drift_warning_when_desynced() {
        let vtt = "WEBVTT\n\n00:00:05.000 --> 00:00:08.000\nHi\n";
        let probe = probe_webvtt(vtt);
        let sync = compute_subtitle_drift(&probe, Some(1000));
        assert_eq!(sync.subtitle_drift_ms, Some(4000));
        assert!(sync.desync_warning);
    }
}
