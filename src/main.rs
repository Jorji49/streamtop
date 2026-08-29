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
use streamtop::engine::report_export::run_report_export;
use streamtop::engine::summary::{run_summary, SummaryFormat};
use streamtop::engine::vod::run_vod;
use streamtop::models::ChannelEntry;
use streamtop::ui::app::{restore_terminal_global, SessionOpts};
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
    #[arg(required_unless_present_any = ["compare", "export_grafana", "vod", "agent"])]
    url: Option<String>,

    /// Multi-stream headless agent (TOML config with [[streams]])
    #[arg(long = "agent", value_name = "CONFIG.toml")]
    agent: Option<PathBufArg>,

    /// Export HTML or JSON compliance report and exit
    #[arg(long = "export-report", value_name = "PATH")]
    export_report: Option<PathBufArg>,

    /// Compare two live streams side by side
    #[arg(long = "compare", num_args = 2, value_names = ["URL_1", "URL_2"])]
    compare: Option<Vec<String>>,

    /// Write streamtop-grafana.json for Prometheus and exit
    #[arg(long = "export-grafana")]
    export_grafana: bool,

    /// Print a curl command for the last segment after a short poll
    #[arg(long = "export-curl")]
    export_curl: bool,

    /// Write HAR 1.2 for manifest + last segment after a short poll
    #[arg(long = "export-har", value_name = "FILE")]
    export_har: Option<PathBufArg>,

    /// Named profile from ~/.config/streamtop/config.toml
    #[arg(long = "profile", value_name = "NAME")]
    profile: Option<String>,

    /// Extra HTTP header (repeatable). Format: "Key: Value"
    #[arg(short = 'H', long = "header", value_name = "KEY: VALUE")]
    headers: Vec<String>,

    /// User-Agent string
    #[arg(short = 'A', long = "user-agent")]
    user_agent: Option<String>,

    /// Manifest poll interval in milliseconds
    #[arg(short = 'i', long = "interval", value_name = "MS")]
    interval_ms: Option<u64>,

    /// Range-request the start of each segment only (wire/header probe). Default on; use --full-segment to disable.
    #[arg(long = "probe-headers", alias = "range-probe")]
    probe_headers: bool,

    /// Download full segments instead of 64 KB range probe (more bandwidth, bitrate timing)
    #[arg(long = "full-segment")]
    full_segment: bool,

    /// Staging ClearKey: KID_HEX:KEY_HEX for encrypted probe validation
    #[arg(long = "clearkey", value_name = "KID:KEY")]
    clearkey: Option<String>,

    /// Export incident bundle to PATH (or diagnostics/incident_<time>.json) and exit after --timeout
    #[arg(long = "export-incident", value_name = "PATH", num_args = 0..=1, default_missing_value = "")]
    export_incident: Option<String>,

    /// Probe DRM license / EXT-X-KEY / DASH LA_URL ClearKey TTFB
    #[arg(long = "probe-drm")]
    probe_drm: bool,

    /// Range-probe every channel; write audit_report.json/.csv
    #[arg(long = "audit", alias = "matrix")]
    audit: bool,

    /// Headless PASS/FAIL summary (no TUI)
    #[arg(long = "summary", alias = "headless")]
    summary: bool,

    /// VOD inspection: crawl playlist/MPD tree without live polling
    #[arg(long = "vod", value_name = "URL")]
    vod: Option<String>,

    /// OTLP trace export endpoint (e.g. http://127.0.0.1:4318)
    #[arg(long = "otel-endpoint", value_name = "URL")]
    otel_endpoint: Option<String>,

    /// Summary format: text or json (streamtop.summary.v1)
    #[arg(long = "summary-format", value_enum, default_value_t = SummaryFormatArg::Text)]
    summary_format: SummaryFormatArg,

    /// Listen seconds for --summary / export modes (default: 8)
    #[arg(long = "timeout", value_name = "SECS", default_value_t = 8)]
    timeout_secs: u64,

    /// Serve Prometheus /metrics (default port 9184, bind 127.0.0.1)
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

    /// Metrics listen address (default: 127.0.0.1)
    #[arg(
        long = "metrics-bind",
        value_name = "ADDR",
        default_value = "127.0.0.1"
    )]
    metrics_bind: String,

    /// Bearer token for /metrics (Authorization: Bearer …)
    #[arg(long = "metrics-token", value_name = "TOKEN")]
    metrics_token: Option<String>,

    /// Webhook URL (Slack / Discord / HTTP)
    #[arg(long = "webhook", value_name = "URL")]
    webhook: Option<String>,

    /// Alert kinds: stall,shi_below_70,http_5xx,mismatch,ad_start,ad_mismatch
    #[arg(long = "alert-on", value_name = "EVENTS")]
    alert_on: Option<String>,

    /// Allow webhook targets on private/link-local/metadata hosts (tests only)
    #[arg(long = "allow-insecure-webhooks")]
    allow_insecure_webhooks: bool,

    /// Allow OTLP targets on private/link-local/metadata hosts (tests only)
    #[arg(long = "allow-insecure-otel")]
    allow_insecure_otel: bool,

    /// Allow ingest targets on private/link-local/metadata hosts (tests only)
    #[arg(long = "allow-insecure-ingest")]
    allow_insecure_ingest: bool,

    /// ETSI TR 101 290 P1/P2 MPEG-TS compliance probe
    #[arg(long = "tr101290")]
    tr101290: bool,

    /// SEI NAL probe: captions (CEA-608/708) and HDR metadata
    #[arg(long = "probe-sei")]
    probe_sei: bool,

    /// Synthetic player QoE simulator (TDR, rebuffer risk, ABR selection)
    #[arg(long = "simulate-player")]
    simulate_player: bool,

    /// Virtual throughput cap for --simulate-player (kbps)
    #[arg(long = "throttle-kbps", value_name = "KBPS")]
    throttle_kbps: Option<u64>,

    /// Simulated RTT added to segment fetch time (ms)
    #[arg(long = "simulated-rtt-ms", value_name = "MS")]
    simulated_rtt_ms: Option<u64>,
}

/// Path argument for clap.
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
        eprintln!("Wrote {GRAFANA_DASHBOARD_FILENAME}");
        return Ok(ExitCode::SUCCESS);
    }

    if let Some(PathBufArg(config)) = &cli.agent {
        let path = config
            .to_str()
            .ok_or_else(|| eyre!("agent config path is not valid UTF-8"))?;
        return streamtop::engine::agent::run_agent(path).await;
    }

    let mut session = session_from_profile(
        cli.profile.as_deref(),
        SessionOpts {
            headers: Vec::new(),
            user_agent: None,
            interval_ms: None,
            probe_headers: true,
            probe_drm: false,
            clearkey: None,
            export_incident: None,
            webhook_url: None,
            alert_on: "stall,shi_below_70,http_5xx".into(),
            allow_insecure_webhooks: false,
            allow_insecure_otel: false,
            allow_insecure_ingest: false,
            otel_endpoint: None,
            tr101290: false,
            probe_sei: false,
            simulate_player: false,
            throttle_kbps: None,
            simulated_rtt_ms: None,
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
    if cli.full_segment {
        session.probe_headers = false;
    } else if cli.probe_headers {
        session.probe_headers = true;
    }
    if cli.probe_drm {
        session.probe_drm = true;
    }
    if let Some(ck) = &cli.clearkey {
        session.clearkey = Some(ck.clone());
    }
    if cli.export_incident.is_some() {
        session.export_incident = cli.export_incident.clone();
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
    if cli.allow_insecure_otel {
        session.allow_insecure_otel = true;
        eprintln!(
            "warning: --allow-insecure-otel enables private/link-local/metadata OTLP targets"
        );
    }
    if cli.allow_insecure_ingest {
        session.allow_insecure_ingest = true;
        eprintln!(
            "warning: --allow-insecure-ingest enables private/link-local/metadata ingest targets"
        );
    }
    if cli.otel_endpoint.is_some() {
        session.otel_endpoint = cli.otel_endpoint.clone();
    }
    if cli.tr101290 {
        session.tr101290 = true;
    }
    if cli.probe_sei {
        session.probe_sei = true;
    }
    if cli.simulate_player {
        session.simulate_player = true;
    }
    if cli.throttle_kbps.is_some() {
        session.throttle_kbps = cli.throttle_kbps;
    }
    if cli.simulated_rtt_ms.is_some() {
        session.simulated_rtt_ms = cli.simulated_rtt_ms;
    }

    if let Some(vod_url) = &cli.vod {
        let exit = run_vod(vod_url.clone(), session, cli.summary_format.into()).await?;
        restore_terminal_global();
        return Ok(exit);
    }
    if let Some(hook) = &session.webhook_url {
        streamtop::engine::webhook::AlertKind::parse_list(&session.alert_on)
            .wrap_err("invalid --alert-on")?;
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
        restore_terminal_global();
        return Ok(ExitCode::SUCCESS);
    }

    let input_url = cli
        .url
        .as_deref()
        .ok_or_else(|| eyre!("missing stream URL (or use --compare)"))?;

    if streamtop::engine::ingest_probe::is_ingest_url(input_url) {
        if metrics_port.is_some() {
            restore_terminal_global();
            return Err(eyre!(
                "Prometheus mode for ingest URLs is not supported; use the TUI or --summary"
            ));
        }
        if cli.export_curl || cli.export_har.is_some() {
            restore_terminal_global();
            return Err(eyre!(
                "--export-curl/--export-har are not supported for ingest URLs"
            ));
        }
        let exit = if cli.summary {
            streamtop::engine::summary::run_ingest_summary(
                input_url.to_string(),
                cli.timeout_secs,
                cli.summary_format.into(),
                session.allow_insecure_ingest,
            )
            .await?
        } else {
            App::run_diagnostics(input_url.to_string(), input_url.to_string(), session).await?;
            ExitCode::SUCCESS
        };
        restore_terminal_global();
        return Ok(exit);
    }

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
            } else if let Some(PathBufArg(report_path)) = &cli.export_report {
                run_report_export(url, session, report_path.as_path(), cli.timeout_secs).await
            } else {
                App::run_diagnostics(origin, url, session).await?;
                Ok(ExitCode::SUCCESS)
            }
        }
    };

    restore_terminal_global();
    exit.wrap_err("streamtop ended with an error")
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
