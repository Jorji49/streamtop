//! Concurrent channel audit: range-probe each entry in a lineup.

use std::fs::File;
use std::io::Write;
use std::sync::Arc;
use std::time::Instant;

use chrono::Utc;
use color_eyre::eyre::{eyre, Result, WrapErr};
use crossterm::style::{Color, ResetColor, SetForegroundColor};
use futures::stream::{self, StreamExt};
use m3u8_rs::Playlist;
use reqwest::header::RANGE;
use reqwest::Client;
use tokio::sync::Semaphore;
use url::Url;

use crate::engine::dash::{looks_like_dash, parse_dash_mpd};
use crate::engine::linter::parse_cdn_headers;
use crate::engine::playlist_parser::local_path_from_url;
use crate::engine::poller::{build_audit_http_client, collect_variants};
use crate::models::{
    AuditReport, AuditRow, AuditVerdict, ChannelEntry, AUDIT_CONCURRENCY, AUDIT_REPORT_CSV,
    AUDIT_REPORT_JSON, RANGE_PROBE_BYTES, STALL_TTFB_MS,
};

pub async fn run_audit(
    source_label: &str,
    channels: Vec<ChannelEntry>,
    headers: Vec<String>,
    user_agent: Option<String>,
) -> Result<AuditReport> {
    let client = Arc::new(build_audit_http_client(&headers, user_agent)?);
    let sem = Arc::new(Semaphore::new(AUDIT_CONCURRENCY));
    let total = channels.len();

    eprintln!(
        "streamtop audit: {total} channels, {AUDIT_CONCURRENCY} workers, Range bytes=0-{RANGE_PROBE_BYTES}"
    );

    let mut jobs = Vec::with_capacity(total);
    for (idx, ch) in channels.into_iter().enumerate() {
        let client = Arc::clone(&client);
        let sem = Arc::clone(&sem);
        jobs.push(async move {
            let _permit = sem.acquire().await.expect("semaphore");
            let row = audit_one(&client, &ch).await;
            (idx, row)
        });
    }

    let mut indexed: Vec<(usize, AuditRow)> = stream::iter(jobs)
        .buffer_unordered(AUDIT_CONCURRENCY)
        .collect()
        .await;
    indexed.sort_by_key(|(i, _)| *i);
    let rows: Vec<AuditRow> = indexed.into_iter().map(|(_, r)| r).collect();

    let live = rows
        .iter()
        .filter(|r| r.verdict == AuditVerdict::Live)
        .count();
    let errors = rows
        .iter()
        .filter(|r| r.verdict == AuditVerdict::Error)
        .count();
    let stalls = rows
        .iter()
        .filter(|r| r.verdict == AuditVerdict::Stall)
        .count();

    let report = AuditReport {
        captured_at: Utc::now(),
        source: source_label.to_string(),
        total,
        live,
        errors,
        stalls,
        channels: rows,
    };

    print_matrix(&report)?;
    write_json(&report)?;
    write_csv(&report)?;
    Ok(report)
}

async fn audit_one(client: &Client, ch: &ChannelEntry) -> AuditRow {
    match audit_one_inner(client, ch).await {
        Ok(row) => row,
        Err(err) => AuditRow {
            name: ch.name.clone(),
            group: ch.group.clone(),
            url: ch.url.clone(),
            verdict: AuditVerdict::Error,
            http_status: None,
            protocol: None,
            cdn: "—".into(),
            ttfb_ms: None,
            bitrate_profiles: Vec::new(),
            has_pdt: false,
            error: Some(format!("{err:#}")),
        },
    }
}

async fn audit_one_inner(client: &Client, ch: &ChannelEntry) -> Result<AuditRow> {
    let (body, status, content_type) = fetch_manifest(client, &ch.url).await?;
    if !(200..400).contains(&status) {
        let err = match status {
            403 => Some("403 Token / Forbidden".into()),
            404 => Some("404 Not Found".into()),
            s if s >= 500 => Some(format!("{s} Origin error")),
            s => Some(format!("HTTP {s}")),
        };
        return Ok(AuditRow {
            name: ch.name.clone(),
            group: ch.group.clone(),
            url: ch.url.clone(),
            verdict: AuditVerdict::Error,
            http_status: Some(status),
            protocol: None,
            cdn: "—".into(),
            ttfb_ms: None,
            bitrate_profiles: Vec::new(),
            has_pdt: false,
            error: err,
        });
    }

    let dash = looks_like_dash(&ch.url, &body, content_type.as_deref());
    if dash {
        return audit_dash(client, ch, &body, status).await;
    }
    audit_hls(client, ch, &body, status).await
}

async fn audit_hls(
    client: &Client,
    ch: &ChannelEntry,
    body: &[u8],
    manifest_status: u16,
) -> Result<AuditRow> {
    let playlist = m3u8_rs::parse_playlist_res(body).map_err(|e| eyre!("HLS parse: {e}"))?;
    let base = Url::parse(&ch.url).wrap_err("channel URL")?;

    let (profiles, media_url, has_pdt, empty) = match playlist {
        Playlist::MasterPlaylist(master) => {
            let variants = collect_variants(&master.variants, &base);
            let bws: Vec<u64> = variants.iter().map(|v| v.bandwidth).collect();
            let best = variants
                .iter()
                .max_by_key(|v| v.bandwidth)
                .ok_or_else(|| eyre!("master has no variants"))?;
            let media_body = fetch_bytes(client, &best.uri).await?;
            let media = m3u8_rs::parse_media_playlist_res(&media_body)
                .map_err(|e| eyre!("media parse: {e}"))?;
            let pdt = media.segments.iter().any(|s| s.program_date_time.is_some());
            let empty = media.segments.is_empty();
            let media_url = last_segment_url(&base, &best.uri, &media)?;
            (bws, media_url, pdt, empty)
        }
        Playlist::MediaPlaylist(media) => {
            let pdt = media.segments.iter().any(|s| s.program_date_time.is_some());
            let empty = media.segments.is_empty();
            let media_url = last_segment_url(&base, &ch.url, &media)?;
            (Vec::new(), media_url, pdt, empty)
        }
    };

    if empty {
        return Ok(AuditRow {
            name: ch.name.clone(),
            group: ch.group.clone(),
            url: ch.url.clone(),
            verdict: AuditVerdict::Stall,
            http_status: Some(manifest_status),
            protocol: Some("HLS".into()),
            cdn: "—".into(),
            ttfb_ms: None,
            bitrate_profiles: profiles,
            has_pdt,
            error: Some("empty media playlist (possible origin stall)".into()),
        });
    }

    let (probe_status, ttfb, cdn) = range_probe(client, &media_url).await?;
    Ok(AuditRow {
        name: ch.name.clone(),
        group: ch.group.clone(),
        url: ch.url.clone(),
        verdict: classify_probe(probe_status, ttfb),
        http_status: Some(normalize_status(probe_status.max(manifest_status))),
        protocol: Some("HLS".into()),
        cdn,
        ttfb_ms: Some(ttfb),
        bitrate_profiles: profiles,
        has_pdt,
        error: token_error(probe_status),
    })
}

async fn audit_dash(
    client: &Client,
    ch: &ChannelEntry,
    body: &[u8],
    manifest_status: u16,
) -> Result<AuditRow> {
    let xml = String::from_utf8_lossy(body);
    let base = Url::parse(&ch.url)?;
    let summary = parse_dash_mpd(&xml, &base)?;
    let profiles: Vec<u64> = summary.variants.iter().map(|v| v.bandwidth).collect();
    let probe = summary
        .probe_url
        .or_else(|| summary.variants.first().map(|v| v.uri.clone()))
        .ok_or_else(|| eyre!("DASH has no probe URL"))?;
    let (probe_status, ttfb, cdn) = range_probe(client, &probe).await?;
    Ok(AuditRow {
        name: ch.name.clone(),
        group: ch.group.clone(),
        url: ch.url.clone(),
        verdict: classify_probe(probe_status, ttfb),
        http_status: Some(normalize_status(probe_status.max(manifest_status))),
        protocol: Some("DASH".into()),
        cdn,
        ttfb_ms: Some(ttfb),
        bitrate_profiles: profiles,
        has_pdt: summary.availability_start_time.is_some(),
        error: token_error(probe_status),
    })
}

fn last_segment_url(
    playlist_base: &Url,
    playlist_url: &str,
    media: &m3u8_rs::MediaPlaylist,
) -> Result<String> {
    let seg = media.segments.last().ok_or_else(|| eyre!("no segments"))?;
    let base = Url::parse(playlist_url).unwrap_or_else(|_| playlist_base.clone());
    if let Ok(u) = Url::parse(&seg.uri) {
        return Ok(u.to_string());
    }
    Ok(base
        .join(&seg.uri)
        .map(|u| u.to_string())
        .unwrap_or_else(|_| seg.uri.clone()))
}

fn normalize_status(status: u16) -> u16 {
    if status == 206 {
        200
    } else {
        status
    }
}

fn classify_probe(status: u16, ttfb_ms: u64) -> AuditVerdict {
    if status == 403 || status == 401 || status == 404 || status >= 500 {
        return AuditVerdict::Error;
    }
    if !(200..400).contains(&status) {
        return AuditVerdict::Error;
    }
    if ttfb_ms >= STALL_TTFB_MS {
        AuditVerdict::Stall
    } else {
        AuditVerdict::Live
    }
}

fn token_error(status: u16) -> Option<String> {
    match status {
        401 => Some("401 Unauthorized".into()),
        403 => Some("403 Token / Forbidden".into()),
        404 => Some("404 Not Found".into()),
        s if s >= 500 => Some(format!("{s} Origin error")),
        _ => None,
    }
}

async fn fetch_manifest(client: &Client, url: &str) -> Result<(Vec<u8>, u16, Option<String>)> {
    if let Some(path) = local_path_from_url(url) {
        let body = tokio::fs::read(&path)
            .await
            .wrap_err_with(|| format!("read {}", path.display()))?;
        return Ok((body, 200, None));
    }
    let response = client
        .get(url)
        .send()
        .await
        .wrap_err_with(|| format!("GET {url}"))?;
    let status = response.status().as_u16();
    let ct = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let body = response.bytes().await.unwrap_or_default().to_vec();
    Ok((body, status, ct))
}

async fn fetch_bytes(client: &Client, url: &str) -> Result<Vec<u8>> {
    Ok(fetch_manifest(client, url).await?.0)
}

async fn range_probe(client: &Client, url: &str) -> Result<(u16, u64, String)> {
    if let Some(path) = local_path_from_url(url) {
        let started = Instant::now();
        let data = tokio::fs::read(&path).await.unwrap_or_default();
        let n = data
            .len()
            .min((RANGE_PROBE_BYTES as usize).saturating_add(1));
        let _ = &data[..n];
        let ttfb = started.elapsed().as_millis() as u64;
        return Ok((200, ttfb, "Local · ORIGIN?".into()));
    }

    let started = Instant::now();
    let range = format!("bytes=0-{RANGE_PROBE_BYTES}");
    let response = client
        .get(url)
        .header(RANGE, range)
        .send()
        .await
        .wrap_err_with(|| format!("probe {url}"))?;
    let status = response.status().as_u16();
    let ttfb = started.elapsed().as_millis() as u64;
    let cdn = parse_cdn_headers(response.headers()).badge();
    let _ = response.bytes().await;
    Ok((status, ttfb, cdn))
}

fn print_matrix(report: &AuditReport) -> Result<()> {
    println!();
    color_line(
        Color::Cyan,
        &format!(
            "Audit  |  {}  |  {}",
            report.source,
            report.captured_at.to_rfc3339()
        ),
    )?;
    println!(
        "{:<4} {:<28} {:<12} {:<6} {:<6} {:>7} {:<22} Verdict",
        "#", "Channel", "Group", "HTTP", "PDT", "TTFB", "CDN"
    );
    println!("{}", "-".repeat(100));

    for (i, row) in report.channels.iter().enumerate() {
        let http = row
            .http_status
            .map(|s| s.to_string())
            .unwrap_or_else(|| "—".into());
        let pdt = if row.has_pdt { "yes" } else { "no" };
        let ttfb = row
            .ttfb_ms
            .map(|ms| format!("{ms}ms"))
            .unwrap_or_else(|| "—".into());
        let name = truncate(&row.name, 28);
        let group = truncate(row.group.as_deref().unwrap_or("—"), 12);
        let cdn = truncate(&row.cdn, 22);
        let color = match row.verdict {
            AuditVerdict::Live => Color::Green,
            AuditVerdict::Stall => Color::Yellow,
            AuditVerdict::Error => Color::Red,
        };
        print!(
            "{:<4} {:<28} {:<12} {:<6} {:<6} {:>7} {:<22} ",
            i + 1,
            name,
            group,
            http,
            pdt,
            ttfb,
            cdn
        );
        color_line(color, row.verdict.as_str())?;
        if let Some(err) = &row.error {
            if row.verdict != AuditVerdict::Live {
                println!("     {err}");
            }
        }
    }

    println!();
    print!("Total {} channels: ", report.total);
    color_span(Color::Green, &format!("{} Live", report.live))?;
    print!(", ");
    color_span(Color::Red, &format!("{} Error", report.errors))?;
    print!(", ");
    color_span(Color::Yellow, &format!("{} Stall", report.stalls))?;
    println!();
    println!("Wrote {AUDIT_REPORT_JSON} and {AUDIT_REPORT_CSV}");
    Ok(())
}

fn color_line(color: Color, text: &str) -> Result<()> {
    execute_color(color)?;
    println!("{text}");
    reset_color()?;
    Ok(())
}

fn color_span(color: Color, text: &str) -> Result<()> {
    execute_color(color)?;
    print!("{text}");
    reset_color()?;
    Ok(())
}

fn execute_color(color: Color) -> Result<()> {
    let mut out = std::io::stdout();
    crossterm::execute!(out, SetForegroundColor(color))?;
    Ok(())
}

fn reset_color() -> Result<()> {
    let mut out = std::io::stdout();
    crossterm::execute!(out, ResetColor)?;
    Ok(())
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let t: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{t}…")
    }
}

fn write_json(report: &AuditReport) -> Result<()> {
    let json = serde_json::to_string_pretty(report).wrap_err("audit json")?;
    std::fs::write(AUDIT_REPORT_JSON, json)
        .wrap_err_with(|| format!("write {AUDIT_REPORT_JSON}"))?;
    Ok(())
}

fn write_csv(report: &AuditReport) -> Result<()> {
    let mut f = File::create(AUDIT_REPORT_CSV).wrap_err("audit csv")?;
    writeln!(
        f,
        "name,group,url,verdict,http_status,protocol,cdn,ttfb_ms,bitrate_profiles,has_pdt,error"
    )?;
    for row in &report.channels {
        let bws = row
            .bitrate_profiles
            .iter()
            .map(|b| b.to_string())
            .collect::<Vec<_>>()
            .join("|");
        writeln!(
            f,
            "{},{},{},{},{},{},{},{},{},{},{}",
            csv_escape(&row.name),
            csv_escape(row.group.as_deref().unwrap_or("")),
            csv_escape(&row.url),
            row.verdict.as_str(),
            row.http_status.map(|s| s.to_string()).unwrap_or_default(),
            row.protocol.as_deref().unwrap_or(""),
            csv_escape(&row.cdn),
            row.ttfb_ms.map(|s| s.to_string()).unwrap_or_default(),
            csv_escape(&bws),
            row.has_pdt,
            csv_escape(row.error.as_deref().unwrap_or("")),
        )?;
    }
    Ok(())
}

fn csv_escape(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}
