use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use color_eyre::eyre::{eyre, Result};
use reqwest::Client;
use tokio::sync::mpsc::Sender;
use url::Url;

use crate::engine::abr_model::AbrLadderState;
use crate::engine::aes128_probe::Aes128KeyCache;
use crate::engine::agent::AgentMetricsRegistry;
use crate::engine::dash::looks_like_dash;
use crate::engine::doh::DohProvider;
use crate::engine::drm_probe::{clearkey_license_body, ClearKeySpec};
use crate::engine::gop_tracker::GopCadenceTracker;
use crate::engine::metrics::MetricsSnapshot;
use crate::engine::otel::OtelExporter;
use crate::engine::playlist_parser::is_iptv_channel_list;
use crate::engine::sei_probe::SeiProbeAccumulator;
use crate::engine::tr101290::Tr101290Engine;
use crate::engine::wire_timing::WireTimingTracker;
use crate::models::{
    DiagCategory, LogLevel, StreamEvent, StreamProtocol, StreamStatus, DEEP_WIRE_PROBE_BYTES,
};

mod ctor;
mod dash_poll;
mod events;
mod hls_poll;
mod http;
mod segment_fetch;

pub use hls_poll::collect_variants;
pub use http::{build_audit_http_client, build_http_client};

#[derive(Debug, Clone, Default)]
pub struct DiagnosticOpts {
    pub tr101290: bool,
    pub probe_sei: bool,
}

pub struct ManifestPoller {
    pub(super) client: Client,
    pub(super) source_url: Url,
    pub(super) interval: Option<Duration>,
    pub(super) probe_headers: bool,
    pub(super) probe_drm: bool,
    pub(super) extra_headers: Vec<(String, String)>,
    pub(super) tx: Sender<StreamEvent>,
    pub(super) hook_tx: Option<Sender<StreamEvent>>,
    pub(super) metrics: Option<Arc<RwLock<MetricsSnapshot>>>,
    pub(super) agent_metrics: Option<(Arc<RwLock<AgentMetricsRegistry>>, String)>,
    pub(super) gop_tracker: Arc<Mutex<GopCadenceTracker>>,
    pub(super) wire_timing_tracker: Arc<Mutex<WireTimingTracker>>,
    pub(super) abr_ladder: Arc<Mutex<AbrLadderState>>,
    pub(super) otel: Option<Arc<OtelExporter>>,
    pub(super) diagnostics: DiagnosticOpts,
    pub(super) clearkey: Option<ClearKeySpec>,
    pub(super) last_active_ad: Arc<Mutex<Option<crate::models::AdBreakInfo>>>,
    pub(super) tr101290: Arc<Mutex<Tr101290Engine>>,
    pub(super) sei_acc: Arc<Mutex<SeiProbeAccumulator>>,
    pub(super) segment_wall_ms: Arc<Mutex<u64>>,
    pub(super) aes_key_cache: Arc<Mutex<Aes128KeyCache>>,
    pub(super) last_drm: Arc<Mutex<crate::models::DrmInfo>>,
    pub(super) doh_provider: Option<DohProvider>,
    pub(super) doh_cache: Arc<Mutex<Option<u64>>>,
    pub(super) doh_failed: Arc<Mutex<bool>>,
}

impl ManifestPoller {
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
    pub(super) async fn probe_drm_license(
        &self,
        drm: &mut crate::models::DrmInfo,
        playlist_url: &Url,
    ) {
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
