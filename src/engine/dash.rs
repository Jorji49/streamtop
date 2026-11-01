//! DASH MPD parsing.

use std::fmt::Write as _;
use std::time::Duration;

use color_eyre::eyre::{eyre, Result};
use dash_mpd::{
    is_audio_adaptation, is_video_adaptation, parse, AdaptationSet, Period, Representation,
};
use url::Url;

use crate::engine::pssh::parse_pssh_base64;
use crate::models::{
    parse_frame_rate, AbrVariant, AdBreakInfo, DiagCategory, DiagSeverity, DiagnosticFinding,
    DrmInfo, LlDashInfo, PsshProbeInfo,
};

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
    pub ll_dash: LlDashInfo,
    /// Newest available media segment of the probed Representation, for
    /// dynamic MPDs that use a SegmentTemplate.
    pub live_edge: Option<DashLiveEdge>,
}

/// A media segment near the live edge of a dynamic MPD.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DashLiveEdge {
    pub url: String,
    /// `$Number$` (or `$Time$` for timeline templates without numbers). It
    /// changes when the origin publishes a new segment.
    pub key: u64,
    /// Wall-clock time (Unix ms) the segment became available, if known.
    pub available_unix_ms: Option<i64>,
}

/// Inputs needed to place the live edge on the wall clock.
#[derive(Debug, Clone, Copy)]
struct LiveClock {
    ast_unix_ms: Option<i64>,
    now_unix_ms: i64,
    period_start_secs: f64,
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
    parse_dash_mpd_at(xml, base, chrono::Utc::now().timestamp_millis())
}

fn parse_dash_mpd_at(xml: &str, base: &Url, now_unix_ms: i64) -> Result<DashSummary> {
    let mpd = parse(xml).map_err(|e| eyre!("DASH MPD parse error: {e}"))?;

    let suggested = mpd.suggestedPresentationDelay.as_ref().map(duration_secs);
    let min_buffer = mpd.minBufferTime.as_ref().map(duration_secs);
    let media_dur = mpd.mediaPresentationDuration.as_ref().map(duration_secs);
    let tsbd = mpd.timeShiftBufferDepth.as_ref().map(duration_secs);
    let mup = mpd.minimumUpdatePeriod.as_ref().map(duration_secs);

    let type_live = mpd
        .mpdtype
        .as_deref()
        .is_some_and(|t| t.eq_ignore_ascii_case("dynamic"));

    let availability_start_time = mpd
        .availabilityStartTime
        .as_ref()
        .map(chrono::DateTime::to_rfc3339);
    let publish_time = mpd.publishTime.as_ref().map(chrono::DateTime::to_rfc3339);
    let ast_unix_ms = mpd
        .availabilityStartTime
        .as_ref()
        .map(chrono::DateTime::timestamp_millis);

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
    let active_period = mpd
        .periods
        .last()
        .ok_or_else(|| eyre!("DASH MPD has no Period"))?;
    let active_period_id = active_period.id.clone();

    let mut variants = Vec::new();
    let mut audio_languages = Vec::new();
    let mut probe_url = None;
    let mut segment_duration_hint_secs: f32 = 2.0;
    let mut best_bw: u64 = 0;
    let mut drm = DrmInfo::default();
    let mut live_edge = None;
    let last_period_idx = mpd.periods.len() - 1;

    for (idx, period) in mpd.periods.iter().enumerate() {
        let clock = (type_live && idx == last_period_idx).then(|| LiveClock {
            ast_unix_ms,
            now_unix_ms,
            period_start_secs: period.start.map_or(0.0, |d| d.as_secs_f64()),
        });
        let period_out = parse_period(period, &root, clock);
        if period_out.live_edge.is_some() {
            live_edge.clone_from(&period_out.live_edge);
        }
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

    let ll_dash = scan_ll_dash_xml(xml);
    let mpd_pssh = extract_mpd_pssh(xml);
    if !mpd_pssh.is_empty() {
        if let Some(ref mut existing) = drm.pssh {
            existing.merge(mpd_pssh);
        } else {
            drm.pssh = Some(mpd_pssh);
        }
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
        ll_dash,
        live_edge,
    })
}

struct PeriodParseOut {
    variants: Vec<AbrVariant>,
    audio_languages: Vec<String>,
    probe_url: Option<String>,
    segment_duration_hint_secs: f32,
    best_bw: u64,
    drm: DrmInfo,
    live_edge: Option<DashLiveEdge>,
}

fn parse_period(period: &Period, root: &Url, clock: Option<LiveClock>) -> PeriodParseOut {
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
    let mut live_edge = None;

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
                live_edge = clock.and_then(|c| live_edge_segment(&adapt_base, adaptation, rep, c));
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

    PeriodParseOut {
        variants,
        audio_languages,
        probe_url,
        segment_duration_hint_secs,
        best_bw,
        drm,
        live_edge,
    }
}

fn merge_drm(into: &mut DrmInfo, protections: &[dash_mpd::ContentProtection]) {
    for cp in protections {
        let la = cp
            .laurl
            .as_ref()
            .and_then(|l| l.content.clone())
            .or_else(|| cp.clearkey_laurl.as_ref().and_then(|l| l.content.clone()))
            .filter(|s| !s.trim().is_empty());

        if let Some(b64) = extract_cenc_pssh(cp) {
            if let Some(entry) = parse_pssh_base64(&b64) {
                let mut probe = into.pssh.take().unwrap_or_default();
                probe.entries.push(entry);
                into.pssh = Some(probe);
            }
        }

        if let Some(mut info) = classify_content_protection(&cp.schemeIdUri) {
            if info.key_uri.is_none() {
                info.key_uri = la;
            }
            *into = info;
            return;
        }
        // Scheme unknown but LA_URL present (e.g. ClearKey endpoint only).
        if into.key_uri.is_none() {
            if let Some(uri) = la {
                into.present = true;
                into.key_uri = Some(uri);
                if into.badge.is_empty() {
                    into.badge = "DRM · LA_URL".into();
                }
            }
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
        ..Default::default()
    })
}

fn duration_secs(d: &Duration) -> f64 {
    d.as_secs_f64()
}

fn looks_video(a: &AdaptationSet) -> bool {
    a.contentType
        .as_deref()
        .is_some_and(|c| c.eq_ignore_ascii_case("video"))
        || a.mimeType
            .as_deref()
            .is_some_and(|m| m.starts_with("video/"))
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

/// Find the newest media segment that should be available now.
///
/// `SegmentTimeline`: the last `S` entry. `duration` templates: one segment
/// behind the last complete one, which leaves room for a few seconds of clock
/// skew between this host and the packager.
fn live_edge_segment(
    base: &Url,
    adaptation: &AdaptationSet,
    rep: &Representation,
    clock: LiveClock,
) -> Option<DashLiveEdge> {
    let rep_st = rep.SegmentTemplate.as_ref();
    let adapt_st = adaptation.SegmentTemplate.as_ref();
    let st = rep_st.or(adapt_st)?;
    let media = rep_st
        .and_then(|s| s.media.as_ref())
        .or_else(|| adapt_st.and_then(|s| s.media.as_ref()))?;
    let timescale = rep_st
        .and_then(|s| s.timescale)
        .or_else(|| adapt_st.and_then(|s| s.timescale))
        .unwrap_or(1)
        .max(1);
    let start_number = rep_st
        .and_then(|s| s.startNumber)
        .or_else(|| adapt_st.and_then(|s| s.startNumber))
        .unwrap_or(1);
    let pto = rep_st
        .and_then(|s| s.presentationTimeOffset)
        .or_else(|| adapt_st.and_then(|s| s.presentationTimeOffset))
        .unwrap_or(0);
    let ts = timescale as f64;
    let avail_at = |end_ticks: u64| -> Option<i64> {
        let ast = clock.ast_unix_ms?;
        let secs = clock.period_start_secs + (end_ticks.saturating_sub(pto) as f64) / ts;
        Some(ast + (secs * 1000.0).round() as i64)
    };

    let (number, time, available_unix_ms) = if let Some(tl) = st.SegmentTimeline.as_ref() {
        let mut cursor: u64 = 0;
        let mut count: u64 = 0;
        let mut last: Option<(u64, u64)> = None;
        for s in &tl.segments {
            if let Some(t) = s.t {
                cursor = t;
            }
            // A negative repeat means "until the next S or the period end";
            // the live edge is then the first segment of the run.
            let repeats = u64::try_from(s.r.unwrap_or(0)).unwrap_or(0);
            let last_start = cursor.saturating_add(s.d.saturating_mul(repeats));
            last = Some((last_start, s.d));
            count = count.saturating_add(repeats + 1);
            cursor = last_start.saturating_add(s.d);
        }
        let (t, d) = last?;
        let number = start_number + count.saturating_sub(1);
        (number, t, avail_at(t.saturating_add(d)))
    } else {
        let dur_ticks = rep_st
            .and_then(|s| s.duration)
            .or_else(|| adapt_st.and_then(|s| s.duration))?;
        if !(dur_ticks.is_finite() && dur_ticks > 0.0) {
            return None;
        }
        let ast = clock.ast_unix_ms?;
        let dur_secs = dur_ticks / ts;
        let elapsed = (clock.now_unix_ms - ast) as f64 / 1000.0 - clock.period_start_secs;
        if !(elapsed.is_finite() && elapsed > 0.0) {
            return None;
        }
        let complete = (elapsed / dur_secs).floor();
        if complete < 2.0 || complete > 1e15 {
            return None;
        }
        let index = complete as u64 - 2;
        let time = pto.saturating_add((index as f64 * dur_ticks).round() as u64);
        let end_secs = ((index + 1) as f64).mul_add(dur_secs, clock.period_start_secs);
        (
            start_number + index,
            time,
            Some(ast + (end_secs * 1000.0).round() as i64),
        )
    };

    let filled = substitute_template_var(
        &substitute_template_var(&fill_template(media, rep), "Number", number),
        "Time",
        time,
    );
    let uses_number = media.contains("$Number");
    Some(DashLiveEdge {
        url: join_url(base, &filled)?,
        key: if uses_number { number } else { time },
        available_unix_ms,
    })
}

/// Replace `$Name$` and `$Name%0Nd$` in a SegmentTemplate string.
fn substitute_template_var(tmpl: &str, name: &str, value: u64) -> String {
    let mut out = String::with_capacity(tmpl.len());
    let mut rest = tmpl;
    let open = format!("${name}");
    while let Some(idx) = rest.find(&open) {
        out.push_str(&rest[..idx]);
        let after = &rest[idx + open.len()..];
        if let Some(tail) = after.strip_prefix('$') {
            out.push_str(&value.to_string());
            rest = tail;
            continue;
        }
        if let Some(fmt) = after.strip_prefix('%') {
            if let Some(end) = fmt.find('$') {
                let spec = &fmt[..end];
                if let Some(width) = spec
                    .strip_suffix('d')
                    .and_then(|w| w.trim_start_matches('0').parse::<usize>().ok().or(Some(0)))
                {
                    let _ = write!(out, "{value:0width$}");
                    rest = &fmt[end + 1..];
                    continue;
                }
            }
        }
        out.push_str(&open);
        rest = after;
    }
    out.push_str(rest);
    out
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

/// Scan raw MPD XML for LL-DASH / CMAF attributes not exposed by dash-mpd.
pub fn scan_ll_dash_xml(xml: &str) -> LlDashInfo {
    let lower = xml.to_ascii_lowercase();
    let mut info = LlDashInfo::default();

    if lower.contains("servicedescription") || lower.contains("<latency") {
        info.is_ll_dash = true;
        if let Some(idx) = lower.find("<latency") {
            let slice = &xml[idx..idx.saturating_add(384).min(xml.len())];
            info.latency_target_ms = parse_duration_attr_ms(slice, "target");
            info.min_latency_ms = parse_duration_attr_ms(slice, "min");
            info.max_latency_ms = parse_duration_attr_ms(slice, "max");
        }
    }

    if let Some(ato) = parse_xml_attr_f64(xml, "availabilityTimeOffset") {
        info.availability_time_offset_secs = Some(ato);
        info.is_ll_dash = true;
    }

    if lower.contains("utctiming") {
        info.is_ll_dash = true;
        info.utc_timing_scheme = parse_utc_timing_scheme(xml);
    }

    if lower.contains("chunked") && lower.contains("transfer") {
        info.is_ll_dash = true;
    }

    info
}

/// Parse MPD `<EventStream>` / `<Event>` for SCTE-35 / ad markers (manifest-only, zero-decode).
pub fn extract_dash_ad_events(xml: &str) -> Vec<AdBreakInfo> {
    let lower = xml.to_ascii_lowercase();
    if !lower.contains("eventstream") {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut search_from = 0usize;
    while let Some(es_idx) = lower[search_from..].find("<eventstream") {
        let base = search_from + es_idx;
        let end_tag = lower[base..]
            .find("</eventstream>")
            .map_or_else(|| xml.len().min(base + 8192), |i| base + i);
        let block = &xml[base..end_tag];
        let scheme = parse_xml_attr_string(block, "schemeIdUri")
            .unwrap_or_default()
            .to_ascii_lowercase();
        let timescale = parse_xml_attr_u64(block, "timescale").unwrap_or(1).max(1);
        let ad_related = scheme.contains("scte")
            || scheme.contains("splice")
            || scheme.contains("ad")
            || scheme.contains("id3");
        if !ad_related {
            search_from = end_tag;
            continue;
        }
        let block_lower = block.to_ascii_lowercase();
        let mut ev_from = 0usize;
        while let Some(ev_idx) = block_lower[ev_from..].find("<event") {
            let ev_base = ev_from + ev_idx;
            let after = ev_base + 6;
            if after < block_lower.len()
                && !matches!(
                    block_lower.as_bytes().get(after),
                    Some(b' ' | b'/' | b'>' | b'\t' | b'\n' | b'\r')
                )
            {
                ev_from = ev_base + 1;
                continue;
            }
            let tag_end = block[ev_base..]
                .find('>')
                .map_or(ev_base + 1, |i| ev_base + i + 1);
            let tag = &block[ev_base..tag_end.min(block.len())];
            let id = parse_xml_attr_string(tag, "id");
            let duration_ticks = parse_xml_attr_u64(tag, "duration");
            let dur_secs = duration_ticks.map(|d| d as f64 / timescale as f64);
            let summary = format!(
                "DASH EventStream {}{}",
                scheme,
                id.as_deref()
                    .map(|i| format!(" id={i}"))
                    .unwrap_or_default()
            );
            out.push(AdBreakInfo {
                kind: "DASH-Event".into(),
                id,
                planned_duration_secs: dur_secs,
                elapsed_secs: None,
                remaining_secs: dur_secs,
                summary: summary.clone(),
                active: dur_secs.is_some_and(|d| d > 0.0),
                scte35_binary: None,
            });
            ev_from = tag_end;
        }
        search_from = end_tag;
    }
    out
}

fn parse_xml_attr_u64(xml: &str, attr: &str) -> Option<u64> {
    parse_xml_attr_string(xml, attr)?.parse().ok()
}

fn parse_utc_timing_scheme(xml: &str) -> Option<String> {
    let lower = xml.to_ascii_lowercase();
    let idx = lower.find("utctiming")?;
    let slice = &xml[idx..idx.saturating_add(512).min(xml.len())];
    parse_xml_attr_string(slice, "schemeIdUri")
        .or_else(|| parse_xml_attr_string(slice, "schemeiduri"))
}

fn parse_xml_attr_f64(xml: &str, attr: &str) -> Option<f64> {
    let needle = format!("{attr}=\"");
    let idx = xml.find(&needle)?;
    let rest = &xml[idx + needle.len()..];
    let end = rest.find('"')?;
    rest[..end].parse().ok()
}

fn parse_xml_attr_string(xml: &str, attr: &str) -> Option<String> {
    let needle = format!("{attr}=\"");
    let idx = xml.find(&needle)?;
    let rest = &xml[idx + needle.len()..];
    let end = rest.find('"')?;
    let val = rest[..end].trim();
    if val.is_empty() {
        None
    } else {
        Some(val.to_string())
    }
}

fn parse_duration_attr_ms(xml: &str, attr: &str) -> Option<u64> {
    let raw = parse_xml_attr_string(xml, attr)?;
    if let Ok(ms) = raw.parse::<u64>() {
        return Some(ms);
    }
    iso8601_duration_to_ms(&raw)
}

fn iso8601_duration_to_ms(s: &str) -> Option<u64> {
    let s = s.trim();
    if !s.starts_with("PT") && !s.starts_with("pt") {
        return None;
    }
    let body = &s[2..];
    let mut secs = 0.0f64;
    let mut num = String::new();
    for ch in body.chars() {
        if ch.is_ascii_digit() || ch == '.' {
            num.push(ch);
        } else if ch == 'H' || ch == 'h' {
            secs = num.parse::<f64>().ok()?.mul_add(3600.0, secs);
            num.clear();
        } else if ch == 'M' || ch == 'm' {
            secs = num.parse::<f64>().ok()?.mul_add(60.0, secs);
            num.clear();
        } else if ch == 'S' || ch == 's' {
            secs += num.parse::<f64>().ok()?;
            num.clear();
        }
    }
    Some((secs * 1000.0).round() as u64)
}

/// Compare measured segment latency against ServiceDescription target.
pub fn ll_dash_production_drift(target_ms: u64, measured_ms: u64) -> i64 {
    measured_ms as i64 - target_ms as i64
}

fn extract_cenc_pssh(cp: &dash_mpd::ContentProtection) -> Option<String> {
    cp.cenc_pssh
        .iter()
        .find_map(|p| p.content.clone().filter(|s| !s.trim().is_empty()))
}

/// Scan raw MPD XML for base64 `cenc:pssh` payloads.
pub fn extract_mpd_pssh(xml: &str) -> PsshProbeInfo {
    let mut info = PsshProbeInfo::default();
    let lower = xml.to_ascii_lowercase();
    for tag in ["cenc:pssh", "pssh"] {
        let mut search = lower.as_str();
        while let Some(rel) = search.find(&format!("<{tag}")) {
            let start = lower.len() - search.len() + rel;
            let rest = &xml[start..];
            if let Some(gt) = rest.find('>') {
                let inner_start = start + gt + 1;
                if let Some(close) = rest[gt + 1..].find(&format!("</{tag}>")) {
                    let b64 = xml[inner_start..inner_start + close].trim();
                    if let Some(entry) = parse_pssh_base64(b64) {
                        info.entries.push(entry);
                    }
                }
            }
            search = search[rel.saturating_add(tag.len())..].trim_start();
        }
    }
    info
}

/// Multi-period / dynamic MPD consistency checks.
pub fn audit_multi_period_mpd(xml: &str, summary: &DashSummary) -> Vec<String> {
    let mut issues = Vec::new();
    let lower = xml.to_ascii_lowercase();
    if summary.type_live && summary.minimum_update_period_secs.is_none() {
        issues.push("dynamic MPD missing minimumUpdatePeriod".into());
    }
    if summary.period_count > 1 {
        let period_tags = lower.matches("<period").count();
        if period_tags != summary.period_count as usize {
            issues.push(format!(
                "period count mismatch: parsed {} vs xml tags {period_tags}",
                summary.period_count
            ));
        }
        if summary.media_presentation_duration_secs.is_some()
            && summary.time_shift_buffer_depth_secs.is_none()
        {
            issues.push("VOD multi-period MPD without timeShiftBufferDepth".into());
        }
    }
    if summary.type_live {
        if let Some(mpd) = lower
            .find("type=\"dynamic\"")
            .or_else(|| lower.find("type='dynamic'"))
        {
            let _ = mpd;
            if !lower.contains("availabilitystarttime") {
                issues.push("dynamic MPD missing availabilityStartTime".into());
            }
        }
    }
    issues
}

/// DASH-IF IOP operational lint (timescale, timeline, live clock sync).
pub fn audit_dash_iop(xml: &str, summary: &DashSummary) -> Vec<DiagnosticFinding> {
    let mut out = Vec::new();
    let lower = xml.to_ascii_lowercase();

    if lower.contains("<segmenttimeline")
        && lower.contains("t=\"0\"")
        && lower.matches("<s ").count() > 1
    {
        let gaps = lower.matches("r=\"0\"").count();
        if gaps > 0 {
            out.push(DiagnosticFinding {
                category: DiagCategory::Rfc,
                severity: DiagSeverity::Warn,
                rule: "DASH_TIMELINE_GAP".into(),
                message: "SegmentTimeline may contain zero-repeat gaps".into(),
                reason: None,
            });
        }
    }

    if summary.type_live {
        if summary.availability_start_time.is_none() {
            out.push(DiagnosticFinding {
                category: DiagCategory::Rfc,
                severity: DiagSeverity::Error,
                rule: "DASH_AVAIL_START".into(),
                message: "Dynamic MPD missing availabilityStartTime".into(),
                reason: None,
            });
        }
        if let (Some(spd), Some(mup)) = (
            summary.suggested_presentation_delay_secs,
            summary.minimum_update_period_secs,
        ) {
            if spd < mup {
                out.push(DiagnosticFinding {
                    category: DiagCategory::Rfc,
                    severity: DiagSeverity::Warn,
                    rule: "DASH_SPD_MUP".into(),
                    message: format!(
                        "suggestedPresentationDelay ({spd}s) < minimumUpdatePeriod ({mup}s)"
                    ),
                    reason: None,
                });
            }
        }
    }

    if !lower.contains("timescale=") && summary.variants.is_empty() {
        out.push(DiagnosticFinding {
            category: DiagCategory::Rfc,
            severity: DiagSeverity::Warn,
            rule: "DASH_TIMESCALE".into(),
            message: "No Representation timescale found in MPD".into(),
            reason: None,
        });
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_vars_with_width() {
        assert_eq!(
            substitute_template_var("a/$Number$.m4s", "Number", 42),
            "a/42.m4s"
        );
        assert_eq!(
            substitute_template_var("a/$Number%05d$.m4s", "Number", 42),
            "a/00042.m4s"
        );
        assert_eq!(
            substitute_template_var("t$Time$-$Number$", "Time", 7),
            "t7-$Number$"
        );
    }

    const LIVE_NUMBER_MPD: &str = r#"<?xml version="1.0"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="dynamic" availabilityStartTime="1970-01-01T00:00:00Z" publishTime="1970-01-01T00:00:00Z" minimumUpdatePeriod="PT2S" minBufferTime="PT2S" profiles="urn:mpeg:dash:profile:isoff-live:2011">
 <Period id="p0" start="PT0S">
  <AdaptationSet contentType="video" mimeType="video/mp4" segmentAlignment="true">
   <SegmentTemplate media="$RepresentationID$/$Number%05d$.m4s" initialization="$RepresentationID$/init.mp4" duration="2" timescale="1" startNumber="0"/>
   <Representation id="V300" bandwidth="300000" width="640" height="360" codecs="avc1.64001e"/>
  </AdaptationSet>
  <AdaptationSet contentType="audio" mimeType="audio/mp4" lang="en">
   <Representation id="A48" bandwidth="48000" codecs="mp4a.40.2"/>
  </AdaptationSet>
 </Period>
</MPD>"#;

    #[test]
    fn live_edge_number_template_follows_wall_clock() {
        let base = Url::parse("https://ex.com/live/manifest.mpd").unwrap();
        // 101 s after AST with 2 s segments: 50 complete, edge is index 48.
        let s = parse_dash_mpd_at(LIVE_NUMBER_MPD, &base, 101_000).unwrap();
        let edge = s.live_edge.expect("live edge");
        assert_eq!(edge.key, 48);
        assert_eq!(edge.url, "https://ex.com/live/V300/00048.m4s");
        assert_eq!(edge.available_unix_ms, Some(98_000));
        let later = parse_dash_mpd_at(LIVE_NUMBER_MPD, &base, 103_000).unwrap();
        assert_eq!(later.live_edge.unwrap().key, 49);
    }

    #[test]
    fn live_edge_timeline_uses_last_s() {
        let xml = r#"<?xml version="1.0"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="dynamic" availabilityStartTime="2026-01-01T00:00:00Z" minBufferTime="PT2S" profiles="urn:mpeg:dash:profile:isoff-live:2011">
 <Period id="p0" start="PT0S">
  <AdaptationSet contentType="video" mimeType="video/mp4">
   <SegmentTemplate media="v/$Time$.m4s" initialization="v/init.mp4" timescale="90000">
    <SegmentTimeline><S t="900000" d="180000" r="2"/><S d="90000"/></SegmentTimeline>
   </SegmentTemplate>
   <Representation id="v" bandwidth="1000000" width="1280" height="720" codecs="avc1.64001f"/>
  </AdaptationSet>
 </Period>
</MPD>"#;
        let base = Url::parse("https://ex.com/a.mpd").unwrap();
        let s = parse_dash_mpd_at(xml, &base, 0).unwrap();
        let edge = s.live_edge.expect("live edge");
        // 900000 + 3 * 180000 = 1440000
        assert_eq!(edge.key, 1_440_000);
        assert_eq!(edge.url, "https://ex.com/v/1440000.m4s");
    }

    #[test]
    fn static_mpd_has_no_live_edge() {
        let xml = LIVE_NUMBER_MPD.replace("type=\"dynamic\"", "type=\"static\"");
        let base = Url::parse("https://ex.com/a.mpd").unwrap();
        assert!(parse_dash_mpd_at(&xml, &base, 101_000)
            .unwrap()
            .live_edge
            .is_none());
    }

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

    #[test]
    fn scan_ll_dash_service_description() {
        let xml = r#"<MPD><ServiceDescription><Latency target="PT2S" min="PT1S" max="PT4S"/></ServiceDescription>
        <SegmentTemplate availabilityTimeOffset="3.5"/>
        <UTCTiming schemeIdUri="urn:mpeg:dash:utc:direct:2014" value="2026-01-01T00:00:00Z"/>
        </MPD>"#;
        let info = scan_ll_dash_xml(xml);
        assert!(info.is_ll_dash);
        assert_eq!(info.latency_target_ms, Some(2000));
        assert_eq!(info.min_latency_ms, Some(1000));
        assert_eq!(info.max_latency_ms, Some(4000));
        assert!((info.availability_time_offset_secs.unwrap() - 3.5).abs() < 0.001);
        assert!(info.utc_timing_scheme.is_some());
    }

    #[test]
    fn utc_timing_truncated_mpd_is_none() {
        let xml = r#"<MPD><UTCTiming schemeIdUri="urn:mpeg:dash:utc:dir"#;
        assert!(parse_utc_timing_scheme(xml).is_none());
    }

    #[test]
    fn utc_timing_fixture_dash_live() {
        let xml = include_str!("../../tests/fixtures/dash_live.mpd");
        let scheme = parse_utc_timing_scheme(xml);
        assert!(scheme.is_some_and(|s| s.contains("http-iso")));
    }

    #[test]
    fn iso8601_duration_parses_seconds() {
        assert_eq!(iso8601_duration_to_ms("PT2.5S"), Some(2500));
        assert_eq!(iso8601_duration_to_ms("PT1M"), Some(60_000));
    }

    #[test]
    fn dash_eventstream_extracts_ad() {
        let xml = r#"<MPD><EventStream schemeIdUri="urn:scte:scte35:2013:xml" timescale="90000">
        <Event id="1" duration="900000"/>
        </EventStream></MPD>"#;
        let ads = extract_dash_ad_events(xml);
        assert_eq!(ads.len(), 1);
        assert_eq!(ads[0].kind, "DASH-Event");
        assert!(ads[0].active);
    }
}
