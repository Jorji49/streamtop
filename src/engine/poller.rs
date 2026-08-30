use std::collections::HashSet;
use std::sync::{Arc, Mutex, RwLock};
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

use crate::engine::abr_model::{simulate_segment_fetch, AbrLadderState};
use crate::engine::agent::AgentMetricsRegistry;
use crate::engine::channel_stats::record_channel_drop;
use crate::engine::container_probe::{
    deep_wire_probe, fill_abr_from_wire, manifest_wire_mismatches,
};
use crate::engine::dai_validator::{
    inband_events_from_wire, validate_ad_alignment, validate_inband_vs_manifest,
};
use crate::engine::dash::{
    extract_dash_ad_events, ll_dash_production_drift, looks_like_dash, parse_dash_mpd,
};
use crate::engine::drm_probe::{
    apply_clearkey_to_wire, clearkey_license_body, probe_clearkey, ClearKeySpec,
};
use crate::engine::g2g::{compute_g2g, wall_now_unix_ms};
use crate::engine::gop_tracker::GopCadenceTracker;
use crate::engine::linter::{
    ad_log_key, analyze_abr_ladder, apply_abr_penalty, apply_hls_blocking_params,
    extract_ad_signals_near_live_edge, inspect_container, lint_abr_player, lint_subtitle_drift,
    ll_hls_probe_range, next_blocking_targets, parse_cdn_headers, scan_drm_keys, scan_ll_hls,
    scan_media_renditions, SpecLinter,
};
use crate::engine::metrics::{update_metrics, MetricsSnapshot};
use crate::engine::network_trace::{
    parse_header_pairs, reqwest_headers_chunked, timing_from_ttfb, traced_get,
};
use crate::engine::otel::OtelExporter;
use crate::engine::playlist_parser::{is_iptv_channel_list, local_path_from_url};
use crate::engine::sei_probe::SeiProbeAccumulator;
use crate::engine::subtitle_probe::{compute_subtitle_drift, probe_subtitle_payload};
use crate::engine::synthetic_qoe::SyntheticQoeEngine;
use crate::engine::tr101290::{probe_container_tr101290, Tr101290Engine};
use crate::engine::wire_timing::WireTimingTracker;
use crate::models::{
    AbrVariant, CdnEdgeInfo, ContainerKind, DiagCategory, DiagSeverity, LatencyState, LlDashInfo,
    LlHlsInfo, LogLevel, MediaRenditions, NetworkTiming, PlaylistMeta, SegmentMetrics, StreamEvent,
    StreamProtocol, StreamStatus, VirtualBuffer, WireProbeInfo, AD_SCAN_LIVE_EDGE_SEGMENTS,
    DEEP_WIRE_PROBE_BYTES, HLS_LIVE_EDGE_SEGMENTS, MAX_MANIFEST_BYTES, MAX_PLAYLIST_DEPTH,
    MAX_SEGMENT_BYTES, MEDIA_SEQ_GAP_TOLERANCE,
};

const DEFAULT_UA: &str = concat!("streamtop/", env!("CARGO_PKG_VERSION"));

/// Optional next-gen diagnostic probes (TR 101 290, SEI, synthetic QoE).
#[derive(Debug, Clone, Default)]
pub struct DiagnosticOpts {
    pub tr101290: bool,
    pub probe_sei: bool,
    pub simulate_player: bool,
    pub throttle_kbps: Option<u64>,
    pub simulated_rtt_ms: Option<u64>,
}

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
    probe_drm: bool,
    extra_headers: Vec<(String, String)>,
    tx: Sender<StreamEvent>,
    hook_tx: Option<Sender<StreamEvent>>,
    metrics: Option<Arc<RwLock<MetricsSnapshot>>>,
    agent_metrics: Option<(Arc<RwLock<AgentMetricsRegistry>>, String)>,
    gop_tracker: Arc<Mutex<GopCadenceTracker>>,
    wire_timing_tracker: Arc<Mutex<WireTimingTracker>>,
    abr_ladder: Arc<Mutex<AbrLadderState>>,
    otel: Option<Arc<OtelExporter>>,
    diagnostics: DiagnosticOpts,
    clearkey: Option<ClearKeySpec>,
    last_active_ad: Arc<Mutex<Option<crate::models::AdBreakInfo>>>,
    tr101290: Arc<Mutex<Tr101290Engine>>,
    sei_acc: Arc<Mutex<SeiProbeAccumulator>>,
    qoe: Arc<Mutex<SyntheticQoeEngine>>,
    segment_wall_ms: Arc<Mutex<u64>>,
    ladder_scratch: Arc<Mutex<Vec<u64>>>,
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
    chunked_transfer: bool,
    segment_url: String,
    probe_bytes: Vec<u8>,
}

impl ManifestPoller {
    pub fn new(
        source_url: &str,
        headers: &[String],
        user_agent: Option<&str>,
        interval_ms: Option<u64>,
        probe_headers: bool,
        probe_drm: bool,
        tx: Sender<StreamEvent>,
    ) -> Result<Self> {
        let source_url = Url::parse(source_url).wrap_err("invalid stream URL")?;
        let extra_headers = parse_header_pairs(headers);
        let client = build_http_client(headers, user_agent)?;
        let interval = interval_ms.map(Duration::from_millis);

        Ok(Self {
            client,
            source_url,
            interval,
            probe_headers,
            probe_drm,
            extra_headers,
            tx,
            hook_tx: None,
            metrics: None,
            agent_metrics: None,
            gop_tracker: Arc::new(Mutex::new(GopCadenceTracker::default())),
            wire_timing_tracker: Arc::new(Mutex::new(WireTimingTracker::default())),
            abr_ladder: Arc::new(Mutex::new(AbrLadderState::default())),
            otel: None,
            diagnostics: DiagnosticOpts::default(),
            clearkey: None,
            last_active_ad: Arc::new(Mutex::new(None)),
            tr101290: Arc::new(Mutex::new(Tr101290Engine::new())),
            sei_acc: Arc::new(Mutex::new(SeiProbeAccumulator::new())),
            qoe: Arc::new(Mutex::new(SyntheticQoeEngine::new(None, None))),
            segment_wall_ms: Arc::new(Mutex::new(0)),
            ladder_scratch: Arc::new(Mutex::new(Vec::with_capacity(16))),
        })
    }

    #[must_use]
    pub fn with_clearkey(mut self, spec: Option<ClearKeySpec>) -> Self {
        self.clearkey = spec;
        self
    }

    #[must_use]
    pub fn with_diagnostics(mut self, opts: &DiagnosticOpts) -> Self {
        self.diagnostics = opts.clone();
        if let Ok(mut qoe) = self.qoe.lock() {
            *qoe = SyntheticQoeEngine::new(opts.throttle_kbps, opts.simulated_rtt_ms);
        }
        self
    }

    #[must_use]
    pub fn with_otel(mut self, otel: Arc<OtelExporter>) -> Self {
        self.otel = Some(otel);
        self
    }

    #[must_use]
    pub fn with_metrics(mut self, metrics: Arc<RwLock<MetricsSnapshot>>) -> Self {
        self.metrics = Some(metrics);
        self
    }

    #[must_use]
    pub fn with_agent_metrics(
        mut self,
        registry: Arc<RwLock<AgentMetricsRegistry>>,
        stream_id: String,
    ) -> Self {
        self.agent_metrics = Some((registry, stream_id));
        self
    }

    #[must_use]
    pub fn with_webhook_tx(mut self, hook_tx: Sender<StreamEvent>) -> Self {
        self.hook_tx = Some(hook_tx);
        self
    }

    fn send_event(&self, event: StreamEvent) {
        if let Some(m) = &self.metrics {
            if let Ok(mut snap) = m.write() {
                update_metrics(&mut snap, &event);
                if let Some(otel) = &self.otel {
                    otel.record_metrics_snapshot(&snap);
                }
            }
        }
        if let Some((reg, id)) = &self.agent_metrics {
            if let Ok(mut guard) = reg.write() {
                if let Some(snap) = guard.streams.get_mut(id) {
                    update_metrics(snap, &event);
                }
            }
        }
        // Bounded: drop when UI/webhook cannot keep up (prefer liveness over backlog).
        let dropped = self.tx.try_send(event.clone()).is_err()
            || self
                .hook_tx
                .as_ref()
                .is_some_and(|h| h.try_send(event).is_err());
        if dropped {
            record_channel_drop();
            if let Some((reg, _)) = &self.agent_metrics {
                if let Ok(mut guard) = reg.write() {
                    guard.dropped_events = guard.dropped_events.saturating_add(1);
                }
            }
        }
    }

    fn traceparent(&self) -> Option<String> {
        self.otel.as_ref().map(|o| o.traceparent())
    }

    fn emit_g2g(
        &self,
        wire: &WireProbeInfo,
        pdt: Option<chrono::DateTime<Utc>>,
        dash_avail_ms: Option<i64>,
        ttfb_ms: u64,
    ) {
        let g2g = compute_g2g(
            wire.timing.prft_ntp_unix_ms,
            pdt.as_ref(),
            dash_avail_ms,
            Some(ttfb_ms),
            wall_now_unix_ms(),
        );
        if !g2g.is_empty() {
            if let Some(otel) = &self.otel {
                otel.record_g2g(&g2g);
            }
            self.send_event(StreamEvent::G2g(g2g));
        }
    }

    fn merge_wire_pssh(drm: &mut crate::models::DrmInfo, wire: &WireProbeInfo) {
        if wire.pssh.is_empty() {
            return;
        }
        if let Some(ref mut existing) = drm.pssh {
            existing.merge(wire.pssh.clone());
        } else {
            drm.pssh = Some(wire.pssh.clone());
        }
    }

    fn post_segment_diagnostics(
        &self,
        fetch: &SegmentFetch,
        duration_secs: f32,
        download_kbps: Option<u64>,
        ladder_bps: &[u64],
    ) {
        let d = &self.diagnostics;
        if !d.tr101290 && !d.probe_sei && !d.simulate_player {
            return;
        }
        if fetch.probe_bytes.is_empty() {
            return;
        }
        let wall_ms = self.segment_wall_ms.lock().map_or(0, |mut w| {
            *w = w.saturating_add((f64::from(duration_secs.max(0.001)) * 1000.0) as u64);
            *w
        });
        if d.tr101290 {
            if let Ok(mut eng) = self.tr101290.lock() {
                if let Some(report) =
                    probe_container_tr101290(&mut eng, &fetch.probe_bytes, fetch.container, wall_ms)
                {
                    self.send_event(StreamEvent::Tr101290(report));
                }
            }
        }
        if d.probe_sei {
            if let Ok(mut acc) = self.sei_acc.lock() {
                let sei = acc.ingest(&fetch.probe_bytes, fetch.container);
                if sei.nal_units_scanned > 0
                    || sei.cea608_present
                    || sei.cea708_present
                    || sei.hdr10_present
                {
                    self.send_event(StreamEvent::SeiProbe(sei));
                }
            }
        }
        if d.simulate_player {
            if let Ok(mut qoe) = self.qoe.lock() {
                let snap = qoe.observe_segment(
                    duration_secs,
                    fetch.download_ms,
                    download_kbps,
                    ladder_bps,
                );
                self.send_event(StreamEvent::SyntheticQoe(snap));
            }
        }
    }

    fn post_wire_extras(&self, fetch: &SegmentFetch, wire: &mut WireProbeInfo) {
        if let Some(spec) = &self.clearkey {
            if !fetch.probe_bytes.is_empty() {
                let result = probe_clearkey(&fetch.probe_bytes, spec);
                apply_clearkey_to_wire(wire, &result);
                if result.kid_matched || result.cenc_boxes_seen {
                    self.emit_log(LogLevel::Info, DiagCategory::Drm, result.message);
                }
                if let Some(metrics) = &self.metrics {
                    if let Ok(mut snap) = metrics.write() {
                        snap.clearkey_decrypt_ok = if result.decrypt_ok { 1.0 } else { 0.0 };
                    }
                }
            }
        }
        if let Ok(guard) = self.last_active_ad.lock() {
            if let Some(ad) = guard.as_ref() {
                if let Some(mismatch) = validate_ad_alignment(ad, wire, ad.scte35_binary.as_deref())
                {
                    self.send_event(StreamEvent::AdMarkerMismatch(mismatch));
                }
            }
        }
        for ev in inband_events_from_wire(wire) {
            let summary = ev.scte35_summary.clone().unwrap_or_else(|| {
                format!("emsg id={} scheme={}", ev.emsg.id, ev.emsg.scheme_id_uri)
            });
            self.emit_log(
                LogLevel::Info,
                DiagCategory::Ad,
                format!("[EMSG] {summary}"),
            );
            self.send_event(StreamEvent::InbandAdEvent(ev.clone()));
            if let Ok(guard) = self.last_active_ad.lock() {
                if let Some(ad) = guard.as_ref() {
                    if let Some(mismatch) = validate_inband_vs_manifest(ad, &ev) {
                        self.send_event(StreamEvent::AdMarkerMismatch(mismatch));
                    }
                }
            }
        }
    }

    fn finalize_wire(&self, wire: &mut WireProbeInfo) {
        if let Ok(mut tracker) = self.gop_tracker.lock() {
            tracker.observe_keyframe(wire.keyframe_pts_sec);
            tracker.apply(wire);
        }
        if let Ok(mut timing) = self.wire_timing_tracker.lock() {
            timing.apply(&mut wire.timing, None);
            timing.observe_segment(&wire.timing, wire.keyframe_pts_sec);
        }
    }

    fn apply_wire_target_duration(&self, wire: &mut WireProbeInfo, target_secs: f32) {
        WireTimingTracker::apply_target(&mut wire.timing, Some(target_secs));
        if let Some(label) = wire.timing.timing_label() {
            self.emit_log(
                LogLevel::Warn,
                DiagCategory::Segment,
                format!("Wire timing: {label}"),
            );
        }
    }

    fn record_segment_otel(&self, fetch: &SegmentFetch) {
        if let Some(exporter) = &self.otel {
            exporter.record_network("http.ttfb", &fetch.network, &fetch.segment_url);
            exporter.record_segment_download(
                &fetch.segment_url,
                &fetch.network,
                fetch.download_ms,
                fetch.http_status,
                fetch.chunked_transfer,
            );
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
                last_mup.map_or_else(
                    || {
                        let ms = if target_duration == 0 {
                            2_000
                        } else {
                            (target_duration * 500).max(500)
                        };
                        Duration::from_millis(ms)
                    },
                    |mup| Duration::from_millis((mup * 1000.0).max(500.0) as u64),
                )
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

        for issue in crate::engine::dash::audit_multi_period_mpd(&xml, &summary) {
            self.emit_log(LogLevel::Warn, DiagCategory::Rfc, issue);
        }
        for finding in crate::engine::dash::audit_dash_iop(&xml, &summary) {
            linter.ingest_finding(finding);
        }

        if summary.period_count > 1 {
            self.emit_log(
                LogLevel::Info,
                DiagCategory::Info,
                format!(
                    "DASH multi-period MPD: {} periods | active={}",
                    summary.period_count,
                    summary.active_period_id.as_deref().unwrap_or("-")
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
            _ => true,
        };
        if let Some(p) = &summary.publish_time {
            *last_publish = Some(p.clone());
        }

        for ad in extract_dash_ad_events(&xml) {
            if ad.active {
                if let Ok(mut slot) = self.last_active_ad.lock() {
                    *slot = Some(ad.clone());
                }
            }
            self.emit_log(LogLevel::Warn, DiagCategory::Ad, ad.summary.clone());
            self.send_event(StreamEvent::AdBreak(ad));
        }

        let should_probe = publish_changed || *probe_seq == 0;
        if should_probe {
            *probe_seq = probe_seq.saturating_add(1);
        }

        let mut dash_drm = summary.drm.clone();
        if dash_drm.present {
            self.emit_log(
                LogLevel::Warn,
                DiagCategory::Drm,
                format!(
                    "{} | scheme={}",
                    dash_drm.badge,
                    dash_drm.key_format.as_deref().unwrap_or("-")
                ),
            );
            if self.probe_drm {
                self.probe_drm_license(&mut dash_drm, &self.source_url)
                    .await;
            }
        }

        let mut ll_dash = summary.ll_dash.clone();
        if ll_dash.is_ll_dash && *probe_seq <= 1 {
            if let Some(badge) = ll_dash.header_badge() {
                self.emit_log(LogLevel::Info, DiagCategory::Info, badge);
            }
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

            linter.on_cdn_headers(&fetch.cdn, fetch.ttfb_ms, seq);

            let kbps = if fetch.probed {
                None
            } else if fetch.download_ms > 0 {
                Some((fetch.transferred_bytes.saturating_mul(8)).saturating_div(fetch.download_ms))
            } else {
                None
            };

            let wall = buffer_clock.elapsed().as_secs_f64();
            *buffer_clock = Instant::now();
            let declared_bw = variants
                .iter()
                .find(|v| v.selected)
                .or_else(|| variants.first())
                .map(|v| v.bandwidth);
            if let Ok(mut ladder) = self.abr_ladder.lock() {
                simulate_segment_fetch(
                    vbuf,
                    seg_hint,
                    fetch.download_ms,
                    wall,
                    kbps,
                    declared_bw,
                    &mut ladder,
                );
            } else {
                vbuf.on_new_segment(seg_hint, wall);
            }
            self.send_event(StreamEvent::Buffer(*vbuf));
            for w in lint_abr_player(vbuf) {
                self.emit_log(LogLevel::Warn, DiagCategory::Abr, w);
            }

            self.apply_wire_to_variants(&mut variants, &fetch.wire);

            if fetch.chunked_transfer {
                ll_dash.chunked_transfer = true;
            }
            if let Some(target_ms) = ll_dash.latency_target_ms {
                let drift = ll_dash_production_drift(target_ms, fetch.ttfb_ms);
                ll_dash.production_drift_ms = Some(drift);
                if drift > target_ms as i64 {
                    self.emit_log(
                        LogLevel::Warn,
                        DiagCategory::Segment,
                        format!(
                            "LL-DASH production drift {drift}ms exceeds latency target {target_ms}ms"
                        ),
                    );
                }
            }

            self.record_segment_otel(&fetch);
            if let Some(otel) = &self.otel {
                otel.record_wire_parse(&fetch.segment_url, &fetch.wire);
            }
            let dash_avail_ms = summary
                .publish_time
                .as_ref()
                .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
                .map(|dt| dt.timestamp_millis());
            self.emit_g2g(&fetch.wire, None, dash_avail_ms, fetch.ttfb_ms);
            Self::merge_wire_pssh(&mut dash_drm, &fetch.wire);

            let mut wire = fetch.wire.clone();
            self.post_wire_extras(&fetch, &mut wire);

            if let Ok(mut ladder) = self.ladder_scratch.lock() {
                ladder.clear();
                ladder.extend(variants.iter().map(|v| v.bandwidth));
                self.post_segment_diagnostics(&fetch, seg_hint, kbps, ladder.as_slice());
            }

            let metrics = SegmentMetrics {
                media_sequence: seq,
                duration_secs: seg_hint,
                size_bytes: fetch.size_bytes,
                transferred_bytes: fetch.transferred_bytes,
                ttfb_ms: fetch.ttfb_ms,
                download_ms: fetch.download_ms,
                dl_to_dur_ratio: SegmentMetrics::compute_dl_to_dur_ratio(
                    fetch.download_ms,
                    seg_hint,
                ),
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
                wire: Some(wire.clone()),
            };

            self.emit_log(
                LogLevel::Info,
                DiagCategory::Segment,
                format!(
                    "DASH probe seq={seq} {} | {} | {} | {}{}",
                    metrics.rate_label(),
                    fetch.container.as_str(),
                    metrics.cdn.badge(),
                    metrics
                        .network
                        .as_ref()
                        .map(super::super::models::stream::NetworkTiming::display_line)
                        .unwrap_or_default(),
                    metrics
                        .dl_to_dur_ratio
                        .map_or_else(String::new, |r| format!(" | dl_to_dur_ratio={r:.2}"))
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
        self.send_event(StreamEvent::Variants(variants.clone()));

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
            ll_hls: LlHlsInfo::default(),
            ll_dash: ll_dash.clone(),
            drm: dash_drm,
            renditions: MediaRenditions::default(),
        }));

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
                ll_hls_state.part_interval_ms.map_or_else(
                    || {
                        let ms = if target_duration == 0 {
                            2_000
                        } else {
                            (target_duration * 500).max(500)
                        };
                        Duration::from_millis(ms)
                    },
                    Duration::from_millis,
                )
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
                "IPTV channel list detected - refused to parse as HLS MediaPlaylist. \
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

                cached_variants.clone_from(&marked);
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

                let media_body = self.fetch_bytes_with_depth(media_url.as_str(), 1).await?;
                let media = m3u8_rs::parse_media_playlist_res(&media_body)
                    .map_err(|e| eyre!("media playlist parse error: {e}"))?;
                let master_text = String::from_utf8_lossy(&body);
                let _ = self
                    .announce_drm_and_renditions(&master_text, &self.source_url, announced_drm)
                    .await;
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
                    audio_url.as_ref(),
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
                    audio_url.as_ref(),
                    seen_ads,
                    cached_variants,
                )
                .await
            }
        }
    }

    async fn announce_drm_and_renditions(
        &self,
        text: &str,
        playlist_url: &Url,
        announced: &mut bool,
    ) -> crate::models::DrmInfo {
        let mut drm = scan_drm_keys(text);
        let rends = scan_media_renditions(text);
        if *announced {
            return drm;
        }
        if !drm.present && rends.audio.is_empty() && rends.subtitles.is_empty() {
            return drm;
        }
        *announced = true;
        if drm.present {
            self.emit_log(
                LogLevel::Warn,
                DiagCategory::Drm,
                format!(
                    "{} | method={} keyformat={}",
                    drm.badge,
                    drm.method.as_deref().unwrap_or("-"),
                    drm.key_format.as_deref().unwrap_or("-")
                ),
            );
            if self.probe_drm {
                self.probe_drm_license(&mut drm, playlist_url).await;
            }
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
        drm
    }

    async fn probe_drm_license(&self, drm: &mut crate::models::DrmInfo, playlist_url: &Url) {
        let Some(uri) = drm.key_uri.clone() else {
            return;
        };
        let key_url = if uri.starts_with("http://") || uri.starts_with("https://") {
            uri
        } else {
            match playlist_url.join(&uri) {
                Ok(u) => u.to_string(),
                Err(err) => {
                    drm.license_error = Some(format!("resolve key URI: {err}"));
                    return;
                }
            }
        };
        // Same SSRF policy as webhooks: block private/link-local/metadata targets.
        if let Err(err) = crate::engine::webhook::validate_webhook_url(&key_url, false) {
            drm.license_error = Some(format!("DRM probe blocked: {err}"));
            self.emit_log(
                LogLevel::Warn,
                DiagCategory::Drm,
                format!(
                    "License/key probe blocked ({}): {err}",
                    crate::engine::redact::redact_url(&key_url)
                ),
            );
            return;
        }
        let started = Instant::now();
        // Re-validate immediately before request (DNS rebinding mitigation).
        if let Err(err) = crate::engine::webhook::validate_webhook_url(&key_url, false) {
            drm.license_error = Some(format!("DRM probe blocked: {err}"));
            return;
        }

        let clearkey_post = self.clearkey.is_some()
            || drm
                .key_format
                .as_deref()
                .is_some_and(|k| k.to_ascii_lowercase().contains("clearkey"))
            || drm
                .method
                .as_deref()
                .is_some_and(|m| m.eq_ignore_ascii_case("clearkey"));

        if clearkey_post {
            let body = self.clearkey.as_ref().map_or_else(
                || serde_json::json!({ "kids": [], "type": "temporary" }),
                clearkey_license_body,
            );
            drm.license_method = Some("POST".into());
            match crate::engine::network_trace::pinned_post_json(
                &key_url,
                &body,
                false,
                Duration::from_secs(10),
            )
            .await
            {
                Ok(status) => {
                    drm.license_ttfb_ms = Some(started.elapsed().as_millis() as u64);
                    drm.license_http_status = Some(status);
                    self.emit_log(
                        LogLevel::Info,
                        DiagCategory::Drm,
                        format!(
                            "ClearKey license POST {} -> HTTP {} in {}ms",
                            crate::engine::redact::redact_url(&key_url),
                            status,
                            drm.license_ttfb_ms.unwrap_or(0)
                        ),
                    );
                }
                Err(err) => {
                    drm.license_ttfb_ms = Some(started.elapsed().as_millis() as u64);
                    drm.license_error = Some(crate::engine::redact::redact_text(&err.to_string()));
                    self.emit_log(
                        LogLevel::Warn,
                        DiagCategory::Drm,
                        format!(
                            "ClearKey license POST failed ({}): {}",
                            crate::engine::redact::redact_url(&key_url),
                            crate::engine::redact::redact_text(&err.to_string())
                        ),
                    );
                }
            }
            return;
        }

        drm.license_method = Some("GET".into());
        match crate::engine::network_trace::pinned_get_range(
            &key_url,
            Some("bytes=0-0"),
            false,
            Duration::from_secs(10),
            4096,
        )
        .await
        {
            Ok((status, ttfb_ms)) => {
                drm.license_ttfb_ms = Some(ttfb_ms.max(started.elapsed().as_millis() as u64));
                drm.license_http_status = Some(status);
                self.emit_log(
                    LogLevel::Info,
                    DiagCategory::Drm,
                    format!(
                        "License/key probe {} -> HTTP {} in {}ms",
                        crate::engine::redact::redact_url(&key_url),
                        drm.license_http_status.unwrap_or(0),
                        drm.license_ttfb_ms.unwrap_or(0)
                    ),
                );
            }
            Err(err) => {
                drm.license_ttfb_ms = Some(started.elapsed().as_millis() as u64);
                drm.license_error = Some(crate::engine::redact::redact_text(&err.to_string()));
                self.emit_log(
                    LogLevel::Warn,
                    DiagCategory::Drm,
                    format!(
                        "License/key probe failed ({}): {}",
                        crate::engine::redact::redact_url(&key_url),
                        crate::engine::redact::redact_text(&err.to_string())
                    ),
                );
            }
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
        audio_url: Option<&Url>,
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
        linter.lint_ll_hls(&ll);
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
                .map_or_else(|| "n/a".into(), |s| format!("{s:.3}s"));
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

        let renditions = scan_media_renditions(&raw_text);
        let drm = self
            .announce_drm_and_renditions(&raw_text, media_url, announced_drm)
            .await;

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
            ll_dash: LlDashInfo::default(),
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
                if let Ok(mut slot) = self.last_active_ad.lock() {
                    *slot = None;
                }
            }
            let key = ad_log_key(&ad);
            if seen_ads.insert(key) {
                let line = ad
                    .scte35_binary
                    .clone()
                    .unwrap_or_else(|| ad.summary.clone());
                self.emit_log(LogLevel::Warn, DiagCategory::Ad, line);
            }
            if ad.active {
                if let Ok(mut slot) = self.last_active_ad.lock() {
                    *slot = Some(ad.clone());
                }
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
                        "No PDT - estimated latency ~{secs:.2}s (target×{HLS_LIVE_EDGE_SEGMENTS})"
                    ),
                );
                *announced_estimate = true;
            }
        }

        if let Some(audio) = audio_url {
            self.check_av_drift(
                audio,
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
                    // Do not advance last_seen_seq - retry this seq on the next cycle.
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

        linter.on_cdn_headers(&fetch.cdn, fetch.ttfb_ms, media_sequence);

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
        let declared_bw = variants
            .iter()
            .find(|v| v.selected)
            .or_else(|| variants.first())
            .map(|v| v.bandwidth);
        if let Ok(mut ladder) = self.abr_ladder.lock() {
            simulate_segment_fetch(
                vbuf,
                segment.duration,
                fetch.download_ms,
                elapsed,
                download_kbps,
                declared_bw,
                &mut ladder,
            );
        } else {
            vbuf.on_new_segment(segment.duration, elapsed);
        }
        if vbuf.stall_risk_pct > 0 || vbuf.rebuffer_probability_pct > 0 {
            self.emit_log(LogLevel::Warn, DiagCategory::Buffer, vbuf.display());
        }
        for w in lint_abr_player(vbuf) {
            self.emit_log(LogLevel::Warn, DiagCategory::Abr, w);
        }

        let (latency, latency_ms) = segment.program_date_time.as_ref().map_or_else(
            || (LatencyState::Estimated(estimated_ms), Some(estimated_ms)),
            |pdt| {
                let ms = (Utc::now() - pdt.with_timezone(&Utc)).num_milliseconds();
                let ms = if ms < 0 { 0 } else { ms as u64 };
                (LatencyState::Measured(ms), Some(ms))
            },
        );

        let mut wire = fetch.wire.clone();
        if segment.duration > 0.0 {
            self.apply_wire_target_duration(&mut wire, segment.duration);
        }

        self.apply_wire_to_variants(variants, &wire);
        self.record_segment_otel(&fetch);
        if let Some(otel) = &self.otel {
            otel.record_wire_parse(&fetch.segment_url, &wire);
        }
        self.emit_g2g(
            &wire,
            segment
                .program_date_time
                .as_ref()
                .map(|p| p.with_timezone(&Utc)),
            None,
            fetch.ttfb_ms,
        );

        if let Some(pdt) = &segment.program_date_time {
            let pdt_ms = pdt.with_timezone(&Utc).timestamp_millis();
            if let Some(wire_pts_ms) = wire.keyframe_pts_sec.map(|s| s * 1000.0).or_else(|| {
                wire.timing
                    .moof_base_decode_time
                    .zip(wire.timing.moof_timescale)
                    .map(|(b, ts)| b as f64 * 1000.0 / ts as f64)
            }) {
                linter.lint_pdt_wire_drift(pdt_ms, wire_pts_ms, media_sequence);
            }
        }

        self.post_wire_extras(&fetch, &mut wire);

        let video_pts_ms = wire
            .keyframe_pts_sec
            .or(wire.timing.wire_duration_sec)
            .map(|s| (s * 1000.0).round() as u64);
        if let Some(sync) = self
            .probe_subtitle_sync(&fetch.segment_url, video_pts_ms)
            .await
        {
            if sync.desync_warning {
                if let Some(drift) = sync.subtitle_drift_ms {
                    self.emit_log(
                        LogLevel::Warn,
                        DiagCategory::AvSync,
                        format!("Subtitle drift {drift}ms exceeds ±200ms threshold"),
                    );
                }
            }
            for msg in lint_subtitle_drift(&sync) {
                self.emit_log(LogLevel::Warn, DiagCategory::AvSync, msg);
            }
        }

        if let Ok(mut ladder) = self.ladder_scratch.lock() {
            ladder.clear();
            ladder.extend(variants.iter().map(|v| v.bandwidth));
            self.post_segment_diagnostics(
                &fetch,
                segment.duration,
                download_kbps,
                ladder.as_slice(),
            );
        }

        let metrics = SegmentMetrics {
            media_sequence,
            duration_secs: segment.duration,
            size_bytes: fetch.size_bytes,
            transferred_bytes: fetch.transferred_bytes,
            ttfb_ms: fetch.ttfb_ms,
            download_ms: fetch.download_ms,
            dl_to_dur_ratio: SegmentMetrics::compute_dl_to_dur_ratio(
                fetch.download_ms,
                segment.duration,
            ),
            download_kbps,
            latency_ms,
            uri: segment_url.to_string(),
            cdn: fetch.cdn,
            probed: fetch.probed,
            container: fetch.container,
            http_status: fetch.http_status,
            network: Some(fetch.network.clone()),
            wire: Some(wire.clone()),
        };

        let rate_label = metrics.rate_label();
        let net_line = fetch.network.display_line();
        let rtf_line = metrics
            .dl_to_dur_ratio
            .map_or_else(String::new, |r| format!(" | dl_to_dur_ratio={r:.2}"));
        self.send_event(StreamEvent::WireProbe(wire));
        self.send_event(StreamEvent::Segment(metrics));
        self.send_event(StreamEvent::Latency(latency));
        self.send_event(StreamEvent::Buffer(*vbuf));
        self.send_event(StreamEvent::Variants(variants.clone()));

        let mode = if fetch.probed { "probe" } else { "full" };
        self.emit_log(
            LogLevel::Info,
            DiagCategory::Segment,
            format!(
                "seq={media_sequence} {mode} {} declared={}B xfer={}B | {rate_label}{rtf_line} | {net_line}",
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
                codecs: wire.profile_level.clone().or_else(|| wire.codec.clone()),
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
            return Err(eyre!("HTTP {status} - {url}"));
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(std::string::ToString::to_string);
        let body = read_response_bytes_limited(response, MAX_MANIFEST_BYTES)
            .await
            .wrap_err("failed to read body")?;
        Ok((body, content_type))
    }

    async fn fetch_bytes(&self, url: &str) -> Result<Vec<u8>> {
        Ok(self.fetch_manifest(url).await?.0)
    }

    async fn fetch_bytes_with_depth(&self, url: &str, depth: u32) -> Result<Vec<u8>> {
        if depth > MAX_PLAYLIST_DEPTH {
            return Err(eyre!(
                "playlist nesting exceeds MAX_PLAYLIST_DEPTH ({MAX_PLAYLIST_DEPTH})"
            ));
        }
        let _ = depth;
        self.fetch_bytes(url).await
    }

    async fn probe_subtitle_sync(
        &self,
        url: &str,
        video_pts_ms: Option<u64>,
    ) -> Option<crate::models::SubtitleSyncInfo> {
        let lower = url.to_ascii_lowercase();
        let path = std::path::Path::new(url);
        let is_vtt = path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("vtt"))
            || lower.contains("webvtt");
        let is_ttml = path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("ttml"))
            || lower.contains("ttml");
        if !is_vtt && !is_ttml {
            return None;
        }
        let bytes = self.fetch_bytes(url).await.ok()?;
        let probe = probe_subtitle_payload(&bytes);
        Some(compute_subtitle_drift(&probe, video_pts_ms))
    }

    async fn download_segment(&self, url: &str) -> Result<SegmentFetch> {
        if let Some(path) = local_path_from_url(url) {
            return read_local_segment(&path, false).await;
        }
        match traced_get(
            url,
            &self.extra_headers,
            None,
            Some(MAX_SEGMENT_BYTES),
            self.traceparent().as_deref(),
        )
        .await
        {
            Ok(resp) => {
                let code = resp.status;
                if !((200..300).contains(&code)) {
                    return Err(eyre!("segment HTTP {code} - {url}"));
                }
                let cdn = parse_cdn_headers_http(&resp.headers);
                let total = resp.body.len() as u64;
                let head_len = (DEEP_WIRE_PROBE_BYTES as usize + 1).min(resp.body.len());
                let head = resp.body[..head_len].to_vec();
                let mut wire = deep_wire_probe(&head);
                self.finalize_wire(&mut wire);
                let container = if wire.container == ContainerKind::Unknown {
                    inspect_container(&head)
                } else {
                    wire.container
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
                    chunked_transfer: resp.chunked_transfer,
                    segment_url: url.to_string(),
                    probe_bytes: probe_slice(&head),
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
            return Err(eyre!("segment HTTP {status} - {url}"));
        }
        let cdn = parse_cdn_headers(response.headers());
        let chunked = reqwest_headers_chunked(response.headers());
        let ttfb_ms = started.elapsed().as_millis() as u64;
        let mut stream = response.bytes_stream();
        let mut total: u64 = 0;
        let mut head: Vec<u8> = Vec::new();
        let max = MAX_SEGMENT_BYTES;

        while let Some(chunk) = stream.next().await {
            let chunk = chunk.wrap_err("segment stream read error")?;
            if total.saturating_add(chunk.len() as u64) > max as u64 {
                return Err(eyre!("segment exceeds {max} byte limit"));
            }
            if head.len() < DEEP_WIRE_PROBE_BYTES as usize + 1 {
                let take = ((DEEP_WIRE_PROBE_BYTES as usize + 1) - head.len()).min(chunk.len());
                head.extend_from_slice(&chunk[..take]);
            }
            total = total.saturating_add(chunk.len() as u64);
        }

        let download_ms = started.elapsed().as_millis() as u64;
        let mut wire = deep_wire_probe(&head);
        self.finalize_wire(&mut wire);
        let container = if wire.container == ContainerKind::Unknown {
            inspect_container(&head)
        } else {
            wire.container
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
            chunked_transfer: chunked,
            segment_url: url.to_string(),
            probe_bytes: probe_slice(&head),
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
            self.traceparent().as_deref(),
        )
        .await
        {
            Ok(resp) => {
                let code = resp.status;
                if !(code == 200 || code == 206) {
                    return Err(eyre!("probe HTTP {code} - {url}"));
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
                let mut wire = deep_wire_probe(&resp.body);
                self.finalize_wire(&mut wire);
                let container = if wire.container == ContainerKind::Unknown {
                    inspect_container(&resp.body)
                } else {
                    wire.container
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
                    chunked_transfer: resp.chunked_transfer,
                    segment_url: url.to_string(),
                    probe_bytes: probe_slice(&resp.body),
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
            return Err(eyre!("probe HTTP {status} - {url}"));
        }

        let cdn = parse_cdn_headers(response.headers());
        let chunked = reqwest_headers_chunked(response.headers());
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
        let mut wire = deep_wire_probe(&buf);
        self.finalize_wire(&mut wire);
        let container = if wire.container == ContainerKind::Unknown {
            inspect_container(&buf)
        } else {
            wire.container
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
            chunked_transfer: chunked,
            segment_url: url.to_string(),
            probe_bytes: probe_slice(&buf),
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
            return Err(eyre!("LL-HLS hint HTTP {status} - {url}"));
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

pub fn build_http_client(headers: &[String], user_agent: Option<&str>) -> Result<Client> {
    build_http_client_with_timeouts(headers, user_agent, 10, 30)
}

async fn read_response_bytes_limited(response: reqwest::Response, max: usize) -> Result<Vec<u8>> {
    let mut stream = response.bytes_stream();
    let mut buf = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.wrap_err("response stream read error")?;
        if buf.len().saturating_add(chunk.len()) > max {
            return Err(eyre!("response exceeds {max} byte limit"));
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(buf)
}

/// Audit HTTP client with short connect/total timeouts.
pub fn build_audit_http_client(headers: &[String], user_agent: Option<&str>) -> Result<Client> {
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
    user_agent: Option<&str>,
    connect_secs: u64,
    timeout_secs: u64,
) -> Result<Client> {
    let mut builder = Client::builder()
        .user_agent(user_agent.unwrap_or(DEFAULT_UA))
        .gzip(true)
        .brotli(true)
        .redirect(reqwest::redirect::Policy::none())
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
    let mut seen: HashSet<String> = HashSet::new();

    for v in variants {
        if v.is_i_frame || v.uri.is_empty() {
            continue;
        }

        let resolved = resolve_url(base, &v.uri).map_or_else(|_| v.uri.clone(), |u| u.to_string());

        if seen.contains(&resolved) {
            continue;
        }
        seen.insert(resolved.clone());

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
        cf_ray: None,
        akamai_cache_status: None,
        x_cache_hits: None,
        server_timing_edge_ms: None,
        server_timing_origin_ms: None,
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
    let container = if wire.container == ContainerKind::Unknown {
        inspect_container(slice)
    } else {
        wire.container
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
        chunked_transfer: false,
        segment_url: path.display().to_string(),
        probe_bytes: probe_slice(slice),
    })
}

fn probe_slice(bytes: &[u8]) -> Vec<u8> {
    bytes[..bytes.len().min(DEEP_WIRE_PROBE_BYTES as usize)].to_vec()
}

fn parse_cdn_headers_http(headers: &http::HeaderMap) -> CdnEdgeInfo {
    let mut map = HeaderMap::new();
    for (k, v) in headers {
        if let (Ok(name), Ok(val)) = (
            HeaderName::from_bytes(k.as_str().as_bytes()),
            HeaderValue::from_bytes(v.as_bytes()),
        ) {
            map.append(name, val);
        }
    }
    parse_cdn_headers(&map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::get;
    use axum::Router;
    use std::net::SocketAddr;
    use tokio::sync::mpsc;
    use tokio::time::{timeout, Duration as TokioDuration};

    const MEDIA: &str = r"#EXTM3U
#EXT-X-VERSION:3
#EXT-X-TARGETDURATION:4
#EXT-X-MEDIA-SEQUENCE:10
#EXTINF:4.0,
seg.ts
";

    async fn spawn_mock_hls() -> (SocketAddr, tokio::task::JoinHandle<()>) {
        let app = Router::new()
            .route(
                "/index.m3u8",
                get(|| async {
                    (
                        [(
                            axum::http::header::CONTENT_TYPE,
                            "application/vnd.apple.mpegurl",
                        )],
                        MEDIA.to_string(),
                    )
                }),
            )
            .route(
                "/seg.ts",
                get(|| async {
                    let mut body = vec![0x47u8; 188];
                    body[0] = 0x47;
                    body
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        tokio::time::sleep(TokioDuration::from_millis(20)).await;
        (addr, handle)
    }

    #[tokio::test]
    async fn poller_emits_segment_from_local_mock() {
        let (addr, handle) = spawn_mock_hls().await;
        let url = format!("http://{addr}/index.m3u8");
        let (tx, mut rx) = mpsc::channel(64);
        let poller =
            ManifestPoller::new(&url, &[], None, Some(200), true, false, tx).expect("poller");
        let runner = tokio::spawn(async move { poller.run().await });

        let mut saw_segment = false;
        let deadline = TokioDuration::from_secs(3);
        let start = std::time::Instant::now();
        while start.elapsed() < deadline {
            if let Ok(Some(StreamEvent::Segment(_))) =
                timeout(TokioDuration::from_millis(400), rx.recv()).await
            {
                saw_segment = true;
                break;
            }
        }
        runner.abort();
        handle.abort();
        assert!(saw_segment, "expected Segment event from local mock HLS");
    }
}
