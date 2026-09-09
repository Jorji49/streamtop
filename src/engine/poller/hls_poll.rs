use std::collections::HashSet;
use std::time::{Duration, Instant};

use chrono::Utc;
use color_eyre::eyre::{eyre, Result, WrapErr};
use futures::StreamExt;
use m3u8_rs::{AlternativeMediaType, Playlist, VariantStream};
use reqwest::header::RANGE;
use tokio::time::sleep;
use url::Url;

use crate::engine::linter::{
    ad_log_key, analyze_abr_ladder, apply_abr_penalty, apply_hls_blocking_params,
    extract_ad_signals_near_live_edge,
    lint_variant_alignment, ll_hls_probe_range, next_blocking_targets,
    scan_drm_keys, scan_ll_hls, scan_media_renditions, SpecLinter,
};
use crate::engine::playlist_parser::{is_iptv_channel_list, local_path_from_url};
use crate::models::{
    AbrVariant, DiagCategory, DiagSeverity, DiagnosticFinding,
    DiagnosticReasonCode, LatencyState, LlDashInfo, LogLevel, PlaylistMeta, StreamEvent, StreamStatus,
    VirtualBuffer, AD_SCAN_LIVE_EDGE_SEGMENTS,
    HLS_LIVE_EDGE_SEGMENTS,
    MEDIA_SEQ_GAP_TOLERANCE,
};

use super::ManifestPoller;

use super::http::resolve_url;

#[derive(Debug, Default)]
pub(super) struct LlHlsBlockingState {
    pub(super) is_ll_hls: bool,
    pub(super) can_block_reload: bool,
    pub(super) blocking_msn: Option<u64>,
    pub(super) blocking_part: Option<u64>,
    pub(super) part_interval_ms: Option<u64>,
}

#[derive(Debug, Clone, Copy, Default)]
pub(super) struct LlHlsProbeStats {
    pub(super) transfer_kbps: f64,
    pub(super) ttfb_ms: u64,
    pub(super) download_ms: u64,
    #[allow(dead_code)]
    pub(super) transferred_bytes: u64,
}

impl ManifestPoller {
    pub(super) async fn run_hls_loop(self) {
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
    pub(super) async fn poll_once(
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
                for finding in lint_variant_alignment(&marked) {
                    self.send_event(StreamEvent::Finding(finding));
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

    pub(super) async fn announce_drm_and_renditions(
        &self,
        text: &str,
        playlist_url: &Url,
        announced: &mut bool,
    ) -> crate::models::DrmInfo {
        let mut drm = scan_drm_keys(text);
        if let Ok(mut slot) = self.last_drm.lock() {
            *slot = drm.clone();
        }
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
    pub(super) async fn handle_media(
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
                        let part_dur = ll
                            .last_part_duration_secs
                            .or(ll.part_target_secs)
                            .unwrap_or(0.33);
                        let part_metrics = crate::models::LlHlsPartMetrics {
                            part_sequence: ll_meta.last_part_sequence.unwrap_or(0),
                            ttfb_ms: probe.ttfb_ms,
                            download_ms: probe.download_ms,
                            part_duration_secs: part_dur,
                            part_dl_duration_ratio:
                                crate::models::LlHlsPartMetrics::compute_part_rtf(
                                    probe.download_ms,
                                    part_dur,
                                ),
                            transfer_kbps: Some(probe.transfer_kbps),
                        };
                        let rtf_line = part_metrics
                            .part_dl_duration_ratio
                            .map_or_else(|| "-".into(), |r| format!("{r:.2}"));
                        if let Some(ratio) = part_metrics.part_dl_duration_ratio {
                            if ratio > 1.0 {
                                self.send_event(StreamEvent::Finding(
                                    DiagnosticFinding::with_reason_code(
                                        DiagCategory::LlHls,
                                        DiagSeverity::Warn,
                                        "PART_RTF_STALL",
                                        format!(
                                            "LL-HLS part RTF {ratio:.2} > 1.0 (part seq={})",
                                            part_metrics.part_sequence
                                        ),
                                        DiagnosticReasonCode::ErrPartRtfStall,
                                    ),
                                ));
                            }
                        }
                        self.send_event(StreamEvent::LlHlsPart(part_metrics));
                        self.emit_log(
                            LogLevel::Info,
                            DiagCategory::LlHls,
                            format!(
                                "PART/HINT probe | seq={} | ttfb={}ms | dl={}ms | rtf={rtf_line} | {:.0} kbps | {}",
                                ll_meta.last_part_sequence.unwrap_or(0),
                                probe.ttfb_ms,
                                probe.download_ms,
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

    pub(super) async fn check_av_drift(
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
    pub(super) async fn probe_ll_hls_hint(
        &self,
        url: &str,
        offset: Option<u64>,
        length: Option<u64>,
    ) -> Result<LlHlsProbeStats> {
        if local_path_from_url(url).is_some() {
            return Ok(LlHlsProbeStats::default());
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
        let max_read = length.map_or(65536, |l| l as usize).clamp(512, 65536);
        let mut stream = response.bytes_stream();
        let mut buf = Vec::new();
        let mut ttfb_ms = None::<u64>;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.wrap_err("LL-HLS part stream read error")?;
            if ttfb_ms.is_none() {
                ttfb_ms = Some(started.elapsed().as_millis() as u64);
            }
            if buf.len().saturating_add(chunk.len()) > max_read {
                break;
            }
            buf.extend_from_slice(&chunk);
        }
        let download_ms = started.elapsed().as_millis().max(1) as u64;
        let bytes = buf.len() as u64;
        let transfer_kbps = (bytes as f64 * 8.0) / download_ms as f64;
        Ok(LlHlsProbeStats {
            transfer_kbps,
            ttfb_ms: ttfb_ms.unwrap_or(download_ms),
            download_ms,
            transferred_bytes: bytes,
        })
    }
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
