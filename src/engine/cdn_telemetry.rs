//! CDN edge header parse, Server-Timing, and multi-origin drift metrics.

use crate::models::{CacheVerdict, CdnEdgeInfo, CdnStats, SegmentMetrics};

/// Parsed W3C Server-Timing metrics (ms).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServerTiming {
    pub edge_cache_ms: Option<u64>,
    pub origin_ms: Option<u64>,
    pub total_ms: Option<u64>,
}

/// Parse `Server-Timing` (e.g. `cdn-cache;desc=hit;dur=1, origin;dur=42`).
pub fn parse_server_timing(raw: &str) -> ServerTiming {
    let mut out = ServerTiming::default();
    for part in raw.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let name = part
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        let dur = part
            .split(';')
            .find_map(|p| {
                let p = p.trim();
                p.strip_prefix("dur=").and_then(|v| v.parse::<f64>().ok())
            })
            .map(|d| d.round() as u64);
        if name.contains("cdn") || name.contains("cache") || name == "edge" {
            out.edge_cache_ms = dur.or(out.edge_cache_ms);
        } else if name.contains("origin") || name == "fetch" {
            out.origin_ms = dur.or(out.origin_ms);
        }
        if let Some(d) = dur {
            out.total_ms = Some(out.total_ms.unwrap_or(0).saturating_add(d));
        }
    }
    out
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
    let cache_control = get("cache-control");
    let cdn_pullzone = get("cdn-pullzone");
    let bunny = get("cdn-proxyver").or_else(|| get("bunnycdn-cache"));
    let azure_ref = get("x-azure-ref").or_else(|| get("x-msedge-ref"));
    let goog = get("x-goog-generation")
        .or_else(|| get("x-goog-uploadid"))
        .or_else(|| get("x-guploader-uploadid"))
        .or_else(|| get("x-goog-hash"));
    let akamai_cache = get("akamai-cache-status");
    let x_cache_hits = get("x-cache-hits");

    let server_timing = get("server-timing").map(|s| parse_server_timing(&s));

    let provider = detect_cdn_provider(CdnDetectHints {
        server: server.as_deref(),
        cf_cache: cf_cache.as_deref(),
        x_cache: x_cache.as_deref(),
        amz_pop: amz_pop.as_deref(),
        served_by: served_by.as_deref(),
        via: via.as_deref(),
        x_check_cacheable: x_check_cacheable.as_deref(),
        cf_ray: cf_ray.as_deref(),
        cdn_pullzone: cdn_pullzone.as_deref(),
        bunny: bunny.as_deref(),
        azure_ref: azure_ref.as_deref(),
        goog: goog.as_deref(),
        cache_control: cache_control.as_deref(),
        akamai_cache: akamai_cache.as_deref(),
    });

    let cache_status = cf_cache
        .clone()
        .or_else(|| akamai_cache.clone())
        .or_else(|| x_cache.clone())
        .or(x_cache_status)
        .or_else(|| get("cdn-cache"));

    let verdict = match provider.as_deref() {
        Some("Akamai") => classify_akamai(x_cache.as_deref()),
        Some("Cloudflare") => classify_cloudflare(cf_cache.as_deref()),
        Some("CloudFront") => classify_cloudfront(x_cache.as_deref()),
        Some("Fastly") => classify_fastly(x_cache.as_deref(), age, cache_control.as_deref()),
        Some("BunnyCDN") => classify_bunny(cache_status.as_deref(), age, cache_control.as_deref()),
        Some("Azure CDN") => {
            classify_generic_cdn(cache_status.as_deref(), age, cache_control.as_deref())
        }
        Some("Google Cloud CDN") => {
            classify_generic_cdn(cache_status.as_deref(), age, cache_control.as_deref())
        }
        _ => classify_generic_cdn(cache_status.as_deref(), age, cache_control.as_deref()),
    };

    let pop = amz_pop
        .or_else(|| {
            cf_ray
                .as_deref()
                .map(|r| r.split('-').next_back().unwrap_or(r).to_string())
        })
        .or(azure_ref);

    let served_by = served_by.or(server);

    CdnEdgeInfo {
        verdict,
        provider,
        cache_status,
        age,
        pop,
        served_by,
        via,
        cf_ray,
        akamai_cache_status: akamai_cache,
        x_cache_hits,
        server_timing_edge_ms: server_timing.as_ref().and_then(|t| t.edge_cache_ms),
        server_timing_origin_ms: server_timing.as_ref().and_then(|t| t.origin_ms),
    }
}

struct CdnDetectHints<'a> {
    server: Option<&'a str>,
    cf_cache: Option<&'a str>,
    x_cache: Option<&'a str>,
    amz_pop: Option<&'a str>,
    served_by: Option<&'a str>,
    via: Option<&'a str>,
    x_check_cacheable: Option<&'a str>,
    cf_ray: Option<&'a str>,
    cdn_pullzone: Option<&'a str>,
    bunny: Option<&'a str>,
    azure_ref: Option<&'a str>,
    goog: Option<&'a str>,
    cache_control: Option<&'a str>,
    akamai_cache: Option<&'a str>,
}

fn detect_cdn_provider(h: CdnDetectHints<'_>) -> Option<String> {
    let server_u = h.server.map(|s| s.to_ascii_uppercase()).unwrap_or_default();
    let x_cache_u = h
        .x_cache
        .map(|s| s.to_ascii_uppercase())
        .unwrap_or_default();
    let via_u = h.via.map(|s| s.to_ascii_uppercase()).unwrap_or_default();
    let served_u = h
        .served_by
        .map(|s| s.to_ascii_uppercase())
        .unwrap_or_default();

    let cc_u = h
        .cache_control
        .map(|s| s.to_ascii_uppercase())
        .unwrap_or_default();

    if h.cf_cache.is_some() || h.cf_ray.is_some() {
        return Some("Cloudflare".into());
    }
    if h.amz_pop.is_some() || server_u.contains("CLOUDFRONT") || x_cache_u.contains("CLOUDFRONT") {
        return Some("CloudFront".into());
    }
    if h.akamai_cache.is_some()
        || server_u.contains("AKAMAI")
        || x_cache_u.contains("AKAMAI")
        || h.x_check_cacheable.is_some()
    {
        return Some("Akamai".into());
    }
    if served_u.contains("FASTLY") || via_u.contains("FASTLY") || server_u.contains("FASTLY") {
        return Some("Fastly".into());
    }
    if h.bunny.is_some() || h.cdn_pullzone.is_some() {
        return Some("BunnyCDN".into());
    }
    if h.azure_ref.is_some() || server_u.contains("AZURE") {
        return Some("Azure CDN".into());
    }
    if h.goog.is_some()
        || via_u.contains("GOOGLE")
        || server_u.contains("UPLOADSERVER")
        || (via_u.contains("GFE") && !cc_u.is_empty())
    {
        return Some("Google Cloud CDN".into());
    }
    if served_u.contains("CACHE-")
        || via_u.contains("VARNISH")
        || via_u.contains("FASTLY")
        || server_u.contains("VARNISH")
    {
        return Some("Fastly".into());
    }
    None
}

fn classify_akamai(x_cache: Option<&str>) -> CacheVerdict {
    let s = x_cache.unwrap_or("").to_ascii_uppercase();
    if s.contains("HIT") {
        CacheVerdict::Hit
    } else if s.contains("MISS") {
        CacheVerdict::Miss
    } else {
        CacheVerdict::Unknown
    }
}

fn classify_cloudflare(cf: Option<&str>) -> CacheVerdict {
    match cf.map(|s| s.to_ascii_uppercase()).as_deref() {
        Some(s) if s.contains("HIT") => CacheVerdict::Hit,
        Some(s) if s.contains("MISS") || s.contains("EXPIRED") || s.contains("BYPASS") => {
            CacheVerdict::Miss
        }
        _ => CacheVerdict::Unknown,
    }
}

fn classify_cloudfront(x_cache: Option<&str>) -> CacheVerdict {
    let s = x_cache.unwrap_or("").to_ascii_uppercase();
    if s.contains("HIT") {
        CacheVerdict::Hit
    } else if s.contains("MISS") {
        CacheVerdict::Miss
    } else {
        CacheVerdict::Unknown
    }
}

fn classify_fastly(
    x_cache: Option<&str>,
    age: Option<u64>,
    cache_control: Option<&str>,
) -> CacheVerdict {
    classify_generic_cdn(x_cache, age, cache_control)
}

fn classify_bunny(
    cache_status: Option<&str>,
    age: Option<u64>,
    cache_control: Option<&str>,
) -> CacheVerdict {
    classify_generic_cdn(cache_status, age, cache_control)
}

fn classify_generic_cdn(
    cache_status: Option<&str>,
    age: Option<u64>,
    cache_control: Option<&str>,
) -> CacheVerdict {
    let s = cache_status.unwrap_or("").to_ascii_uppercase();
    if s.contains("HIT") {
        return CacheVerdict::Hit;
    }
    if s.contains("MISS") || s.contains("BYPASS") || s.contains("EXPIRED") {
        return CacheVerdict::Miss;
    }
    if age.unwrap_or(0) > 0 {
        return CacheVerdict::Hit;
    }
    if let Some(cc) = cache_control {
        let cc = cc.to_ascii_lowercase();
        if cc.contains("no-store") || cc.contains("private") {
            return CacheVerdict::Miss;
        }
    }
    CacheVerdict::Unknown
}

/// Cross-CDN drift between two concurrent segment fetches.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CdnDriftMetrics {
    pub seq_delta: Option<i64>,
    pub latency_ms_delta: Option<i64>,
    pub pts_ms_delta: Option<i64>,
    pub g2g_ms_delta: Option<i64>,
    pub pop_left: Option<String>,
    pub pop_right: Option<String>,
}

pub fn compare_cdn_drift(left: &SegmentMetrics, right: &SegmentMetrics) -> CdnDriftMetrics {
    let seq_delta = Some(right.media_sequence as i64 - left.media_sequence as i64);

    let latency_ms_delta = right
        .latency_ms
        .map(|r| r as i64)
        .zip(left.latency_ms.map(|l| l as i64))
        .map(|(r, l)| r - l);

    let wire_l = left.wire.as_ref();
    let wire_r = right.wire.as_ref();
    let pts_left = wire_l
        .and_then(|w| w.timing.pcr_pts_drift_ms)
        .or_else(|| wire_l.and_then(|w| w.keyframe_pts_sec.map(|p| p * 1000.0)));
    let pts_right = wire_r
        .and_then(|w| w.timing.pcr_pts_drift_ms)
        .or_else(|| wire_r.and_then(|w| w.keyframe_pts_sec.map(|p| p * 1000.0)));
    let pts_ms_delta = pts_right.zip(pts_left).map(|(r, l)| (r - l).round() as i64);

    let g2g_ms_delta = wire_r
        .and_then(|w| w.timing.glass_to_glass_ms)
        .zip(wire_l.and_then(|w| w.timing.glass_to_glass_ms))
        .map(|(r, l)| r - l);

    CdnDriftMetrics {
        seq_delta,
        latency_ms_delta,
        pts_ms_delta,
        g2g_ms_delta,
        pop_left: left.cdn.pop.clone(),
        pop_right: right.cdn.pop.clone(),
    }
}

pub fn format_compare_drift(left: &CdnStats, right: &CdnStats, drift: &CdnDriftMetrics) -> String {
    let pop = match (&drift.pop_left, &drift.pop_right) {
        (Some(a), Some(b)) => format!("PoP {a} vs {b}"),
        _ => "PoP -".into(),
    };
    let seq = drift
        .seq_delta
        .map(|d| format!("Δ Seq {d:+}"))
        .unwrap_or_else(|| "Δ Seq -".into());
    let lat = drift
        .latency_ms_delta
        .map(|d| format!("Δ Lat {d:+}ms"))
        .unwrap_or_else(|| "Δ Lat -".into());
    let pts = drift
        .pts_ms_delta
        .map(|d| format!("Δ PTS {d:+}ms"))
        .unwrap_or_else(|| "Δ PTS -".into());
    let cache = format!(
        "Cache {} vs {}",
        left.last
            .as_ref()
            .map(|c| c.badge())
            .unwrap_or_else(|| "-".into()),
        right
            .last
            .as_ref()
            .map(|c| c.badge())
            .unwrap_or_else(|| "-".into())
    );
    format!("{seq}  |  {lat}  |  {pts}  |  {pop}  |  {cache}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::{HeaderMap, HeaderValue};

    #[test]
    fn server_timing_parse() {
        let t = parse_server_timing("cdn-cache;desc=hit;dur=1.2, origin;dur=45");
        assert_eq!(t.edge_cache_ms, Some(1));
        assert_eq!(t.origin_ms, Some(45));
    }

    #[test]
    fn cf_ray_pop_extract() {
        let mut h = HeaderMap::new();
        h.insert("cf-ray", HeaderValue::from_static("abc123-AMS"));
        h.insert("cf-cache-status", HeaderValue::from_static("HIT"));
        let info = parse_cdn_headers(&h);
        assert_eq!(info.provider.as_deref(), Some("Cloudflare"));
        assert_eq!(info.pop.as_deref(), Some("AMS"));
        assert_eq!(info.verdict, CacheVerdict::Hit);
    }
}
