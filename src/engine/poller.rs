use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use chrono::Utc;
use color_eyre::eyre::{eyre, Result, WrapErr};
use futures::StreamExt;
use m3u8_rs::{AlternativeMediaType, Playlist, VariantStream};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, RANGE};
use reqwest::Client;
use tokio::sync::mpsc::Sender;
use tokio::time::sleep;
use url::Url;

use crate::engine::container_probe::{
    deep_wire_probe, fill_abr_from_wire, manifest_wire_mismatches,
};
use crate::engine::dash::{looks_like_dash, parse_dash_mpd};
use crate::engine::linter::{
    ad_log_key, analyze_abr_ladder, apply_abr_penalty, apply_hls_blocking_params,
    extract_ad_signals_near_live_edge, inspect_container, ll_hls_probe_range,
    next_blocking_targets, parse_cdn_headers, scan_drm_keys, scan_ll_hls, scan_media_renditions,
    SpecLinter,
};
use crate::engine::metrics::{update_metrics, MetricsSnapshot};
use crate::engine::network_trace::{parse_header_pairs, timing_from_ttfb, traced_get};
use crate::engine::playlist_parser::{is_iptv_channel_list, local_path_from_url};
use crate::models::{
    AbrVariant, CdnEdgeInfo, ContainerKind, DiagCategory, DiagSeverity, LatencyState, LogLevel,
    NetworkTiming, PlaylistMeta, SegmentMetrics, StreamEvent, StreamProtocol, StreamStatus,
    VirtualBuffer, WireProbeInfo, AD_SCAN_LIVE_EDGE_SEGMENTS, DEEP_WIRE_PROBE_BYTES,
    HLS_LIVE_EDGE_SEGMENTS, MEDIA_SEQ_GAP_TOLERANCE,
};

const DEFAULT_UA: &str = concat!("streamtop/", env!("CARGO_PKG_VERSION"));

#[derive(Debug, Default)]
struct LlHlsBlockingState {
    is_ll_hls: bool,
    can_block_reload: bool,
    blocking_msn: Option<u64>,
    blocking_part: Option<u64>,
    part_interval_ms: Option<u64>,
}

pub struct ManifestPoller {
    client: Client,
    source_url: Url,
    interval: Option<Duration>,
    probe_headers: bool,
    extra_headers: Vec<(String, String)>,
    tx: Sender<StreamEvent>,
    hook_tx: Option<Sender<StreamEvent>>,
    metrics: Option<Arc<RwLock<MetricsSnapshot>>>,
}

struct SegmentFetch {
    size_bytes: u64,
    transferred_bytes: u64,
    ttfb_ms: u64,
    download_ms: u64,
    cdn: CdnEdgeInfo,
    container: ContainerKind,
    probed: bool,
    http_status: u16,
    network: NetworkTiming,
    wire: WireProbeInfo,
}

impl ManifestPoller {
    pub fn new(
        source_url: String,
        headers: Vec<String>,
        user_agent: Option<String>,
        interval_ms: Option<u64>,
        probe_headers: bool,
        tx: Sender<StreamEvent>,
    ) -> Result<Self> {
        let source_url = Url::parse(&source_url).wrap_err("invalid stream URL")?;
        let extra_headers = parse_header_pairs(&headers);
        let client = build_http_client(&headers, user_agent)?;
        let interval = interval_ms.map(Duration::from_millis);

        Ok(Self {
            client,
            source_url,
            interval,
            probe_headers,
            extra_headers,
            tx,
            hook_tx: None,
            metrics: None,
        })
    }

    pub fn with_metrics(mut self, metrics: Arc<RwLock<MetricsSnapshot>>) -> Self {
        self.metrics = Some(metrics);
        self
    }

    pub fn with_webhook_tx(mut self, hook_tx: Sender<StreamEvent>) -> Self {
        self.hook_tx = Some(hook_tx);
        self
    }

    fn send_event(&self, event: StreamEvent) {
        if let Some(m) = &self.metrics {
            if let Ok(mut snap) = m.write() {
                update_metrics(&mut snap, &event);
            }
        }
        // Bounded: drop when UI/webhook cannot keep up (prefer liveness over backlog).
        let _ = self.tx.try_send(event.clone());
        if let Some(h) = &self.hook_tx {
            let _ = h.try_send(event);
        }
    }

    pub async fn run(self) {
        self.send_event(StreamEvent::Status(StreamStatus::live("Polling…")));
        self.emit_log(
            LogLevel::Info,
            DiagCategory::Info,
            format!("Polling started: {}", self.source_url),
        );
        if self.probe_headers {
            self.emit_log(
                LogLevel::Info,
                DiagCategory::Info,
                format!("Range probe enabled (bytes=0-{DEEP_WIRE_PROBE_BYTES})"),
            );
            self.send_event(StreamEvent::ProbeMode(true));
        }

        match self.detect_protocol().await {
            Ok(proto) => {
                self.emit_log(
                    LogLevel::Info,
                    DiagCategory::Info,
                    format!("Protocol: {}", proto.as_str()),
                );
                match proto {
                    StreamProtocol::Dash => self.run_dash_loop().await,
                    StreamProtocol::Hls => self.run_hls_loop().await,
                }
            }
            Err(err) => {
                let msg = format!("{err:#}");
                self.send_event(StreamEvent::Error(msg.clone()));
                self.emit_log(LogLevel::Error, DiagCategory::Info, msg.clone());
                self.send_event(StreamEvent::Status(StreamStatus::error(msg)));
            }
        }
    }

    async fn detect_protocol(&self) -> Result<StreamProtocol> {
        let (body, content_type) = self.fetch_manifest(self.source_url.as_str()).await?;
        let text = String::from_utf8_lossy(&body);

        if is_iptv_channel_list(&text) {
            return Err(eyre!(
                "IPTV channel list detected (#EXTINF without TARGETDURATION/MEDIA-SEQUENCE). \
                 Open the URL in Channel Picker mode (or use --audit), not as a single stream."
            ));
        }

        if looks_like_dash(self.source_url.as_str(), &body, content_type.as_deref())
            || text.contains("<MPD")
            || text.contains("<mpd")
        {
            return Ok(StreamProtocol::Dash);
        }

        if text.contains("#EXT-X-STREAM-INF")
            || text.contains("#EXT-X-TARGETDURATION")
            || text.contains("#EXT-X-MEDIA-SEQUENCE")
            || m3u8_rs::parse_playlist_res(&body).is_ok()
        {
            return Ok(StreamProtocol::Hls);
        }

        let lower = self.source_url.as_str().to_ascii_lowercase();
        if lower.contains(".mpd") {
            return Ok(StreamProtocol::Dash);
        }
        if lower.contains(".m3u8") || lower.contains("m3u8") || lower.contains(".m3u") {
            return Ok(StreamProtocol::Hls);
        }
        Ok(StreamProtocol::Hls)
    }

    async fn run_dash_loop(self) {
        let mut consecutive_errors: u32 = 0;
        let mut linter = SpecLinter::new();
        let mut abr_health = crate::models::AbrHealth::default();
        let mut vbuf = VirtualBuffer::default();
        let mut buffer_clock = Instant::now();
        let mut last_publish: Option<String> = None;
        let mut probe_seq: u64 = 0;
        let mut announced_audio = false;
        let mut announced_ast = false;
        let mut target_duration: u64 = 2;
        let mut last_mup: Option<f64> = None;
        let mut last_period_id: Option<String> = None;

        loop {
            let now = Instant::now();
            let elapsed = now.duration_since(buffer_clock).as_secs_f64();
            buffer_clock = now;
            vbuf.drain_elapsed(elapsed);
            self.send_event(StreamEvent::Buffer(vbuf));

            if target_duration > 0 {
                linter.check_stalling(target_duration, now);
            }

            match self
                .poll_dash_once(
                    &mut linter,
                    &mut abr_health,
                    &mut vbuf,
                    &mut buffer_clock,
                    &mut last_publish,
                    &mut probe_seq,
                    &mut announced_audio,
                    &mut announced_ast,
                    &mut last_period_id,
                )
                .await
            {
                Ok((td, mup)) => {
                    target_duration = td;
                    if let Some(m) = mup {
                        last_mup = Some(m);
                    }
                    consecutive_errors = 0;
                    self.send_event(StreamEvent::Status(StreamStatus::live("Live")));
                }
                Err(err) => {
                    consecutive_errors = consecutive_errors.saturating_add(1);
                    let msg = format!("{err:#}");
                    self.send_event(StreamEvent::Error(msg.clone()));
                    self.emit_log(LogLevel::Error, DiagCategory::Info, msg.clone());
                    let status = if consecutive_errors >= 3 {
                        StreamStatus::error(msg)
                    } else {
                        StreamStatus::degraded(msg)
                    };
                    self.send_event(StreamEvent::Status(status));
                }
            }

            if !abr_health.warnings.is_empty() {
                self.send_event(StreamEvent::AbrHealth(abr_health.clone()));
            }

            self.flush_findings(&mut linter);
            linter.clear_rfc_flag_if_clean();
            let health = apply_abr_penalty(linter.compute_health(), &abr_health);
            self.send_event(StreamEvent::Buffer(vbuf));
            self.send_event(StreamEvent::Health(health));
            self.send_event(StreamEvent::CdnStats(linter.cdn_stats()));

            let wait = self.interval.unwrap_or_else(|| {
                if let Some(mup) = last_mup {
                    Duration::from_millis((mup * 1000.0).max(500.0) as u64)
                } else {
                    let ms = if target_duration == 0 {
                        2_000
                    } else {
                        (target_duration * 500).max(500)
                    };
                    Duration::from_millis(ms)
                }
            });
            sleep(wait).await;
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn poll_dash_once(
        &self,
        linter: &mut SpecLinter,
        abr_health: &mut crate::models::AbrHealth,
        vbuf: &mut VirtualBuffer,
        buffer_clock: &mut Instant,
        last_publish: &mut Option<String>,
        probe_seq: &mut u64,
        announced_audio: &mut bool,
        announced_ast: &mut bool,
        last_period_id: &mut Option<String>,
    ) -> Result<(u64, Option<f64>)> {
        let (body, _) = self.fetch_manifest(self.source_url.as_str()).await?;
        let xml = String::from_utf8_lossy(&body);
        let summary = parse_dash_mpd(&xml, &self.source_url)?;

        if summary.period_count > 1 {
            self.emit_log(
                LogLevel::Info,
                DiagCategory::Info,
                format!(
                    "DASH multi-period MPD: {} periods | active={}",
                    summary.period_count,
                    summary.active_period_id.as_deref().unwrap_or("—")
                ),
            );
        }
        if let Some(id) = &summary.active_period_id {
            if let Some(prev) = last_period_id.as_ref() {
                if prev != id {
                    self.emit_log(
                        LogLevel::Warn,
                        DiagCategory::Rfc,
                        format!("DASH period transition: {prev} → {id}"),
                    );
                }
            }
            *last_period_id = Some(id.clone());
        }

        *abr_health = analyze_abr_ladder(&summary.variants);
        self.send_event(StreamEvent::AbrHealth(abr_health.clone()));
        for w in &abr_health.warnings {
            self.emit_log(LogLevel::Warn, DiagCategory::Abr, w.clone());
        }

        let mut variants = summary.variants.clone();
        self.send_event(StreamEvent::Variants(variants.clone()));

        if !*announced_audio {
            for lang in &summary.audio_languages {
                self.emit_log(
                    LogLevel::Info,
                    DiagCategory::AvSync,
                    format!("DASH demuxed AUDIO: {lang}"),
                );
            }
            *announced_audio = true;
        }

        if !*announced_ast {
            if let Some(ast) = &summary.availability_start_time {
                self.emit_log(
                    LogLevel::Info,
                    DiagCategory::Info,
                    format!(
                        "MPD AST={} | SPD={:?}s | minBufferTime={:?}s | live={}",
                        ast,
                        summary.suggested_presentation_delay_secs,
                        summary.min_buffer_time_secs,
                        summary.type_live
                    ),
                );
            } else {
                self.emit_log(
                    LogLevel::Info,
                    DiagCategory::Info,
                    format!(
                        "MPD SPD={:?}s | minBufferTime={:?}s | live={}",
                        summary.suggested_presentation_delay_secs,
                        summary.min_buffer_time_secs,
                        summary.type_live
                    ),
                );
            }
            *announced_ast = true;
        }

        let latency = if let Some(spd) = summary.suggested_presentation_delay_secs {
            LatencyState::Estimated((spd * 1000.0).round() as u64)
        } else if let Some(mbt) = summary.min_buffer_time_secs {
            LatencyState::Estimated((mbt * 1000.0).round() as u64)
        } else {
            LatencyState::Estimated(
                (f64::from(summary.segment_duration_hint_secs) * 3.0 * 1000.0).round() as u64,
            )
        };
        self.send_event(StreamEvent::Latency(latency));

        let window_secs = summary
            .time_shift_buffer_depth_secs
            .or(summary.media_presentation_duration_secs)
            .unwrap_or(0.0);
        let seg_hint = summary.segment_duration_hint_secs.clamp(0.1, 60.0);
        let window_segments = if window_secs > 0.0 && seg_hint > 0.0 {
            (window_secs / f64::from(seg_hint))
                .round()
                .clamp(0.0, u32::MAX as f64) as u32
        } else {
            0
        };

        let target = seg_hint.ceil() as u64;
        let publish_changed = match (&summary.publish_time, last_publish.as_ref()) {
            (Some(p), Some(prev)) => p != prev,
            (Some(_), None) => true,
            _ => true,
        };
        if let Some(p) = &summary.publish_time {
            *last_publish = Some(p.clone());
        }

        let should_probe = publish_changed || *probe_seq == 0;
        if should_probe {
            *probe_seq = probe_seq.saturating_add(1);
        }

        self.send_event(StreamEvent::PlaylistMeta(PlaylistMeta {
            media_sequence: *probe_seq,
            target_duration: target.max(1),
            url: self.source_url.to_string(),
            window_segments,
            window_secs,
            has_pdt: summary.availability_start_time.is_some(),
            has_master_playlist: variants.len() > 1,
            refresh_interval_ms: summary
                .minimum_update_period_secs
                .map(|s| (s * 1000.0).round() as u64),
            ll_hls: Default::default(),
            drm: summary.drm.clone(),
            renditions: Default::default(),
        }));

        if summary.drm.present {
            self.emit_log(
                LogLevel::Warn,
                DiagCategory::Drm,
                format!(
                    "{} | scheme={}",
                    summary.drm.badge,
                    summary.drm.key_format.as_deref().unwrap_or("—")
                ),
            );
        }

        let probe_url = summary
            .probe_url
            .clone()
            .or_else(|| variants.first().map(|v| v.uri.clone()))
            .ok_or_else(|| eyre!("DASH MPD has no probeable Representation URL"))?;

        if should_probe {
            let seq = *probe_seq;
            linter.on_new_segment(seq, seg_hint, target.max(1), false, 0, Instant::now());

            let fetch = if self.probe_headers {
                self.probe_segment(&probe_url).await?
            } else {
                self.download_segment(&probe_url).await?
            };

            linter.on_cdn_headers(fetch.cdn.clone(), fetch.ttfb_ms, seq);

            let wall = buffer_clock.elapsed().as_secs_f64();
            *buffer_clock = Instant::now();
            vbuf.on_new_segment(seg_hint, wall);
            self.send_event(StreamEvent::Buffer(*vbuf));

            let kbps = if fetch.probed {
                None
            } else if fetch.download_ms > 0 {
                Some((fetch.transferred_bytes.saturating_mul(8)).saturating_div(fetch.download_ms))
            } else {
                None
            };

            self.apply_wire_to_variants(&mut variants, &fetch.wire);

            let metrics = SegmentMetrics {
                media_sequence: seq,
                duration_secs: seg_hint,
                size_bytes: fetch.size_bytes,
                transferred_bytes: fetch.transferred_bytes,
                ttfb_ms: fetch.ttfb_ms,
                download_ms: fetch.download_ms,
                download_kbps: kbps,
                latency_ms: match latency {
                    LatencyState::Measured(ms) | LatencyState::Estimated(ms) => Some(ms),
                    LatencyState::Unknown => None,
                },
                uri: probe_url,
                cdn: fetch.cdn,
                probed: fetch.probed,
                container: fetch.container,
                http_status: fetch.http_status,
                network: Some(fetch.network),
                wire: Some(fetch.wire.clone()),
            };

            self.emit_log(
                LogLevel::Info,
                DiagCategory::Segment,
                format!(
                    "DASH probe seq={seq} {} | {} | {} | {}",
                    metrics.rate_label(),
                    fetch.container.as_str(),
                    metrics.cdn.badge(),
                    metrics
                        .network
                        .as_ref()
                        .map(|n| n.display_line())
                        .unwrap_or_default()
                ),
            );
            if let Some(w) = &metrics.wire {
                self.send_event(StreamEvent::WireProbe(w.clone()));
            }
            self.send_event(StreamEvent::Segment(metrics));
        }

        if let Some(best) = variants.iter_mut().max_by_key(|v| v.bandwidth) {
            let best_uri = best.uri.clone();
            for v in &mut variants {
                v.selected = v.uri == best_uri;
            }
        }
        self.send_event(StreamEvent::Variants(variants));

        Ok((target.max(1), summary.minimum_update_period_secs))
    }

    async fn run_hls_loop(self) {
        let mut media_url = self.source_url.clone();
        let mut last_seen_seq: Option<u64> = None;
        let mut target_duration: u64 = 6;
        let mut has_master = false;
        let mut cached_variants: Vec<AbrVariant> = Vec::new();
        let mut consecutive_errors: u32 = 0;
        let mut announced_estimate = false;
        let mut announced_single = false;
        let mut announced_ll = false;
        let mut announced_blocking = false;
        let mut announced_drm = false;
        let mut ll_hls_state = LlHlsBlockingState::default();
        let mut linter = SpecLinter::new();
        let mut abr_health = crate::models::AbrHealth::default();
        let mut vbuf = VirtualBuffer::default();
        let mut buffer_clock = Instant::now();
        let mut audio_url: Option<Url> = None;
        let mut seen_ads: HashSet<String> = HashSet::new();

        loop {
            let now = Instant::now();
            let elapsed = now.duration_since(buffer_clock).as_secs_f64();
            buffer_clock = now;
            vbuf.drain_elapsed(elapsed);
            self.send_event(StreamEvent::Buffer(vbuf));

            if target_duration > 0 {
                linter.check_stalling(target_duration, now);
            }

            match self
                .poll_once(
                    &mut media_url,
                    &mut last_seen_seq,
                    &mut has_master,
                    &mut cached_variants,
                    &mut announced_estimate,
                    &mut announced_single,
                    &mut announced_ll,
                    &mut announced_blocking,
                    &mut announced_drm,
                    &mut ll_hls_state,
                    &mut linter,
                    &mut abr_health,
                    &mut vbuf,
                    &mut buffer_clock,
                    &mut audio_url,
                    &mut seen_ads,
                )
                .await
            {
                Ok(td) => {
                    target_duration = td;
                    consecutive_errors = 0;
                    self.send_event(StreamEvent::Status(StreamStatus::live("Live")));
                }
                Err(err) => {
                    consecutive_errors = consecutive_errors.saturating_add(1);
                    let msg = format!("{err:#}");
                    self.send_event(StreamEvent::Error(msg.clone()));
                    self.emit_log(LogLevel::Error, DiagCategory::Info, msg.clone());
                    let status = if consecutive_errors >= 3 {
                        StreamStatus::error(msg)
                    } else {
                        StreamStatus::degraded(msg)
                    };
                    self.send_event(StreamEvent::Status(status));
                }
            }

            if !cached_variants.is_empty() {
                self.send_event(StreamEvent::Variants(cached_variants.clone()));
            }

            self.flush_findings(&mut linter);
            linter.clear_rfc_flag_if_clean();
            let health = apply_abr_penalty(linter.compute_health(), &abr_health);
            self.send_event(StreamEvent::Buffer(vbuf));
            self.send_event(StreamEvent::Health(health));
            self.send_event(StreamEvent::CdnStats(linter.cdn_stats()));

            let wait = self.interval.unwrap_or_else(|| {
                if let Some(ms) = ll_hls_state.part_interval_ms {
                    Duration::from_millis(ms)
                } else {
                    let ms = if target_duration == 0 {
                        2_000
                    } else {
                        (target_duration * 500).max(500)
                    };
                    Duration::from_millis(ms)
                }
            });
            sleep(wait).await;
        }
    }

    fn emit_log(&self, level: LogLevel, category: DiagCategory, message: impl Into<String>) {
        self.send_event(StreamEvent::Log {
            level,
            category,
            message: message.into(),
        });
    }

    fn flush_findings(&self, linter: &mut SpecLinter) {
        for finding in linter.take_findings() {
            let level = match finding.severity {
                DiagSeverity::Info => LogLevel::Info,
                DiagSeverity::Warn => LogLevel::Warn,
                DiagSeverity::Error => LogLevel::Error,
            };
            self.emit_log(level, finding.category, finding.message.clone());
            self.send_event(StreamEvent::Finding(finding));
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn poll_once(
        &self,
        media_url: &mut Url,
        last_seen_seq: &mut Option<u64>,
        has_master: &mut bool,
        cached_variants: &mut Vec<AbrVariant>,
        announced_estimate: &mut bool,
        announced_single: &mut bool,
        announced_ll: &mut bool,
        announced_blocking: &mut bool,
        announced_drm: &mut bool,
        ll_hls_state: &mut LlHlsBlockingState,
        linter: &mut SpecLinter,
        abr_health: &mut crate::models::AbrHealth,
        vbuf: &mut VirtualBuffer,
        buffer_clock: &mut Instant,
        audio_url: &mut Option<Url>,
        seen_ads: &mut HashSet<String>,
    ) -> Result<u64> {
        let base_fetch = if *has_master {
            media_url.as_str()
        } else {
            self.source_url.as_str()
        };
        let fetch_url = if ll_hls_state.can_block_reload {
            if let Some(msn) = ll_hls_state.blocking_msn {
                apply_hls_blocking_params(base_fetch, msn, ll_hls_state.blocking_part)
            } else {
                base_fetch.to_string()
            }
        } else {
            base_fetch.to_string()
        };
        if ll_hls_state.can_block_reload && !*announced_blocking {
            *announced_blocking = true;
            self.emit_log(
                LogLevel::Info,
                DiagCategory::LlHls,
                format!(
                    "LL-HLS blocking reload enabled (_HLS_msn={:?}, _HLS_part={:?})",
                    ll_hls_state.blocking_msn, ll_hls_state.blocking_part
                ),
            );
        }
        let body = self.fetch_bytes(&fetch_url).await?;
        let text = String::from_utf8_lossy(&body);
        if is_iptv_channel_list(&text) {
            return Err(eyre!(
                "IPTV channel list detected — refused to parse as HLS MediaPlaylist. \
                 Use Channel Picker or --audit on this URL."
            ));
        }
        let playlist =
            m3u8_rs::parse_playlist_res(&body).map_err(|e| eyre!("manifest parse error: {e}"))?;

        match playlist {
            Playlist::MasterPlaylist(master) => {
                let variants = collect_variants(&master.variants, &self.source_url);
                if variants.is_empty() {
                    return Err(eyre!("master playlist has no variants"));
                }

                let best = variants
                    .iter()
                    .max_by_key(|v| v.bandwidth)
                    .cloned()
                    .ok_or_else(|| eyre!("could not select highest bitrate"))?;

                let mut marked = variants;
                for v in &mut marked {
                    v.selected = v.uri == best.uri && v.bandwidth == best.bandwidth;
                }

                *abr_health = analyze_abr_ladder(&marked);
                self.send_event(StreamEvent::AbrHealth(abr_health.clone()));
                for w in &abr_health.warnings {
                    self.emit_log(LogLevel::Warn, DiagCategory::Abr, w.clone());
                }

                *cached_variants = marked.clone();
                *has_master = true;
                self.send_event(StreamEvent::Variants(marked));
                self.emit_log(
                    LogLevel::Info,
                    DiagCategory::Info,
                    format!(
                        "Master playlist: selected highest bitrate ({} kbps)",
                        best.bandwidth / 1000
                    ),
                );

                for audio in master
                    .alternatives
                    .iter()
                    .filter(|a| a.media_type == AlternativeMediaType::Audio)
                {
                    let lang = audio.language.as_deref().unwrap_or("und");
                    self.emit_log(
                        LogLevel::Info,
                        DiagCategory::AvSync,
                        format!("Separate AUDIO rendition: {} ({})", audio.name, lang),
                    );
                    if let Some(uri) = &audio.uri {
                        if audio_url.is_none() {
                            if let Ok(u) = resolve_url(&self.source_url, uri) {
                                *audio_url = Some(u);
                            }
                        }
                    }
                }

                *media_url = resolve_url(&self.source_url, &best.uri)?;
                *last_seen_seq = None;
                *announced_estimate = false;

                let media_body = self.fetch_bytes(media_url.as_str()).await?;
                let media = m3u8_rs::parse_media_playlist_res(&media_body)
                    .map_err(|e| eyre!("media playlist parse error: {e}"))?;
                let master_text = String::from_utf8_lossy(&body);
                self.announce_drm_and_renditions(&master_text, announced_drm);
                self.handle_media(
                    &media,
                    &media_body,
                    media_url,
                    last_seen_seq,
                    announced_estimate,
                    announced_ll,
                    announced_drm,
                    ll_hls_state,
                    true,
                    linter,
                    vbuf,
                    buffer_clock,
                    audio_url,
                    seen_ads,
                    cached_variants,
                )
                .await
            }
            Playlist::MediaPlaylist(media) => {
                if !*has_master && cached_variants.is_empty() && !*announced_single {
                    self.emit_log(
                        LogLevel::Info,
                        DiagCategory::Abr,
                        "Single media stream (no master playlist)",
                    );
                    *announced_single = true;
                    self.send_event(StreamEvent::Variants(Vec::new()));
                }
                self.handle_media(
                    &media,
                    &body,
                    media_url,
                    last_seen_seq,
                    announced_estimate,
                    announced_ll,
                    announced_drm,
                    ll_hls_state,
                    *has_master,
                    linter,
                    vbuf,
                    buffer_clock,
                    audio_url,
                    seen_ads,
                    cached_variants,
                )
                .await
            }
        }
    }

    fn announce_drm_and_renditions(&self, text: &str, announced: &mut bool) {
        if *announced {
            return;
        }
        let drm = scan_drm_keys(text);
        let rends = scan_media_renditions(text);
        if !drm.present && rends.audio.is_empty() && rends.subtitles.is_empty() {
            return;
        }
        *announced = true;
        if drm.present {
            self.emit_log(
                LogLevel::Warn,
                DiagCategory::Drm,
                format!(
                    "{} | method={} keyformat={}",
                    drm.badge,
                    drm.method.as_deref().unwrap_or("—"),
                    drm.key_format.as_deref().unwrap_or("—")
                ),
            );
        }
        for a in &rends.audio {
            self.emit_log(
                LogLevel::Info,
                DiagCategory::AvSync,
                format!("AUDIO rendition: {a}"),
            );
        }
        for s in &rends.subtitles {
            self.emit_log(
                LogLevel::Info,
                DiagCategory::Info,
                format!("SUBTITLES: {s}"),
            );
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn handle_media(
        &self,
        media: &m3u8_rs::MediaPlaylist,
        raw_body: &[u8],
        media_url: &Url,
        last_seen_seq: &mut Option<u64>,
        announced_estimate: &mut bool,
        announced_ll: &mut bool,
        announced_drm: &mut bool,
        ll_hls_state: &mut LlHlsBlockingState,
        has_master: bool,
        linter: &mut SpecLinter,
        vbuf: &mut VirtualBuffer,
        buffer_clock: &mut Instant,
        audio_url: &mut Option<Url>,
        seen_ads: &mut HashSet<String>,
        variants: &mut Vec<AbrVariant>,
    ) -> Result<u64> {
        let now = Instant::now();
        let window_segments = media.segments.len() as u32;
        let window_secs: f64 = media.segments.iter().map(|s| f64::from(s.duration)).sum();
        let has_pdt = media.segments.iter().any(|s| s.program_date_time.is_some());

        let estimated_ms = if media.target_duration > 0 {
            media
                .target_duration
                .saturating_mul(HLS_LIVE_EDGE_SEGMENTS)
                .saturating_mul(1000)
        } else if window_secs > 0.0 && window_segments > 0 {
            let avg = window_secs / f64::from(window_segments);
            (avg * HLS_LIVE_EDGE_SEGMENTS as f64 * 1000.0).round() as u64
        } else {
            0
        };

        let refresh_interval_ms = linter.on_playlist_refresh(
            media.media_sequence,
            media.target_duration,
            media.segments.len(),
            now,
        );

        let raw_text = String::from_utf8_lossy(raw_body);
        let ll = scan_ll_hls(&raw_text);
        ll_hls_state.is_ll_hls = ll.is_ll_hls;
        if ll.can_block_reload {
            ll_hls_state.can_block_reload = true;
        }
        if ll.is_ll_hls {
            ll_hls_state.part_interval_ms = Some(ll.poll_interval_ms());
        }
        if !media.segments.is_empty() || ll.is_ll_hls {
            let (next_msn, next_part) =
                next_blocking_targets(media.media_sequence, media.segments.len(), ll.part_count);
            ll_hls_state.blocking_msn = Some(next_msn);
            ll_hls_state.blocking_part = next_part;
        }

        let mut ll_meta = ll.clone();
        if let Some(hint_uri) = ll.preload_hint_uri.clone() {
            if let Ok(abs) = resolve_url(media_url, &hint_uri) {
                match self
                    .probe_ll_hls_hint(
                        abs.as_str(),
                        ll.preload_byterange_offset,
                        ll.preload_byterange_length,
                    )
                    .await
                {
                    Ok(probe) => {
                        ll_meta.preload_hint_fetched = true;
                        ll_meta.last_part_transfer_kbps = Some(probe.transfer_kbps);
                        self.emit_log(
                            LogLevel::Info,
                            DiagCategory::LlHls,
                            format!(
                                "PART/HINT probe | seq={} | {}ms | {:.0} kbps | {}",
                                ll_meta.last_part_sequence.unwrap_or(0),
                                ll_meta.part_latency_ms().unwrap_or(0),
                                probe.transfer_kbps,
                                ll_hls_probe_range(
                                    ll.preload_byterange_offset,
                                    ll.preload_byterange_length
                                )
                            ),
                        );
                    }
                    Err(err) => {
                        self.emit_log(
                            LogLevel::Warn,
                            DiagCategory::LlHls,
                            format!("PRELOAD-HINT probe failed: {err:#}"),
                        );
                    }
                }
            }
        }

        if ll.is_ll_hls && !*announced_ll {
            *announced_ll = true;
            let target = ll
                .part_target_secs
                .map(|s| format!("{s:.3}s"))
                .unwrap_or_else(|| "n/a".into());
            self.emit_log(
                LogLevel::Info,
                DiagCategory::LlHls,
                format!(
                    "LL-HLS detected | PART-TARGET={target} | parts={} | preload-hint={} | next _HLS_msn={:?} _HLS_part={:?}",
                    ll.part_count,
                    ll.has_preload_hint,
                    ll_hls_state.blocking_msn,
                    ll_hls_state.blocking_part
                ),
            );
        } else if ll.is_ll_hls {
            self.emit_log(
                LogLevel::Info,
                DiagCategory::LlHls,
                format!(
                    "parts={} part-target={}s preload={} fetched={}",
                    ll.part_count,
                    ll.part_target_secs.unwrap_or(0.0),
                    ll.has_preload_hint,
                    ll_meta.preload_hint_fetched
                ),
            );
        }

        let drm = scan_drm_keys(&raw_text);
        let renditions = scan_media_renditions(&raw_text);
        self.announce_drm_and_renditions(&raw_text, announced_drm);

        self.send_event(StreamEvent::PlaylistMeta(PlaylistMeta {
            media_sequence: media.media_sequence,
            target_duration: media.target_duration,
            url: media_url.to_string(),
            window_segments,
            window_secs,
            has_pdt,
            has_master_playlist: has_master,
            refresh_interval_ms,
            ll_hls: ll_meta,
            drm,
            renditions,
        }));

        for ad in
            extract_ad_signals_near_live_edge(&raw_text, AD_SCAN_LIVE_EDGE_SEGMENTS, Utc::now())
        {
            if ad.kind.starts_with("CUE-OUT") {
                seen_ads.remove("cue-in");
            }
            if ad.kind == "CUE-IN" {
                seen_ads.retain(|k| !k.starts_with("cont:") && !k.starts_with("out:"));
            }
            let key = ad_log_key(&ad);
            if seen_ads.insert(key) {
                let line = ad
                    .scte35_binary
                    .clone()
                    .unwrap_or_else(|| ad.summary.clone());
                self.emit_log(LogLevel::Warn, DiagCategory::Ad, line);
            }
            self.send_event(StreamEvent::AdBreak(ad));
        }

        if !has_pdt {
            self.send_event(StreamEvent::Latency(LatencyState::Estimated(estimated_ms)));
            if !*announced_estimate {
                let secs = estimated_ms as f64 / 1000.0;
                self.emit_log(
                    LogLevel::Info,
                    DiagCategory::Info,
                    format!(
                        "No PDT — estimated latency ~{secs:.2}s (target×{HLS_LIVE_EDGE_SEGMENTS})"
                    ),
                );
                *announced_estimate = true;
            }
        }

        if let Some(audio) = audio_url.clone() {
            self.check_av_drift(
                &audio,
                media.media_sequence,
                window_secs,
                media.target_duration,
            )
            .await;
        }

        if media.segments.is_empty() {
            self.emit_log(
                LogLevel::Warn,
                DiagCategory::Segment,
                "Media playlist returned an empty segment list",
            );
            return Ok(media.target_duration);
        }

        let base_seq = media.media_sequence;
        let start_idx = match *last_seen_seq {
            None => media.segments.len().saturating_sub(1),
            Some(last) => {
                let next = last.saturating_add(1);
                if next < base_seq {
                    let jump = base_seq.saturating_sub(last);
                    if jump > MEDIA_SEQ_GAP_TOLERANCE {
                        self.emit_log(
                            LogLevel::Warn,
                            DiagCategory::Rfc,
                            format!(
                                "Media sequence slid forward ({last} → {base_seq}); realigning to live edge"
                            ),
                        );
                    }
                    media.segments.len().saturating_sub(1)
                } else {
                    let offset = (next - base_seq) as usize;
                    if offset >= media.segments.len() {
                        return Ok(media.target_duration);
                    }
                    offset
                }
            }
        };

        for (i, segment) in media.segments.iter().enumerate().skip(start_idx) {
            let seq = base_seq + i as u64;
            linter.on_new_segment(
                seq,
                segment.duration,
                media.target_duration,
                segment.discontinuity,
                media.discontinuity_sequence,
                Instant::now(),
            );

            match self
                .process_segment(
                    media_url,
                    segment,
                    seq,
                    estimated_ms,
                    linter,
                    vbuf,
                    buffer_clock,
                    variants,
                )
                .await
            {
                Ok(()) => *last_seen_seq = Some(seq),
                Err(err) => {
                    self.emit_log(
                        LogLevel::Warn,
                        DiagCategory::Segment,
                        format!("Segment {seq} download failed: {err:#}"),
                    );
                    // Do not advance last_seen_seq — retry this seq on the next cycle.
                }
            }
        }

        Ok(media.target_duration)
    }

    async fn check_av_drift(
        &self,
        audio_url: &Url,
        video_seq: u64,
        video_window: f64,
        target_duration: u64,
    ) {
        match self.fetch_bytes(audio_url.as_str()).await {
            Ok(body) => match m3u8_rs::parse_media_playlist_res(&body) {
                Ok(audio) => {
                    let audio_window: f64 =
                        audio.segments.iter().map(|s| f64::from(s.duration)).sum();
                    let seq_delta = (audio.media_sequence as i64 - video_seq as i64).unsigned_abs();
                    let dur_delta = (audio_window - video_window).abs();
                    if seq_delta > 2 || dur_delta > target_duration as f64 {
                        self.emit_log(
                            LogLevel::Warn,
                            DiagCategory::AvSync,
                            format!(
                                "A/V drift: video seq={video_seq} window={video_window:.1}s | audio seq={} window={audio_window:.1}s | Δseq={seq_delta} Δdur={dur_delta:.1}s",
                                audio.media_sequence
                            ),
                        );
                    }
                }
                Err(e) => {
                    self.emit_log(
                        LogLevel::Warn,
                        DiagCategory::AvSync,
                        format!("Audio playlist parse error: {e}"),
                    );
                }
            },
            Err(e) => {
                self.emit_log(
                    LogLevel::Warn,
                    DiagCategory::AvSync,
                    format!("Audio playlist fetch failed: {e:#}"),
                );
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn process_segment(
        &self,
        media_url: &Url,
        segment: &m3u8_rs::MediaSegment,
        media_sequence: u64,
        estimated_ms: u64,
        linter: &mut SpecLinter,
        vbuf: &mut VirtualBuffer,
        buffer_clock: &mut Instant,
        variants: &mut Vec<AbrVariant>,
    ) -> Result<()> {
        let segment_url = resolve_url(media_url, &segment.uri)?;
        let fetch = if self.probe_headers {
            self.probe_segment(segment_url.as_str()).await?
        } else {
            self.download_segment(segment_url.as_str()).await?
        };

        linter.on_cdn_headers(fetch.cdn.clone(), fetch.ttfb_ms, media_sequence);

        let download_kbps = if fetch.probed {
            None
        } else if fetch.download_ms > 0 && fetch.transferred_bytes > 0 {
            Some(
                (fetch
                    .transferred_bytes
                    .saturating_mul(8)
                    .saturating_mul(1000))
                    / fetch.download_ms
                    / 1000,
            )
        } else {
            Some(0)
        };

        let now = Instant::now();
        let elapsed = now.duration_since(*buffer_clock).as_secs_f64();
        *buffer_clock = now;
        vbuf.on_new_segment(segment.duration, elapsed);
        if vbuf.stall_risk_pct > 0 {
            self.emit_log(LogLevel::Warn, DiagCategory::Buffer, vbuf.display());
        }

        let (latency, latency_ms) = match &segment.program_date_time {
            Some(pdt) => {
                let ms = (Utc::now() - pdt.with_timezone(&Utc)).num_milliseconds();
                let ms = if ms < 0 { 0 } else { ms as u64 };
                (LatencyState::Measured(ms), Some(ms))
            }
            None => (LatencyState::Estimated(estimated_ms), Some(estimated_ms)),
        };

        self.apply_wire_to_variants(variants, &fetch.wire);

        let metrics = SegmentMetrics {
            media_sequence,
            duration_secs: segment.duration,
            size_bytes: fetch.size_bytes,
            transferred_bytes: fetch.transferred_bytes,
            ttfb_ms: fetch.ttfb_ms,
            download_ms: fetch.download_ms,
            download_kbps,
            latency_ms,
            uri: segment_url.to_string(),
            cdn: fetch.cdn,
            probed: fetch.probed,
            container: fetch.container,
            http_status: fetch.http_status,
            network: Some(fetch.network.clone()),
            wire: Some(fetch.wire.clone()),
        };

        let rate_label = metrics.rate_label();
        let net_line = fetch.network.display_line();
        self.send_event(StreamEvent::WireProbe(fetch.wire.clone()));
        self.send_event(StreamEvent::Segment(metrics));
        self.send_event(StreamEvent::Latency(latency));
        self.send_event(StreamEvent::Buffer(*vbuf));
        self.send_event(StreamEvent::Variants(variants.clone()));

        let mode = if fetch.probed { "probe" } else { "full" };
        self.emit_log(
            LogLevel::Info,
            DiagCategory::Segment,
            format!(
                "seq={media_sequence} {mode} {} declared={}B xfer={}B | {rate_label} | {net_line}",
                fetch.container.as_str(),
                fetch.size_bytes,
                fetch.transferred_bytes
            ),
        );

        Ok(())
    }

    fn apply_wire_to_variants(&self, variants: &mut Vec<AbrVariant>, wire: &WireProbeInfo) {
        if wire.width.is_none()
            && wire.height.is_none()
            && wire.frame_rate.is_none()
            && wire.codec.is_none()
            && wire.profile_level.is_none()
        {
            return;
        }
        if variants.is_empty() {
            variants.push(AbrVariant {
                bandwidth: 0,
                resolution: wire.resolution_label(),
                codecs: wire.profile_level.clone().or(wire.codec.clone()),
                frame_rate: wire.frame_rate,
                uri: String::new(),
                selected: true,
                from_wire: true,
                mismatch: None,
            });
            return;
        }
        let idx = variants.iter().position(|v| v.selected).unwrap_or(0);
        let v = &mut variants[idx];
        let mismatches = manifest_wire_mismatches(
            v.resolution.as_deref(),
            v.frame_rate,
            v.codecs.as_deref(),
            wire,
        );
        for msg in &mismatches {
            self.emit_log(LogLevel::Warn, DiagCategory::Abr, msg.clone());
        }
        if let Some(first) = mismatches.first() {
            v.mismatch = Some(first.clone());
        }
        let filled = fill_abr_from_wire(&mut v.resolution, &mut v.frame_rate, &mut v.codecs, wire);
        if filled {
            v.from_wire = true;
        }
    }

    async fn fetch_manifest(&self, url: &str) -> Result<(Vec<u8>, Option<String>)> {
        if let Some(path) = local_path_from_url(url) {
            let body = tokio::fs::read(&path)
                .await
                .wrap_err_with(|| format!("failed to read {}", path.display()))?;
            return Ok((body, None));
        }
        let response = self
            .client
            .get(url)
            .send()
            .await
            .wrap_err_with(|| format!("GET failed: {url}"))?;
        let status = response.status();
        if !status.is_success() {
            return Err(eyre!("HTTP {status} — {url}"));
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
        let body = response
            .bytes()
            .await
            .wrap_err("failed to read body")?
            .to_vec();
        Ok((body, content_type))
    }

    async fn fetch_bytes(&self, url: &str) -> Result<Vec<u8>> {
        Ok(self.fetch_manifest(url).await?.0)
    }

    async fn download_segment(&self, url: &str) -> Result<SegmentFetch> {
        if let Some(path) = local_path_from_url(url) {
            return read_local_segment(&path, false).await;
        }
        match traced_get(url, &self.extra_headers, None, None).await {
            Ok(resp) => {
                let code = resp.status;
                if !((200..300).contains(&code)) {
                    return Err(eyre!("segment HTTP {code} — {url}"));
                }
                let cdn = parse_cdn_headers_http(&resp.headers);
                let total = resp.body.len() as u64;
                let head_len = (DEEP_WIRE_PROBE_BYTES as usize + 1).min(resp.body.len());
                let head = resp.body[..head_len].to_vec();
                let wire = deep_wire_probe(&head);
                let container = if wire.container != ContainerKind::Unknown {
                    wire.container
                } else {
                    inspect_container(&head)
                };
                Ok(SegmentFetch {
                    size_bytes: total,
                    transferred_bytes: total,
                    ttfb_ms: resp.timing.ttfb_ms,
                    download_ms: resp.download_ms,
                    cdn,
                    container,
                    probed: false,
                    http_status: code,
                    network: resp.timing,
                    wire,
                })
            }
            Err(_) => self.download_segment_reqwest(url).await,
        }
    }

    async fn download_segment_reqwest(&self, url: &str) -> Result<SegmentFetch> {
        let started = Instant::now();
        let response = self
            .client
            .get(url)
            .send()
            .await
            .wrap_err_with(|| format!("segment GET failed: {url}"))?;
        let status = response.status();
        let code = status.as_u16();
        if !status.is_success() {
            return Err(eyre!("segment HTTP {status} — {url}"));
        }
        let cdn = parse_cdn_headers(response.headers());
        let ttfb_ms = started.elapsed().as_millis() as u64;
        let mut stream = response.bytes_stream();
        let mut total: u64 = 0;
        let mut head: Vec<u8> = Vec::new();

        while let Some(chunk) = stream.next().await {
            let chunk = chunk.wrap_err("segment stream read error")?;
            if head.len() < DEEP_WIRE_PROBE_BYTES as usize + 1 {
                let take = ((DEEP_WIRE_PROBE_BYTES as usize + 1) - head.len()).min(chunk.len());
                head.extend_from_slice(&chunk[..take]);
            }
            total = total.saturating_add(chunk.len() as u64);
        }

        let download_ms = started.elapsed().as_millis() as u64;
        let wire = deep_wire_probe(&head);
        let container = if wire.container != ContainerKind::Unknown {
            wire.container
        } else {
            inspect_container(&head)
        };
        Ok(SegmentFetch {
            size_bytes: total,
            transferred_bytes: total,
            ttfb_ms,
            download_ms: download_ms.max(1),
            cdn,
            container,
            probed: false,
            http_status: code,
            network: timing_from_ttfb(ttfb_ms),
            wire,
        })
    }

    async fn probe_segment(&self, url: &str) -> Result<SegmentFetch> {
        if let Some(path) = local_path_from_url(url) {
            return read_local_segment(&path, true).await;
        }
        let range = format!("bytes=0-{DEEP_WIRE_PROBE_BYTES}");
        match traced_get(
            url,
            &self.extra_headers,
            Some(&range),
            Some(DEEP_WIRE_PROBE_BYTES as usize + 1),
        )
        .await
        {
            Ok(resp) => {
                let code = resp.status;
                if !(code == 200 || code == 206) {
                    return Err(eyre!("probe HTTP {code} — {url}"));
                }
                let cdn = parse_cdn_headers_http(&resp.headers);
                let declared = resp
                    .headers
                    .get("content-range")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| s.split('/').next_back())
                    .and_then(|n| n.parse::<u64>().ok())
                    .unwrap_or(0);
                let transferred = resp.body.len() as u64;
                let size_bytes = if declared > 0 { declared } else { transferred };
                let wire = deep_wire_probe(&resp.body);
                let container = if wire.container != ContainerKind::Unknown {
                    wire.container
                } else {
                    inspect_container(&resp.body)
                };
                Ok(SegmentFetch {
                    size_bytes,
                    transferred_bytes: transferred,
                    ttfb_ms: resp.timing.ttfb_ms,
                    download_ms: resp.download_ms,
                    cdn,
                    container,
                    probed: true,
                    http_status: code,
                    network: resp.timing,
                    wire,
                })
            }
            Err(_) => self.probe_segment_reqwest(url).await,
        }
    }

    async fn probe_segment_reqwest(&self, url: &str) -> Result<SegmentFetch> {
        let started = Instant::now();
        let range = format!("bytes=0-{DEEP_WIRE_PROBE_BYTES}");
        let response = self
            .client
            .get(url)
            .header(RANGE, range)
            .send()
            .await
            .wrap_err_with(|| format!("range probe failed: {url}"))?;

        let status = response.status();
        let code = status.as_u16();
        if !(status.is_success() || code == 206) {
            return Err(eyre!("probe HTTP {status} — {url}"));
        }

        let cdn = parse_cdn_headers(response.headers());
        let ttfb_ms = started.elapsed().as_millis() as u64;
        let declared = response
            .headers()
            .get("content-range")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.split('/').next_back())
            .and_then(|n| n.parse::<u64>().ok())
            .unwrap_or(0);

        let mut stream = response.bytes_stream();
        let max = DEEP_WIRE_PROBE_BYTES as usize + 1;
        let mut buf = Vec::with_capacity(max.min(8 * 1024));
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.wrap_err("probe body read failed")?;
            let remain = max.saturating_sub(buf.len());
            if remain == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..chunk.len().min(remain)]);
            if buf.len() >= max {
                break;
            }
        }
        let download_ms = started.elapsed().as_millis() as u64;
        let transferred = buf.len() as u64;
        let size_bytes = if declared > 0 {
            declared
        } else if code == 200 && transferred as usize >= max {
            0
        } else {
            transferred
        };
        let wire = deep_wire_probe(&buf);
        let container = if wire.container != ContainerKind::Unknown {
            wire.container
        } else {
            inspect_container(&buf)
        };
        Ok(SegmentFetch {
            size_bytes,
            transferred_bytes: transferred,
            ttfb_ms,
            download_ms: download_ms.max(1),
            cdn,
            container,
            probed: true,
            http_status: code,
            network: timing_from_ttfb(ttfb_ms),
            wire,
        })
    }

    /// Range-probe a PRELOAD-HINT / PART URI; returns measured transfer rate.
    async fn probe_ll_hls_hint(
        &self,
        url: &str,
        offset: Option<u64>,
        length: Option<u64>,
    ) -> Result<LlHlsProbeStats> {
        if local_path_from_url(url).is_some() {
            return Ok(LlHlsProbeStats { transfer_kbps: 0.0 });
        }
        let range = ll_hls_probe_range(offset, length);
        let started = Instant::now();
        let response = self
            .client
            .get(url)
            .header(RANGE, range)
            .send()
            .await
            .wrap_err_with(|| format!("LL-HLS hint probe failed: {url}"))?;
        let status = response.status();
        let code = status.as_u16();
        if !(status.is_success() || code == 206) {
            return Err(eyre!("LL-HLS hint HTTP {status} — {url}"));
        }
        let body = response.bytes().await?;
        let elapsed_ms = started.elapsed().as_millis().max(1) as u64;
        let bytes = body.len() as u64;
        let transfer_kbps = (bytes as f64 * 8.0) / elapsed_ms as f64;
        Ok(LlHlsProbeStats { transfer_kbps })
    }
}

#[derive(Debug, Clone, Copy)]
struct LlHlsProbeStats {
    transfer_kbps: f64,
}

pub fn build_http_client(headers: &[String], user_agent: Option<String>) -> Result<Client> {
    build_http_client_with_timeouts(headers, user_agent, 10, 30)
}

/// Audit HTTP client with short connect/total timeouts.
pub fn build_audit_http_client(headers: &[String], user_agent: Option<String>) -> Result<Client> {
    use crate::models::{AUDIT_CONNECT_TIMEOUT_SECS, AUDIT_REQUEST_TIMEOUT_SECS};
    build_http_client_with_timeouts(
        headers,
        user_agent,
        AUDIT_CONNECT_TIMEOUT_SECS,
        AUDIT_REQUEST_TIMEOUT_SECS,
    )
}

fn build_http_client_with_timeouts(
    headers: &[String],
    user_agent: Option<String>,
    connect_secs: u64,
    timeout_secs: u64,
) -> Result<Client> {
    let mut builder = Client::builder()
        .user_agent(user_agent.unwrap_or_else(|| DEFAULT_UA.to_string()))
        .gzip(true)
        .brotli(true)
        .timeout(Duration::from_secs(timeout_secs))
        .connect_timeout(Duration::from_secs(connect_secs))
        .pool_idle_timeout(Duration::from_secs(90));

    let header_map = parse_headers(headers)?;
    if !header_map.is_empty() {
        builder = builder.default_headers(header_map);
    }

    builder.build().wrap_err("failed to build HTTP client")
}

pub(crate) fn parse_headers(raw: &[String]) -> Result<HeaderMap> {
    let mut map = HeaderMap::new();
    for entry in raw {
        let (name, value) = entry
            .split_once(':')
            .ok_or_else(|| eyre!("invalid header (expected KEY: VALUE): {entry}"))?;
        let name = HeaderName::from_bytes(name.trim().as_bytes())
            .wrap_err_with(|| format!("invalid header name: {name}"))?;
        let value = HeaderValue::from_str(value.trim())
            .wrap_err_with(|| format!("invalid header value: {value}"))?;
        map.insert(name, value);
    }
    Ok(map)
}

fn resolve_url(base: &Url, href: &str) -> Result<Url> {
    if let Ok(absolute) = Url::parse(href) {
        return Ok(absolute);
    }
    base.join(href)
        .wrap_err_with(|| format!("failed to join URL: base={base} href={href}"))
}

pub fn collect_variants(variants: &[VariantStream], base: &Url) -> Vec<AbrVariant> {
    let mut out = Vec::new();
    let mut seen: HashMap<String, ()> = HashMap::new();

    for v in variants {
        if v.is_i_frame || v.uri.is_empty() {
            continue;
        }

        let resolved = resolve_url(base, &v.uri)
            .map(|u| u.to_string())
            .unwrap_or_else(|_| v.uri.clone());

        if seen.contains_key(&resolved) {
            continue;
        }
        seen.insert(resolved.clone(), ());

        let resolution = v.resolution.map(|r| format!("{}x{}", r.width, r.height));

        out.push(AbrVariant {
            bandwidth: v.bandwidth,
            resolution,
            codecs: v.codecs.clone(),
            frame_rate: v.frame_rate,
            uri: resolved,
            selected: false,
            from_wire: false,
            mismatch: None,
        });
    }

    out.sort_by_key(|a| std::cmp::Reverse(a.bandwidth));
    out
}

fn local_cdn() -> CdnEdgeInfo {
    CdnEdgeInfo {
        verdict: crate::models::CacheVerdict::Unknown,
        provider: Some("Local".into()),
        cache_status: None,
        age: None,
        pop: None,
        served_by: Some("filesystem".into()),
        via: None,
    }
}

async fn read_local_segment(path: &std::path::Path, probe: bool) -> Result<SegmentFetch> {
    let started = Instant::now();
    let data = tokio::fs::read(path)
        .await
        .wrap_err_with(|| format!("failed to read {}", path.display()))?;
    let ttfb_ms = started.elapsed().as_millis() as u64;
    let take = if probe {
        data.len()
            .min((DEEP_WIRE_PROBE_BYTES as usize).saturating_add(1))
    } else {
        data.len()
    };
    let slice = &data[..take];
    let wire = deep_wire_probe(slice);
    let container = if wire.container != ContainerKind::Unknown {
        wire.container
    } else {
        inspect_container(slice)
    };
    let download_ms = started.elapsed().as_millis() as u64;
    Ok(SegmentFetch {
        size_bytes: data.len() as u64,
        transferred_bytes: slice.len() as u64,
        ttfb_ms,
        download_ms: download_ms.max(1),
        cdn: local_cdn(),
        container,
        probed: probe,
        http_status: if probe { 206 } else { 200 },
        network: timing_from_ttfb(ttfb_ms),
        wire,
    })
}

fn parse_cdn_headers_http(headers: &http::HeaderMap) -> CdnEdgeInfo {
    let mut map = HeaderMap::new();
    for (k, v) in headers.iter() {
        if let (Ok(name), Ok(val)) = (
            HeaderName::from_bytes(k.as_str().as_bytes()),
            HeaderValue::from_bytes(v.as_bytes()),
        ) {
            map.append(name, val);
        }
    }
    parse_cdn_headers(&map)
}
