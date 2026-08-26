//! Headless summary: poll briefly, print PASS/FAIL, set exit code.

use std::process::ExitCode;
use std::time::Duration;

use color_eyre::eyre::Result;
use crossterm::style::{Color, ResetColor, SetForegroundColor};
use serde::Serialize;
use tokio::sync::mpsc;
use tokio::time::{timeout, Instant};

use crate::engine::redact::redact_url;
use crate::engine::ManifestPoller;
use crate::models::{
    CdnStats, DiagCategory, DiagSeverity, HealthReport, LatencyState, StreamEvent, StreamStatus,
    StreamStatusKind, EVENT_CHANNEL_CAPACITY,
};
use crate::ui::app::SessionOpts;

/// Stable machine-readable schema id for `--summary --summary-format json`.
pub const SUMMARY_SCHEMA: &str = "streamtop.summary.v1";
pub const SUMMARY_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SummaryFormat {
    Text,
    Json,
}

/// Documented stable JSON shape for CI gates (`schemas/summary.v1.json`).
#[derive(Debug, Serialize)]
pub struct SummaryJson {
    pub schema: &'static str,
    pub schema_version: u32,
    pub verdict: &'static str,
    pub ok: bool,
    pub health_score: u8,
    pub health_label: String,
    pub status: &'static str,
    pub latency: String,
    pub cdn: String,
    pub ttfb_ms: Option<u64>,
    pub last_http_status: Option<u16>,
    pub origin_stalls: u32,
    pub critical_rfc_errors: u32,
    pub url: String,
    pub errors: u32,
    pub saw_segment: bool,
}

pub fn build_summary_json(
    url: String,
    ok: bool,
    health: &HealthReport,
    status_label: &'static str,
    latency: &LatencyState,
    cdn_badge: String,
    last_ttfb: Option<u64>,
    last_http_status: Option<u16>,
    origin_stalls: u32,
    critical_rfc_errors: u32,
    errors: u32,
    saw_segment: bool,
) -> SummaryJson {
    SummaryJson {
        schema: SUMMARY_SCHEMA,
        schema_version: SUMMARY_SCHEMA_VERSION,
        verdict: if ok { "PASS" } else { "FAIL" },
        ok,
        health_score: health.score,
        health_label: health.label.clone(),
        status: status_label,
        latency: latency.display(),
        cdn: cdn_badge,
        ttfb_ms: last_ttfb,
        last_http_status,
        origin_stalls,
        critical_rfc_errors,
        url: redact_url(&url),
        errors,
        saw_segment,
    }
}

pub async fn run_summary(
    url: String,
    session: SessionOpts,
    timeout_secs: u64,
    format: SummaryFormat,
) -> Result<ExitCode> {
    let (tx, mut rx) = mpsc::channel(EVENT_CHANNEL_CAPACITY);
    let mut poller = ManifestPoller::new(
        url.clone(),
        session.headers.clone(),
        session.user_agent.clone(),
        session.interval_ms,
        session.probe_headers,
        session.probe_drm,
        tx,
    )?;
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
                url.clone(),
            );
        }
    }
    let handle = tokio::spawn(async move {
        poller.run().await;
    });

    let mut health = HealthReport::perfect();
    let mut status = StreamStatus::live("probing…");
    let mut latency = LatencyState::Unknown;
    let mut cdn = CdnStats::default();
    let mut last_ttfb: Option<u64> = None;
    let mut last_http_status: Option<u16> = None;
    let mut errors = 0u32;
    let mut origin_stalls = 0u32;
    let mut critical_rfc_errors = 0u32;
    let mut saw_segment = false;

    let deadline = Instant::now() + Duration::from_secs(timeout_secs.max(1));
    while Instant::now() < deadline {
        let left = deadline.saturating_duration_since(Instant::now());
        match timeout(left, rx.recv()).await {
            Ok(Some(ev)) => match ev {
                StreamEvent::Health(h) => health = h,
                StreamEvent::Status(s) => status = s,
                StreamEvent::Latency(l) => latency = l,
                StreamEvent::CdnStats(c) => cdn = c,
                StreamEvent::Segment(s) => {
                    saw_segment = true;
                    last_ttfb = Some(s.ttfb_ms);
                    if s.http_status > 0 {
                        last_http_status = Some(s.http_status);
                    }
                }
                StreamEvent::Finding(f) => {
                    if f.category == DiagCategory::Stalling {
                        origin_stalls = origin_stalls.saturating_add(1);
                    }
                    if f.category == DiagCategory::Rfc && f.severity == DiagSeverity::Error {
                        critical_rfc_errors = critical_rfc_errors.saturating_add(1);
                    }
                }
                StreamEvent::Error(_)
                | StreamEvent::Log {
                    level: crate::models::LogLevel::Error,
                    ..
                } => {
                    errors = errors.saturating_add(1);
                }
                StreamEvent::Log {
                    category: DiagCategory::Stalling,
                    ..
                } => {
                    origin_stalls = origin_stalls.saturating_add(1);
                }
                _ => {}
            },
            Ok(None) => break,
            Err(_) => break,
        }
    }

    handle.abort();

    let cdn_badge = cdn
        .last
        .as_ref()
        .map(|c| c.badge())
        .unwrap_or_else(|| "UNKNOWN".into());
    let ttfb = last_ttfb
        .map(|ms| format!("{ms}ms"))
        .unwrap_or_else(|| "-".into());

    let http_ok = matches!(last_http_status, Some(200) | Some(206));
    let ok = matches!(status.kind, StreamStatusKind::Live)
        && health.score >= 85
        && critical_rfc_errors == 0
        && origin_stalls == 0
        && http_ok
        && saw_segment;

    let status_label = match status.kind {
        StreamStatusKind::Live => "LIVE",
        StreamStatusKind::Degraded => "DEGRADED",
        StreamStatusKind::Error => "ERROR",
    };

    match format {
        SummaryFormat::Json => {
            let payload = build_summary_json(
                url.clone(),
                ok,
                &health,
                status_label,
                &latency,
                cdn_badge,
                last_ttfb,
                last_http_status,
                origin_stalls,
                critical_rfc_errors,
                errors,
                saw_segment,
            );
            println!("{}", serde_json::to_string(&payload)?);
        }
        SummaryFormat::Text => {
            let verdict = if ok { "PASS" } else { "FAIL" };
            let color = if ok { Color::Green } else { Color::Red };
            let mut out = std::io::stdout();
            crossterm::execute!(out, SetForegroundColor(color))?;
            print!("{verdict}");
            crossterm::execute!(out, ResetColor)?;
            println!(
                "  SHI={:>3} ({})  status={}  latency={}  CDN={}  TTFB={}  HTTP={}  stalls={}  rfc_err={}  url={}",
                health.score,
                health.label,
                status_label,
                latency.display(),
                cdn_badge,
                ttfb,
                last_http_status
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "-".into()),
                origin_stalls,
                critical_rfc_errors,
                redact_url(&url)
            );
        }
    }

    Ok(if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_json_schema_fields() {
        let health = HealthReport::perfect();
        let payload = build_summary_json(
            "https://ex/m.m3u8?token=secret".into(),
            true,
            &health,
            "LIVE",
            &LatencyState::Measured(1200),
            "Cloudflare · HIT".into(),
            Some(80),
            Some(206),
            0,
            0,
            0,
            true,
        );
        let v = serde_json::to_value(&payload).unwrap();
        assert_eq!(v["schema"], SUMMARY_SCHEMA);
        assert_eq!(v["schema_version"], SUMMARY_SCHEMA_VERSION);
        assert_eq!(v["verdict"], "PASS");
        assert_eq!(v["ok"], true);
        assert_eq!(v["saw_segment"], true);
        assert!(v.get("health_score").is_some());
        assert!(!v["url"].as_str().unwrap().contains("secret"));
        assert!(v["url"].as_str().unwrap().contains("[REDACTED]"));
    }

    #[test]
    fn summary_uses_redact_helpers() {
        use crate::engine::redact::redact_text;
        assert!(redact_text("Cookie: a=b").contains("[REDACTED]"));
    }
}
