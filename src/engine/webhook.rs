//! Async webhook alerting for stall / SHI / HTTP crisis events.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use color_eyre::eyre::{eyre, Result};
use reqwest::Client;
use serde::Serialize;
use tokio::sync::mpsc::Receiver;

use crate::models::{HealthReport, SegmentMetrics, StreamEvent};

#[derive(Debug, Clone)]
pub struct WebhookConfig {
    pub url: String,
    pub alerts: HashSet<AlertKind>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AlertKind {
    Stall,
    ShiBelow70,
    Http5xx,
    Mismatch,
    AdStart,
}

impl AlertKind {
    pub fn parse_list(raw: &str) -> Result<HashSet<Self>> {
        let mut set = HashSet::new();
        for part in raw.split(',') {
            let p = part.trim().to_ascii_lowercase();
            if p.is_empty() {
                continue;
            }
            let kind = match p.as_str() {
                "stall" => Self::Stall,
                "shi_below_70" | "shi70" | "shi_below70" => Self::ShiBelow70,
                "http_5xx" | "http5xx" | "5xx" => Self::Http5xx,
                "mismatch" => Self::Mismatch,
                "ad_start" | "ad" => Self::AdStart,
                other => return Err(eyre!("unknown alert kind: {other}")),
            };
            set.insert(kind);
        }
        if set.is_empty() {
            return Err(eyre!("--alert-on list is empty"));
        }
        Ok(set)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stall => "stall",
            Self::ShiBelow70 => "shi_below_70",
            Self::Http5xx => "http_5xx",
            Self::Mismatch => "mismatch",
            Self::AdStart => "ad_start",
        }
    }
}

#[derive(Debug, Serialize)]
struct WebhookPayload {
    source: &'static str,
    alert: String,
    severity: String,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream_url: Option<String>,
    timestamp: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    health_score: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    http_status: Option<u16>,
}

/// Spawn a background task that posts matching events to the webhook URL.
pub fn spawn_webhook_listener(
    cfg: WebhookConfig,
    mut rx: Receiver<StreamEvent>,
    stream_url: String,
) {
    tokio::spawn(async move {
        let client = Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap_or_else(|_| Client::new());
        let cfg = Arc::new(cfg);
        let mut last_shi_alert = false;
        while let Some(event) = rx.recv().await {
            if let Some(payload) = event_to_payload(&cfg, &event, &stream_url, &mut last_shi_alert)
            {
                let url = cfg.url.clone();
                let client = client.clone();
                tokio::spawn(async move {
                    let _ = client.post(&url).json(&payload).send().await;
                });
            }
        }
    });
}

fn event_to_payload(
    cfg: &WebhookConfig,
    event: &StreamEvent,
    stream_url: &str,
    last_shi_alert: &mut bool,
) -> Option<WebhookPayload> {
    match event {
        StreamEvent::Health(h) => {
            if cfg.alerts.contains(&AlertKind::ShiBelow70) && h.score < 70 {
                if *last_shi_alert {
                    return None;
                }
                *last_shi_alert = true;
                return Some(payload(
                    AlertKind::ShiBelow70,
                    "warning",
                    format!("Stream Health Index dropped to {} ({})", h.score, h.label),
                    stream_url,
                    Some(h.score),
                    None,
                ));
            }
            if h.score >= 70 {
                *last_shi_alert = false;
            }
            if cfg.alerts.contains(&AlertKind::Stall)
                && h.deductions
                    .iter()
                    .any(|d| d.to_ascii_lowercase().contains("stall"))
            {
                return Some(payload(
                    AlertKind::Stall,
                    "critical",
                    format!("Stall indicated: {}", h.deductions.join("; ")),
                    stream_url,
                    Some(h.score),
                    None,
                ));
            }
            None
        }
        StreamEvent::Segment(seg) => segment_alerts(cfg, seg, stream_url),
        StreamEvent::Buffer(b)
            if cfg.alerts.contains(&AlertKind::Stall) && b.stall_risk_pct >= 80 =>
        {
            Some(payload(
                AlertKind::Stall,
                "critical",
                format!(
                    "Virtual buffer stall risk {}% (buffer {:.1}s)",
                    b.stall_risk_pct, b.buffer_secs
                ),
                stream_url,
                None,
                None,
            ))
        }
        StreamEvent::Log { message, .. }
            if cfg.alerts.contains(&AlertKind::Mismatch) && message.contains("[MISMATCH]") =>
        {
            Some(payload(
                AlertKind::Mismatch,
                "warning",
                message.clone(),
                stream_url,
                None,
                None,
            ))
        }
        StreamEvent::AdBreak(ad) if cfg.alerts.contains(&AlertKind::AdStart) && ad.active => {
            Some(payload(
                AlertKind::AdStart,
                "info",
                ad.summary.clone(),
                stream_url,
                None,
                None,
            ))
        }
        StreamEvent::Error(msg) if cfg.alerts.contains(&AlertKind::Http5xx) => {
            if msg.contains("HTTP 5") || msg.contains("HTTP 50") {
                Some(payload(
                    AlertKind::Http5xx,
                    "critical",
                    msg.clone(),
                    stream_url,
                    None,
                    None,
                ))
            } else {
                None
            }
        }
        _ => None,
    }
}

fn segment_alerts(
    cfg: &WebhookConfig,
    seg: &SegmentMetrics,
    stream_url: &str,
) -> Option<WebhookPayload> {
    if cfg.alerts.contains(&AlertKind::Http5xx) && seg.http_status >= 500 {
        return Some(payload(
            AlertKind::Http5xx,
            "critical",
            format!("Segment HTTP {}", seg.http_status),
            stream_url,
            None,
            Some(seg.http_status),
        ));
    }
    if cfg.alerts.contains(&AlertKind::Stall) && seg.ttfb_ms >= 2500 {
        return Some(payload(
            AlertKind::Stall,
            "critical",
            format!(
                "Segment TTFB stall: {} ms (seq {})",
                seg.ttfb_ms, seg.media_sequence
            ),
            stream_url,
            None,
            Some(seg.http_status),
        ));
    }
    None
}

fn payload(
    kind: AlertKind,
    severity: &str,
    message: String,
    stream_url: &str,
    health_score: Option<u8>,
    http_status: Option<u16>,
) -> WebhookPayload {
    WebhookPayload {
        source: "streamtop",
        alert: kind.as_str().into(),
        severity: severity.into(),
        message,
        stream_url: Some(stream_url.into()),
        timestamp: Utc::now().to_rfc3339(),
        health_score,
        http_status,
    }
}

/// Fan-out helper: send event to UI and optional webhook channel.
pub fn fanout(
    ui: &tokio::sync::mpsc::Sender<StreamEvent>,
    hook: Option<&tokio::sync::mpsc::Sender<StreamEvent>>,
    event: StreamEvent,
) {
    let _ = ui.try_send(event.clone());
    if let Some(h) = hook {
        let _ = h.try_send(event);
    }
}

#[allow(dead_code)]
pub fn health_triggers_shi(h: &HealthReport) -> bool {
    h.score < 70
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_alert_list() {
        let set = AlertKind::parse_list("stall,shi_below_70,http_5xx").unwrap();
        assert!(set.contains(&AlertKind::Stall));
        assert!(set.contains(&AlertKind::ShiBelow70));
        assert!(set.contains(&AlertKind::Http5xx));
    }
}
