//! Concurrent multi-CDN edge skew analyzer (`--multi-cdn`).

use std::sync::{Arc, RwLock};
use std::time::Duration;

use color_eyre::eyre::{eyre, Result, WrapErr};
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio::time::{timeout, Instant};

use crate::engine::metrics::{update_metrics, MetricsSnapshot};
use crate::engine::poller::ManifestPoller;
use crate::engine::session_poller::apply_session_doh;
use crate::models::{
    DiagCategory, DiagSeverity, DiagnosticFinding, DiagnosticReasonCode, MultiCdnEdgeSnapshot,
    MultiCdnSkewReport, PlaylistMeta, SegmentMetrics, StreamEvent, EVENT_CHANNEL_CAPACITY,
};
use crate::ui::app::SessionOpts;

#[derive(Debug, Clone)]
pub struct MultiCdnTarget {
    pub label: String,
    pub url: String,
}

#[derive(Debug, Clone, Default)]
struct EdgeState {
    label: String,
    url: String,
    media_sequence: Option<u64>,
    pdt_offset_ms: Option<i64>,
    segment_delay_ms: Option<u64>,
    cdn_hits: u64,
    cdn_misses: u64,
    ttfb_ms: Option<u64>,
    variant_count: usize,
    target_duration: Option<u64>,
}

/// Parse `--multi-cdn URL1,URL2,...` or `label=URL` pairs.
pub fn parse_multi_cdn(raw: &str) -> Result<Vec<MultiCdnTarget>> {
    let mut out = Vec::new();
    for (idx, part) in raw
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .enumerate()
    {
        if let Some((label, url)) = part.split_once('=') {
            out.push(MultiCdnTarget {
                label: label.trim().to_string(),
                url: url.trim().to_string(),
            });
        } else {
            out.push(MultiCdnTarget {
                label: format!("edge{idx}"),
                url: part.to_string(),
            });
        }
    }
    if out.len() < 2 {
        return Err(eyre!("--multi-cdn requires at least two URLs"));
    }
    Ok(out)
}

fn compute_skew(edges: &[EdgeState]) -> MultiCdnSkewReport {
    let snapshots: Vec<MultiCdnEdgeSnapshot> = edges
        .iter()
        .map(|e| MultiCdnEdgeSnapshot {
            label: e.label.clone(),
            url: e.url.clone(),
            media_sequence: e.media_sequence,
            pdt_offset_ms: e.pdt_offset_ms,
            segment_delay_ms: e.segment_delay_ms,
            cdn_hits: e.cdn_hits,
            cdn_misses: e.cdn_misses,
            ttfb_ms: e.ttfb_ms,
            http_version: None,
        })
        .collect();
    compute_skew_from_snapshots(&snapshots)
}

/// Skew report from edge snapshots (shared by TUI and headless analyzer).
pub fn compute_skew_from_snapshots(snapshots: &[MultiCdnEdgeSnapshot]) -> MultiCdnSkewReport {
    let seqs: Vec<u64> = snapshots.iter().filter_map(|e| e.media_sequence).collect();
    let mut max_skew_ms = if seqs.len() >= 2 {
        let min = seqs.iter().copied().min().unwrap_or(0);
        let max = seqs.iter().copied().max().unwrap_or(0);
        ((max.saturating_sub(min)) as i64).saturating_mul(1000)
    } else {
        0
    };

    let pdt_vals: Vec<i64> = snapshots.iter().filter_map(|e| e.pdt_offset_ms).collect();
    let pdt_skew = if pdt_vals.len() >= 2 {
        pdt_vals.iter().max().copied().unwrap_or(0) - pdt_vals.iter().min().copied().unwrap_or(0)
    } else {
        0
    };
    max_skew_ms = max_skew_ms.max(pdt_skew);

    let delays: Vec<u64> = snapshots
        .iter()
        .filter_map(|e| e.segment_delay_ms)
        .collect();
    let propagation_latency_ms = if delays.len() >= 2 {
        let min = delays.iter().copied().min().unwrap_or(0);
        let max = delays.iter().copied().max().unwrap_or(0);
        Some(max.saturating_sub(min) as i64)
    } else {
        None
    };

    MultiCdnSkewReport {
        edges: snapshots.to_vec(),
        max_skew_ms,
        propagation_latency_ms,
        manifest_desync: None,
    }
}

fn apply_event(state: &mut EdgeState, event: StreamEvent) {
    match event {
        StreamEvent::PlaylistMeta(PlaylistMeta {
            media_sequence,
            target_duration,
            ..
        }) => {
            state.media_sequence = Some(media_sequence);
            state.target_duration = Some(target_duration);
        }
        StreamEvent::Segment(SegmentMetrics {
            media_sequence,
            ttfb_ms,
            ..
        }) => {
            state.media_sequence = Some(media_sequence);
            state.ttfb_ms = Some(ttfb_ms);
            state.segment_delay_ms = Some(ttfb_ms);
        }
        StreamEvent::CdnStats(c) => {
            state.cdn_hits = c.hits;
            state.cdn_misses = c.misses;
        }
        StreamEvent::Variants(v) => {
            state.variant_count = v.len();
        }
        StreamEvent::Latency(crate::models::LatencyState::Measured(ms)) => {
            state.pdt_offset_ms = Some(ms as i64);
        }
        _ => {}
    }
}

/// Poll all CDN edges concurrently for `duration`, return skew report.
pub async fn analyze_multi_cdn(
    targets: &[MultiCdnTarget],
    session: &SessionOpts,
    duration: Duration,
    max_skew_ms: i64,
) -> Result<(MultiCdnSkewReport, Vec<DiagnosticFinding>)> {
    let mut join = JoinSet::new();
    let deadline = Instant::now() + duration;

    for target in targets {
        let label = target.label.clone();
        let url = target.url.clone();
        let session = session.clone();
        join.spawn(async move {
            let (tx, mut rx) = mpsc::channel(EVENT_CHANNEL_CAPACITY);
            let metrics = Arc::new(RwLock::new(MetricsSnapshot::default()));
            let poller = ManifestPoller::new(
                &url,
                &session.headers,
                session.user_agent.as_deref(),
                session.interval_ms,
                session.probe_headers,
                session.probe_drm,
                tx,
            )
            .wrap_err_with(|| format!("multi-cdn poller init failed: {url}"))?
            .with_metrics(metrics.clone());
            let poller = apply_session_doh(poller, &session)?;
            let handle = tokio::spawn(async move { poller.run().await });
            let mut state = EdgeState {
                label,
                url,
                ..EdgeState::default()
            };
            let remaining = deadline.saturating_duration_since(Instant::now());
            let _ = timeout(remaining, async {
                while let Some(ev) = rx.recv().await {
                    if let Ok(mut snap) = metrics.write() {
                        update_metrics(&mut snap, &ev);
                    }
                    apply_event(&mut state, ev);
                }
            })
            .await;
            handle.abort();
            Ok::<EdgeState, color_eyre::Report>(state)
        });
    }

    let mut edges = Vec::new();
    while let Some(res) = join.join_next().await {
        match res {
            Ok(Ok(state)) => edges.push(state),
            Ok(Err(e)) => return Err(e),
            Err(e) => return Err(e.into()),
        }
    }

    let report = compute_skew(&edges);
    let mut findings = Vec::new();
    if report.max_skew_ms > max_skew_ms {
        findings.push(DiagnosticFinding {
            category: DiagCategory::Cdn,
            severity: DiagSeverity::Error,
            rule: "cdn_sync_skew".into(),
            message: format!(
                "CDN live-edge skew {}ms exceeds threshold {}ms",
                report.max_skew_ms, max_skew_ms
            ),
            reason: Some(DiagnosticReasonCode::ErrCdnSyncSkew.as_str().into()),
        });
    }
    if let Some(desc) = &report.manifest_desync {
        findings.push(DiagnosticFinding {
            category: DiagCategory::Cdn,
            severity: DiagSeverity::Warn,
            rule: "cdn_manifest_desync".into(),
            message: desc.clone(),
            reason: None,
        });
    }
    Ok((report, findings))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_multi_cdn_urls() {
        let t = parse_multi_cdn("https://a.example/live.m3u8,https://b.example/live.m3u8").unwrap();
        assert_eq!(t.len(), 2);
        assert_eq!(t[0].label, "edge0");
    }

    #[test]
    fn parse_multi_cdn_labeled() {
        let t =
            parse_multi_cdn("akamai=https://a.example/x,cloudflare=https://b.example/x").unwrap();
        assert_eq!(t[0].label, "akamai");
        assert_eq!(t[1].label, "cloudflare");
    }

    #[test]
    fn skew_from_sequences() {
        let edges = vec![
            EdgeState {
                media_sequence: Some(100),
                pdt_offset_ms: Some(5000),
                ..EdgeState::default()
            },
            EdgeState {
                media_sequence: Some(103),
                pdt_offset_ms: Some(8000),
                ..EdgeState::default()
            },
        ];
        let r = compute_skew(&edges);
        assert_eq!(r.max_skew_ms, 3000);
    }
}
