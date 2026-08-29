//! OTLP/HTTP JSON trace export with W3C `traceparent` propagation.

use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use color_eyre::eyre::{eyre, Result, WrapErr};
use serde_json::{json, Value};

use crate::engine::ip_pin::validate_outbound_url;
use crate::engine::network_trace::pinned_post_json;
use crate::engine::redact::redact_url;
use crate::models::{G2gMetrics, NetworkTiming, WireProbeInfo};

const SERVICE_NAME: &str = "streamtop";
const OTEL_PENDING_CAP: usize = 256;

#[derive(Debug, Clone)]
struct SpanRecord {
    name: String,
    start_ns: u128,
    end_ns: u128,
    attributes: Vec<(String, String)>,
    trace_id: String,
    span_id: String,
    parent_span_id: Option<String>,
}

/// W3C trace context shared across outbound probe requests in one session.
#[derive(Debug, Clone)]
pub struct TraceContext {
    pub trace_id: String,
    pub root_span_id: String,
}

impl TraceContext {
    pub fn new() -> Self {
        Self {
            trace_id: random_hex_id(16),
            root_span_id: random_hex_id(8),
        }
    }

    /// Build `traceparent` header for child outbound spans (`00-trace-span-01`).
    pub fn traceparent(&self) -> String {
        let span = random_hex_id(8);
        format!("00-{}-{}-01", self.trace_id, span)
    }

    pub fn trace_id(&self) -> &str {
        &self.trace_id
    }
}

impl Default for TraceContext {
    fn default() -> Self {
        Self::new()
    }
}

/// Buffers spans and POSTs OTLP JSON batches to `{endpoint}/v1/traces`.
pub struct OtelExporter {
    endpoint: String,
    allow_insecure: bool,
    pending: Mutex<Vec<SpanRecord>>,
    trace: Mutex<TraceContext>,
}

impl OtelExporter {
    pub fn new(endpoint: &str, allow_insecure: bool) -> Result<Arc<Self>> {
        let endpoint = endpoint.trim().trim_end_matches('/').to_string();
        if endpoint.is_empty() {
            return Err(eyre!("otel endpoint is empty"));
        }
        if !endpoint.starts_with("http://") && !endpoint.starts_with("https://") {
            return Err(eyre!("otel endpoint must be http(s) URL"));
        }
        validate_outbound_url(&endpoint, allow_insecure)?;
        Ok(Arc::new(Self {
            endpoint,
            allow_insecure,
            pending: Mutex::new(Vec::new()),
            trace: Mutex::new(TraceContext::new()),
        }))
    }

    pub fn traceparent(&self) -> String {
        self.trace
            .lock()
            .map(|t| t.traceparent())
            .unwrap_or_else(|_| "00-00000000000000000000000000000000-0000000000000000-01".into())
    }

    fn push_span(&self, name: &str, start_ns: u128, end_ns: u128, attrs: Vec<(String, String)>) {
        let (trace_id, parent) = self
            .trace
            .lock()
            .map(|t| (t.trace_id.clone(), Some(t.root_span_id.clone())))
            .unwrap_or_else(|_| (random_hex_id(16), None));
        let span_id = random_hex_id(8);
        if let Ok(mut pending) = self.pending.lock() {
            if pending.len() >= OTEL_PENDING_CAP {
                pending.remove(0);
            }
            pending.push(SpanRecord {
                name: name.into(),
                start_ns,
                end_ns,
                attributes: attrs,
                trace_id,
                span_id,
                parent_span_id: parent,
            });
        }
    }

    pub fn record_manifest_resolution(&self, url: &str, duration_ms: u64) {
        let end_ns = now_unix_nano();
        let start_ns = end_ns.saturating_sub(u128::from(duration_ms.max(1)) * 1_000_000);
        self.push_span(
            "manifest.resolve",
            start_ns,
            end_ns,
            vec![("url".into(), redact_url(url))],
        );
    }

    pub fn record_network(&self, span_name: &str, timing: &NetworkTiming, url: &str) {
        let end_ns = now_unix_nano();
        let span_ms = timing.ttfb_ms.max(1);
        let start_ns = end_ns.saturating_sub(u128::from(span_ms) * 1_000_000);
        let mut attrs = vec![
            ("url".into(), redact_url(url)),
            ("ttfb_ms".into(), timing.ttfb_ms.to_string()),
        ];
        if let Some(v) = timing.dns_ms {
            attrs.push(("dns_ms".into(), v.to_string()));
            self.record_stage("dns.lookup", v, url);
        }
        if let Some(v) = timing.tcp_ms {
            attrs.push(("tcp_ms".into(), v.to_string()));
            self.record_stage("tcp.connect", v, url);
        }
        if let Some(v) = timing.tls_ms {
            attrs.push(("tls_ms".into(), v.to_string()));
            self.record_stage("tls.handshake", v, url);
        }
        self.record_stage("http.ttfb", timing.ttfb_ms, url);
        self.push_span(span_name, start_ns, end_ns, attrs);
    }

    fn record_stage(&self, name: &str, ms: u64, url: &str) {
        let end_ns = now_unix_nano();
        let start_ns = end_ns.saturating_sub(u128::from(ms.max(1)) * 1_000_000);
        self.push_span(
            name,
            start_ns,
            end_ns,
            vec![
                ("url".into(), redact_url(url)),
                ("duration_ms".into(), ms.to_string()),
            ],
        );
    }

    pub fn record_segment_download(
        &self,
        url: &str,
        timing: &NetworkTiming,
        download_ms: u64,
        http_status: u16,
        chunked: bool,
    ) {
        let end_ns = now_unix_nano();
        let total_ms = download_ms.max(timing.ttfb_ms).max(1);
        let start_ns = end_ns.saturating_sub(u128::from(total_ms) * 1_000_000);
        let attrs = vec![
            ("url".into(), redact_url(url)),
            ("http.status_code".into(), http_status.to_string()),
            ("download_ms".into(), download_ms.to_string()),
            ("ttfb_ms".into(), timing.ttfb_ms.to_string()),
            ("chunked_transfer".into(), chunked.to_string()),
        ];
        self.push_span("segment.download", start_ns, end_ns, attrs);
    }

    pub fn record_wire_parse(&self, url: &str, wire: &WireProbeInfo) {
        let end_ns = now_unix_nano();
        let start_ns = end_ns.saturating_sub(2_000_000);
        let mut attrs = vec![("url".into(), redact_url(url))];
        if let Some(w) = wire.width {
            attrs.push(("video.width".into(), w.to_string()));
        }
        if let Some(h) = wire.height {
            attrs.push(("video.height".into(), h.to_string()));
        }
        if !wire.pssh.is_empty() {
            attrs.push(("pssh.count".into(), wire.pssh.entries.len().to_string()));
        }
        self.push_span("wire.parse", start_ns, end_ns, attrs);
    }

    pub fn record_g2g(&self, g2g: &G2gMetrics) {
        let end_ns = now_unix_nano();
        let start_ns = end_ns.saturating_sub(1_000_000);
        let mut attrs = Vec::new();
        if let Some(v) = g2g.g2g_total_ms {
            attrs.push(("g2g_total_ms".into(), v.to_string()));
        }
        if let Some(v) = g2g.ingestion_lag_ms {
            attrs.push(("ingestion_lag_ms".into(), v.to_string()));
        }
        if let Some(v) = g2g.edge_propagation_ms {
            attrs.push(("edge_propagation_ms".into(), v.to_string()));
        }
        if !attrs.is_empty() {
            self.push_span("g2g.latency", start_ns, end_ns, attrs);
        }
    }

    pub async fn flush(&self) -> Result<()> {
        let spans = {
            let mut guard = self
                .pending
                .lock()
                .map_err(|_| eyre!("otel span buffer poisoned"))?;
            std::mem::take(&mut *guard)
        };
        if spans.is_empty() {
            return Ok(());
        }
        validate_outbound_url(&self.endpoint, self.allow_insecure)?;
        let payload = build_otlp_payload(&spans);
        let url = format!("{}/v1/traces", self.endpoint);
        let status = pinned_post_json(&url, &payload, self.allow_insecure, Duration::from_secs(10))
            .await
            .wrap_err("otel trace export failed")?;
        if !(200..300).contains(&status) {
            return Err(eyre!("otel trace export HTTP {status}"));
        }
        Ok(())
    }
}

fn now_unix_nano() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

fn random_hex_id(bytes: usize) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static CTR: AtomicU64 = AtomicU64::new(1);
    let n = CTR.fetch_add(1, Ordering::Relaxed);
    format!("{n:0width$x}", width = bytes * 2)
}

fn build_otlp_payload(spans: &[SpanRecord]) -> Value {
    let otel_spans: Vec<Value> = spans
        .iter()
        .map(|s| {
            let attrs: Vec<Value> = s
                .attributes
                .iter()
                .map(|(k, v)| {
                    json!({
                        "key": k,
                        "value": { "stringValue": v }
                    })
                })
                .collect();
            let mut span = json!({
                "traceId": s.trace_id,
                "spanId": s.span_id,
                "name": s.name,
                "kind": 1,
                "startTimeUnixNano": s.start_ns.to_string(),
                "endTimeUnixNano": s.end_ns.to_string(),
                "attributes": attrs,
            });
            if let Some(parent) = &s.parent_span_id {
                span["parentSpanId"] = json!(parent);
            }
            span
        })
        .collect();

    json!({
        "resourceSpans": [{
            "resource": {
                "attributes": [{
                    "key": "service.name",
                    "value": { "stringValue": SERVICE_NAME }
                }]
            },
            "scopeSpans": [{
                "scope": { "name": SERVICE_NAME },
                "spans": otel_spans
            }]
        }]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn traceparent_format() {
        let ctx = TraceContext::new();
        let tp = ctx.traceparent();
        assert!(tp.starts_with("00-"));
        assert_eq!(tp.matches('-').count(), 3);
    }

    #[test]
    fn otlp_payload_has_spans() {
        let spans = vec![SpanRecord {
            name: "dns.lookup".into(),
            start_ns: 100,
            end_ns: 200,
            attributes: vec![("ttfb_ms".into(), "5".into())],
            trace_id: "abc".into(),
            span_id: "def".into(),
            parent_span_id: None,
        }];
        let payload = build_otlp_payload(&spans);
        assert!(payload["resourceSpans"][0]["scopeSpans"][0]["spans"]
            .as_array()
            .is_some_and(|a| !a.is_empty()));
    }
}
