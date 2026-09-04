#![forbid(unsafe_code)]

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
use streamtop::engine::budget::{run_budget, BudgetOpts};
use streamtop::engine::config::session_from_profile;
use streamtop::engine::export::{build_curl, capture_for_export, write_har};
use streamtop::engine::export_cli::{parse_export_spec, ExportFormat, ExportPlan};
use streamtop::engine::grafana::{export_grafana_dashboard, GRAFANA_DASHBOARD_FILENAME};
use streamtop::engine::metrics::run_prometheus;
use streamtop::engine::playlist_parser::{
    detect_and_parse, looks_like_remote_url, path_to_file_url, ParsedInput,
};
use streamtop::engine::poller::build_http_client;
use streamtop::engine::summary::{run_summary, SummaryFormat};
use streamtop::engine::vod::run_vod;
use streamtop::models::ChannelEntry;
use streamtop::ui::app::{restore_terminal_global, SessionOpts};
use streamtop::ui::{App, CompareApp, MultiCdnApp};

#[derive(Debug, Clone, Copy, clap::ValueEnum, Default)]
enum SummaryFormatArg {
    #[default]
    Text,
    Json,
    Sarif,
}

impl From<SummaryFormatArg> for SummaryFormat {
    fn from(v: SummaryFormatArg) -> Self {
        match v {
            SummaryFormatArg::Text => Self::Text,
            SummaryFormatArg::Json => Self::Json,
            SummaryFormatArg::Sarif => Self::Sarif,
        }
    }
}

#[derive(Debug, Parser)]
#[command(
    name = "streamtop",
    version,
    about = "HLS/DASH/IPTV stream diagnostics TUI"
)]
#[allow(clippy::struct_excessive_bools)] // clap CLI flags
struct Cli {
    /// Stream URL, local playlist/MPD, or channel lineup (M3U / JSON / YAML)
    #[arg(required_unless_present_any = ["compare", "export", "vod", "agent", "multi_cdn"])]
    url: Option<String>,

    /// Export: `report-html[:PATH]`, `report-json`, `curl`, `har[:PATH]`, `incident[:PATH]`, `grafana`, `sarif[:PATH]`
    #[arg(long = "export", value_name = "FORMAT[:FILE]")]
    export: Vec<String>,

    /// Multi-stream headless agent (TOML config with [[streams]])
    #[arg(long = "agent", value_name = "CONFIG.toml")]
    agent: Option<std::path::PathBuf>,

    /// Compare two live streams side by side
    #[arg(long = "compare", num_args = 2, value_names = ["URL_1", "URL_2"])]
    compare: Option<Vec<String>>,

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
    #[arg(long = "probe-headers")]
    probe_headers: bool,

    /// Download full segments instead of 64 KB range probe (more bandwidth, bitrate timing)
    #[arg(long = "full-segment")]
    full_segment: bool,

    /// Staging `ClearKey`: `KID_HEX:KEY_HEX` for encrypted probe validation
    #[arg(long = "clearkey", value_name = "KID:KEY")]
    clearkey: Option<String>,

    /// Probe DRM license / EXT-X-KEY / DASH `LA_URL` `ClearKey` TTFB
    #[arg(long = "probe-drm")]
    probe_drm: bool,

    /// Range-probe every channel; write `audit_report.json` / `.csv`
    #[arg(long = "audit")]
    audit: bool,

    /// Headless PASS/FAIL summary (no TUI)
    #[arg(long = "summary")]
    summary: bool,

    /// VOD inspection: crawl playlist/MPD tree without live polling
    #[arg(long = "vod", value_name = "URL")]
    vod: Option<String>,

    /// OTLP trace export endpoint (e.g. <http://127.0.0.1:4318>)
    #[arg(long = "otel-endpoint", value_name = "URL")]
    otel_endpoint: Option<String>,

    /// Summary format: text, json, or sarif (streamtop.summary.v1 / SARIF 2.1.0)
    #[arg(long = "summary-format", value_enum, default_value_t = SummaryFormatArg::Text)]
    summary_format: SummaryFormatArg,

    /// Write GitHub Actions step summary markdown (SHI, ABR, budget table)
    #[arg(long = "github-step-summary", value_name = "FILE")]
    github_step_summary: Option<std::path::PathBuf>,

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

    /// Metrics listen address (default: 127.0.0.1)
    #[arg(
        long = "metrics-bind",
        value_name = "ADDR",
        default_value = "127.0.0.1"
    )]
    metrics_bind: String,

    /// Bearer token for /metrics (Authorization: Bearer ...)
    #[arg(long = "metrics-token", value_name = "TOKEN")]
    metrics_token: Option<String>,

    /// Webhook URL (Slack / Discord / HTTP)
    #[arg(long = "webhook", value_name = "URL")]
    webhook: Option<String>,

    /// Alert kinds: `stall,shi_below_70,http_5xx,mismatch,ad_start,ad_mismatch`
    #[arg(long = "alert-on", value_name = "EVENTS")]
    alert_on: Option<String>,

    /// Allow webhook targets on private/link-local/metadata hosts (tests only)
    #[arg(long = "allow-insecure-webhooks")]
    allow_insecure_webhooks: bool,

    /// Allow OTLP targets on private/link-local/metadata hosts (tests only)
    #[arg(long = "allow-insecure-otel")]
    allow_insecure_otel: bool,

    /// ETSI TR 101 290 P1/P2 MPEG-TS compliance probe
    #[arg(long = "tr101290")]
    tr101290: bool,

    /// SEI NAL probe: captions (CEA-608/708) and HDR metadata
    #[arg(long = "probe-sei")]
    probe_sei: bool,

    /// Stream budget: max download-to-duration ratio (CI assertion mode)
    #[arg(long = "budget-max-rtf", value_name = "RATIO")]
    budget_max_rtf: Option<f32>,

    /// Stream budget: max segment TTFB (e.g. 250ms, 2s)
    #[arg(long = "budget-max-ttfb", value_name = "DURATION")]
    budget_max_ttfb: Option<String>,

    /// Stream budget: max TR 101 290 continuity counter errors
    #[arg(long = "budget-max-cc-errors", value_name = "N")]
    budget_max_cc_errors: Option<u32>,

    /// Stream budget: max A/V or subtitle drift (e.g. 2000ms)
    #[arg(long = "budget-max-drift", value_name = "DURATION")]
    budget_max_drift: Option<String>,

    /// Stream budget probe window (default 30s when any budget flag is set)
    #[arg(long = "budget-duration", value_name = "SECS")]
    budget_duration: Option<u64>,

    /// DNS-over-HTTPS provider for resolution timing: cloudflare, google, or custom URL
    #[arg(long = "doh-provider", value_name = "PROVIDER")]
    doh_provider: Option<String>,

    /// Concurrent multi-CDN edge skew analysis (comma-separated URLs or label=URL pairs)
    #[arg(long = "multi-cdn", value_name = "URL1,URL2,...")]
    multi_cdn: Option<String>,

    /// Max live-edge skew across CDN edges before `ERR_CDN_SYNC_SKEW` (default 3000ms)
    #[arg(long = "max-cdn-skew-ms", value_name = "MS", default_value_t = 3000)]
    max_cdn_skew_ms: i64,
}

#[allow(clippy::too_many_lines)]
#[tokio::main]
async fn main() -> Result<ExitCode> {
    let _ = rustls::crypto::ring::default_provider().install_default();

    color_eyre::install()?;
    install_panic_hook();

    let cli = Cli::parse();

    if let Some(config) = &cli.agent {
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
            otel_endpoint: None,
            tr101290: false,
            probe_sei: false,
            doh_provider: None,
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
    if cli.otel_endpoint.is_some() {
        session.otel_endpoint = cli.otel_endpoint.clone();
    }
    if cli.tr101290 {
        session.tr101290 = true;
    }
    if cli.probe_sei {
        session.probe_sei = true;
    }
    if let Some(doh) = &cli.doh_provider {
        session.doh_provider = Some(doh.clone());
    }

    let mut export_plan = ExportPlan::default();
    for spec in &cli.export {
        export_plan.push(parse_export_spec(spec)?);
    }
    if export_plan
        .exports
        .iter()
        .any(|e| e.format == ExportFormat::Grafana)
    {
        export_grafana_dashboard(GRAFANA_DASHBOARD_FILENAME)?;
        eprintln!("Wrote {GRAFANA_DASHBOARD_FILENAME}");
        if cli.url.is_none() && cli.compare.is_none() && cli.vod.is_none() && cli.agent.is_none() {
            return Ok(ExitCode::SUCCESS);
        }
    }

    let budget = BudgetOpts {
        max_rtf: cli.budget_max_rtf,
        max_ttfb_ms: cli
            .budget_max_ttfb
            .as_deref()
            .map(parse_budget_duration_ms)
            .transpose()?,
        max_cc_errors: cli.budget_max_cc_errors,
        max_drift_ms: cli
            .budget_max_drift
            .as_deref()
            .map(parse_budget_duration_ms)
            .transpose()?
            .map(u64::cast_signed),
        duration_secs: cli.budget_duration.unwrap_or(30),
    };

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

    let client = build_http_client(&session.headers, session.user_agent.as_deref())?;
    let metrics_port = cli.prometheus;
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
        streamtop::engine::metrics::require_metrics_token_for_bind(
            metrics_bind,
            metrics_token.as_deref(),
        )
        .wrap_err("metrics bind security check failed")?;
    }

    if let Some(raw) = &cli.multi_cdn {
        let targets = streamtop::engine::multi_cdn::parse_multi_cdn(raw)?;
        if cli.summary {
            let duration = Duration::from_secs(cli.timeout_secs.max(8));
            let (report, findings) = streamtop::engine::multi_cdn::analyze_multi_cdn(
                &targets,
                &session,
                duration,
                cli.max_cdn_skew_ms,
            )
            .await?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            if !findings.is_empty() {
                eprintln!("findings: {}", findings.len());
            }
            restore_terminal_global();
            return Ok(if report.max_skew_ms > cli.max_cdn_skew_ms {
                ExitCode::from(1)
            } else {
                ExitCode::SUCCESS
            });
        }
        MultiCdnApp::new(targets, session, cli.max_cdn_skew_ms)
            .run()
            .await?;
        restore_terminal_global();
        return Ok(ExitCode::SUCCESS);
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

    if input_url.starts_with("srt://") || input_url.starts_with("rtmp://") {
        restore_terminal_global();
        return Err(eyre!(
            "srt:// and rtmp:// are not supported; use WHEP HTTP endpoints (https://host/.../whep)"
        ));
    }

    if streamtop::engine::whep::is_whep_url(input_url) {
        let report = streamtop::engine::whep::probe_whep(&client, input_url).await?;
        println!("{}", serde_json::to_string_pretty(&report)?);
        restore_terminal_global();
        return Ok(if report.ready {
            ExitCode::SUCCESS
        } else {
            ExitCode::from(1)
        });
    }

    let (origin, body, content_type) = load_input(&client, input_url).await?;
    let parsed = detect_and_parse(&origin, &body, content_type.as_deref())
        .wrap_err("failed to detect input type (HLS / DASH / IPTV / catalog)")?;

    let want_export = export_plan.wants_curl_or_har();

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
                    "--export curl/har require a single HLS/DASH stream URL"
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
            if export_plan.wants_any() {
                execute_export_plan(&export_plan, &url, &session, cli.timeout_secs, input_url).await
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
            } else if budget.active() {
                run_budget(
                    url,
                    session,
                    budget,
                    cli.github_step_summary.as_deref(),
                )
                .await
            } else if cli.summary {
                run_summary(
                    url,
                    session,
                    cli.timeout_secs,
                    cli.summary_format.into(),
                    cli.github_step_summary.as_deref(),
                )
                .await
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
            .map(std::string::ToString::to_string);
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

async fn execute_export_plan(
    plan: &ExportPlan,
    url: &str,
    session: &SessionOpts,
    timeout_secs: u64,
    input_label: &str,
) -> Result<ExitCode> {
    use streamtop::engine::export_cli::ExportFormat;
    use streamtop::engine::report_export::run_report_export;
    use streamtop::engine::sarif::render_sarif_json;
    use streamtop::models::DIAGNOSTIC_DIR;

    let mut exit = ExitCode::SUCCESS;
    for item in &plan.exports {
        if item.format == ExportFormat::Grafana {
            continue;
        }
        match item.format {
            ExportFormat::Curl => {
                let cap =
                    capture_for_export(url.to_string(), session.clone(), timeout_secs).await?;
                println!("{}", build_curl(&cap));
            }
            ExportFormat::Har => {
                let cap =
                    capture_for_export(url.to_string(), session.clone(), timeout_secs).await?;
                let path = item
                    .path
                    .clone()
                    .unwrap_or_else(|| std::path::PathBuf::from("incident.har"));
                write_har(&path, &cap)?;
                eprintln!("Wrote HAR {}", path.display());
            }
            ExportFormat::ReportHtml | ExportFormat::ReportJson => {
                let path = item.path.clone().unwrap_or_else(|| {
                    if item.format == ExportFormat::ReportJson {
                        std::path::PathBuf::from("report.json")
                    } else {
                        std::path::PathBuf::from("report.html")
                    }
                });
                let code = run_report_export(url.to_string(), session.clone(), &path, timeout_secs)
                    .await?;
                if code != ExitCode::SUCCESS {
                    exit = code;
                }
            }
            ExportFormat::Incident => {
                let mut s = session.clone();
                s.export_incident = Some(
                    item.path
                        .as_ref()
                        .and_then(|p| p.to_str())
                        .unwrap_or("")
                        .to_string(),
                );
                App::run_diagnostics(input_label.to_string(), url.to_string(), s).await?;
            }
            ExportFormat::Sarif => {
                let cap =
                    capture_for_export(url.to_string(), session.clone(), timeout_secs).await?;
                let path = item
                    .path
                    .clone()
                    .unwrap_or_else(|| std::path::PathBuf::from("streamtop.sarif"));
                let json = render_sarif_json(&cap.findings, &cap.spec_violations)?;
                std::fs::write(&path, json)?;
                eprintln!(
                    "Wrote SARIF {} (findings={})",
                    path.display(),
                    cap.findings.len()
                );
                let _ = DIAGNOSTIC_DIR;
            }
            ExportFormat::Grafana => {}
        }
    }
    Ok(exit)
}

fn parse_budget_duration_ms(s: &str) -> Result<u64> {
    let s = s.trim();
    if let Some(raw) = s.strip_suffix("ms") {
        return raw
            .trim()
            .parse::<u64>()
            .wrap_err_with(|| format!("invalid duration: {s}"));
    }
    if let Some(raw) = s.strip_suffix('s') {
        let secs: u64 = raw
            .trim()
            .parse()
            .wrap_err_with(|| format!("invalid duration: {s}"))?;
        return Ok(secs.saturating_mul(1000));
    }
    s.parse::<u64>()
        .wrap_err_with(|| format!("invalid duration (use 250ms or 2s): {s}"))
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
