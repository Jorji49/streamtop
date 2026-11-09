use chrono::Utc;

use crate::engine::channel_stats::record_channel_drop;
use crate::engine::evidence::{header_pairs, ExchangeKind, ExchangeNote};
use crate::engine::g2g::{compute_g2g, wall_now_unix_ms};
use crate::engine::linter::SpecLinter;
use crate::engine::metrics::update_metrics;
use crate::engine::middlebox::classify_transport_failure;
use crate::engine::redirect::RedirectLoopError;
use crate::models::{
    DiagCategory, DiagSeverity, DiagnosticFinding, DiagnosticReasonCode, LogLevel, StreamEvent,
    WireProbeInfo,
};

use super::segment_fetch::SegmentFetch;
use super::ManifestPoller;

impl ManifestPoller {
    pub(super) fn send_event(&self, event: StreamEvent) {
        if let Some(rec) = &self.evidence {
            rec.on_event(&event);
        }
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
    pub(super) fn emit_g2g(
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
    pub(super) fn record_segment_otel(&self, fetch: &SegmentFetch) {
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
    pub(super) fn emit_transport_failure(&self, err: &reqwest::Error) {
        if err.is_redirect() {
            let reason = if err.to_string().contains("cycle") {
                RedirectLoopError::CycleDetected
            } else {
                RedirectLoopError::LimitExceeded
            };
            self.send_event(StreamEvent::Finding(DiagnosticFinding::with_reason_code(
                DiagCategory::Rfc,
                DiagSeverity::Error,
                "redirect_loop",
                reason.message(),
                RedirectLoopError::reason_code(),
            )));
        }
        let io_error = err.is_connect() || err.is_request() || err.is_timeout();
        let hints = classify_transport_failure(true, 0, io_error);
        if hints.tcp_reset_suspected {
            self.send_event(StreamEvent::Finding(DiagnosticFinding::with_reason_code(
                DiagCategory::Rfc,
                DiagSeverity::Error,
                "tcp_io_reset",
                "TCP I/O drop after connect (passive heuristic)",
                DiagnosticReasonCode::ErrTcpIoReset,
            )));
        }
    }

    pub(super) fn emit_log(
        &self,
        level: LogLevel,
        category: DiagCategory,
        message: impl Into<String>,
    ) {
        self.send_event(StreamEvent::Log {
            level,
            category,
            message: message.into(),
        });
    }

    pub(super) fn flush_findings(&self, linter: &mut SpecLinter) {
        for finding in linter.take_findings() {
            let level = match finding.severity {
                DiagSeverity::Info => LogLevel::Info,
                DiagSeverity::Warn => LogLevel::Warn,
                DiagSeverity::Error => LogLevel::Error,
            };
            let log_msg = if let Some(reason) = &finding.reason {
                format!("[{reason}] {}", finding.message)
            } else {
                finding.message.clone()
            };
            self.emit_log(level, finding.category, log_msg);
            self.send_event(StreamEvent::Finding(finding));
        }
    }

    pub(super) fn capture_headers(&self, headers: &http::HeaderMap) -> Vec<(String, String)> {
        if self.evidence.is_none() {
            return Vec::new();
        }
        header_pairs(headers)
    }

    pub(super) fn note_exchange(
        &self,
        kind: ExchangeKind,
        url: &str,
        status: u16,
        ttfb_ms: u64,
        download_ms: u64,
        bytes: u64,
        response_headers: &[(String, String)],
        body: &[u8],
    ) {
        let Some(rec) = &self.evidence else {
            return;
        };
        let saved = match kind {
            ExchangeKind::Manifest => body,
            ExchangeKind::Segment if rec.saves_segment_bytes() => body,
            ExchangeKind::Segment => &[],
        };
        rec.record(&ExchangeNote {
            kind,
            url,
            status,
            ttfb_ms,
            download_ms,
            bytes,
            request_headers: &self.extra_headers,
            response_headers,
            body: saved,
        });
    }
}
