use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use color_eyre::eyre::{Result, WrapErr};
use tokio::sync::mpsc::Sender;
use url::Url;

use crate::engine::abr_model::AbrLadderState;
use crate::engine::aes128_probe::Aes128KeyCache;
use crate::engine::agent::AgentMetricsRegistry;
use crate::engine::doh::DohProvider;
use crate::engine::drm_probe::ClearKeySpec;
use crate::engine::gop_tracker::GopCadenceTracker;
use crate::engine::metrics::MetricsSnapshot;
use crate::engine::network_trace::parse_header_pairs;
use crate::engine::otel::OtelExporter;
use crate::engine::sei_probe::SeiProbeAccumulator;
use crate::engine::tr101290::Tr101290Engine;
use crate::engine::wire_timing::WireTimingTracker;
use crate::models::StreamEvent;

use super::http::build_http_client;
use super::{DiagnosticOpts, ManifestPoller};

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
            segment_wall_ms: Arc::new(Mutex::new(0)),
            aes_key_cache: Arc::new(Mutex::new(Aes128KeyCache::new())),
            last_drm: Arc::new(Mutex::new(crate::models::DrmInfo::default())),
            doh_provider: None,
            doh_cache: Arc::new(Mutex::new(None)),
            doh_failed: Arc::new(Mutex::new(false)),
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
    pub fn with_doh_provider(mut self, provider: Option<DohProvider>) -> Self {
        self.doh_provider = provider;
        self
    }

    #[must_use]
    pub fn with_webhook_tx(mut self, hook_tx: Sender<StreamEvent>) -> Self {
        self.hook_tx = Some(hook_tx);
        self
    }
}
