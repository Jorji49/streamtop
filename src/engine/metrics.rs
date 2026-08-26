//! Prometheus `/metrics` exporter.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::process::ExitCode;
use std::sync::{Arc, RwLock};

use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use color_eyre::eyre::Result;
use tokio::sync::mpsc;

use crate::engine::ManifestPoller;
use crate::models::{CdnStats, DiagCategory, LatencyState, StreamEvent, StreamStatusKind, EVENT_CHANNEL_CAPACITY};
use crate::ui::app::SessionOpts;

/// Shared metric state updated by the poller and scraped by `/metrics`.
#[derive(Debug, Clone, Default)]
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
}

pub fn update_metrics(snap: &mut MetricsSnapshot, event: &StreamEvent) {
    match event {
        StreamEvent::Health(h) => snap.health_score = h.score,
        StreamEvent::Segment(s) => {
            snap.segment_ttfb_secs = s.ttfb_ms as f64 / 1000.0;
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
    let url = label_escape(&snap.url);
    let cdn = label_escape(&snap.cdn_provider);
    let mut out = format!(
        r#"# HELP streamtop_stream_health_score Stream Health Index (SHI) 0-100
# TYPE streamtop_stream_health_score gauge
streamtop_stream_health_score{{url="{url}"}} {health}
# HELP streamtop_segment_ttfb_seconds Last segment time-to-first-byte
# TYPE streamtop_segment_ttfb_seconds gauge
streamtop_segment_ttfb_seconds{{url="{url}"}} {ttfb:.6}
# HELP streamtop_latency_seconds Live-edge latency (PDT or estimated)
# TYPE streamtop_latency_seconds gauge
streamtop_latency_seconds{{url="{url}"}} {latency:.6}
# HELP streamtop_bitstream_fps Declared or wire-probed video frame rate
# TYPE streamtop_bitstream_fps gauge
streamtop_bitstream_fps{{url="{url}"}} {fps:.3}
# HELP streamtop_cdn_cache_hits_total CDN edge cache hits
# TYPE streamtop_cdn_cache_hits_total counter
streamtop_cdn_cache_hits_total{{url="{url}",cdn="{cdn}"}} {hits}
# HELP streamtop_cdn_cache_misses_total CDN edge cache misses
# TYPE streamtop_cdn_cache_misses_total counter
streamtop_cdn_cache_misses_total{{url="{url}",cdn="{cdn}"}} {misses}
# HELP streamtop_virtual_buffer_seconds Simulated player buffer depth
# TYPE streamtop_virtual_buffer_seconds gauge
streamtop_virtual_buffer_seconds{{url="{url}"}} {vbuf:.3}
# HELP streamtop_ad_active DAI ad break active (1=yes, 0=no)
# TYPE streamtop_ad_active gauge
streamtop_ad_active{{url="{url}"}} {ad:.0}
# HELP streamtop_origin_stalls_total Origin stalling alarms
# TYPE streamtop_origin_stalls_total counter
streamtop_origin_stalls_total{{url="{url}"}} {stalls}
# HELP streamtop_ll_hls_enabled LL-HLS detected on playlist (1=yes, 0=no)
# TYPE streamtop_ll_hls_enabled gauge
streamtop_ll_hls_enabled{{url="{url}"}} {ll:.0}
# HELP streamtop_http_errors_total HTTP 4xx/5xx responses
# TYPE streamtop_http_errors_total counter
"#,
        health = snap.health_score,
        ttfb = snap.segment_ttfb_secs,
        latency = snap.latency_secs,
        fps = snap.bitstream_fps,
        hits = snap.cdn_hits,
        misses = snap.cdn_misses,
        vbuf = snap.virtual_buffer_secs,
        ad = snap.ad_active,
        stalls = snap.origin_stalls_total,
        ll = snap.ll_hls_enabled,
    );

    if snap.http_errors.is_empty() {
        out.push_str(&format!(
            "streamtop_http_errors_total{{url=\"{url}\",status=\"none\"}} 0\n"
        ));
    } else {
        let mut keys: Vec<_> = snap.http_errors.keys().cloned().collect();
        keys.sort();
        for status in keys {
            let n = snap.http_errors[&status];
            out.push_str(&format!(
                "streamtop_http_errors_total{{url=\"{url}\",status=\"{status}\"}} {n}\n"
            ));
        }
    }
    out
}

async fn metrics_handler(
    state: axum::extract::State<Arc<RwLock<MetricsSnapshot>>>,
) -> impl IntoResponse {
    let snap = state.read().unwrap_or_else(|e| e.into_inner());
    (
        StatusCode::OK,
        [("content-type", "text/plain; version=0.0.4; charset=utf-8")],
        render_openmetrics(&snap),
    )
}

/// Run headless diagnostics with Prometheus scrape endpoint (no TUI).
pub async fn run_prometheus(url: String, session: SessionOpts, port: u16) -> Result<ExitCode> {
    let metrics = Arc::new(RwLock::new(MetricsSnapshot {
        url: url.clone(),
        ..Default::default()
    }));

    let (tx, mut rx) = mpsc::channel(EVENT_CHANNEL_CAPACITY);
    // Drain UI events so the bounded queue never backs up (metrics updated in poller).
    tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let mut poller = ManifestPoller::new(
        url.clone(),
        session.headers.clone(),
        session.user_agent.clone(),
        session.interval_ms,
        session.probe_headers,
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
        .with_state(state);

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    eprintln!("streamtop metrics listening on http://{addr}/metrics");

    axum::serve(listener, app).await?;
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openmetrics_contains_required_series() {
        let mut http_errors = HashMap::new();
        http_errors.insert("403".into(), 2);
        let snap = MetricsSnapshot {
            url: "https://ex.com/live.m3u8".into(),
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
        };
        let out = render_openmetrics(&snap);
        assert!(out.contains("streamtop_stream_health_score"));
        assert!(out.contains("streamtop_bitstream_fps"));
        assert!(out.contains("streamtop_latency_seconds"));
        assert!(out.contains("streamtop_origin_stalls_total"));
        assert!(out.contains("streamtop_http_errors_total"));
        assert!(out.contains("streamtop_ll_hls_enabled"));
        assert!(out.contains("status=\"403\""));
    }
}
