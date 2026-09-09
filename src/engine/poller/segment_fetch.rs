use std::time::Instant;

use chrono::Utc;
use color_eyre::eyre::{eyre, Result, WrapErr};
use futures::StreamExt;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, RANGE};
use url::Url;

use crate::engine::abr_model::simulate_segment_fetch;
use crate::engine::aes128_probe::{
    decrypt_aes128_cbc_probe, derive_iv, fetch_aes128_key, parse_iv_hex,
};
use crate::engine::container_probe::{
    deep_wire_probe, fill_abr_from_wire, manifest_wire_mismatches,
};
use crate::engine::dai_validator::{
    inband_events_from_wire, validate_ad_alignment, validate_inband_vs_manifest,
};
use crate::engine::drm_probe::{
    apply_clearkey_to_wire, probe_clearkey,
};
use crate::engine::linter::{
    inspect_container, lint_abr_player, lint_subtitle_drift, parse_cdn_headers, SpecLinter,
};
use crate::engine::network_trace::{
    reqwest_headers_chunked, timing_from_ttfb, traced_get,
};
use crate::engine::playlist_parser::local_path_from_url;
use crate::engine::subtitle_probe::{compute_subtitle_drift, probe_subtitle_payload};
use crate::engine::tr101290::probe_container_tr101290;
use crate::engine::transport::timing_from_reqwest_version;
use crate::engine::wire_timing::WireTimingTracker;
use crate::models::{
    AbrVariant, CdnEdgeInfo, ContainerKind, DiagCategory, DiagSeverity, DiagnosticFinding,
    DiagnosticReasonCode, LatencyState, LogLevel,
    NetworkTiming, SegmentMetrics, StreamEvent,
    VirtualBuffer, WireProbeInfo, DEEP_WIRE_PROBE_BYTES, MAX_SEGMENT_BYTES,
};

use super::ManifestPoller;

use super::http::resolve_url;

#[derive(Debug)]
pub(super) struct SegmentFetch {
    pub(super) size_bytes: u64,
    pub(super) transferred_bytes: u64,
    pub(super) ttfb_ms: u64,
    pub(super) download_ms: u64,
    pub(super) cdn: CdnEdgeInfo,
    pub(super) container: ContainerKind,
    pub(super) probed: bool,
    pub(super) http_status: u16,
    pub(super) network: NetworkTiming,
    pub(super) wire: WireProbeInfo,
    pub(super) chunked_transfer: bool,
    pub(super) segment_url: String,
    pub(super) probe_bytes: Vec<u8>,
}

impl ManifestPoller {
    pub(super) fn merge_wire_pssh(drm: &mut crate::models::DrmInfo, wire: &WireProbeInfo) {
        if wire.pssh.is_empty() {
            return;
        }
        if let Some(ref mut existing) = drm.pssh {
            existing.merge(wire.pssh.clone());
        } else {
            drm.pssh = Some(wire.pssh.clone());
        }
    }

    pub(super) fn post_segment_diagnostics(
        &self,
        fetch: &SegmentFetch,
        duration_secs: f32,
        probe_bytes: &[u8],
    ) {
        let d = &self.diagnostics;
        if !d.tr101290 && !d.probe_sei {
            return;
        }
        if probe_bytes.is_empty() {
            return;
        }
        let wall_ms = self.segment_wall_ms.lock().map_or(0, |mut w| {
            *w = w.saturating_add((f64::from(duration_secs.max(0.001)) * 1000.0) as u64);
            *w
        });
        if d.tr101290 {
            if let Ok(mut eng) = self.tr101290.lock() {
                if let Some(report) =
                    probe_container_tr101290(&mut eng, probe_bytes, fetch.container, wall_ms)
                {
                    for check in &report.checks {
                        if let Some(code) = DiagnosticReasonCode::from_tr101290_rule(&check.code) {
                            self.send_event(StreamEvent::Finding(
                                DiagnosticFinding::with_reason_code(
                                    DiagCategory::Info,
                                    if check.priority == 1 {
                                        DiagSeverity::Error
                                    } else {
                                        DiagSeverity::Warn
                                    },
                                    check.code.clone(),
                                    check.message.clone(),
                                    code,
                                ),
                            ));
                        }
                    }
                    self.send_event(StreamEvent::Tr101290(report));
                }
            }
        }
        if d.probe_sei {
            if let Ok(mut acc) = self.sei_acc.lock() {
                let sei = acc.ingest(probe_bytes, fetch.container);
                if sei.nal_units_scanned > 0
                    || sei.cea608_present
                    || sei.cea708_present
                    || sei.hdr10_present
                {
                    self.send_event(StreamEvent::SeiProbe(sei));
                }
            }
        }
    }

    pub(super) async fn maybe_decrypt_aes128_probe(
        &self,
        fetch: &SegmentFetch,
        media_sequence: u64,
    ) -> Vec<u8> {
        let drm = self.last_drm.lock().ok().map(|g| g.clone());
        let Some(drm) = drm else {
            return fetch.probe_bytes.clone();
        };
        let Some(method) = drm.method.as_deref() else {
            return fetch.probe_bytes.clone();
        };
        if !method.eq_ignore_ascii_case("AES-128") {
            return fetch.probe_bytes.clone();
        }
        let Some(uri) = drm.key_uri.as_deref() else {
            return fetch.probe_bytes.clone();
        };
        let explicit_iv = drm.key_iv.as_deref().and_then(|s| parse_iv_hex(s).ok());
        let iv = derive_iv(media_sequence, explicit_iv);
        let key = match fetch_aes128_key(&self.client, uri, &self.aes_key_cache).await {
            Ok(k) => k,
            Err(e) => {
                self.send_event(StreamEvent::Finding(DiagnosticFinding::with_reason_code(
                    DiagCategory::Drm,
                    DiagSeverity::Warn,
                    "AES128_KEY_FETCH",
                    format!("AES-128 key fetch failed: {e:#}"),
                    DiagnosticReasonCode::ErrAesKeyFetchFailed,
                )));
                return fetch.probe_bytes.clone();
            }
        };
        match decrypt_aes128_cbc_probe(&key, &iv, &fetch.probe_bytes) {
            Ok(plain) => plain,
            Err(e) => {
                self.emit_log(
                    LogLevel::Warn,
                    DiagCategory::Drm,
                    format!("AES-128 probe decrypt failed: {e:#}"),
                );
                fetch.probe_bytes.clone()
            }
        }
    }

    pub(super) fn post_wire_extras(&self, fetch: &SegmentFetch, wire: &mut WireProbeInfo) {
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

    pub(super) fn finalize_wire(&self, wire: &mut WireProbeInfo) {
        if let Ok(mut tracker) = self.gop_tracker.lock() {
            tracker.observe_keyframe(wire.keyframe_pts_sec);
            tracker.apply(wire);
        }
        if let Ok(mut timing) = self.wire_timing_tracker.lock() {
            timing.apply(&mut wire.timing, None);
            timing.observe_segment(&wire.timing, wire.keyframe_pts_sec);
        }
    }

    pub(super) fn apply_wire_target_duration(&self, wire: &mut WireProbeInfo, target_secs: f32) {
        WireTimingTracker::apply_target(&mut wire.timing, Some(target_secs));
        if let Some(label) = wire.timing.timing_label() {
            self.emit_log(
                LogLevel::Warn,
                DiagCategory::Segment,
                format!("Wire timing: {label}"),
            );
        }
    }
    pub(super) async fn process_segment(
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

        let probe_plain = self
            .maybe_decrypt_aes128_probe(&fetch, media_sequence)
            .await;
        self.post_segment_diagnostics(&fetch, segment.duration, &probe_plain);

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
        self.send_event(StreamEvent::Segment(metrics.clone()));
        self.send_event(StreamEvent::Latency(latency));
        self.send_event(StreamEvent::Buffer(*vbuf));
        self.send_event(StreamEvent::Variants(variants.clone()));

        if metrics.dl_to_dur_state() == Some(crate::models::DlToDurState::Draining) {
            if let Some(ratio) = metrics.dl_to_dur_ratio {
                self.send_event(StreamEvent::Finding(DiagnosticFinding::with_reason_code(
                    DiagCategory::Stalling,
                    DiagSeverity::Warn,
                    "RTF_STALL",
                    format!("dl_to_dur_ratio {ratio:.2} indicates buffer drain risk"),
                    DiagnosticReasonCode::ErrRtfStallRisk,
                )));
            }
        }

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

    pub(super) fn apply_wire_to_variants(&self, variants: &mut Vec<AbrVariant>, wire: &WireProbeInfo) {
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
    pub(super) async fn probe_subtitle_sync(
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

    pub(super) async fn download_segment(&self, url: &str) -> Result<SegmentFetch> {
        if let Some(path) = local_path_from_url(url) {
            let mut fetch = read_local_segment(&path, false).await?;
            self.apply_doh_timing(url, &mut fetch.network).await;
            return Ok(fetch);
        }
        if let Ok(resp) = traced_get(
            url,
            &self.extra_headers,
            None,
            Some(MAX_SEGMENT_BYTES),
            self.traceparent().as_deref(),
        )
        .await
        {
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
            let mut fetch = SegmentFetch {
                size_bytes: total,
                transferred_bytes: total,
                ttfb_ms: resp.timing.ttfb_ms,
                download_ms: resp.download_ms,
                cdn,
                container,
                probed: false,
                http_status: code,
                network: resp.timing.clone(),
                wire,
                chunked_transfer: resp.chunked_transfer,
                segment_url: url.to_string(),
                probe_bytes: probe_slice(&head),
            };
            self.apply_doh_timing(url, &mut fetch.network).await;
            self.send_event(StreamEvent::Transport(fetch.network.clone()));
            Ok(fetch)
        } else {
            let mut fetch = self.download_segment_reqwest(url).await?;
            self.apply_doh_timing(url, &mut fetch.network).await;
            Ok(fetch)
        }
    }

    pub(super) async fn download_segment_reqwest(&self, url: &str) -> Result<SegmentFetch> {
        let started = Instant::now();
        let response = match self.client.get(url).send().await {
            Ok(r) => r,
            Err(e) => {
                self.emit_transport_failure(&e);
                return Err(e).wrap_err_with(|| format!("segment GET failed: {url}"));
            }
        };
        let version = response.version();
        let status = response.status();
        let code = status.as_u16();
        if !status.is_success() {
            return Err(eyre!("segment HTTP {status} - {url}"));
        }
        let cdn = parse_cdn_headers(response.headers());
        let chunked = reqwest_headers_chunked(response.headers());
        let ttfb_ms = started.elapsed().as_millis() as u64;
        let content_length = response.content_length();
        let mut stream = response.bytes_stream();
        let mut total: u64 = 0;
        let mut head: Vec<u8> = Vec::new();
        let max = MAX_SEGMENT_BYTES;
        let probe_cap = DEEP_WIRE_PROBE_BYTES as usize + 1;

        while let Some(chunk) = stream.next().await {
            let chunk = chunk.wrap_err("segment stream read error")?;
            if total.saturating_add(chunk.len() as u64) > max as u64 {
                return Err(eyre!("segment exceeds {max} byte limit"));
            }
            if head.len() < probe_cap {
                let take = (probe_cap - head.len()).min(chunk.len());
                head.extend_from_slice(&chunk[..take]);
            }
            total = total.saturating_add(chunk.len() as u64);
            if content_length.is_none() && head.len() >= probe_cap {
                break;
            }
        }

        let download_ms = started.elapsed().as_millis() as u64;
        let mut network = timing_from_reqwest_version(version, started, ttfb_ms);
        network.transfer_ms = Some(download_ms.saturating_sub(ttfb_ms));
        if version == reqwest::Version::HTTP_3 {
            crate::engine::transport::apply_quic_handshake(&mut network, ttfb_ms, false);
        }
        self.apply_doh_timing(url, &mut network).await;
        self.send_event(StreamEvent::Transport(network.clone()));
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
            network,
            wire,
            chunked_transfer: chunked,
            segment_url: url.to_string(),
            probe_bytes: probe_slice(&head),
        })
    }

    pub(super) async fn probe_segment(&self, url: &str) -> Result<SegmentFetch> {
        if let Some(path) = local_path_from_url(url) {
            let mut fetch = read_local_segment(&path, true).await?;
            self.apply_doh_timing(url, &mut fetch.network).await;
            return Ok(fetch);
        }
        let range = format!("bytes=0-{DEEP_WIRE_PROBE_BYTES}");
        if let Ok(resp) = traced_get(
            url,
            &self.extra_headers,
            Some(&range),
            Some(DEEP_WIRE_PROBE_BYTES as usize + 1),
            self.traceparent().as_deref(),
        )
        .await
        {
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
            let mut fetch = SegmentFetch {
                size_bytes,
                transferred_bytes: transferred,
                ttfb_ms: resp.timing.ttfb_ms,
                download_ms: resp.download_ms,
                cdn,
                container,
                probed: true,
                http_status: code,
                network: resp.timing.clone(),
                wire,
                chunked_transfer: resp.chunked_transfer,
                segment_url: url.to_string(),
                probe_bytes: probe_slice(&resp.body),
            };
            self.apply_doh_timing(url, &mut fetch.network).await;
            self.send_event(StreamEvent::Transport(fetch.network.clone()));
            Ok(fetch)
        } else {
            let mut fetch = self.probe_segment_reqwest(url).await?;
            self.apply_doh_timing(url, &mut fetch.network).await;
            Ok(fetch)
        }
    }

    pub(super) async fn probe_segment_reqwest(&self, url: &str) -> Result<SegmentFetch> {
        let started = Instant::now();
        let range = format!("bytes=0-{DEEP_WIRE_PROBE_BYTES}");
        let response = match self.client.get(url).header(RANGE, range).send().await {
            Ok(r) => r,
            Err(e) => {
                self.emit_transport_failure(&e);
                return Err(e).wrap_err_with(|| format!("range probe failed: {url}"));
            }
        };
        let version = response.version();

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
        let mut network = timing_from_reqwest_version(version, started, ttfb_ms);
        network.transfer_ms = Some(download_ms.saturating_sub(ttfb_ms));
        if version == reqwest::Version::HTTP_3 {
            crate::engine::transport::apply_quic_handshake(&mut network, ttfb_ms, false);
        }
        self.send_event(StreamEvent::Transport(network.clone()));
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
            network,
            wire,
            chunked_transfer: chunked,
            segment_url: url.to_string(),
            probe_bytes: probe_slice(&buf),
        })
    }
}

pub(super) fn local_cdn() -> CdnEdgeInfo {
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

pub(super) async fn read_local_segment(path: &std::path::Path, probe: bool) -> Result<SegmentFetch> {
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

pub(super) fn probe_slice(bytes: &[u8]) -> Vec<u8> {
    bytes[..bytes.len().min(DEEP_WIRE_PROBE_BYTES as usize)].to_vec()
}

pub(super) fn parse_cdn_headers_http(headers: &http::HeaderMap) -> CdnEdgeInfo {
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
