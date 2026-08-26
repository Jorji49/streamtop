//! Playlist / CDN / ABR / ad-tag / LL-HLS checks.

use std::time::Instant;

use crate::models::{
    AbrHealth, AbrVariant, AdBreakInfo, CacheVerdict, CdnEdgeInfo, CdnStats, ContainerKind,
    DiagCategory, DiagSeverity, DiagnosticFinding, HealthReport, LlHlsInfo,
    MEDIA_SEQ_GAP_TOLERANCE, STALL_MULTIPLIER, TARGET_DURATION_SLACK_SECS, TTFB_SPIKE_MS,
};

#[derive(Debug, Default)]
pub struct SpecLinter {
    last_media_sequence: Option<u64>,
    last_highest_seq: Option<u64>,
    last_refresh_at: Option<Instant>,
    last_new_segment_at: Option<Instant>,
    last_discontinuity_seq: Option<u64>,
    recent_ttfb: Vec<u64>,
    cdn: CdnStats,
    flags: HealthFlags,
    findings_buffer: Vec<DiagnosticFinding>,
}

#[derive(Debug, Default, Clone)]
struct HealthFlags {
    rfc_violation: bool,
    origin_stall: bool,
    cdn_miss: bool,
    high_ttfb: bool,
}

impl SpecLinter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn take_findings(&mut self) -> Vec<DiagnosticFinding> {
        std::mem::take(&mut self.findings_buffer)
    }

    pub fn cdn_stats(&self) -> CdnStats {
        self.cdn.clone()
    }

    fn push(
        &mut self,
        category: DiagCategory,
        severity: DiagSeverity,
        rule: impl Into<String>,
        message: impl Into<String>,
    ) {
        let finding = DiagnosticFinding {
            category,
            severity,
            rule: rule.into(),
            message: message.into(),
        };
        match category {
            DiagCategory::Rfc => self.flags.rfc_violation = true,
            DiagCategory::Stalling => self.flags.origin_stall = true,
            _ => {}
        }
        self.findings_buffer.push(finding);
    }

    pub fn on_playlist_refresh(
        &mut self,
        media_sequence: u64,
        target_duration: u64,
        segment_count: usize,
        now: Instant,
    ) -> Option<u64> {
        let refresh_ms = self
            .last_refresh_at
            .map(|t| now.duration_since(t).as_millis() as u64);

        if let Some(prev) = self.last_refresh_at {
            if target_duration > 0 {
                let elapsed = now.duration_since(prev);
                let limit = std::time::Duration::from_secs(target_duration);
                if elapsed > limit && segment_count > 0 {
                    self.push(
                        DiagCategory::Rfc,
                        DiagSeverity::Error,
                        "PLAYLIST_REFRESH",
                        format!(
                            "Playlist not refreshed for {elapsed:?} (limit ≤ {target_duration}s) — RFC 8216 §6.2.1"
                        ),
                    );
                }
            }
        }

        self.last_refresh_at = Some(now);

        if let Some(prev) = self.last_media_sequence {
            if media_sequence < prev {
                self.push(
                    DiagCategory::Rfc,
                    DiagSeverity::Error,
                    "MEDIA_SEQUENCE_REGRESS",
                    format!(
                        "MEDIA-SEQUENCE moved backwards ({prev} → {media_sequence}) — packager gap"
                    ),
                );
            } else {
                let gap = media_sequence.saturating_sub(prev);
                if gap > MEDIA_SEQ_GAP_TOLERANCE {
                    self.push(
                        DiagCategory::Rfc,
                        DiagSeverity::Warn,
                        "MEDIA_SEQUENCE_GAP",
                        format!(
                            "MEDIA-SEQUENCE jumped by {gap} ({prev} → {media_sequence}) — possible packet loss"
                        ),
                    );
                }
            }
        }
        self.last_media_sequence = Some(media_sequence);
        refresh_ms
    }

    pub fn on_new_segment(
        &mut self,
        seq: u64,
        duration_secs: f32,
        target_duration: u64,
        discontinuity: bool,
        discontinuity_sequence: u64,
        now: Instant,
    ) {
        let max_allowed = target_duration as f32 + TARGET_DURATION_SLACK_SECS;
        if duration_secs > max_allowed {
            self.push(
                DiagCategory::Rfc,
                DiagSeverity::Error,
                "TARGET_DURATION",
                format!(
                    "Segment {seq} duration {duration_secs:.3}s exceeds TARGETDURATION+0.5 ({max_allowed:.1}s)"
                ),
            );
        }

        if let Some(prev) = self.last_highest_seq {
            if seq < prev {
                self.push(
                    DiagCategory::Rfc,
                    DiagSeverity::Error,
                    "SEQ_REGRESS",
                    format!("Segment sequence moved backwards ({prev} → {seq})"),
                );
            } else if seq > prev + MEDIA_SEQ_GAP_TOLERANCE {
                self.push(
                    DiagCategory::Rfc,
                    DiagSeverity::Warn,
                    "SEQ_SKIP",
                    format!(
                        "Segment sequence gap too large (last={prev}, got={seq}, tolerance={MEDIA_SEQ_GAP_TOLERANCE})"
                    ),
                );
            }
        }
        self.last_highest_seq = Some(seq.max(self.last_highest_seq.unwrap_or(0)));

        if discontinuity {
            self.push(
                DiagCategory::Rfc,
                DiagSeverity::Info,
                "DISCONTINUITY",
                format!(
                    "EXT-X-DISCONTINUITY at seq={seq} (discontinuity-sequence={discontinuity_sequence})"
                ),
            );
            if let Some(prev_d) = self.last_discontinuity_seq {
                if discontinuity_sequence > 0 && discontinuity_sequence < prev_d {
                    self.push(
                        DiagCategory::Rfc,
                        DiagSeverity::Warn,
                        "DISCONTINUITY_SEQ",
                        format!(
                            "DISCONTINUITY-SEQUENCE decreased ({prev_d} → {discontinuity_sequence})"
                        ),
                    );
                }
            }
            if discontinuity_sequence > 0 {
                self.last_discontinuity_seq = Some(discontinuity_sequence);
            }
        }

        self.last_new_segment_at = Some(now);
        self.flags.origin_stall = false;
    }

    pub fn check_stalling(&mut self, target_duration: u64, now: Instant) {
        if target_duration == 0 {
            return;
        }
        let Some(last) = self.last_new_segment_at else {
            return;
        };
        let limit_secs = target_duration as f64 * STALL_MULTIPLIER;
        if limit_secs <= 0.0 {
            return;
        }
        let elapsed = now.duration_since(last).as_secs_f64();
        if elapsed > limit_secs {
            self.flags.origin_stall = true;
            self.push(
                DiagCategory::Stalling,
                DiagSeverity::Warn,
                "ORIGIN_STALL",
                format!(
                    "[ORIGIN STALLING] no new segment for {elapsed:.1}s (threshold {limit_secs:.1}s = target×{STALL_MULTIPLIER})"
                ),
            );
        }
    }

    pub fn on_cdn_headers(&mut self, info: CdnEdgeInfo, ttfb_ms: u64, seq: u64) {
        self.cdn.record(&info);
        match info.verdict {
            CacheVerdict::Miss => {
                self.flags.cdn_miss = true;
                self.push(
                    DiagCategory::Cdn,
                    DiagSeverity::Warn,
                    "CACHE_MISS",
                    format!("seq={seq} {}", info.badge()),
                );
            }
            CacheVerdict::Hit => {
                let age = info.age.map(|a| format!(" age={a}s")).unwrap_or_default();
                let pop = info
                    .pop
                    .as_deref()
                    .map(|p| format!(" pop={p}"))
                    .unwrap_or_default();
                self.push(
                    DiagCategory::Cdn,
                    DiagSeverity::Info,
                    "CACHE_HIT",
                    format!("seq={seq} {}{age}{pop}", info.badge()),
                );
            }
            CacheVerdict::Unknown => {}
        }

        let avg = if self.recent_ttfb.is_empty() {
            ttfb_ms
        } else {
            self.recent_ttfb.iter().sum::<u64>() / self.recent_ttfb.len() as u64
        };
        self.recent_ttfb.push(ttfb_ms);
        if self.recent_ttfb.len() > 20 {
            self.recent_ttfb.remove(0);
        }

        if ttfb_ms > TTFB_SPIKE_MS {
            self.flags.high_ttfb = true;
            self.push(
                DiagCategory::Cdn,
                DiagSeverity::Warn,
                "TTFB_HIGH",
                format!("TTFB exceeds {TTFB_SPIKE_MS}ms (seq={seq} ttfb={ttfb_ms}ms)"),
            );
        } else if self.recent_ttfb.len() >= 5 && ttfb_ms > avg.saturating_mul(3).max(200) {
            self.push(
                DiagCategory::Cdn,
                DiagSeverity::Warn,
                "TTFB_SPIKE",
                format!("seq={seq} TTFB spike {ttfb_ms}ms (avg {avg}ms)"),
            );
        }
    }

    pub fn compute_health(&mut self) -> HealthReport {
        let mut score: i32 = 100;
        let mut deductions = Vec::new();

        if self.flags.rfc_violation {
            score -= 15;
            deductions.push("RFC violation (−15)".into());
        }
        if self.flags.origin_stall {
            score -= 20;
            deductions.push("Origin stalling (−20)".into());
        }
        if self.flags.cdn_miss {
            score -= 5;
            deductions.push("CDN cache MISS (−5)".into());
        }
        if self.flags.high_ttfb {
            score -= 10;
            deductions.push("High TTFB (−10)".into());
        }

        let score = score.clamp(0, 100) as u8;
        let label = health_label(score);

        self.flags.cdn_miss = false;
        self.flags.high_ttfb = false;

        HealthReport {
            score,
            label,
            deductions,
        }
    }

    pub fn clear_rfc_flag_if_clean(&mut self) {
        if !self
            .findings_buffer
            .iter()
            .any(|f| f.category == DiagCategory::Rfc && f.severity == DiagSeverity::Error)
        {
            self.flags.rfc_violation = false;
        }
    }
}

fn health_label(score: u8) -> String {
    if score >= 90 {
        "Excellent".into()
    } else if score >= 70 {
        "Fair".into()
    } else {
        "Critical".into()
    }
}

pub fn analyze_abr_ladder(variants: &[AbrVariant]) -> AbrHealth {
    let mut warnings = Vec::new();
    let mut penalty: u8 = 0;

    let mut sorted: Vec<&AbrVariant> = variants.iter().filter(|v| v.bandwidth > 0).collect();
    sorted.sort_by_key(|v| v.bandwidth);

    for pair in sorted.windows(2) {
        let low = pair[0].bandwidth as f64;
        let high = pair[1].bandwidth as f64;
        if low <= 0.0 {
            continue;
        }
        let delta_pct = ((high - low) / low) * 100.0;
        if delta_pct < 15.0 {
            warnings.push(format!(
                "Redundant ABR rung: {}→{} kbps (Δ {delta_pct:.1}% < 15%)",
                pair[0].bandwidth / 1000,
                pair[1].bandwidth / 1000
            ));
            penalty = penalty.saturating_add(3);
        }
    }

    for v in &sorted {
        let height = parse_height(v.resolution.as_deref());
        let kbps = v.bandwidth / 1000;
        if let Some(h) = height {
            if h >= 1080 && kbps < 2500 {
                warnings.push(format!(
                    "Inefficient encode: {h}p @ {kbps} kbps (bitrate too low)"
                ));
                penalty = penalty.saturating_add(5);
            }
            if h > 0 && h <= 480 && kbps > 4000 {
                warnings.push(format!(
                    "Inefficient encode: {h}p @ {kbps} kbps (bitrate too high)"
                ));
                penalty = penalty.saturating_add(5);
            }
            if h >= 1440 && kbps < 5000 {
                warnings.push(format!(
                    "Inefficient encode: {h}p @ {kbps} kbps (high res / low bitrate)"
                ));
                penalty = penalty.saturating_add(4);
            }
        }
    }

    AbrHealth {
        warnings,
        score_penalty: penalty.min(20),
    }
}

fn parse_height(res: Option<&str>) -> Option<u64> {
    let res = res?;
    let mut parts = res.split('x');
    let _w = parts.next()?;
    parts.next()?.parse().ok()
}

pub fn parse_cdn_headers(headers: &reqwest::header::HeaderMap) -> CdnEdgeInfo {
    let get = |name: &str| -> Option<String> {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string())
    };

    let server = get("server");
    let x_cache = get("x-cache");
    let cf_cache = get("cf-cache-status");
    let x_cache_status = get("x-cache-status");
    let x_check_cacheable = get("x-check-cacheable");
    let age = get("age").and_then(|s| s.parse().ok());
    let amz_pop = get("x-amz-cf-pop");
    let cf_ray = get("cf-ray");
    let served_by = get("x-served-by");
    let via = get("via");

    let provider = detect_cdn_provider(
        server.as_deref(),
        cf_cache.as_deref(),
        x_cache.as_deref(),
        amz_pop.as_deref(),
        served_by.as_deref(),
        via.as_deref(),
        x_check_cacheable.as_deref(),
        cf_ray.as_deref(),
    );

    let cache_status = cf_cache
        .clone()
        .or_else(|| x_cache.clone())
        .or(x_cache_status);

    let verdict = match provider.as_deref() {
        Some("Akamai") => classify_akamai(x_cache.as_deref()),
        Some("Cloudflare") => classify_cloudflare(cf_cache.as_deref()),
        Some("CloudFront") => classify_cloudfront(x_cache.as_deref()),
        Some("Fastly") => classify_fastly(x_cache.as_deref(), age),
        _ => classify_cache(cache_status.as_deref()),
    };

    let pop =
        amz_pop.or_else(|| cf_ray.map(|r| r.split('-').next_back().unwrap_or(&r).to_string()));
    let served_by = served_by.or(server);

    CdnEdgeInfo {
        verdict,
        provider,
        cache_status,
        age,
        pop,
        served_by,
        via,
    }
}

#[allow(clippy::too_many_arguments)]
fn detect_cdn_provider(
    server: Option<&str>,
    cf_cache: Option<&str>,
    x_cache: Option<&str>,
    amz_pop: Option<&str>,
    served_by: Option<&str>,
    via: Option<&str>,
    x_check_cacheable: Option<&str>,
    cf_ray: Option<&str>,
) -> Option<String> {
    let server_u = server.map(|s| s.to_ascii_uppercase()).unwrap_or_default();
    let x_cache_u = x_cache.map(|s| s.to_ascii_uppercase()).unwrap_or_default();
    let via_u = via.map(|s| s.to_ascii_uppercase()).unwrap_or_default();
    let served_u = served_by
        .map(|s| s.to_ascii_uppercase())
        .unwrap_or_default();

    if server_u.contains("AKAMAIGHOST")
        || x_check_cacheable.is_some()
        || x_cache_u.contains("TCP_HIT")
        || x_cache_u.contains("TCP_MISS")
        || x_cache_u.contains("TCP_MEM")
        || x_cache_u.contains("TCP_REFRESH")
    {
        return Some("Akamai".into());
    }
    if cf_cache.is_some() || cf_ray.is_some() || server_u.contains("CLOUDFLARE") {
        return Some("Cloudflare".into());
    }
    if amz_pop.is_some() || x_cache_u.contains("CLOUDFRONT") || server_u.contains("CLOUDFRONT") {
        return Some("CloudFront".into());
    }
    if served_u.contains("CACHE-")
        || via_u.contains("VARNISH")
        || via_u.contains("FASTLY")
        || server_u.contains("VARNISH")
        || (served_by.is_some() && x_cache.is_some() && amz_pop.is_none() && cf_cache.is_none())
    {
        return Some("Fastly".into());
    }
    None
}

fn classify_akamai(x_cache: Option<&str>) -> CacheVerdict {
    let Some(s) = x_cache else {
        return CacheVerdict::Unknown;
    };
    let u = s.to_ascii_uppercase();
    if u.contains("TCP_HIT") || u.contains("TCP_MEM_HIT") {
        CacheVerdict::Hit
    } else if u.contains("TCP_MISS")
        || u.contains("TCP_REFRESH_MISS")
        || u.contains("TCP_CLIENT_REFRESH_MISS")
    {
        CacheVerdict::Miss
    } else if u.contains("HIT") {
        CacheVerdict::Hit
    } else if u.contains("MISS") {
        CacheVerdict::Miss
    } else {
        CacheVerdict::Unknown
    }
}

fn classify_cloudflare(status: Option<&str>) -> CacheVerdict {
    let Some(s) = status else {
        return CacheVerdict::Unknown;
    };
    match s.to_ascii_uppercase().as_str() {
        "HIT" | "REVALIDATED" => CacheVerdict::Hit,
        "MISS" | "EXPIRED" | "BYPASS" | "DYNAMIC" | "UPDATING" => CacheVerdict::Miss,
        other if other.contains("HIT") => CacheVerdict::Hit,
        other if other.contains("MISS") || other.contains("EXPIRED") => CacheVerdict::Miss,
        _ => CacheVerdict::Unknown,
    }
}

fn classify_cloudfront(x_cache: Option<&str>) -> CacheVerdict {
    let Some(s) = x_cache else {
        return CacheVerdict::Unknown;
    };
    let u = s.to_ascii_uppercase();
    // RefreshHit = conditional revalidation served from edge → treat as Hit.
    if u.contains("HIT FROM CLOUDFRONT")
        || u.contains("REFRESHHIT")
        || u.contains("REFRESH_HIT")
        || (u.contains("HIT") && u.contains("CLOUDFRONT"))
    {
        CacheVerdict::Hit
    } else if u.contains("MISS FROM CLOUDFRONT")
        || (u.contains("MISS") && u.contains("CLOUDFRONT"))
    {
        CacheVerdict::Miss
    } else {
        classify_cache(Some(s))
    }
}

fn classify_fastly(x_cache: Option<&str>, age: Option<u64>) -> CacheVerdict {
    if let Some(s) = x_cache {
        let u = s.to_ascii_uppercase();
        if u.split(',').any(|p| p.trim() == "HIT") && !u.contains("MISS") {
            return CacheVerdict::Hit;
        }
        if u.contains("HIT") && !u.starts_with("MISS") {
            return CacheVerdict::Hit;
        }
        if u.contains("MISS") {
            return CacheVerdict::Miss;
        }
    }
    // Age alone is ambiguous without x-cache / x-served-by confirmation.
    let _ = age;
    CacheVerdict::Unknown
}

fn classify_cache(status: Option<&str>) -> CacheVerdict {
    let Some(s) = status else {
        return CacheVerdict::Unknown;
    };
    let upper = s.to_ascii_uppercase();
    if upper.contains("HIT") && !upper.contains("MISS") {
        CacheVerdict::Hit
    } else if upper.contains("MISS")
        || upper.contains("EXPIRED")
        || upper.contains("BYPASS")
        || upper.contains("ORIGIN")
        || upper.contains("DYNAMIC")
    {
        CacheVerdict::Miss
    } else {
        CacheVerdict::Unknown
    }
}

/// Only scan tags associated with the last N media segments (live edge).
pub fn extract_ad_signals_near_live_edge(
    raw_playlist: &str,
    keep_last_segments: usize,
    now: chrono::DateTime<chrono::Utc>,
) -> Vec<AdBreakInfo> {
    let lines: Vec<&str> = raw_playlist.lines().collect();
    let mut uri_indices = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let t = line.trim();
        if !t.is_empty() && !t.starts_with('#') {
            uri_indices.push(i);
        }
    }

    if uri_indices.is_empty() {
        return Vec::new();
    }

    let keep = keep_last_segments.max(1);
    let start_uri = uri_indices.len().saturating_sub(keep);
    let start_line = if start_uri == 0 {
        0
    } else {
        uri_indices[start_uri - 1].saturating_add(1)
    };

    extract_ad_signals_from_lines(lines[start_line..].iter().copied(), now)
}

fn extract_ad_signals_from_lines<'a, I>(
    lines: I,
    now: chrono::DateTime<chrono::Utc>,
) -> Vec<AdBreakInfo>
where
    I: IntoIterator<Item = &'a str>,
{
    let mut out = Vec::new();

    for line in lines {
        let trimmed = line.trim();
        if trimmed.starts_with("#EXT-X-CUE-OUT-CONT") {
            let duration =
                attr_float_ci(trimmed, "Duration").or_else(|| attr_float_ci(trimmed, "DURATION"));
            let elapsed = attr_float_ci(trimmed, "ElapsedTime")
                .or_else(|| attr_float_ci(trimmed, "ELAPSEDTIME"))
                .or_else(|| attr_float_ci(trimmed, "Elapsed"));
            let remaining = match (duration, elapsed) {
                (Some(d), Some(e)) => Some((d - e).max(0.0)),
                (Some(d), None) => Some(d),
                _ => None,
            };
            out.push(ad_info(
                "CUE-OUT-CONT",
                None,
                duration,
                elapsed,
                remaining,
                true,
            ));
        } else if trimmed.starts_with("#EXT-X-CUE-OUT") {
            let duration = attr_float_ci(trimmed, "DURATION")
                .or_else(|| attr_float_ci(trimmed, "Duration"))
                .or_else(|| {
                    trimmed.split_once(':').and_then(|(_, rest)| {
                        let rest = rest.trim();
                        if rest.is_empty() {
                            None
                        } else {
                            rest.split(',').next()?.trim().parse().ok()
                        }
                    })
                });
            out.push(ad_info(
                "CUE-OUT",
                None,
                duration,
                Some(0.0),
                duration,
                true,
            ));
        } else if trimmed.starts_with("#EXT-X-CUE-IN") {
            out.push(ad_info("CUE-IN", None, None, None, Some(0.0), false));
        } else if trimmed.starts_with("#EXT-OATCLS-SCTE35") || trimmed.starts_with("#EXT-X-SCTE35")
        {
            let mut info = ad_info("SCTE35", None, None, None, None, true);
            if let Some(section) = crate::engine::scte35::parse_scte35_tag(trimmed) {
                info.scte35_binary = Some(section.summary_line());
                info.summary = section.summary_line();
                if let Some(seg) = section.descriptors.first() {
                    info.planned_duration_secs = seg.segmentation_duration_secs;
                    info.id = Some(seg.segmentation_event_id.to_string());
                    if matches!(seg.segmentation_type_id, 0x31 | 0x33 | 0x35 | 0x37) {
                        info.active = false;
                    }
                }
                if let Some(oon) = section.out_of_network_indicator {
                    info.active = oon;
                }
            }
            out.push(info);
        } else if trimmed.starts_with("#EXT-X-DATERANGE") {
            let id = attr_quoted(trimmed, "ID");
            let planned = attr_float_ci(trimmed, "PLANNED-DURATION")
                .or_else(|| attr_float_ci(trimmed, "DURATION"));
            let start = attr_quoted(trimmed, "START-DATE")
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(&s).ok())
                .map(|d| d.with_timezone(&chrono::Utc));
            let remaining = match (start, planned) {
                (Some(start), Some(dur)) => {
                    let end = start + chrono::Duration::milliseconds((dur * 1000.0) as i64);
                    Some((end - now).num_milliseconds() as f64 / 1000.0)
                }
                _ => planned,
            };
            let class = attr_quoted(trimmed, "CLASS").unwrap_or_default();
            let is_ad = class.to_ascii_lowercase().contains("ad")
                || class.to_ascii_lowercase().contains("scte")
                || trimmed.contains("SCTE35")
                || trimmed.contains("X-AD")
                || planned.is_some();
            if is_ad {
                let kind = if class.is_empty() {
                    "DATERANGE".into()
                } else {
                    format!("DATERANGE/{class}")
                };
                let mut info = ad_info(
                    &kind,
                    id,
                    planned,
                    None,
                    remaining,
                    remaining.map(|r| r > 0.0).unwrap_or(true),
                );
                if let Some(section) = crate::engine::scte35::parse_scte35_tag(trimmed) {
                    info.scte35_binary = Some(section.summary_line());
                    info.summary = format!("{} | {}", info.summary, section.summary_line());
                }
                out.push(info);
            }
        }
    }

    out
}

/// Dedup key for ad-break log lines.
pub fn ad_log_key(ad: &AdBreakInfo) -> String {
    match ad.kind.as_str() {
        "CUE-OUT-CONT" => format!(
            "cont:{}",
            ad.planned_duration_secs.unwrap_or(0.0).round() as i64
        ),
        "CUE-OUT" => format!(
            "out:{}",
            ad.planned_duration_secs.unwrap_or(0.0).round() as i64
        ),
        "CUE-IN" => "cue-in".into(),
        other => {
            if let Some(id) = &ad.id {
                format!("{other}:{id}")
            } else {
                format!("{other}:{}", ad.summary)
            }
        }
    }
}

fn ad_info(
    kind: &str,
    id: Option<String>,
    duration: Option<f64>,
    elapsed: Option<f64>,
    remaining: Option<f64>,
    active: bool,
) -> AdBreakInfo {
    let mut parts = vec![kind.to_string()];
    if let Some(d) = duration {
        parts.push(format!("Total: {d:.1}s"));
    }
    if let Some(r) = remaining {
        parts.push(format!("Remaining: {r:.1}s"));
    }
    if let Some(e) = elapsed {
        parts.push(format!("Elapsed: {e:.1}s"));
    }
    if let Some(ref id) = id {
        parts.push(format!("id={id}"));
    }
    AdBreakInfo {
        kind: kind.to_string(),
        id,
        planned_duration_secs: duration,
        elapsed_secs: elapsed,
        remaining_secs: remaining,
        summary: parts.join(" | "),
        active,
        scte35_binary: None,
    }
}

pub fn scan_ll_hls(raw: &str) -> LlHlsInfo {
    let mut info = LlHlsInfo::default();
    let mut last_part_uri: Option<String> = None;
    let mut last_part_br: Option<(u64, Option<u64>)> = None;

    for line in raw.lines() {
        let t = line.trim();
        if t.starts_with("#EXT-X-PART-INF") {
            info.is_ll_hls = true;
            info.part_target_secs = attr_float_ci(t, "PART-TARGET");
        } else if t.starts_with("#EXT-X-PART:") || t.starts_with("#EXT-X-PART\t") {
            info.is_ll_hls = true;
            info.part_count = info.part_count.saturating_add(1);
            info.last_part_sequence = Some(info.part_count);
            if let Some(d) = attr_float_ci(t, "DURATION") {
                info.last_part_duration_secs = Some(d);
                info.last_part_duration_ms = Some((d * 1000.0).round() as u64);
                if info.part_target_secs.is_none() {
                    info.part_target_secs = Some(d);
                }
            }
            if let Some(uri) = attr_quoted(t, "URI") {
                last_part_uri = Some(uri);
            }
            if let Some(br) = attr_quoted(t, "BYTERANGE").or_else(|| attr_raw(t, "BYTERANGE")) {
                last_part_br = parse_byterange_attr(&br);
            }
        } else if t.starts_with("#EXT-X-PRELOAD-HINT") {
            info.is_ll_hls = true;
            info.has_preload_hint = true;
            if let Some(uri) = attr_quoted(t, "URI") {
                info.preload_hint_uri = Some(uri);
            }
            if let Some(br) = attr_quoted(t, "BYTERANGE").or_else(|| attr_raw(t, "BYTERANGE")) {
                if let Some((len, off)) = parse_byterange_attr(&br) {
                    info.preload_byterange_length = Some(len);
                    info.preload_byterange_offset = off;
                }
            }
        } else if t.starts_with("#EXT-X-SERVER-CONTROL") {
            info.is_ll_hls = true;
            let upper = t.to_ascii_uppercase();
            if upper.contains("CAN-BLOCK-RELOAD=YES") || upper.contains("CAN-BLOCK-RELOAD=\"YES\"")
            {
                info.can_block_reload = true;
            }
        }
    }

    if info.preload_hint_uri.is_none() {
        if let Some(uri) = last_part_uri {
            info.has_preload_hint = info.has_preload_hint || info.part_count > 0;
            info.preload_hint_uri = Some(uri);
            if let Some((len, off)) = last_part_br {
                info.preload_byterange_length = Some(len);
                info.preload_byterange_offset = off;
            }
        }
    }
    info
}

/// Parse HLS BYTERANGE `n` or `n@o` → (length, offset).
pub fn parse_byterange_attr(raw: &str) -> Option<(u64, Option<u64>)> {
    let s = raw.trim().trim_matches('"');
    if let Some((len, off)) = s.split_once('@') {
        let length = len.parse().ok()?;
        let offset = off.parse().ok()?;
        Some((length, Some(offset)))
    } else {
        let length = s.parse().ok()?;
        Some((length, None))
    }
}

/// Build a Range header for LL-HLS part/hint probe (2 KB capped from offset).
pub fn ll_hls_probe_range(offset: Option<u64>, length: Option<u64>) -> String {
    let start = offset.unwrap_or(0);
    let max_len = crate::models::RANGE_PROBE_BYTES;
    let span = length
        .map(|l| l.min(max_len).saturating_sub(1))
        .unwrap_or(max_len);
    let end = start.saturating_add(span);
    format!("bytes={start}-{end}")
}

/// Next blocking-reload `_HLS_msn` / `_HLS_part` after the current playlist edge.
pub fn next_blocking_targets(
    media_sequence: u64,
    segment_count: usize,
    part_count: u32,
) -> (u64, Option<u64>) {
    if segment_count == 0 {
        return (media_sequence, Some(0));
    }
    let last_msn = media_sequence
        .saturating_add(segment_count as u64)
        .saturating_sub(1);
    if part_count > 0 {
        (last_msn, Some(u64::from(part_count)))
    } else {
        (last_msn.saturating_add(1), Some(0))
    }
}

/// Append RFC 8216bis blocking playlist reload query params (`_HLS_msn`, `_HLS_part`).
pub fn apply_hls_blocking_params(base_url: &str, msn: u64, part: Option<u64>) -> String {
    let sep = if base_url.contains('?') { '&' } else { '?' };
    match part {
        Some(p) => format!("{base_url}{sep}_HLS_msn={msn}&_HLS_part={p}"),
        None => format!("{base_url}{sep}_HLS_msn={msn}"),
    }
}

pub fn inspect_container(bytes: &[u8]) -> ContainerKind {
    if bytes
        .windows(4)
        .any(|w| w == b"ftyp" || w == b"styp" || w == b"moof" || w == b"traf")
    {
        return ContainerKind::Fmp4;
    }
    if bytes.first() == Some(&0x47) {
        return ContainerKind::Ts;
    }
    if bytes.len() >= 188 && bytes.iter().step_by(188).take(3).all(|b| *b == 0x47) {
        return ContainerKind::Ts;
    }
    ContainerKind::Unknown
}

/// Parse `#EXT-X-KEY` for DRM / encryption badges (AES-128, Sample-AES, Widevine, FairPlay).
pub fn scan_drm_keys(raw: &str) -> crate::models::DrmInfo {
    use crate::models::DrmInfo;
    let mut info = DrmInfo {
        present: false,
        method: None,
        key_format: None,
        badge: String::new(),
    };
    for line in raw.lines() {
        let t = line.trim();
        if !t.starts_with("#EXT-X-KEY") {
            continue;
        }
        info.present = true;
        let method = attr_quoted(t, "METHOD").or_else(|| {
            t.split("METHOD=")
                .nth(1)
                .and_then(|s| s.split(',').next())
                .map(|s| s.trim().trim_matches('"').to_string())
        });
        let key_format = attr_quoted(t, "KEYFORMAT");
        info.method = method.clone();
        info.key_format = key_format.clone();

        let upper_m = method.as_deref().unwrap_or("").to_ascii_uppercase();
        let upper_k = key_format.as_deref().unwrap_or("").to_ascii_lowercase();
        info.badge = if upper_k.contains("widevine") || upper_k.contains("clearkey") {
            "DRM · Widevine".into()
        } else if upper_k.contains("fairplay") || upper_k.contains("apple") {
            "DRM · FairPlay".into()
        } else if upper_k.contains("playready") {
            "DRM · PlayReady".into()
        } else if upper_m.contains("SAMPLE-AES") {
            "DRM · Sample-AES".into()
        } else if upper_m.contains("AES-128") {
            "ENC · AES-128".into()
        } else if !upper_m.is_empty() {
            format!("ENC · {upper_m}")
        } else {
            "DRM".into()
        };
        break;
    }
    info
}

/// Collect `#EXT-X-MEDIA` AUDIO / SUBTITLES renditions (language + name + format hints).
pub fn scan_media_renditions(raw: &str) -> crate::models::MediaRenditions {
    use crate::models::MediaRenditions;
    let mut out = MediaRenditions::default();
    for line in raw.lines() {
        let t = line.trim();
        if !t.starts_with("#EXT-X-MEDIA:") {
            continue;
        }
        let media_type = attr_quoted(t, "TYPE").unwrap_or_default();
        let name = attr_quoted(t, "NAME").unwrap_or_else(|| "unnamed".into());
        let lang = attr_quoted(t, "LANGUAGE").unwrap_or_else(|| "und".into());
        let channels = attr_quoted(t, "CHANNELS");
        let chars = attr_quoted(t, "CHARACTERISTICS");
        match media_type.to_ascii_uppercase().as_str() {
            "AUDIO" => {
                let extra = channels.map(|c| format!(" · {c}ch")).unwrap_or_default();
                out.audio.push(format!("{name} ({lang}){extra}"));
            }
            "SUBTITLES" | "CLOSED-CAPTIONS" => {
                let fmt = if chars
                    .as_deref()
                    .map(|c| c.to_ascii_lowercase().contains("cea-608") || c.contains("608"))
                    .unwrap_or(false)
                {
                    "CEA-608"
                } else if t.to_ascii_lowercase().contains("webvtt")
                    || chars
                        .as_deref()
                        .map(|c| c.to_ascii_lowercase().contains("vtt"))
                        .unwrap_or(false)
                {
                    "WebVTT"
                } else {
                    "subs"
                };
                out.subtitles.push(format!("{name} ({lang}) · {fmt}"));
            }
            _ => {}
        }
    }
    out
}

fn attr_quoted(line: &str, key: &str) -> Option<String> {
    let pattern = format!("{key}=\"");
    let start = line.find(&pattern)? + pattern.len();
    let rest = &line[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn attr_raw(line: &str, key: &str) -> Option<String> {
    let pattern = format!("{key}=");
    let start = line.find(&pattern)? + pattern.len();
    let rest = &line[start..];
    let token = rest
        .split([',', ' ', '\r', '\n'])
        .next()?
        .trim()
        .trim_matches('"');
    if token.is_empty() {
        None
    } else {
        Some(token.to_string())
    }
}

fn attr_float_ci(line: &str, key: &str) -> Option<f64> {
    let upper_line = line.to_ascii_uppercase();
    let upper_key = key.to_ascii_uppercase();
    let pattern = format!("{upper_key}=");
    let start = upper_line.find(&pattern)? + pattern.len();
    let rest = &line[start..];
    let token = rest.split([',', ' ', '\r', '\n']).next()?.trim_matches('"');
    token.parse().ok()
}

pub fn apply_abr_penalty(mut health: HealthReport, abr: &AbrHealth) -> HealthReport {
    if abr.score_penalty > 0 {
        health.score = health.score.saturating_sub(abr.score_penalty);
        health
            .deductions
            .push(format!("ABR inefficiency (−{})", abr.score_penalty));
        health.label = health_label(health.score);
    }
    health
}

#[cfg(test)]
mod cdn_tests {
    use super::*;
    use reqwest::header::{HeaderMap, HeaderValue};

    #[test]
    fn cloudfront_refresh_hit_is_hit() {
        assert_eq!(
            classify_cloudfront(Some("RefreshHit from cloudfront")),
            CacheVerdict::Hit
        );
    }

    #[test]
    fn fastly_age_alone_is_unknown() {
        assert_eq!(classify_fastly(None, Some(120)), CacheVerdict::Unknown);
    }

    #[test]
    fn fastly_x_cache_hit() {
        assert_eq!(classify_fastly(Some("HIT"), Some(0)), CacheVerdict::Hit);
    }

    #[test]
    fn parse_cdn_headers_cloudflare() {
        let mut h = HeaderMap::new();
        h.insert("cf-cache-status", HeaderValue::from_static("HIT"));
        h.insert("cf-ray", HeaderValue::from_static("abc-AMS"));
        h.insert("age", HeaderValue::from_static("42"));
        let edge = parse_cdn_headers(&h);
        assert_eq!(edge.provider.as_deref(), Some("Cloudflare"));
        assert_eq!(edge.verdict, CacheVerdict::Hit);
        assert_eq!(edge.age, Some(42));
    }

    #[test]
    fn inspect_ts_and_fmp4_in_2k_window() {
        let mut ts = vec![0u8; 2048];
        for i in (0..2048).step_by(188) {
            if i < ts.len() {
                ts[i] = 0x47;
            }
        }
        assert_eq!(inspect_container(&ts), ContainerKind::Ts);

        let mut fmp4 = vec![0u8; 2048];
        fmp4[4..8].copy_from_slice(b"moof");
        assert_eq!(inspect_container(&fmp4), ContainerKind::Fmp4);
    }
}
