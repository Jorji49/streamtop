use std::io::{self, Write};
use std::panic;
use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use clap::Parser;
use color_eyre::eyre::{eyre, Result, WrapErr};
use crossterm::cursor::Show;
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, LeaveAlternateScreen};
use reqwest::Client;

use streamtop::engine::audit::run_audit;
use streamtop::engine::config::session_from_profile;
use streamtop::engine::export::{build_curl, capture_for_export, write_har};
use streamtop::engine::grafana::{export_grafana_dashboard, GRAFANA_DASHBOARD_FILENAME};
use streamtop::engine::metrics::run_prometheus;
use streamtop::engine::playlist_parser::{
    detect_and_parse, looks_like_remote_url, path_to_file_url, ParsedInput,
};
use streamtop::engine::poller::build_http_client;
use streamtop::engine::summary::{run_summary, SummaryFormat};
use streamtop::models::ChannelEntry;
use streamtop::ui::app::SessionOpts;
use streamtop::ui::{App, CompareApp};

#[derive(Debug, Clone, Copy, clap::ValueEnum, Default)]
enum SummaryFormatArg {
    #[default]
    Text,
    Json,
}

impl From<SummaryFormatArg> for SummaryFormat {
    fn from(v: SummaryFormatArg) -> Self {
        match v {
            SummaryFormatArg::Text => SummaryFormat::Text,
            SummaryFormatArg::Json => SummaryFormat::Json,
        }
    }
}

#[derive(Debug, Parser)]
#[command(
    name = "streamtop",
    version,
    about = "HLS/DASH/IPTV stream diagnostics TUI"
)]
struct Cli {
    /// Stream URL, local playlist/MPD, or channel lineup (M3U / JSON / YAML)
    #[arg(required_unless_present_any = ["compare", "export_grafana"])]
    url: Option<String>,

    /// Split-screen compare two live streams (Primary | Backup)
    #[arg(long = "compare", num_args = 2, value_names = ["URL_1", "URL_2"])]
    compare: Option<Vec<String>>,

    /// Write Grafana dashboard JSON for Prometheus metrics (streamtop-grafana.json) and exit
    #[arg(long = "export-grafana")]
    export_grafana: bool,

    /// After a short poll, print a curl command for the last segment
    #[arg(long = "export-curl")]
    export_curl: bool,

    /// After a short poll, write HAR 1.2 for manifest + last segment
    #[arg(long = "export-har", value_name = "FILE")]
    export_har: Option<PathBufArg>,

    /// Load named profile from ~/.config/streamtop/config.toml
    #[arg(long = "profile", value_name = "NAME")]
    profile: Option<String>,

    /// Extra HTTP header (repeatable). Format: "Key: Value"
    #[arg(short = 'H', long = "header", value_name = "KEY: VALUE")]
    headers: Vec<String>,

    /// Custom User-Agent
    #[arg(short = 'A', long = "user-agent")]
    user_agent: Option<String>,

    /// Manifest poll interval in milliseconds
    #[arg(short = 'i', long = "interval", value_name = "MS")]
    interval_ms: Option<u64>,

    /// Fetch only Range bytes for deep wire probe (no full segment download)
    #[arg(long = "probe-headers", alias = "range-probe")]
    probe_headers: bool,

    /// Probe DRM license / `#EXT-X-KEY` URI / DASH LA_URL ClearKey endpoints (TTFB)
    #[arg(long = "probe-drm")]
    probe_drm: bool,

    /// Batch range-probe every channel; write audit_report.json/.csv
    #[arg(long = "audit", alias = "matrix")]
    audit: bool,

    /// Headless summary (no TUI); print PASS/FAIL
    #[arg(long = "summary", alias = "headless")]
    summary: bool,

    /// Summary output: text or json (`streamtop.summary.v1`)
    #[arg(long = "summary-format", value_enum, default_value_t = SummaryFormatArg::Text)]
    summary_format: SummaryFormatArg,

    /// Seconds to listen in --summary / export modes (default: 8)
    #[arg(long = "timeout", value_name = "SECS", default_value_t = 8)]
    timeout_secs: u64,

    /// Prometheus exporter on /metrics (default port 9184, bind 127.0.0.1)
    #[arg(
        long = "prometheus",
        alias = "metrics",
        value_name = "PORT",
        num_args = 0..=1,
        default_missing_value = "9184"
    )]
    prometheus: Option<u16>,

    /// Alias for --prometheus <PORT>
    #[arg(long = "metrics-port", value_name = "PORT", hide = true)]
    metrics_port: Option<u16>,

    /// Metrics bind address (default: 127.0.0.1)
    #[arg(
        long = "metrics-bind",
        value_name = "ADDR",
        default_value = "127.0.0.1"
    )]
    metrics_bind: String,

    /// Bearer token required to scrape /metrics (Authorization: Bearer only)
    #[arg(long = "metrics-token", value_name = "TOKEN")]
    metrics_token: Option<String>,

    /// Webhook URL for alerts (Slack / Discord / generic REST)
    #[arg(long = "webhook", value_name = "URL")]
    webhook: Option<String>,

    /// Comma-separated alert kinds: stall,shi_below_70,http_5xx,mismatch,ad_start
    #[arg(long = "alert-on", value_name = "EVENTS")]
    alert_on: Option<String>,

    /// Allow webhooks to private/link-local/metadata hosts (local tests only; default: blocked)
    #[arg(long = "allow-insecure-webhooks")]
    allow_insecure_webhooks: bool,
}

/// Clap-friendly path wrapper.
#[derive(Debug, Clone)]
struct PathBufArg(std::path::PathBuf);

impl std::str::FromStr for PathBufArg {
    type Err = std::convert::Infallible;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(std::path::PathBuf::from(s)))
    }
}

#[tokio::main]
async fn main() -> Result<ExitCode> {
    let _ = rustls::crypto::ring::default_provider().install_default();

    color_eyre::install()?;
    install_panic_hook();

    let cli = Cli::parse();

    if cli.export_grafana {
        export_grafana_dashboard(GRAFANA_DASHBOARD_FILENAME)?;
        eprintln!("Wrote {GRAFANA_DASHBOARD_FILENAME} (import into Grafana; scrape streamtop --prometheus)");
        return Ok(ExitCode::SUCCESS);
    }

    let mut session = session_from_profile(
        cli.profile.as_deref(),
        SessionOpts {
            headers: Vec::new(),
            user_agent: None,
            interval_ms: None,
            probe_headers: false,
            probe_drm: false,
            webhook_url: None,
            alert_on: "stall,shi_below_70,http_5xx".into(),
            allow_insecure_webhooks: false,
        },
    )?;

    // CLI overrides profile / defaults.
    if !cli.headers.is_empty() {
        session.headers = cli.headers.clone();
    }
    if cli.user_agent.is_some() {
        session.user_agent = cli.user_agent.clone();
    }
    if cli.interval_ms.is_some() {
        session.interval_ms = cli.interval_ms;
    }
    if cli.probe_headers {
        session.probe_headers = true;
    }
    if cli.probe_drm {
        session.probe_drm = true;
    }
    if cli.webhook.is_some() {
        session.webhook_url = cli.webhook.clone();
    }
    if let Some(a) = &cli.alert_on {
        session.alert_on = a.clone();
    }
    if session.alert_on.is_empty() {
        session.alert_on = "stall,shi_below_70,http_5xx".into();
    }
    if cli.allow_insecure_webhooks {
        session.allow_insecure_webhooks = true;
        eprintln!(
            "warning: --allow-insecure-webhooks enables private/link-local/metadata webhook targets"
        );
    }
    if let Some(hook) = &session.webhook_url {
        streamtop::engine::webhook::validate_webhook_url(hook, session.allow_insecure_webhooks)
            .wrap_err("webhook SSRF check failed")?;
    }

    let client = build_http_client(&session.headers, session.user_agent.clone())?;
    let metrics_port = cli.metrics_port.or(cli.prometheus);
    let metrics_bind: std::net::IpAddr = cli
        .metrics_bind
        .parse()
        .wrap_err("invalid --metrics-bind")?;
    let metrics_token = streamtop::engine::metrics::normalize_metrics_token(
        cli.metrics_token
            .clone()
            .or_else(|| std::env::var("STREAMTOP_METRICS_TOKEN").ok()),
    );
    if metrics_port.is_some() {
        streamtop::engine::metrics::require_metrics_token_for_bind(metrics_bind, &metrics_token)
            .wrap_err("metrics bind security check failed")?;
    }

    if let Some(urls) = &cli.compare {
        if urls.len() != 2 {
            return Err(eyre!("--compare requires exactly two URLs"));
        }
        CompareApp::run(urls[0].clone(), urls[1].clone(), session).await?;
        restore_terminal();
        return Ok(ExitCode::SUCCESS);
    }

    let input_url = cli
        .url
        .as_deref()
        .ok_or_else(|| eyre!("missing stream URL (or use --compare)"))?;

    let (origin, body, content_type) = load_input(&client, input_url).await?;
    let parsed = detect_and_parse(&origin, &body, content_type.as_deref())
        .wrap_err("failed to detect input type (HLS / DASH / IPTV / catalog)")?;

    let want_export = cli.export_curl || cli.export_har.is_some();

    let exit = match parsed {
        ParsedInput::IptvChannels { origin, channels } => {
            if channels.is_empty() {
                return Err(eyre!("channel list is empty"));
            }
            if metrics_port.is_some() {
                return Err(eyre!(
                    "Prometheus mode requires a single HLS/DASH stream URL"
                ));
            }
            if want_export {
                return Err(eyre!(
                    "--export-curl/--export-har require a single HLS/DASH stream URL"
                ));
            }
            if cli.audit {
                let report = run_audit(
                    &origin,
                    channels,
                    session.headers.clone(),
                    session.user_agent.clone(),
                )
                .await?;
                Ok(if report.errors == 0 && report.stalls == 0 {
                    ExitCode::SUCCESS
                } else {
                    ExitCode::from(1)
                })
            } else if cli.summary {
                Err(eyre!(
                    "--summary requires a single HLS/DASH stream URL; use --audit for channel lists"
                ))
            } else {
                App::run_picker(origin, channels, session).await?;
                Ok(ExitCode::SUCCESS)
            }
        }
        ParsedInput::SingleStream { origin, url } => {
            if want_export {
                let cap =
                    capture_for_export(url.clone(), session.clone(), cli.timeout_secs).await?;
                if cli.export_curl {
                    println!("{}", build_curl(&cap));
                }
                if let Some(PathBufArg(path)) = &cli.export_har {
                    write_har(path, &cap)?;
                    eprintln!("Wrote HAR {}", path.display());
                }
                Ok(ExitCode::SUCCESS)
            } else if let Some(port) = metrics_port {
                run_prometheus(url, session, port, metrics_bind, metrics_token).await
            } else if cli.audit {
                let name = Path::new(input_url)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("stream")
                    .to_string();
                let channels = vec![ChannelEntry {
                    name,
                    url: url.clone(),
                    group: None,
                    logo: None,
                    tvg_id: None,
                }];
                let report = run_audit(
                    &origin,
                    channels,
                    session.headers.clone(),
                    session.user_agent.clone(),
                )
                .await?;
                Ok(if report.errors == 0 && report.stalls == 0 {
                    ExitCode::SUCCESS
                } else {
                    ExitCode::from(1)
                })
            } else if cli.summary {
                run_summary(url, session, cli.timeout_secs, cli.summary_format.into()).await
            } else {
                App::run_diagnostics(origin, url, session).await?;
                Ok(ExitCode::SUCCESS)
            }
        }
    };

    restore_terminal();
    exit.wrap_err("streamtop ended with an error")
}

fn restore_terminal() {
    let _ = disable_raw_mode();
    let mut stdout = io::stdout();
    let _ = execute!(stdout, LeaveAlternateScreen, Show);
}

async fn load_input(client: &Client, input: &str) -> Result<(String, Vec<u8>, Option<String>)> {
    if looks_like_remote_url(input) {
        let response = client
            .get(input)
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .wrap_err_with(|| format!("failed to fetch {input}"))?;
        let status = response.status();
        if !status.is_success() {
            return Err(eyre!("HTTP {status} - {input}"));
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
        let body = response.bytes().await?.to_vec();
        return Ok((input.to_string(), body, content_type));
    }

    let path = Path::new(input);
    if !path.exists() {
        return Err(eyre!("input is not a URL or an existing file: {input}"));
    }
    let origin = path_to_file_url(path)?;
    let body = tokio::fs::read(path)
        .await
        .wrap_err_with(|| format!("failed to read {}", path.display()))?;
    Ok((origin, body, None))
}

fn install_panic_hook() {
    let original = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let mut out = io::stdout();
        let _ = execute!(out, LeaveAlternateScreen, Show);
        let mut stderr = io::stderr();
        let _ = writeln!(stderr);
        let _ = writeln!(
            stderr,
            "streamtop panicked; terminal restored from raw mode."
        );
        original(info);
    }));
}
