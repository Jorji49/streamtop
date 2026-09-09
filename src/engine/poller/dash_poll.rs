use std::time::{Duration, Instant};

use color_eyre::eyre::{eyre, Result};
use tokio::time::sleep;

use crate::engine::abr_model::simulate_segment_fetch;
use crate::engine::dash::{extract_dash_ad_events, ll_dash_production_drift, parse_dash_mpd};
use crate::engine::linter::{
    analyze_abr_ladder, apply_abr_penalty, lint_abr_player, lint_variant_alignment, SpecLinter,
};
use crate::models::{
    DiagCategory, LatencyState, LlHlsInfo, LogLevel, MediaRenditions, NetworkTiming, PlaylistMeta,
    SegmentMetrics, StreamEvent, StreamStatus, VirtualBuffer,
};

use super::ManifestPoller;

impl ManifestPoller {
    pub(super) async fn run_dash_loop(self) {
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
    pub(super) async fn poll_dash_once(
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
        for finding in lint_variant_alignment(&summary.variants) {
            self.send_event(StreamEvent::Finding(finding));
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

            self.post_segment_diagnostics(&fetch, seg_hint, &fetch.probe_bytes);

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
                        .map(NetworkTiming::display_line)
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
}
