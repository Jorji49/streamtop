//! Self-contained HTML / JSON compliance and incident reports.

use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use chrono::Utc;
use color_eyre::eyre::{eyre, Result, WrapErr};
use serde_json::json;
use tokio::sync::mpsc;
use tokio::time::timeout;

use crate::engine::incident::{build_incident_report, write_incident_report};
use crate::engine::metrics::MetricsSnapshot;
use crate::engine::poller::DiagnosticOpts;
use crate::engine::redact::{redact_text, redact_url};
use crate::engine::summary::SUMMARY_SCHEMA_VERSION;
use crate::engine::ManifestPoller;
use crate::models::{
    AbrHealth, CdnStats, DiagnosticSummary, HealthReport, HttpTransaction, LatencyState, LogEntry,
    PlaylistMeta, StreamEvent, StreamSnapshot, StreamStatusKind, EVENT_CHANNEL_CAPACITY,
};
use crate::ui::app::SessionOpts;

/// Headless poll + export compliance report to `.html` or `.json`.
pub async fn run_report_export(
    url: String,
    session: SessionOpts,
    path: &Path,
    timeout_secs: u64,
) -> Result<ExitCode> {
    let (snapshot, manifests, http_log) =
        collect_report_data(url.clone(), session.clone(), timeout_secs).await?;
    let out = export_report(
        path,
        snapshot,
        &manifests,
        &http_log,
        &session.headers,
        session.user_agent.as_deref(),
    )?;
    eprintln!("Wrote {}", out.display());
    Ok(ExitCode::SUCCESS)
}

async fn collect_report_data(
    url: String,
    session: SessionOpts,
    timeout_secs: u64,
) -> Result<(StreamSnapshot, Vec<PlaylistMeta>, Vec<HttpTransaction>)> {
    let otel = if let Some(ep) = &session.otel_endpoint {
        Some(crate::engine::otel::OtelExporter::new(
            ep,
            session.allow_insecure_otel,
        )?)
    } else {
        None
    };
    let metrics = Arc::new(RwLock::new({
        let mut snap = MetricsSnapshot::default();
        snap.url.clone_from(&url);
        snap
    }));
    let (tx, mut rx) = mpsc::channel(EVENT_CHANNEL_CAPACITY);
    let mut poller = ManifestPoller::new(
        url.as_str(),
        &session.headers,
        session.user_agent.as_deref(),
        session.interval_ms,
        session.probe_headers,
        session.probe_drm,
        tx,
    )?
    .with_metrics(Arc::clone(&metrics))
    .with_diagnostics(&DiagnosticOpts {
        tr101290: session.tr101290,
        probe_sei: session.probe_sei,
        simulate_player: session.simulate_player,
        throttle_kbps: session.throttle_kbps,
        simulated_rtt_ms: session.simulated_rtt_ms,
    });
    if let Some(exporter) = otel.clone() {
        poller = poller.with_otel(exporter);
    }
    if let Some(ck) = &session.clearkey {
        if let Ok(spec) = crate::engine::drm_probe::ClearKeySpec::parse(ck) {
            poller = poller.with_clearkey(Some(spec));
        }
    }
    poller = crate::engine::session_poller::apply_session_doh(poller, &session)?;
    let handle = tokio::spawn(async move {
        poller.run().await;
    });

    let mut health = HealthReport::perfect();
    let mut status = crate::models::StreamStatus::live("probing…");
    let mut latency = LatencyState::Unknown;
    let mut cdn = CdnStats::default();
    let mut buffer = crate::models::VirtualBuffer::default();
    let mut variants = Vec::new();
    let mut playlist: Option<PlaylistMeta> = None;
    let mut last_segment = None;
    let mut findings = Vec::new();
    let mut log: Vec<LogEntry> = Vec::new();
    let mut manifest_history = Vec::new();
    let mut http_log = Vec::new();
    let mut abr_health = AbrHealth::default();
    let mut active_ad = None;

    let deadline = Instant::now() + Duration::from_secs(timeout_secs.max(1));
    while Instant::now() < deadline {
        let left = deadline.saturating_duration_since(Instant::now());
        match timeout(left, rx.recv()).await {
            Ok(Some(ev)) => match ev {
                StreamEvent::Health(h) => health = h,
                StreamEvent::Status(s) => status = s,
                StreamEvent::Latency(l) => latency = l,
                StreamEvent::CdnStats(c) => cdn = c,
                StreamEvent::Buffer(b) => buffer = b,
                StreamEvent::Variants(v) => variants = v,
                StreamEvent::PlaylistMeta(m) => {
                    manifest_history.push(m.clone());
                    if manifest_history.len() > 20 {
                        manifest_history.remove(0);
                    }
                    playlist = Some(m);
                }
                StreamEvent::Segment(s) => {
                    http_log.push(HttpTransaction {
                        method: "GET".into(),
                        url: s.uri.clone(),
                        status: s.http_status,
                        ttfb_ms: s.ttfb_ms,
                        bytes: s.transferred_bytes,
                        cdn_provider: s.cdn.provider.clone(),
                    });
                    if http_log.len() > 100 {
                        http_log.remove(0);
                    }
                    last_segment = Some(s);
                }
                StreamEvent::AbrHealth(a) => abr_health = a,
                StreamEvent::AdBreak(ad) => active_ad = Some(ad),
                StreamEvent::Finding(f) => findings.push(f),
                StreamEvent::Log {
                    level,
                    category,
                    message,
                } => {
                    log.push(LogEntry::make(level, category, message));
                }
                _ => {}
            },
            Ok(None) | Err(_) => break,
        }
    }
    handle.abort();
    if let Some(exporter) = otel {
        let _ = exporter.flush_all().await;
    }

    let status_label = match status.kind {
        StreamStatusKind::Live => "LIVE",
        StreamStatusKind::Degraded => "DEGRADED",
        StreamStatusKind::Error => "ERROR",
    };
    let now = Utc::now();
    let timeline: Vec<String> = log
        .iter()
        .map(|e| redact_text(&e.timeline_line()))
        .collect();
    let snapshot = StreamSnapshot {
        title: format!("streamtop report @ {}", now.format("%Y-%m-%d %H:%M:%S UTC")),
        summary: DiagnosticSummary {
            channel: url::Url::parse(&url)
                .ok()
                .and_then(|u| u.host_str().map(std::string::ToString::to_string)),
            captured_at: now,
            source_url: redact_url(&url),
            active_url: redact_url(playlist.as_ref().map_or(&url, |p| p.url.as_str())),
            status: status_label.into(),
            health_score: health.score,
            health_label: health.label.clone(),
            latency: latency.display(),
            cdn: cdn.last.as_ref().map_or_else(
                || "UNKNOWN".into(),
                super::super::models::stream::CdnEdgeInfo::badge,
            ),
            dvr_window: playlist.as_ref().map_or_else(
                || "-".into(),
                |p| crate::models::format_dvr_window(p.window_segments, p.window_secs),
            ),
            buffer: buffer.display(),
            ll_hls: playlist.as_ref().is_some_and(|p| p.ll_hls.is_ll_hls),
            dropped_events: crate::engine::channel_stats::channel_dropped_total(),
        },
        timeline,
        health,
        cdn,
        abr_health,
        active_ad,
        playlist,
        abr_profiles: variants,
        last_segment,
        findings,
        event_log: log,
    };
    Ok((snapshot, manifest_history, http_log))
}

/// Export path extension selects HTML or JSON bundle.
pub fn export_report(
    path: &Path,
    snapshot: StreamSnapshot,
    manifest_history: &[PlaylistMeta],
    http_log: &[HttpTransaction],
    headers: &[String],
    user_agent: Option<&str>,
) -> Result<PathBuf> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "html" | "htm" => export_html_report(
            path,
            snapshot,
            manifest_history,
            http_log,
            headers,
            user_agent,
        ),
        "json" => export_json_report(
            path,
            snapshot,
            manifest_history,
            http_log,
            headers,
            user_agent,
        ),
        other => Err(eyre!(
            "unsupported report extension: {other} (use .html or .json)"
        )),
    }
}

fn export_json_report(
    path: &Path,
    snapshot: StreamSnapshot,
    manifest_history: &[PlaylistMeta],
    http_log: &[HttpTransaction],
    headers: &[String],
    user_agent: Option<&str>,
) -> Result<PathBuf> {
    let incident = build_incident_report(snapshot, manifest_history, http_log, headers, user_agent);
    let bundle = json!({
        "schema": "streamtop.report.v1",
        "schema_version": SUMMARY_SCHEMA_VERSION,
        "summary_schema": "streamtop.summary.v1",
        "captured_at": Utc::now().to_rfc3339(),
        "incident": incident,
    });
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    let file = File::create(path).wrap_err_with(|| format!("create {}", path.display()))?;
    let mut w = BufWriter::new(file);
    serde_json::to_writer_pretty(&mut w, &bundle)?;
    w.flush()?;
    Ok(path.to_path_buf())
}

fn export_html_report(
    path: &Path,
    snapshot: StreamSnapshot,
    manifest_history: &[PlaylistMeta],
    http_log: &[HttpTransaction],
    headers: &[String],
    user_agent: Option<&str>,
) -> Result<PathBuf> {
    let incident = build_incident_report(snapshot, manifest_history, http_log, headers, user_agent);
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    let file = File::create(path).wrap_err_with(|| format!("create {}", path.display()))?;
    let mut w = BufWriter::new(file);
    write_html_dashboard(
        &mut w,
        &incident.summary,
        manifest_history,
        http_log,
        &incident.curl_commands,
    )?;
    w.flush()?;
    let sidecar = path.with_extension("incident.json");
    write_incident_report(&sidecar, &incident)?;
    Ok(path.to_path_buf())
}

fn write_html_dashboard(
    w: &mut impl Write,
    snap: &StreamSnapshot,
    manifest_history: &[PlaylistMeta],
    http_log: &[HttpTransaction],
    curl_cmds: &[String],
) -> Result<()> {
    let title = html_escape(&snap.title);
    let url = html_escape(&redact_url(&snap.summary.active_url));
    let status = html_escape(&snap.summary.status);
    let health = format!(
        "{} ({})",
        snap.health.score,
        html_escape(&snap.health.label)
    );
    let latency = html_escape(&snap.summary.latency);
    let cdn = html_escape(&snap.summary.cdn);
    let ts = Utc::now().format("%Y-%m-%d %H:%M:%S UTC");

    write!(
        w,
        r#"<!DOCTYPE html>
<html lang="en"><head>
<meta charset="utf-8"/>
<meta name="viewport" content="width=device-width, initial-scale=1"/>
<title>{title} - streamtop report</title>
<style>
body{{font-family:system-ui,sans-serif;background:#0f1419;color:#e6edf3;margin:0;padding:24px}}
h1,h2{{color:#58a6ff}} .card{{background:#161b22;border:1px solid #30363d;border-radius:8px;padding:16px;margin:16px 0}}
table{{border-collapse:collapse;width:100%}} th,td{{border:1px solid #30363d;padding:8px;text-align:left}}
pre{{background:#0d1117;padding:12px;overflow:auto;font-size:12px}}
</style></head><body>
<h1>{title}</h1>
<div class="card"><strong>URL</strong> {url}<br/>
<strong>Status</strong> {status} | <strong>SHI</strong> {health} | <strong>Latency</strong> {latency} | <strong>CDN</strong> {cdn}
</div>
<div class="card"><h2>Spec violations</h2>"#
    )?;
    write_violations_table(w, &snap.findings)?;
    write!(w, r#"</div><div class="card"><h2>Manifest snapshots</h2>"#)?;
    write_manifest_table(w, manifest_history)?;
    write!(w, r#"</div><div class="card"><h2>HTTP transactions</h2>"#)?;
    write_http_table(w, http_log)?;
    write!(
        w,
        r#"</div><div class="card"><h2>DAI / ad alignment log</h2>"#
    )?;
    write_dai_log_section(w, &snap.timeline)?;
    write!(w, "</div>")?;
    if !curl_cmds.is_empty() {
        write!(w, r#"<div class="card"><h2>curl reproducer</h2><pre>"#)?;
        write!(w, "{}", html_escape(&redact_text(&curl_cmds.join("\n\n"))))?;
        write!(w, "</pre></div>")?;
    }
    write!(
        w,
        r"<p><small>streamtop report v1 | summary schema v{SUMMARY_SCHEMA_VERSION} | generated {ts}</small></p>
</body></html>",
    )?;
    Ok(())
}

#[cfg(test)]
fn render_html_dashboard(
    snap: &StreamSnapshot,
    manifest_history: &[PlaylistMeta],
    http_log: &[HttpTransaction],
    curl_cmds: &[String],
) -> String {
    let mut buf = Vec::new();
    write_html_dashboard(&mut buf, snap, manifest_history, http_log, curl_cmds)
        .expect("html to vec");
    String::from_utf8(buf).expect("utf8 html")
}

fn write_violations_table(
    w: &mut impl Write,
    findings: &[crate::models::DiagnosticFinding],
) -> Result<()> {
    if findings.is_empty() {
        write!(w, "<p>No spec violations recorded.</p>")?;
        return Ok(());
    }
    write!(
        w,
        "<table><tr><th>Severity</th><th>Rule</th><th>Message</th></tr>"
    )?;
    for f in findings.iter().take(100) {
        let sev = format!("{:?}", f.severity);
        write!(
            w,
            "<tr><td>{sev}</td><td>{}</td><td>{}</td></tr>",
            html_escape(&f.rule),
            html_escape(&redact_text(&f.message))
        )?;
    }
    write!(w, "</table>")?;
    Ok(())
}

fn write_manifest_table(w: &mut impl Write, history: &[PlaylistMeta]) -> Result<()> {
    if history.is_empty() {
        write!(w, "<p>No manifest snapshots.</p>")?;
        return Ok(());
    }
    write!(w, "<table><tr><th>Seq</th><th>Target</th><th>URL</th></tr>")?;
    for m in history {
        write!(
            w,
            "<tr><td>{}</td><td>{}</td><td>{}</td></tr>",
            m.media_sequence,
            m.target_duration,
            html_escape(&redact_url(&m.url))
        )?;
    }
    write!(w, "</table>")?;
    Ok(())
}

fn write_http_table(w: &mut impl Write, log: &[HttpTransaction]) -> Result<()> {
    if log.is_empty() {
        write!(w, "<p>No HTTP transactions.</p>")?;
        return Ok(());
    }
    write!(
        w,
        "<table><tr><th>Method</th><th>Status</th><th>TTFB ms</th><th>Bytes</th><th>URL</th></tr>"
    )?;
    for t in log.iter().rev().take(50) {
        write!(
            w,
            "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
            html_escape(&t.method),
            t.status,
            t.ttfb_ms,
            t.bytes,
            html_escape(&redact_url(&t.url))
        )?;
    }
    write!(w, "</table>")?;
    Ok(())
}

fn write_dai_log_section(w: &mut impl Write, log: &[String]) -> Result<()> {
    let mut count = 0u32;
    for line in log.iter().take(200) {
        let u = line.to_ascii_uppercase();
        if u.contains("SCTE") || u.contains("MISMATCH") || u.contains("EMSG") || u.contains("[AD]")
        {
            if count == 0 {
                write!(w, "<pre>")?;
            }
            writeln!(w, "{}", html_escape(&redact_text(line)))?;
            count += 1;
        }
    }
    if count == 0 {
        write!(w, "<p>No DAI log lines.</p>")?;
    } else {
        write!(w, "</pre>")?;
    }
    Ok(())
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{AbrHealth, CdnStats, DiagnosticSummary, HealthReport, StreamSnapshot};

    fn sample_snapshot() -> StreamSnapshot {
        StreamSnapshot {
            title: "test".into(),
            summary: DiagnosticSummary {
                channel: None,
                captured_at: Utc::now(),
                source_url: "https://example.com/live.m3u8".into(),
                active_url: "https://example.com/live.m3u8".into(),
                status: "LIVE".into(),
                health_score: 90,
                health_label: "Good".into(),
                latency: "2.0s".into(),
                cdn: "CF HIT".into(),
                dvr_window: "-".into(),
                buffer: "6s".into(),
                ll_hls: false,
                dropped_events: 0,
            },
            timeline: vec!["[AD] cue".into()],
            health: HealthReport::perfect(),
            cdn: CdnStats::default(),
            abr_health: AbrHealth::default(),
            active_ad: None,
            playlist: None,
            abr_profiles: vec![],
            last_segment: None,
            findings: vec![],
            event_log: vec![],
        }
    }

    #[test]
    fn html_contains_title() {
        let html = render_html_dashboard(&sample_snapshot(), &[], &[], &[]);
        assert!(html.contains("<!DOCTYPE html>"));
        assert!(html.contains("test"));
    }

    #[test]
    fn html_escape_entities() {
        assert_eq!(html_escape("<a&>"), "&lt;a&amp;&gt;");
    }
}
