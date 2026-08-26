//! Webhook alerting with Slack Block Kit / Discord embeds, retry, and delivery log.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::Utc;
use color_eyre::eyre::{eyre, Result};
use reqwest::Client;
use serde_json::{json, Value};
use tokio::sync::mpsc::Receiver;

use crate::engine::redact::{redact_text, redact_url};
use crate::models::{SegmentMetrics, StreamEvent};

const DELIVERY_LOG_CAP: usize = 50;
const DEDUPE_WINDOW: Duration = Duration::from_secs(60);
const MAX_RETRIES: u32 = 3;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WebhookPlatform {
    Slack,
    Discord,
    Generic,
}

fn detect_platform(url: &str) -> WebhookPlatform {
    let u = url.to_ascii_lowercase();
    if u.contains("hooks.slack.com") || u.contains("slack.com/services") {
        WebhookPlatform::Slack
    } else if u.contains("discord.com/api/webhooks") || u.contains("discordapp.com/api/webhooks") {
        WebhookPlatform::Discord
    } else {
        WebhookPlatform::Generic
    }
}

#[derive(Debug, Clone)]
struct AlertPayload {
    kind: AlertKind,
    severity: &'static str,
    message: String,
    stream_url: String,
    health_score: Option<u8>,
    http_status: Option<u16>,
    ttfb_ms: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct DeliveryRecord {
    pub at: String,
    pub alert: String,
    pub ok: bool,
    pub detail: String,
}

#[derive(Debug, Default)]
pub struct DeliveryState {
    log: VecDeque<DeliveryRecord>,
    last_sent: HashMap<String, Instant>,
}

impl DeliveryState {
    fn push(&mut self, rec: DeliveryRecord) {
        self.log.push_back(rec);
        while self.log.len() > DELIVERY_LOG_CAP {
            self.log.pop_front();
        }
    }

    fn should_send(&mut self, dedupe_key: &str) -> bool {
        let now = Instant::now();
        if let Some(prev) = self.last_sent.get(dedupe_key) {
            if now.duration_since(*prev) < DEDUPE_WINDOW {
                return false;
            }
        }
        self.last_sent.insert(dedupe_key.to_string(), now);
        true
    }
}

/// Shared delivery log for introspection / tests.
pub type DeliveryLog = Arc<Mutex<DeliveryState>>;

pub fn new_delivery_log() -> DeliveryLog {
    Arc::new(Mutex::new(DeliveryState::default()))
}

pub fn spawn_webhook_listener(cfg: WebhookConfig, rx: Receiver<StreamEvent>, stream_url: String) {
    spawn_webhook_listener_with_log(cfg, rx, stream_url, new_delivery_log());
}

pub fn spawn_webhook_listener_with_log(
    cfg: WebhookConfig,
    mut rx: Receiver<StreamEvent>,
    stream_url: String,
    delivery: DeliveryLog,
) {
    tokio::spawn(async move {
        let client = Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap_or_else(|_| Client::new());
        let cfg = Arc::new(cfg);
        let platform = detect_platform(&cfg.url);
        let mut last_shi_alert = false;
        while let Some(event) = rx.recv().await {
            if let Some(payload) = event_to_alert(&cfg, &event, &stream_url, &mut last_shi_alert) {
                let dedupe_key = format!("{}:{}", payload.kind.as_str(), payload.severity);
                {
                    let mut st = delivery.lock().unwrap_or_else(|e| e.into_inner());
                    if !st.should_send(&dedupe_key) {
                        continue;
                    }
                }
                let url = cfg.url.clone();
                let client = client.clone();
                let delivery = Arc::clone(&delivery);
                let body = build_platform_body(platform, &payload);
                tokio::spawn(async move {
                    let result = post_with_retry(&client, &url, &body).await;
                    let mut st = delivery.lock().unwrap_or_else(|e| e.into_inner());
                    match result {
                        Ok(status) => st.push(DeliveryRecord {
                            at: Utc::now().to_rfc3339(),
                            alert: payload.kind.as_str().into(),
                            ok: true,
                            detail: format!("HTTP {status}"),
                        }),
                        Err(err) => st.push(DeliveryRecord {
                            at: Utc::now().to_rfc3339(),
                            alert: payload.kind.as_str().into(),
                            ok: false,
                            detail: err,
                        }),
                    }
                });
            }
        }
    });
}

async fn post_with_retry(client: &Client, url: &str, body: &Value) -> Result<u16, String> {
    let mut delay = Duration::from_millis(250);
    let mut last_err = String::new();
    for attempt in 0..MAX_RETRIES {
        match client.post(url).json(body).send().await {
            Ok(resp) => {
                let status = resp.status().as_u16();
                if (200..300).contains(&status) {
                    return Ok(status);
                }
                last_err = format!("HTTP {status}");
                if status < 500 && status != 429 {
                    return Err(last_err);
                }
            }
            Err(e) => last_err = e.to_string(),
        }
        if attempt + 1 < MAX_RETRIES {
            tokio::time::sleep(delay).await;
            delay *= 2;
        }
    }
    Err(format!("failed after {MAX_RETRIES} retries: {last_err}"))
}

fn build_platform_body(platform: WebhookPlatform, p: &AlertPayload) -> Value {
    let safe_url = redact_url(&p.stream_url);
    let safe_msg = redact_text(&p.message);
    let color = severity_color(p.severity);
    match platform {
        WebhookPlatform::Slack => {
            let mut fields = vec![
                json!({"type":"mrkdwn","text": format!("*Alert:*\n`{}`", p.kind.as_str())}),
                json!({"type":"mrkdwn","text": format!("*Severity:*\n{}", p.severity)}),
            ];
            if let Some(shi) = p.health_score {
                fields.push(json!({"type":"mrkdwn","text": format!("*SHI:*\n{shi}")}));
            }
            if let Some(ttfb) = p.ttfb_ms {
                fields.push(json!({"type":"mrkdwn","text": format!("*TTFB:*\n{ttfb} ms")}));
            }
            if let Some(code) = p.http_status {
                fields.push(json!({"type":"mrkdwn","text": format!("*HTTP:*\n{code}")}));
            }
            json!({
                "attachments": [{
                    "color": color,
                    "blocks": [
                        {
                            "type": "header",
                            "text": {"type": "plain_text", "text": format!("streamtop · {}", p.kind.as_str()), "emoji": true}
                        },
                        {
                            "type": "section",
                            "text": {"type": "mrkdwn", "text": safe_msg}
                        },
                        {
                            "type": "section",
                            "fields": fields
                        },
                        {
                            "type": "context",
                            "elements": [{"type":"mrkdwn","text": format!("<{safe_url}|stream>")}]
                        }
                    ]
                }]
            })
        }
        WebhookPlatform::Discord => {
            let mut fields = vec![
                json!({"name": "Alert", "value": p.kind.as_str(), "inline": true}),
                json!({"name": "Severity", "value": p.severity, "inline": true}),
            ];
            if let Some(shi) = p.health_score {
                fields.push(json!({"name": "SHI", "value": shi.to_string(), "inline": true}));
            }
            if let Some(ttfb) = p.ttfb_ms {
                fields.push(json!({"name": "TTFB", "value": format!("{ttfb} ms"), "inline": true}));
            }
            if let Some(code) = p.http_status {
                fields.push(json!({"name": "HTTP", "value": code.to_string(), "inline": true}));
            }
            fields.push(json!({"name": "Stream", "value": safe_url, "inline": false}));
            json!({
                "embeds": [{
                    "title": format!("streamtop · {}", p.kind.as_str()),
                    "description": safe_msg,
                    "color": discord_color(p.severity),
                    "fields": fields,
                    "timestamp": Utc::now().to_rfc3339()
                }]
            })
        }
        WebhookPlatform::Generic => json!({
            "source": "streamtop",
            "alert": p.kind.as_str(),
            "severity": p.severity,
            "message": safe_msg,
            "stream_url": safe_url,
            "timestamp": Utc::now().to_rfc3339(),
            "health_score": p.health_score,
            "http_status": p.http_status,
            "ttfb_ms": p.ttfb_ms
        }),
    }
}

fn severity_color(sev: &str) -> &'static str {
    match sev {
        "critical" => "#E01E5A",
        "warning" => "#ECB22E",
        _ => "#2EB67D",
    }
}

fn discord_color(sev: &str) -> u32 {
    match sev {
        "critical" => 0xE01E5A,
        "warning" => 0xECB22E,
        _ => 0x2EB67D,
    }
}

fn event_to_alert(
    cfg: &WebhookConfig,
    event: &StreamEvent,
    stream_url: &str,
    last_shi_alert: &mut bool,
) -> Option<AlertPayload> {
    match event {
        StreamEvent::Health(h) => {
            if cfg.alerts.contains(&AlertKind::ShiBelow70) && h.score < 70 {
                if *last_shi_alert {
                    return None;
                }
                *last_shi_alert = true;
                return Some(alert(
                    AlertKind::ShiBelow70,
                    "warning",
                    format!("Stream Health Index dropped to {} ({})", h.score, h.label),
                    stream_url,
                    Some(h.score),
                    None,
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
                return Some(alert(
                    AlertKind::Stall,
                    "critical",
                    format!("Stall indicated: {}", h.deductions.join("; ")),
                    stream_url,
                    Some(h.score),
                    None,
                    None,
                ));
            }
            None
        }
        StreamEvent::Segment(seg) => segment_alerts(cfg, seg, stream_url),
        StreamEvent::Buffer(b)
            if cfg.alerts.contains(&AlertKind::Stall) && b.stall_risk_pct >= 80 =>
        {
            Some(alert(
                AlertKind::Stall,
                "critical",
                format!(
                    "Virtual buffer stall risk {}% (buffer {:.1}s)",
                    b.stall_risk_pct, b.buffer_secs
                ),
                stream_url,
                None,
                None,
                None,
            ))
        }
        StreamEvent::Log { message, .. }
            if cfg.alerts.contains(&AlertKind::Mismatch) && message.contains("[MISMATCH]") =>
        {
            Some(alert(
                AlertKind::Mismatch,
                "warning",
                message.clone(),
                stream_url,
                None,
                None,
                None,
            ))
        }
        StreamEvent::AdBreak(ad) if cfg.alerts.contains(&AlertKind::AdStart) && ad.active => {
            Some(alert(
                AlertKind::AdStart,
                "info",
                ad.summary.clone(),
                stream_url,
                None,
                None,
                None,
            ))
        }
        StreamEvent::Error(msg) if cfg.alerts.contains(&AlertKind::Http5xx) => {
            if msg.contains("HTTP 5") || msg.contains("HTTP 50") {
                Some(alert(
                    AlertKind::Http5xx,
                    "critical",
                    msg.clone(),
                    stream_url,
                    None,
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
) -> Option<AlertPayload> {
    if cfg.alerts.contains(&AlertKind::Http5xx) && seg.http_status >= 500 {
        return Some(alert(
            AlertKind::Http5xx,
            "critical",
            format!("Segment HTTP {}", seg.http_status),
            stream_url,
            None,
            Some(seg.http_status),
            Some(seg.ttfb_ms),
        ));
    }
    if cfg.alerts.contains(&AlertKind::Stall) && seg.ttfb_ms >= 2500 {
        return Some(alert(
            AlertKind::Stall,
            "critical",
            format!(
                "Segment TTFB stall: {} ms (seq {})",
                seg.ttfb_ms, seg.media_sequence
            ),
            stream_url,
            None,
            Some(seg.http_status),
            Some(seg.ttfb_ms),
        ));
    }
    None
}

fn alert(
    kind: AlertKind,
    severity: &'static str,
    message: String,
    stream_url: &str,
    health_score: Option<u8>,
    http_status: Option<u16>,
    ttfb_ms: Option<u64>,
) -> AlertPayload {
    AlertPayload {
        kind,
        severity,
        message,
        stream_url: stream_url.into(),
        health_score,
        http_status,
        ttfb_ms,
    }
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

    #[test]
    fn detect_slack_and_discord() {
        assert_eq!(
            detect_platform("https://hooks.slack.com/services/T/B/X"),
            WebhookPlatform::Slack
        );
        assert_eq!(
            detect_platform("https://discord.com/api/webhooks/1/abc"),
            WebhookPlatform::Discord
        );
        assert_eq!(
            detect_platform("https://example.com/hook"),
            WebhookPlatform::Generic
        );
    }

    #[test]
    fn slack_body_has_blocks() {
        let p = alert(
            AlertKind::Stall,
            "critical",
            "stall".into(),
            "https://ex/m.m3u8?token=secret",
            Some(40),
            None,
            Some(3000),
        );
        let body = build_platform_body(WebhookPlatform::Slack, &p);
        assert!(body["attachments"][0]["blocks"].is_array());
        let s = body.to_string();
        assert!(!s.contains("secret"));
        assert!(s.contains("[REDACTED]") || s.contains("token"));
    }

    #[test]
    fn discord_body_has_embed() {
        let p = alert(
            AlertKind::ShiBelow70,
            "warning",
            "low shi".into(),
            "https://ex/m.m3u8",
            Some(65),
            None,
            None,
        );
        let body = build_platform_body(WebhookPlatform::Discord, &p);
        assert!(body["embeds"][0]["fields"].is_array());
        assert_eq!(body["embeds"][0]["title"], "streamtop · shi_below_70");
    }

    #[test]
    fn dedupe_window() {
        let mut st = DeliveryState::default();
        assert!(st.should_send("stall:critical"));
        assert!(!st.should_send("stall:critical"));
    }
}
