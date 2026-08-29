//! VOD inspection: one-shot playlist/MPD tree crawl without live polling.

use std::process::ExitCode;
use std::sync::Arc;
use std::time::Instant;

use color_eyre::eyre::{eyre, Result, WrapErr};
use crossterm::style::{Color, ResetColor, SetForegroundColor};
use futures::stream::{self, StreamExt};
use m3u8_rs::{MediaPlaylist, Playlist};
use reqwest::Client;
use tokio::sync::Semaphore;
use url::Url;

use crate::engine::channel_stats::channel_dropped_total;
use crate::engine::dash::{looks_like_dash, parse_dash_mpd};
use crate::engine::linter::analyze_abr_ladder;
use crate::engine::poller::{build_http_client, collect_variants};
use crate::engine::summary::{build_summary_json, SummaryFormat};
use crate::models::{AbrVariant, HealthReport, LatencyState, AUDIT_CONCURRENCY, RANGE_PROBE_BYTES};
use crate::ui::app::SessionOpts;

const VOD_PROBE_BYTES: u64 = RANGE_PROBE_BYTES;

#[derive(Debug)]
struct VodReport {
    saw_segment: bool,
    errors: u32,
    issues: Vec<String>,
    variants_checked: u32,
    health: HealthReport,
    cdn_badge: String,
    last_ttfb: Option<u64>,
    last_http_status: Option<u16>,
}

impl Default for VodReport {
    fn default() -> Self {
        Self {
            saw_segment: false,
            errors: 0,
            issues: Vec::new(),
            variants_checked: 0,
            health: HealthReport::perfect(),
            cdn_badge: String::new(),
            last_ttfb: None,
            last_http_status: None,
        }
    }
}

pub async fn run_vod(url: String, session: SessionOpts, format: SummaryFormat) -> Result<ExitCode> {
    let client = Arc::new(build_http_client(
        &session.headers,
        session.user_agent.clone(),
    )?);
    let base = Url::parse(&url).wrap_err("invalid VOD URL")?;
    let (body, ct) = fetch_bytes(&client, &url).await?;
    let mut report = VodReport {
        health: HealthReport::perfect(),
        ..Default::default()
    };

    if looks_like_dash(&url, &body, ct.as_deref()) {
        inspect_dash_vod(&client, &base, &body, &mut report).await?;
    } else {
        inspect_hls_vod(&client, &base, &body, &mut report).await?;
    }

    let ok = report.saw_segment && report.errors == 0 && report.health.score >= 70;
    let status_label = if report.errors > 0 {
        "ERROR"
    } else if report.health.score < 85 {
        "DEGRADED"
    } else {
        "LIVE"
    };

    match format {
        SummaryFormat::Json => {
            let doc = build_summary_json(
                url,
                ok,
                &report.health,
                status_label,
                &LatencyState::Unknown,
                report.cdn_badge,
                report.last_ttfb,
                report.last_http_status,
                0,
                report.issues.len() as u32,
                report.errors,
                report.saw_segment,
                channel_dropped_total(),
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
            );
            println!("{}", serde_json::to_string_pretty(&doc)?);
        }
        SummaryFormat::Text => {
            print_vod_text(&url, ok, &report, status_label);
        }
    }

    Ok(if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

async fn inspect_hls_vod(
    client: &Arc<Client>,
    base: &Url,
    body: &[u8],
    report: &mut VodReport,
) -> Result<()> {
    let playlist = m3u8_rs::parse_playlist_res(body).map_err(|e| eyre!("HLS parse error: {e}"))?;

    match playlist {
        Playlist::MasterPlaylist(master) => {
            let variants = collect_variants(&master.variants, base);
            validate_ladder(&variants, report);
            probe_variants(client, variants, report).await?;
        }
        Playlist::MediaPlaylist(media) => {
            validate_media_playlist(&media, report);
            if let Some(seg) = media.segments.last() {
                let seg_url = base.join(&seg.uri).wrap_err("segment URL join")?;
                probe_url(client, seg_url.as_str(), report).await?;
            } else {
                report.errors += 1;
                report
                    .issues
                    .push("VOD media playlist has no segments".into());
            }
        }
    }
    Ok(())
}

async fn inspect_dash_vod(
    client: &Arc<Client>,
    base: &Url,
    body: &[u8],
    report: &mut VodReport,
) -> Result<()> {
    let xml = String::from_utf8_lossy(body);
    let summary = parse_dash_mpd(&xml, base)?;
    validate_ladder(&summary.variants, report);
    if summary.ll_dash.is_ll_dash {
        report
            .issues
            .push("LL-DASH MPD detected on VOD asset".into());
    }
    let urls: Vec<String> = summary.variants.iter().map(|v| v.uri.clone()).collect();
    probe_urls(client, urls, report).await?;
    Ok(())
}

fn validate_ladder(variants: &[AbrVariant], report: &mut VodReport) {
    if variants.is_empty() {
        report.errors += 1;
        report.issues.push("No ABR variants/representations".into());
        return;
    }
    let abr = analyze_abr_ladder(variants);
    for w in abr.warnings {
        report.issues.push(w);
        report.health.score = report
            .health
            .score
            .saturating_sub(abr.score_penalty.min(10));
    }
    let mut codecs = std::collections::HashSet::new();
    for v in variants {
        if let Some(c) = &v.codecs {
            codecs.insert(normalize_codec(c));
        }
    }
    if codecs.len() > 2 {
        report.issues.push(format!(
            "Codec mismatch across ladder: {}",
            codecs.iter().cloned().collect::<Vec<_>>().join(", ")
        ));
        report.health.score = report.health.score.saturating_sub(8);
    }
}

fn validate_media_playlist(media: &MediaPlaylist, report: &mut VodReport) {
    if media.segments.is_empty() {
        report.errors += 1;
        report.issues.push("Empty media playlist".into());
        return;
    }
    let first_dur = media.segments[0].duration;
    let mut uniform = true;
    for seg in &media.segments[1..] {
        if (seg.duration - first_dur).abs() > 0.05 {
            uniform = false;
            break;
        }
    }
    if !uniform {
        report
            .issues
            .push("Non-uniform EXTINF durations in VOD playlist".into());
        report.health.score = report.health.score.saturating_sub(5);
    }
    if media.segments.len() < 2 {
        report.issues.push("Single-segment VOD playlist".into());
    }
}

async fn probe_variants(
    client: &Arc<Client>,
    variants: Vec<AbrVariant>,
    report: &mut VodReport,
) -> Result<()> {
    let urls: Vec<String> = variants.into_iter().map(|v| v.uri).collect();
    probe_urls(client, urls, report).await
}

async fn probe_urls(client: &Arc<Client>, urls: Vec<String>, report: &mut VodReport) -> Result<()> {
    let sem = Arc::new(Semaphore::new(AUDIT_CONCURRENCY));
    let jobs: Vec<_> = urls
        .into_iter()
        .map(|u| {
            let client = Arc::clone(client);
            let sem = Arc::clone(&sem);
            async move {
                let Ok(_permit) = sem.acquire().await else {
                    return Err("worker interrupted".into());
                };
                range_probe(client, &u).await
            }
        })
        .collect();

    let results = stream::iter(jobs)
        .buffer_unordered(AUDIT_CONCURRENCY)
        .collect::<Vec<_>>()
        .await;

    for res in results {
        report.variants_checked += 1;
        match res {
            Ok((ttfb, status, cdn)) => {
                report.saw_segment = true;
                report.last_ttfb = Some(ttfb);
                report.last_http_status = Some(status);
                if !report.cdn_badge.is_empty() && report.cdn_badge != cdn {
                    report
                        .issues
                        .push("CDN badge differs across representations".into());
                }
                if report.cdn_badge.is_empty() {
                    report.cdn_badge = cdn;
                }
            }
            Err(e) => {
                report.errors += 1;
                report.issues.push(e);
                report.health.score = report.health.score.saturating_sub(15);
            }
        }
    }
    Ok(())
}

async fn probe_url(client: &Arc<Client>, url: &str, report: &mut VodReport) -> Result<()> {
    report.variants_checked += 1;
    match range_probe(Arc::clone(client), url).await {
        Ok((ttfb, status, cdn)) => {
            report.saw_segment = true;
            report.last_ttfb = Some(ttfb);
            report.last_http_status = Some(status);
            report.cdn_badge = cdn;
        }
        Err(e) => {
            report.errors += 1;
            report.issues.push(e);
            report.health.score = report.health.score.saturating_sub(20);
        }
    }
    Ok(())
}

async fn range_probe(client: Arc<Client>, url: &str) -> Result<(u64, u16, String), String> {
    let started = Instant::now();
    let response = client
        .get(url)
        .header(reqwest::header::RANGE, format!("bytes=0-{VOD_PROBE_BYTES}"))
        .timeout(std::time::Duration::from_secs(20))
        .send()
        .await
        .map_err(|e| format!("probe failed {url}: {e}"))?;
    let status = response.status().as_u16();
    if !(status == 200 || status == 206) {
        return Err(format!("HTTP {status} for {url}"));
    }
    let ttfb = started.elapsed().as_millis() as u64;
    let cdn = crate::engine::linter::parse_cdn_headers(response.headers()).badge();
    let _ = response
        .bytes()
        .await
        .map_err(|e| format!("read body: {e}"))?;
    Ok((ttfb, status, cdn))
}

async fn fetch_bytes(client: &Client, url: &str) -> Result<(Vec<u8>, Option<String>)> {
    let response = client
        .get(url)
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        .wrap_err_with(|| format!("GET failed: {url}"))?;
    if !response.status().is_success() {
        return Err(eyre!("HTTP {} for {url}", response.status()));
    }
    let ct = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let body = read_vod_body_limited(response).await?;
    Ok((body, ct))
}

async fn read_vod_body_limited(response: reqwest::Response) -> Result<Vec<u8>> {
    use futures::StreamExt;
    let mut stream = response.bytes_stream();
    let mut buf = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.wrap_err("vod body read")?;
        if buf.len().saturating_add(chunk.len()) > crate::models::MAX_MANIFEST_BYTES {
            return Err(eyre!(
                "VOD response exceeds {} byte limit",
                crate::models::MAX_MANIFEST_BYTES
            ));
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(buf)
}

fn normalize_codec(c: &str) -> String {
    let lower = c.to_ascii_lowercase();
    if lower.contains("avc") || lower.contains("h264") {
        "avc".into()
    } else if lower.contains("hvc") || lower.contains("hev") || lower.contains("h265") {
        "hevc".into()
    } else if lower.contains("mp4a") || lower.contains("aac") {
        "aac".into()
    } else {
        lower.split('.').next().unwrap_or(&lower).into()
    }
}

fn print_vod_text(url: &str, ok: bool, report: &VodReport, status: &str) {
    let verdict = if ok { "PASS" } else { "FAIL" };
    let color = if ok { Color::Green } else { Color::Red };
    let mut out = std::io::stdout();
    let _ = crossterm::execute!(out, SetForegroundColor(color));
    println!(
        "VOD {verdict} | {status} | health={} ({}) | checked={} | errors={}",
        report.health.score, report.health.label, report.variants_checked, report.errors
    );
    let _ = crossterm::execute!(out, ResetColor);
    println!("url: {url}");
    for issue in &report.issues {
        println!("  - {issue}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_codec_groups_avc() {
        assert_eq!(normalize_codec("avc1.4d401f"), "avc");
        assert_eq!(normalize_codec("hvc1.1.6.L93.B0"), "hevc");
    }
}
