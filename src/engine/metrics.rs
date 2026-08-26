//! Prometheus `/metrics` exporter (OpenMetrics text).

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::process::ExitCode;
use std::sync::{Arc, RwLock};

use axum::http::{header, HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use color_eyre::eyre::Result;
use subtle::ConstantTimeEq;
use tokio::sync::mpsc;

use crate::engine::channel_stats::channel_dropped_total;

use crate::engine::redact::redact_url;
use crate::engine::ManifestPoller;
use crate::models::{
    CdnStats, DiagCategory, LatencyState, StreamEvent, StreamStatusKind, EVENT_CHANNEL_CAPACITY,
};
use crate::ui::app::SessionOpts;

/// Default scrape port (`--prometheus` without value).
pub const DEFAULT_METRICS_PORT: u16 = 9184;

const TTFB_BUCKETS: &[f64] = &[
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];
const PART_BUCKETS: &[f64] = &[0.05, 0.1, 0.2, 0.5, 1.0, 2.0, 4.0];
const DRM_BUCKETS: &[f64] = &[0.05, 0.1, 0.25, 0.5, 1.0, 2.0, 5.0];

#[derive(Debug, Clone, Default)]
pub struct Hist {
    /// Counts per finite bucket + one for +Inf.
    buckets: Vec<u64>,
    sum: f64,
    count: u64,
}

impl Hist {
    fn with_bucket_count(n: usize) -> Self {
        Self {
            buckets: vec![0; n + 1],
            sum: 0.0,
            count: 0,
        }
    }

    fn observe(&mut self, bounds: &[f64], value: f64) {
        if !value.is_finite() || value < 0.0 {
            return;
        }
        self.sum += value;
        self.count = self.count.saturating_add(1);
        let mut placed = false;
        for (i, le) in bounds.iter().enumerate() {
            if value <= *le {
                self.buckets[i] = self.buckets[i].saturating_add(1);
                placed = true;
                break;
            }
        }
        if !placed {
            let last = self.buckets.len() - 1;
            self.buckets[last] = self.buckets[last].saturating_add(1);
        }
    }

    fn render(&self, name: &str, help: &str, labels: &str, bounds: &[f64]) -> String {
        let mut out = format!("# HELP {name} {help}\n# TYPE {name} histogram\n");
        let mut cumulative = 0u64;
        for (i, le) in bounds.iter().enumerate() {
            cumulative = cumulative.saturating_add(self.buckets.get(i).copied().unwrap_or(0));
            out.push_str(&format!(
                "{name}_bucket{{{labels},le=\"{le}\"}} {cumulative}\n"
            ));
        }
        cumulative = cumulative.saturating_add(self.buckets.last().copied().unwrap_or(0));
        out.push_str(&format!(
            "{name}_bucket{{{labels},le=\"+Inf\"}} {cumulative}\n"
        ));
        out.push_str(&format!("{name}_sum{{{labels}}} {:.6}\n", self.sum));
        out.push_str(&format!("{name}_count{{{labels}}} {}\n", self.count));
        out
    }
}

/// Shared metric state updated by the poller and scraped by `/metrics`.
#[derive(Debug, Clone)]
pub struct MetricsSnapshot {
    pub url: String,
    pub health_score: u8,
    pub segment_ttfb_secs: f64,
    pub latency_secs: f64,
    pub bitstream_fps: f64,
    pub cdn_hits: u64,
    pub cdn_misses: u64,
    pub cdn_provider: String,
    pub virtual_buffer_secs: f64,
    pub ad_active: f64,
    pub origin_stalls_total: u64,
    pub http_errors: HashMap<String, u64>,
    pub ll_hls_enabled: f64,
    pub codec_mismatch_total: u64,
    pub segment_ttfb_hist: Hist,
    pub llhls_part_hist: Hist,
    pub drm_license_hist: Hist,
    last_drm_ttfb_ms: Option<u64>,
}

impl Default for MetricsSnapshot {
    fn default() -> Self {
        Self {
            url: String::new(),
            health_score: 0,
            segment_ttfb_secs: 0.0,
            latency_secs: 0.0,
            bitstream_fps: 0.0,
            cdn_hits: 0,
            cdn_misses: 0,
            cdn_provider: String::new(),
            virtual_buffer_secs: 0.0,
            ad_active: 0.0,
            origin_stalls_total: 0,
            http_errors: HashMap::new(),
            ll_hls_enabled: 0.0,
            codec_mismatch_total: 0,
            segment_ttfb_hist: Hist::with_bucket_count(TTFB_BUCKETS.len()),
            llhls_part_hist: Hist::with_bucket_count(PART_BUCKETS.len()),
            drm_license_hist: Hist::with_bucket_count(DRM_BUCKETS.len()),
            last_drm_ttfb_ms: None,
        }
    }
}

pub fn update_metrics(snap: &mut MetricsSnapshot, event: &StreamEvent) {
    match event {
        StreamEvent::Health(h) => snap.health_score = h.score,
        StreamEvent::Segment(s) => {
            snap.segment_ttfb_secs = s.ttfb_ms as f64 / 1000.0;
            snap.segment_ttfb_hist
                .observe(TTFB_BUCKETS, snap.segment_ttfb_secs);
            if let Some(ms) = s.latency_ms {
                snap.latency_secs = ms as f64 / 1000.0;
            }
            if let Some(wire) = &s.wire {
                if let Some(fps) = wire.frame_rate {
                    if fps > 0.0 {
                        snap.bitstream_fps = fps;
                    }
                }
            }
        }
        StreamEvent::Variants(variants) => {
            if let Some(fps) = variants
                .iter()
                .find(|v| v.selected)
                .or_else(|| variants.first())
                .and_then(|v| v.frame_rate)
            {
                if fps > 0.0 {
                    snap.bitstream_fps = fps;
                }
            }
            for v in variants {
                if v.mismatch.is_some() {
                    snap.codec_mismatch_total = snap.codec_mismatch_total.saturating_add(1);
                }
            }
        }
        StreamEvent::Latency(l) => match l {
            LatencyState::Measured(ms) | LatencyState::Estimated(ms) => {
                snap.latency_secs = *ms as f64 / 1000.0;
            }
            LatencyState::Unknown => {}
        },
        StreamEvent::CdnStats(c) => apply_cdn(snap, c),
        StreamEvent::Buffer(b) => snap.virtual_buffer_secs = b.buffer_secs,
        StreamEvent::AdBreak(ad) => snap.ad_active = if ad.active { 1.0 } else { 0.0 },
        StreamEvent::PlaylistMeta(m) => {
            snap.ll_hls_enabled = if m.ll_hls.is_ll_hls { 1.0 } else { 0.0 };
            if let Some(part) = m.ll_hls.last_part_duration_secs {
                snap.llhls_part_hist.observe(PART_BUCKETS, part);
            }
            if let Some(ms) = m.drm.license_ttfb_ms {
                if snap.last_drm_ttfb_ms != Some(ms) {
                    snap.drm_license_hist
                        .observe(DRM_BUCKETS, ms as f64 / 1000.0);
                    snap.last_drm_ttfb_ms = Some(ms);
                }
            }
        }
        StreamEvent::Finding(f) => {
            if f.category == DiagCategory::Stalling {
                snap.origin_stalls_total = snap.origin_stalls_total.saturating_add(1);
            }
        }
        StreamEvent::Log {
            category: DiagCategory::Stalling,
            ..
        } => {
            snap.origin_stalls_total = snap.origin_stalls_total.saturating_add(1);
        }
        StreamEvent::Log { message, .. } if message.contains("[MISMATCH]") => {
            snap.codec_mismatch_total = snap.codec_mismatch_total.saturating_add(1);
        }
        StreamEvent::Error(msg) => {
            if let Some(code) = parse_http_status(msg) {
                *snap.http_errors.entry(code.to_string()).or_insert(0) = snap
                    .http_errors
                    .get(&code.to_string())
                    .copied()
                    .unwrap_or(0)
                    .saturating_add(1);
            }
        }
        StreamEvent::Status(s) if matches!(s.kind, StreamStatusKind::Error) => {
            if let Some(code) = parse_http_status(&s.message) {
                let key = code.to_string();
                *snap.http_errors.entry(key.clone()).or_insert(0) = snap
                    .http_errors
                    .get(&key)
                    .copied()
                    .unwrap_or(0)
                    .saturating_add(1);
            }
        }
        _ => {}
    }
}

fn parse_http_status(msg: &str) -> Option<u16> {
    for token in msg.split(|c: char| !c.is_ascii_digit()) {
        if token.len() == 3 {
            if let Ok(code) = token.parse::<u16>() {
                if (400..600).contains(&code) {
                    return Some(code);
                }
            }
        }
    }
    if let Some(idx) = msg.find("HTTP ") {
        let rest = &msg[idx + 5..];
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if let Ok(code) = digits.parse::<u16>() {
            if (400..600).contains(&code) {
                return Some(code);
            }
        }
    }
    None
}

fn apply_cdn(snap: &mut MetricsSnapshot, cdn: &CdnStats) {
    snap.cdn_hits = cdn.hits;
    snap.cdn_misses = cdn.misses;
    if let Some(last) = &cdn.last {
        snap.cdn_provider = last.provider.clone().unwrap_or_else(|| "unknown".into());
    }
}

fn label_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

pub fn render_openmetrics(snap: &MetricsSnapshot) -> String {
    let url = label_escape(&redact_url(&snap.url));
    let cdn = label_escape(&snap.cdn_provider);
    let labels = format!("url=\"{url}\"");
    let mut out = format!(
        r#"# HELP streamtop_stream_health_score Stream Health Index (SHI) 0-100
# TYPE streamtop_stream_health_score gauge
streamtop_stream_health_score{{{labels}}} {health}
# HELP streamtop_latency_seconds Live-edge latency (PDT or estimated)
# TYPE streamtop_latency_seconds gauge
streamtop_latency_seconds{{{labels}}} {latency:.6}
# HELP streamtop_bitstream_fps Declared or wire-probed video frame rate
# TYPE streamtop_bitstream_fps gauge
streamtop_bitstream_fps{{{labels}}} {fps:.3}
# HELP streamtop_cdn_cache_hits_total CDN edge cache hits
# TYPE streamtop_cdn_cache_hits_total counter
streamtop_cdn_cache_hits_total{{{labels},cdn="{cdn}"}} {hits}
# HELP streamtop_cdn_cache_misses_total CDN edge cache misses
# TYPE streamtop_cdn_cache_misses_total counter
streamtop_cdn_cache_misses_total{{{labels},cdn="{cdn}"}} {misses}
# HELP streamtop_virtual_buffer_seconds Simulated player buffer depth
# TYPE streamtop_virtual_buffer_seconds gauge
streamtop_virtual_buffer_seconds{{{labels}}} {vbuf:.3}
# HELP streamtop_ad_active DAI ad break active (1=yes, 0=no)
# TYPE streamtop_ad_active gauge
streamtop_ad_active{{{labels}}} {ad:.0}
# HELP streamtop_origin_stalls_total Origin stalling alarms
# TYPE streamtop_origin_stalls_total counter
streamtop_origin_stalls_total{{{labels}}} {stalls}
# HELP streamtop_ll_hls_enabled LL-HLS detected on playlist (1=yes, 0=no)
# TYPE streamtop_ll_hls_enabled gauge
streamtop_ll_hls_enabled{{{labels}}} {ll:.0}
# HELP streamtop_codec_mismatch_total Manifest vs wire codec/resolution/FPS mismatches
# TYPE streamtop_codec_mismatch_total counter
streamtop_codec_mismatch_total{{{labels}}} {mismatch}
# HELP streamtop_channel_dropped_total Events dropped from bounded poller channels
# TYPE streamtop_channel_dropped_total counter
streamtop_channel_dropped_total{{{labels}}} {drops}
# HELP streamtop_http_errors_total HTTP 4xx/5xx responses
# TYPE streamtop_http_errors_total counter
"#,
        health = snap.health_score,
        latency = snap.latency_secs,
        fps = snap.bitstream_fps,
        hits = snap.cdn_hits,
        misses = snap.cdn_misses,
        vbuf = snap.virtual_buffer_secs,
        ad = snap.ad_active,
        stalls = snap.origin_stalls_total,
        ll = snap.ll_hls_enabled,
        mismatch = snap.codec_mismatch_total,
        drops = channel_dropped_total(),
    );

    if snap.http_errors.is_empty() {
        out.push_str(&format!(
            "streamtop_http_errors_total{{{labels},status=\"none\"}} 0\n"
        ));
    } else {
        let mut keys: Vec<_> = snap.http_errors.keys().cloned().collect();
        keys.sort();
        for status in keys {
            let n = snap.http_errors[&status];
            out.push_str(&format!(
                "streamtop_http_errors_total{{{labels},status=\"{status}\"}} {n}\n"
            ));
        }
    }

    out.push_str(&snap.segment_ttfb_hist.render(
        "streamtop_segment_ttfb_seconds",
        "Segment time-to-first-byte",
        &labels,
        TTFB_BUCKETS,
    ));
    out.push_str(&snap.llhls_part_hist.render(
        "streamtop_llhls_part_duration_seconds",
        "LL-HLS part duration",
        &labels,
        PART_BUCKETS,
    ));
    out.push_str(&snap.drm_license_hist.render(
        "streamtop_drm_license_ttfb_seconds",
        "DRM license / key URI TTFB",
        &labels,
        DRM_BUCKETS,
    ));
    out
}

#[derive(Debug, Clone)]
struct MetricsAuth {
    token: Option<String>,
}

/// Validate `Authorization: Bearer <token>` using constant-time comparison.
pub fn authorize_metrics_bearer(headers: &HeaderMap, expected: &str) -> bool {
    let Some(value) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
    else {
        return false;
    };
    let Some(token) = value.strip_prefix("Bearer ") else {
        return false;
    };
    if token.len() != expected.len() {
        return false;
    }
    token.as_bytes().ct_eq(expected.as_bytes()).into()
}

async fn metrics_handler(
    axum::extract::State(state): axum::extract::State<Arc<RwLock<MetricsSnapshot>>>,
    axum::Extension(auth): axum::Extension<MetricsAuth>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Some(expected) = &auth.token {
        if !authorize_metrics_bearer(&headers, expected) {
            return (
                StatusCode::UNAUTHORIZED,
                [("content-type", "text/plain; charset=utf-8")],
                "unauthorized\n".to_string(),
            );
        }
    }
    let snap = state.read().unwrap_or_else(|e| e.into_inner());
    (
        StatusCode::OK,
        [("content-type", "text/plain; version=0.0.4; charset=utf-8")],
        render_openmetrics(&snap),
    )
}

/// Run headless diagnostics with Prometheus scrape endpoint (no TUI).
pub async fn run_prometheus(
    url: String,
    session: SessionOpts,
    port: u16,
    bind: IpAddr,
    metrics_token: Option<String>,
) -> Result<ExitCode> {
    let metrics = Arc::new(RwLock::new(MetricsSnapshot {
        url: url.clone(),
        ..Default::default()
    }));

    let (tx, mut rx) = mpsc::channel(EVENT_CHANNEL_CAPACITY);
    tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let mut poller = ManifestPoller::new(
        url.clone(),
        session.headers.clone(),
        session.user_agent.clone(),
        session.interval_ms,
        session.probe_headers,
        session.probe_drm,
        tx,
    )?
    .with_metrics(Arc::clone(&metrics));
    if let Some(hook_url) = session.webhook_url.clone() {
        if let Ok(alerts) = crate::engine::webhook::AlertKind::parse_list(&session.alert_on) {
            let (hook_tx, hook_rx) = mpsc::channel(EVENT_CHANNEL_CAPACITY);
            poller = poller.with_webhook_tx(hook_tx);
            crate::engine::webhook::spawn_webhook_listener(
                crate::engine::webhook::WebhookConfig {
                    url: hook_url,
                    alerts,
                    allow_insecure: session.allow_insecure_webhooks,
                },
                hook_rx,
                url,
            );
        }
    }

    tokio::spawn(async move {
        poller.run().await;
    });

    let state = Arc::clone(&metrics);
    let app = Router::new()
        .route("/metrics", get(metrics_handler))
        .layer(axum::Extension(MetricsAuth {
            token: metrics_token,
        }))
        .with_state(state);

    let addr = SocketAddr::from((bind, port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    eprintln!("streamtop metrics listening on http://{addr}/metrics");

    axum::serve(listener, app).await?;
    Ok(ExitCode::SUCCESS)
}

/// Default bind for metrics: loopback only.
pub fn default_metrics_bind() -> IpAddr {
    IpAddr::V4(Ipv4Addr::LOCALHOST)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openmetrics_contains_required_series() {
        let mut http_errors = HashMap::new();
        http_errors.insert("403".into(), 2);
        let mut snap = MetricsSnapshot {
            url: "https://ex.com/live.m3u8?token=secret".into(),
            health_score: 92,
            segment_ttfb_secs: 0.045,
            latency_secs: 12.0,
            bitstream_fps: 25.0,
            cdn_hits: 10,
            cdn_misses: 2,
            cdn_provider: "Akamai".into(),
            virtual_buffer_secs: 8.5,
            ad_active: 0.0,
            origin_stalls_total: 1,
            http_errors,
            ll_hls_enabled: 1.0,
            codec_mismatch_total: 3,
            ..Default::default()
        };
        snap.segment_ttfb_hist.observe(TTFB_BUCKETS, 0.045);
        snap.llhls_part_hist.observe(PART_BUCKETS, 0.2);
        snap.drm_license_hist.observe(DRM_BUCKETS, 0.12);
        let out = render_openmetrics(&snap);
        assert!(out.contains("streamtop_stream_health_score"));
        assert!(out.contains("streamtop_bitstream_fps"));
        assert!(out.contains("streamtop_latency_seconds"));
        assert!(out.contains("streamtop_origin_stalls_total"));
        assert!(out.contains("streamtop_http_errors_total"));
        assert!(out.contains("streamtop_ll_hls_enabled"));
        assert!(out.contains("streamtop_segment_ttfb_seconds_bucket"));
        assert!(out.contains("streamtop_llhls_part_duration_seconds_bucket"));
        assert!(out.contains("streamtop_drm_license_ttfb_seconds_bucket"));
        assert!(out.contains("streamtop_codec_mismatch_total"));
        assert!(out.contains("streamtop_channel_dropped_total"));
        assert!(out.contains("status=\"403\""));
        assert!(!out.contains("token=secret"));
        assert!(
            out.contains("[REDACTED]")
                || out.contains("token=%5BREDACTED%5D")
                || out.contains("token=[REDACTED]")
        );
    }

    #[test]
    fn bearer_auth_accepts_valid_token() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            "Bearer secret-token".parse().unwrap(),
        );
        assert!(authorize_metrics_bearer(&headers, "secret-token"));
    }

    #[test]
    fn bearer_auth_rejects_missing_header() {
        let headers = HeaderMap::new();
        assert!(!authorize_metrics_bearer(&headers, "secret-token"));
    }

    #[test]
    fn bearer_auth_rejects_malformed_header() {
        let mut headers = HeaderMap::new();
        headers.insert(header::AUTHORIZATION, "secret-token".parse().unwrap());
        assert!(!authorize_metrics_bearer(&headers, "secret-token"));
    }

    #[test]
    fn bearer_auth_rejects_wrong_token() {
        let mut headers = HeaderMap::new();
        headers.insert(header::AUTHORIZATION, "Bearer other".parse().unwrap());
        assert!(!authorize_metrics_bearer(&headers, "secret-token"));
    }
}
