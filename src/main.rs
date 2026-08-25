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
    #[arg(required_unless_present = "compare")]
    url: Option<String>,

    /// Split-screen compare two live streams (Primary | Backup)
    #[arg(long = "compare", num_args = 2, value_names = ["URL_1", "URL_2"])]
    compare: Option<Vec<String>>,

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

    /// Batch range-probe every channel; write audit_report.json/.csv
    #[arg(long = "audit", alias = "matrix")]
    audit: bool,

    /// Headless summary (no TUI); print PASS/FAIL
    #[arg(long = "summary", alias = "headless")]
    summary: bool,

    /// Summary output: text or json
    #[arg(long = "summary-format", value_enum, default_value_t = SummaryFormatArg::Text)]
    summary_format: SummaryFormatArg,

    /// Seconds to listen in --summary mode (default: 8)
    #[arg(long = "timeout", value_name = "SECS", default_value_t = 8)]
    timeout_secs: u64,

    /// Prometheus exporter on /metrics (default port 9090)
    #[arg(
        long = "prometheus",
        alias = "metrics",
        value_name = "PORT",
        num_args = 0..=1,
        default_missing_value = "9090"
    )]
    prometheus: Option<u16>,

    /// Alias for --prometheus <PORT>
    #[arg(long = "metrics-port", value_name = "PORT", hide = true)]
    metrics_port: Option<u16>,

    /// Webhook URL for crisis alerts (Slack / Discord / generic REST)
    #[arg(long = "webhook", value_name = "URL")]
    webhook: Option<String>,

    /// Comma-separated alert kinds: stall,shi_below_70,http_5xx,mismatch,ad_start
    #[arg(long = "alert-on", value_name = "EVENTS", default_value = "stall,shi_below_70,http_5xx")]
    alert_on: String,
}

#[tokio::main]
async fn main() -> Result<ExitCode> {
    let _ = rustls::crypto::ring::default_provider().install_default();

    color_eyre::install()?;
    install_panic_hook();

    let cli = Cli::parse();
    let client = build_http_client(&cli.headers, cli.user_agent.clone())?;

    let metrics_port = cli.metrics_port.or(cli.prometheus);

    let session = SessionOpts {
        headers: cli.headers.clone(),
        user_agent: cli.user_agent.clone(),
        interval_ms: cli.interval_ms,
        probe_headers: cli.probe_headers,
        webhook_url: cli.webhook.clone(),
        alert_on: cli.alert_on.clone(),
    };

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
            if cli.audit {
                let report = run_audit(&origin, channels, cli.headers, cli.user_agent).await?;
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
            if let Some(port) = metrics_port {
                run_prometheus(url, session, port).await
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
                let report = run_audit(&origin, channels, cli.headers, cli.user_agent).await?;
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
            return Err(eyre!("HTTP {status} — {input}"));
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
