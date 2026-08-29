//! Multi-stream headless agent (`streamtop --agent config.toml`).

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::process::ExitCode;
use std::sync::{Arc, RwLock};

use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use color_eyre::eyre::{eyre, Result, WrapErr};
use serde::Deserialize;
use tokio::sync::mpsc;

use crate::engine::metrics::{
    authorize_metrics_bearer, normalize_metrics_token, render_openmetrics_for_stream,
    require_metrics_token_for_bind, MetricsAuth, MetricsSnapshot,
};
use crate::engine::webhook::{self, AlertKind, WebhookConfig};
use crate::engine::ManifestPoller;
use crate::models::EVENT_CHANNEL_CAPACITY;
use crate::ui::app::SessionOpts;

/// Max concurrent pollers in agent mode (bounded memory).
pub const MAX_AGENT_STREAMS: usize = 64;

/// Per-stream event channel capacity (matches `EVENT_CHANNEL_CAPACITY`).
pub const AGENT_EVENT_CHANNEL_CAPACITY: usize = EVENT_CHANNEL_CAPACITY;

/// Agent-wide metrics registry (one snapshot per stream_id).
#[derive(Debug, Default)]
pub struct AgentMetricsRegistry {
    pub streams: HashMap<String, MetricsSnapshot>,
    pub active_streams: u64,
    pub dropped_events: u64,
}

impl AgentMetricsRegistry {
    pub fn render_openmetrics(&self) -> String {
        let mut out = String::new();
        for (id, snap) in &self.streams {
            out.push_str(&render_openmetrics_for_stream(snap, Some(id)));
        }
        let labels = r#"service="streamtop-agent""#;
        out.push_str(&format!(
            "# HELP streamtop_agent_streams_active Active agent pollers\n\
             # TYPE streamtop_agent_streams_active gauge\n\
             streamtop_agent_streams_active{{{labels}}} {}\n\
             # HELP streamtop_agent_events_dropped_total Agent channel drops (backpressure)\n\
             # TYPE streamtop_agent_events_dropped_total counter\n\
             streamtop_agent_events_dropped_total{{{labels}}} {}\n",
            self.active_streams, self.dropped_events
        ));
        out
    }
}

#[derive(Debug, Deserialize)]
pub struct AgentConfigFile {
    #[serde(default = "default_metrics_bind")]
    pub metrics_bind: String,
    #[serde(default = "default_metrics_port")]
    pub metrics_port: u16,
    pub metrics_token: Option<String>,
    pub otel_endpoint: Option<String>,
    #[serde(default)]
    pub allow_insecure_webhooks: bool,
    #[serde(default)]
    pub allow_insecure_otel: bool,
    #[serde(default)]
    pub streams: Vec<AgentStreamConfig>,
}

fn default_metrics_bind() -> String {
    "127.0.0.1".into()
}

fn default_metrics_port() -> u16 {
    9184
}

#[derive(Debug, Deserialize, Clone)]
pub struct AgentStreamConfig {
    pub id: String,
    pub url: String,
    #[serde(default)]
    pub headers: Vec<String>,
    pub user_agent: Option<String>,
    pub interval_ms: Option<u64>,
    #[serde(default = "default_probe_headers")]
    pub probe_headers: bool,
    #[serde(default)]
    pub probe_drm: bool,
    pub clearkey: Option<String>,
    pub webhook: Option<String>,
    pub alert_on: Option<String>,
    pub compare_with: Option<String>,
    #[serde(default)]
    pub tr101290: bool,
    #[serde(default)]
    pub probe_sei: bool,
}

fn default_probe_headers() -> bool {
    true
}

pub fn load_agent_config(path: &str) -> Result<AgentConfigFile> {
    let raw =
        std::fs::read_to_string(path).wrap_err_with(|| format!("read agent config {path}"))?;
    let cfg: AgentConfigFile =
        toml::from_str(&raw).wrap_err_with(|| format!("parse agent config {path}"))?;
    if cfg.streams.is_empty() {
        return Err(eyre!("agent config has no [[streams]] entries"));
    }
    validate_agent_stream_count(cfg.streams.len())?;
    for s in &cfg.streams {
        if s.id.trim().is_empty() {
            return Err(eyre!("stream id must not be empty"));
        }
    }
    Ok(cfg)
}

fn validate_agent_stream_count(n: usize) -> Result<()> {
    if n > MAX_AGENT_STREAMS {
        return Err(eyre!(
            "agent config has {n} streams; max {MAX_AGENT_STREAMS}"
        ));
    }
    Ok(())
}

pub async fn run_agent(config_path: &str) -> Result<ExitCode> {
    let cfg = load_agent_config(config_path)?;
    let bind: IpAddr = cfg
        .metrics_bind
        .parse()
        .wrap_err("invalid metrics_bind in agent config")?;
    let token = normalize_metrics_token(cfg.metrics_token.clone());
    require_metrics_token_for_bind(bind, &token)?;

    let registry = Arc::new(RwLock::new(AgentMetricsRegistry {
        active_streams: cfg.streams.len() as u64,
        ..Default::default()
    }));

    let streams = cfg.streams.clone();
    for stream in streams {
        spawn_agent_stream(stream, Arc::clone(&registry), &cfg)?;
    }

    let auth = MetricsAuth {
        token: token.clone(),
    };
    let app = Router::new().route(
        "/metrics",
        get(agent_metrics_handler).with_state(Arc::clone(&registry)),
    );
    let app = app.layer(axum::Extension(auth));

    let addr = SocketAddr::new(bind, cfg.metrics_port);
    eprintln!(
        "streamtop agent: {} streams | metrics http://{addr}/metrics",
        registry.read().unwrap().active_streams
    );
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(ExitCode::SUCCESS)
}

fn spawn_agent_stream(
    stream: AgentStreamConfig,
    registry: Arc<RwLock<AgentMetricsRegistry>>,
    agent: &AgentConfigFile,
) -> Result<()> {
    let stream_id = stream.id.clone();
    let url = stream.url.clone();
    {
        let mut reg = registry.write().unwrap();
        reg.streams.insert(stream_id.clone(), {
            let mut snap = MetricsSnapshot::default();
            snap.url = url.clone();
            snap
        });
    }

    let (tx, mut rx) = mpsc::channel(AGENT_EVENT_CHANNEL_CAPACITY);
    let session = SessionOpts {
        headers: stream.headers.clone(),
        user_agent: stream.user_agent.clone(),
        interval_ms: stream.interval_ms,
        probe_headers: stream.probe_headers,
        probe_drm: stream.probe_drm,
        clearkey: stream.clearkey.clone(),
        export_incident: None,
        webhook_url: stream.webhook.clone(),
        alert_on: stream
            .alert_on
            .clone()
            .unwrap_or_else(|| "stall,shi_below_70,http_5xx".into()),
        allow_insecure_webhooks: agent.allow_insecure_webhooks,
        allow_insecure_otel: agent.allow_insecure_otel,
        allow_insecure_ingest: false,
        otel_endpoint: agent.otel_endpoint.clone(),
        tr101290: stream.tr101290,
        probe_sei: stream.probe_sei,
        simulate_player: false,
        throttle_kbps: None,
        simulated_rtt_ms: None,
    };

    let mut poller = ManifestPoller::new(
        url.clone(),
        session.headers.clone(),
        session.user_agent.clone(),
        session.interval_ms,
        session.probe_headers,
        session.probe_drm,
        tx,
    )?
    .with_agent_metrics(Arc::clone(&registry), stream_id.clone())
    .with_diagnostics(crate::engine::poller::DiagnosticOpts {
        tr101290: session.tr101290,
        probe_sei: session.probe_sei,
        simulate_player: false,
        throttle_kbps: None,
        simulated_rtt_ms: None,
    });

    if let Some(ck) = &session.clearkey {
        if let Ok(spec) = crate::engine::drm_probe::ClearKeySpec::parse(ck) {
            poller = poller.with_clearkey(Some(spec));
        }
    }

    if let Some(hook) = session.webhook_url.clone() {
        let alerts = AlertKind::parse_list(&session.alert_on)?;
        webhook::validate_webhook_url(&hook, session.allow_insecure_webhooks)?;
        let (hook_tx, hook_rx) = mpsc::channel(AGENT_EVENT_CHANNEL_CAPACITY);
        poller = poller.with_webhook_tx(hook_tx);
        webhook::spawn_webhook_listener(
            WebhookConfig {
                url: hook,
                alerts,
                allow_insecure: session.allow_insecure_webhooks,
            },
            hook_rx,
            format!("{}:{url}", stream_id),
        );
    }

    if stream.compare_with.is_some() {
        eprintln!(
            "agent stream {}: compare_with noted (pair metrics via stream_id labels)",
            stream_id
        );
    }

    tokio::spawn(async move { while rx.recv().await.is_some() {} });
    tokio::spawn(async move {
        let _ = poller.run().await;
    });
    Ok(())
}

async fn agent_metrics_handler(
    axum::extract::State(registry): axum::extract::State<Arc<RwLock<AgentMetricsRegistry>>>,
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
    let body = registry
        .read()
        .map(|r| r.render_openmetrics())
        .unwrap_or_default();
    (
        StatusCode::OK,
        [("content-type", "text/plain; version=0.0.4; charset=utf-8")],
        body,
    )
}

pub fn sanitize_stream_id(id: &str) -> String {
    let mut out = String::with_capacity(id.len());
    for c in id.chars() {
        if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
            out.push(c);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        "stream".into()
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal_agent_toml() {
        let raw = r#"
[[streams]]
id = "live1"
url = "https://example.com/live.m3u8"
"#;
        let cfg: AgentConfigFile = toml::from_str(raw).unwrap();
        assert_eq!(cfg.streams.len(), 1);
        assert_eq!(cfg.streams[0].id, "live1");
        assert!(cfg.streams[0].probe_headers);
    }

    #[test]
    fn rejects_too_many_streams() {
        assert!(validate_agent_stream_count(MAX_AGENT_STREAMS + 1).is_err());
        assert!(validate_agent_stream_count(MAX_AGENT_STREAMS).is_ok());
    }

    #[test]
    fn sanitize_id_label() {
        assert_eq!(sanitize_stream_id("primary/live"), "primary_live");
    }
}
