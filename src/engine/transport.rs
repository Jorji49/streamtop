//! HTTP version and QUIC transport telemetry from reqwest responses.

use std::time::Instant;

use crate::models::{HttpVersion, NetworkTiming, QuicTelemetry};

/// Merge reqwest response metadata into network timing.
pub fn timing_from_reqwest_version(
    version: reqwest::Version,
    started: Instant,
    ttfb_ms: u64,
) -> NetworkTiming {
    let download_ms = started.elapsed().as_millis() as u64;
    let http_version = HttpVersion::from_reqwest(version);
    NetworkTiming {
        ttfb_ms,
        transfer_ms: Some(download_ms.saturating_sub(ttfb_ms)),
        http_version: Some(http_version),
        quic: quic_from_version(http_version),
        ..NetworkTiming::default()
    }
}

fn quic_from_version(version: HttpVersion) -> Option<QuicTelemetry> {
    if version == HttpVersion::H3 {
        Some(QuicTelemetry {
            handshake_ms: None,
            used_0rtt: None,
            stream_resets: Some(0),
            packet_loss_pct: None,
        })
    } else {
        None
    }
}

/// Record QUIC handshake timing when HTTP/3 is negotiated.
pub fn apply_quic_handshake(timing: &mut NetworkTiming, handshake_ms: u64, used_0rtt: bool) {
    if timing.http_version != Some(HttpVersion::H3) {
        timing.http_version = Some(HttpVersion::H3);
    }
    let quic = timing.quic.get_or_insert_with(QuicTelemetry::default);
    quic.handshake_ms = Some(handshake_ms);
    quic.used_0rtt = Some(used_0rtt);
}

/// Increment QUIC stream reset counter.
pub fn record_quic_stream_reset(timing: &mut NetworkTiming) {
    if let Some(quic) = &mut timing.quic {
        quic.stream_resets = Some(quic.stream_resets.unwrap_or(0).saturating_add(1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_version_labels() {
        assert_eq!(HttpVersion::H1.as_metric_label(), "h1.1");
        assert_eq!(HttpVersion::H2.as_metric_label(), "h2");
        assert_eq!(HttpVersion::H3.as_metric_label(), "h3");
    }
}
