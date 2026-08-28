//! Unified glass-to-glass latency: correlate `prft`, HLS PDT, and DASH availability.

use chrono::{DateTime, Utc};

use crate::models::G2gMetrics;

/// Correlate producer reference time, playlist program date, and segment fetch timing.
pub fn compute_g2g(
    prft_ntp_unix_ms: Option<u64>,
    program_date_time: Option<&DateTime<Utc>>,
    dash_segment_available_ms: Option<i64>,
    segment_ttfb_ms: Option<u64>,
    wall_now_ms: u64,
) -> G2gMetrics {
    let origin_available_ms = program_date_time
        .map(|pdt| pdt.timestamp_millis())
        .or(dash_segment_available_ms);

    let ingestion_lag_ms = match (prft_ntp_unix_ms, origin_available_ms) {
        (Some(prft), Some(origin)) => Some(origin - prft as i64),
        _ => None,
    };

    let edge_propagation_ms = segment_ttfb_ms;

    let g2g_total_ms = if let Some(prft) = prft_ntp_unix_ms {
        Some(wall_now_ms as i64 - prft as i64)
    } else if let (Some(origin), Some(ttfb)) = (origin_available_ms, segment_ttfb_ms) {
        Some(wall_now_ms as i64 - origin + ttfb as i64)
    } else {
        origin_available_ms.map(|origin| wall_now_ms as i64 - origin)
    };

    G2gMetrics {
        ingestion_lag_ms,
        edge_propagation_ms,
        g2g_total_ms,
    }
}

pub fn wall_now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn prft_drives_total_g2g() {
        let prft = 1_700_000_000_000u64;
        let now = prft + 5_000;
        let m = compute_g2g(Some(prft), None, None, Some(120), now);
        assert_eq!(m.g2g_total_ms, Some(5_000));
        assert_eq!(m.edge_propagation_ms, Some(120));
    }

    #[test]
    fn pdt_ingestion_lag() {
        let prft = 1_700_000_000_000u64;
        let pdt = Utc.timestamp_millis_opt(prft as i64 + 800).single();
        let m = compute_g2g(Some(prft), pdt.as_ref(), None, Some(50), prft + 2_000);
        assert_eq!(m.ingestion_lag_ms, Some(800));
        assert_eq!(m.edge_propagation_ms, Some(50));
    }
}
