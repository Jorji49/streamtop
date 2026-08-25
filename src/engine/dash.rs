//! DASH MPD parsing.

use std::time::Duration;

use color_eyre::eyre::{eyre, Result};
use dash_mpd::{
    is_audio_adaptation, is_video_adaptation, parse, AdaptationSet, Period, Representation,
};
use url::Url;

use crate::models::{parse_frame_rate, AbrVariant, DrmInfo};

#[derive(Debug, Clone, Default)]
pub struct DashSummary {
    pub availability_start_time: Option<String>,
    pub publish_time: Option<String>,
    pub suggested_presentation_delay_secs: Option<f64>,
    pub min_buffer_time_secs: Option<f64>,
    pub media_presentation_duration_secs: Option<f64>,
    pub time_shift_buffer_depth_secs: Option<f64>,
    pub minimum_update_period_secs: Option<f64>,
    pub variants: Vec<AbrVariant>,
    pub audio_languages: Vec<String>,
    /// Best candidate URL for zero-payload / segment probe (init or media).
    pub probe_url: Option<String>,
    pub segment_duration_hint_secs: f32,
    pub type_live: bool,
    /// Number of `<Period>` elements in the MPD.
    pub period_count: u32,
    /// Active period id (last period for dynamic live).
    pub active_period_id: Option<String>,
    pub drm: DrmInfo,
}

pub fn looks_like_dash(url: &str, body: &[u8], content_type: Option<&str>) -> bool {
    let head = String::from_utf8_lossy(&body[..body.len().min(512)]);
    let t = head.trim_start();
    if t.starts_with('#') || t.contains("#EXT") {
        return false;
    }
    if let Some(ct) = content_type {
        let ct = ct.to_ascii_lowercase();
        if ct.contains("dash+xml")
            || ct.contains("mpd")
            || (ct.contains("xml") && !ct.contains("mpegurl") && !ct.contains("m3u"))
        {
            return true;
        }
    }
    let lower = url.to_ascii_lowercase();
    if lower.contains(".mpd") {
        return true;
    }
    t.starts_with("<?xml") || t.contains("<MPD") || t.contains("<mpd")
}

pub fn parse_dash_mpd(xml: &str, base: &Url) -> Result<DashSummary> {
    let mpd = parse(xml).map_err(|e| eyre!("DASH MPD parse error: {e}"))?;

    let suggested = mpd.suggestedPresentationDelay.as_ref().map(duration_secs);
    let min_buffer = mpd.minBufferTime.as_ref().map(duration_secs);
    let media_dur = mpd.mediaPresentationDuration.as_ref().map(duration_secs);
    let tsbd = mpd.timeShiftBufferDepth.as_ref().map(duration_secs);
    let mup = mpd.minimumUpdatePeriod.as_ref().map(duration_secs);

    let type_live = mpd
        .mpdtype
        .as_deref()
        .map(|t| t.eq_ignore_ascii_case("dynamic"))
        .unwrap_or(false);

    let availability_start_time = mpd.availabilityStartTime.as_ref().map(|t| t.to_rfc3339());
    let publish_time = mpd.publishTime.as_ref().map(|t| t.to_rfc3339());

    let mut root = base.clone();
    if let Some(b) = mpd.base_url.first() {
        if let Some(joined) = join_url(&root, &b.base) {
            if let Ok(u) = Url::parse(&joined) {
                root = u;
            }
        }
    }

    if mpd.periods.is_empty() {
        return Err(eyre!("DASH MPD has no Period"));
    }

    let period_count = mpd.periods.len() as u32;
    let active_period = mpd.periods.last().expect("non-empty periods");
    let active_period_id = active_period.id.clone();

    let mut variants = Vec::new();
    let mut audio_languages = Vec::new();
    let mut probe_url = None;
    let mut segment_duration_hint_secs: f32 = 2.0;
    let mut best_bw: u64 = 0;
    let mut drm = DrmInfo::default();

    for period in &mpd.periods {
        let period_out = parse_period(period, &root)?;
        for lang in period_out.audio_languages {
            if !audio_languages.contains(&lang) {
                audio_languages.push(lang);
            }
        }
        for v in period_out.variants {
            variants.push(v);
        }
        if period_out.best_bw >= best_bw {
            best_bw = period_out.best_bw;
            if let Some(u) = period_out.probe_url {
                probe_url = Some(u);
            }
            if period_out.segment_duration_hint_secs > 0.0 {
                segment_duration_hint_secs = period_out.segment_duration_hint_secs;
            }
        }
        if !drm.present && period_out.drm.present {
            drm = period_out.drm;
        }
    }

    if variants.is_empty() {
        return Err(eyre!("DASH MPD has no video Representation profiles"));
    }

    variants.sort_by_key(|a| std::cmp::Reverse(a.bandwidth));
    if let Some(first) = variants.first_mut() {
        first.selected = true;
    }

    if segment_duration_hint_secs <= 0.0 {
        segment_duration_hint_secs = 2.0;
    }

    Ok(DashSummary {
        availability_start_time,
        publish_time,
        suggested_presentation_delay_secs: suggested,
        min_buffer_time_secs: min_buffer,
        media_presentation_duration_secs: media_dur,
        time_shift_buffer_depth_secs: tsbd,
        minimum_update_period_secs: mup,
        variants,
        audio_languages,
        probe_url,
        segment_duration_hint_secs,
        type_live,
        period_count,
        active_period_id,
        drm,
    })
}

struct PeriodParseOut {
    variants: Vec<AbrVariant>,
    audio_languages: Vec<String>,
    probe_url: Option<String>,
    segment_duration_hint_secs: f32,
    best_bw: u64,
    drm: DrmInfo,
}

fn parse_period(period: &Period, root: &Url) -> Result<PeriodParseOut> {
    let mut period_base = root.clone();
    if let Some(b) = period.BaseURL.first() {
        if let Some(joined) = join_url(&period_base, &b.base) {
            if let Ok(u) = Url::parse(&joined) {
                period_base = u;
            }
        }
    }

    let mut variants = Vec::new();
    let mut audio_languages = Vec::new();
    let mut probe_url = None;
    let mut segment_duration_hint_secs: f32 = 2.0;
    let mut best_bw: u64 = 0;
    let mut drm = DrmInfo::default();

    for adaptation in &period.adaptations {
        merge_drm(&mut drm, &adaptation.ContentProtection);
        if is_audio_adaptation(&adaptation) {
            let lang = adaptation.lang.clone().unwrap_or_else(|| "und".into());
            let name = adaptation
                .Label
                .first()
                .map(|l| l.content.clone())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "audio".into());
            audio_languages.push(format!("{name} ({lang})"));
            continue;
        }

        if !is_video_adaptation(&adaptation) && !looks_video(adaptation) {
            continue;
        }

        let mut adapt_base = period_base.clone();
        if let Some(b) = adaptation.BaseURL.first() {
            if let Some(joined) = join_url(&adapt_base, &b.base) {
                if let Ok(u) = Url::parse(&joined) {
                    adapt_base = u;
                }
            }
        }

        for rep in &adaptation.representations {
            merge_drm(&mut drm, &rep.ContentProtection);
            let bw = rep.bandwidth.unwrap_or(0);
            let w = rep.width.or(adaptation.width);
            let h = rep.height.or(adaptation.height);
            let resolution = match (w, h) {
                (Some(w), Some(h)) => Some(format!("{w}x{h}")),
                _ => None,
            };
            let codecs = rep.codecs.clone().or_else(|| adaptation.codecs.clone());
            let frame_rate = rep
                .frameRate
                .as_deref()
                .or(adaptation.frameRate.as_deref())
                .and_then(parse_frame_rate);

            let uri = representation_probe_url(&adapt_base, adaptation, rep)
                .unwrap_or_else(|| adapt_base.to_string());

            if bw >= best_bw {
                best_bw = bw;
                probe_url = Some(uri.clone());
                if let Some(d) = segment_duration_from(adaptation, rep) {
                    segment_duration_hint_secs = d;
                }
            }

            variants.push(AbrVariant {
                bandwidth: bw,
                resolution,
                codecs,
                frame_rate,
                uri,
                selected: false,
                from_wire: false,
                mismatch: None,
            });
        }
    }

    Ok(PeriodParseOut {
        variants,
        audio_languages,
        probe_url,
        segment_duration_hint_secs,
        best_bw,
        drm,
    })
}

fn merge_drm(into: &mut DrmInfo, protections: &[dash_mpd::ContentProtection]) {
    for cp in protections {
        if let Some(info) = classify_content_protection(&cp.schemeIdUri) {
            *into = info;
            return;
        }
    }
}

pub fn classify_content_protection(scheme: &str) -> Option<DrmInfo> {
    let s = scheme.to_ascii_lowercase();
    let (method, badge) =
        if s.contains("edef8ba9-79d6-4ace-a3c8-27dcd51d21ed") || s.contains("widevine") {
            ("Widevine", "DRM: Widevine")
        } else if s.contains("9a04f079-9840-4286-ab92-e65be0885f95") || s.contains("playready") {
            ("PlayReady", "DRM: PlayReady")
        } else if s.contains("e2719d58-a985-b3c9-781a-b030af78d30e")
            || s.contains("clearkey")
            || s.contains("org.w3.clearkey")
        {
            ("ClearKey", "DRM: ClearKey")
        } else if s.contains("urn:mpeg:dash:mp4protection") {
            ("cenc", "DRM: CENC")
        } else {
            return None;
        };
    Some(DrmInfo {
        present: true,
        method: Some(method.into()),
        key_format: Some(scheme.to_string()),
        badge: badge.into(),
    })
}

fn duration_secs(d: &Duration) -> f64 {
    d.as_secs_f64()
}

fn looks_video(a: &AdaptationSet) -> bool {
    a.contentType
        .as_deref()
        .map(|c| c.eq_ignore_ascii_case("video"))
        .unwrap_or(false)
        || a.mimeType
            .as_deref()
            .map(|m| m.starts_with("video/"))
            .unwrap_or(false)
}

fn segment_duration_from(adaptation: &AdaptationSet, rep: &Representation) -> Option<f32> {
    let rep_st = rep.SegmentTemplate.as_ref();
    let adapt_st = adaptation.SegmentTemplate.as_ref();
    let duration = rep_st
        .and_then(|s| s.duration)
        .or_else(|| adapt_st.and_then(|s| s.duration))?;
    let timescale = rep_st
        .and_then(|s| s.timescale)
        .or_else(|| adapt_st.and_then(|s| s.timescale))
        .unwrap_or(1) as f64;
    if timescale <= 0.0 {
        return None;
    }
    let secs = (duration / timescale) as f32;
    (secs > 0.0 && secs.is_finite() && secs <= 120.0).then_some(secs)
}

fn representation_probe_url(
    base: &Url,
    adaptation: &AdaptationSet,
    rep: &Representation,
) -> Option<String> {
    if let Some(st) = rep
        .SegmentTemplate
        .as_ref()
        .or(adaptation.SegmentTemplate.as_ref())
    {
        if let Some(init) = &st.initialization {
            let filled = fill_template(init, rep);
            return join_url(base, &filled);
        }
        if let Some(init_el) = &st.Initialization {
            if let Some(src) = &init_el.sourceURL {
                let filled = fill_template(src, rep);
                return join_url(base, &filled);
            }
        }
        if let Some(media) = &st.media {
            let start = st.startNumber.unwrap_or(1);
            let filled = fill_template(media, rep)
                .replace("$Number$", &start.to_string())
                .replace("$Number%05d$", &format!("{start:05}"))
                .replace("$Time$", "0");
            return join_url(base, &filled);
        }
    }

    if let Some(b) = rep.BaseURL.first() {
        return join_url(base, &b.base);
    }
    if let Some(b) = adaptation.BaseURL.first() {
        return join_url(base, &b.base);
    }
    None
}

fn fill_template(tmpl: &str, rep: &Representation) -> String {
    tmpl.replace("$RepresentationID$", rep.id.as_deref().unwrap_or(""))
        .replace("$Bandwidth$", &rep.bandwidth.unwrap_or(0).to_string())
}

fn join_url(base: &Url, href: &str) -> Option<String> {
    if let Ok(u) = Url::parse(href) {
        return Some(u.to_string());
    }
    base.join(href).ok().map(|u| u.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_mpd_url() {
        assert!(looks_like_dash(
            "https://ex.com/live.mpd",
            b"",
            Some("application/dash+xml")
        ));
    }

    #[test]
    fn classify_widevine_uuid() {
        let d =
            classify_content_protection("urn:uuid:edef8ba9-79d6-4ace-a3c8-27dcd51d21ed").unwrap();
        assert!(d.present);
        assert_eq!(d.method.as_deref(), Some("Widevine"));
    }
}
