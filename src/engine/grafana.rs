//! Grafana dashboard JSON export for Prometheus metrics.

use color_eyre::eyre::{Result, WrapErr};
use serde_json::{json, Value};
use std::fs;
use std::path::Path;

pub const GRAFANA_DASHBOARD_FILENAME: &str = "streamtop-grafana.json";

/// Write a ready-to-import Grafana dashboard that scrapes streamtop `--prometheus` metrics.
pub fn export_grafana_dashboard(path: impl AsRef<Path>) -> Result<()> {
    let doc = grafana_dashboard_json();
    let text = serde_json::to_string_pretty(&doc).wrap_err("serialize grafana dashboard")?;
    fs::write(path.as_ref(), text)
        .wrap_err_with(|| format!("write {}", path.as_ref().display()))?;
    Ok(())
}

/// Dashboard JSON (Grafana import format: `{ "dashboard": { ... } }`).
pub fn grafana_dashboard_json() -> Value {
    let panels = vec![
        gauge_panel(
            1,
            "Stream Health Index (SHI)",
            "streamtop_stream_health_score",
            [0.0, 0.0, 12.0, 8.0],
            Some((70.0, 90.0)),
        ),
        timeseries_panel(
            2,
            "Segment TTFB (avg)",
            "rate(streamtop_segment_ttfb_seconds_sum[1m])/rate(streamtop_segment_ttfb_seconds_count[1m])",
            "s",
            [12.0, 0.0, 12.0, 8.0],
        ),
        timeseries_panel(
            3,
            "Live-edge latency",
            "streamtop_latency_seconds",
            "s",
            [0.0, 8.0, 12.0, 8.0],
        ),
        timeseries_panel(
            4,
            "Bitstream FPS",
            "streamtop_bitstream_fps",
            "fps",
            [12.0, 8.0, 12.0, 8.0],
        ),
        timeseries_panel(
            5,
            "CDN cache hits",
            "streamtop_cdn_cache_hits_total",
            "short",
            [0.0, 16.0, 12.0, 8.0],
        ),
        timeseries_panel(
            6,
            "CDN cache misses",
            "streamtop_cdn_cache_misses_total",
            "short",
            [12.0, 16.0, 12.0, 8.0],
        ),
        timeseries_panel(
            7,
            "Virtual buffer",
            "streamtop_virtual_buffer_seconds",
            "s",
            [0.0, 24.0, 12.0, 8.0],
        ),
        timeseries_panel(
            8,
            "LL-HLS enabled",
            "streamtop_ll_hls_enabled",
            "short",
            [12.0, 24.0, 12.0, 8.0],
        ),
        timeseries_panel(
            9,
            "Origin stalls",
            "streamtop_origin_stalls_total",
            "short",
            [0.0, 32.0, 12.0, 8.0],
        ),
        timeseries_panel(
            10,
            "HTTP errors",
            "streamtop_http_errors_total",
            "short",
            [12.0, 32.0, 12.0, 8.0],
        ),
        timeseries_panel(
            11,
            "Ad break active",
            "streamtop_ad_active",
            "short",
            [0.0, 40.0, 12.0, 6.0],
        ),
        timeseries_panel(
            12,
            "DRM license TTFB (avg)",
            "rate(streamtop_drm_license_ttfb_seconds_sum[1m])/clamp_min(rate(streamtop_drm_license_ttfb_seconds_count[1m]),1e-9)",
            "s",
            [12.0, 40.0, 12.0, 6.0],
        ),
        timeseries_panel(
            13,
            "LL-HLS part duration (avg)",
            "rate(streamtop_llhls_part_duration_seconds_sum[1m])/clamp_min(rate(streamtop_llhls_part_duration_seconds_count[1m]),1e-9)",
            "s",
            [0.0, 46.0, 12.0, 6.0],
        ),
        timeseries_panel(
            14,
            "Codec mismatch total",
            "streamtop_codec_mismatch_total",
            "short",
            [12.0, 46.0, 12.0, 6.0],
        ),
        timeseries_panel(
            15,
            "Channel dropped events",
            "streamtop_channel_dropped_total",
            "short",
            [0.0, 52.0, 24.0, 6.0],
        ),
    ];

    json!({
        "dashboard": {
            "id": null,
            "uid": "streamtop",
            "title": "streamtop",
            "tags": ["streamtop", "hls", "dash", "prometheus"],
            "timezone": "browser",
            "schemaVersion": 39,
            "version": 1,
            "refresh": "5s",
            "time": { "from": "now-15m", "to": "now" },
            "templating": { "list": [] },
            "annotations": { "list": [] },
            "panels": panels,
            "editable": true,
            "graphTooltip": 1,
            "links": [],
            "fiscalYearStartMonth": 0,
            "liveNow": false,
            "weekStart": ""
        },
        "overwrite": true,
        "folderUid": null
    })
}

fn prom_target(expr: &str, legend: &str) -> Value {
    json!({
        "datasource": { "type": "prometheus", "uid": "${datasource}" },
        "editorMode": "code",
        "expr": expr,
        "legendFormat": legend,
        "range": true,
        "refId": "A"
    })
}

fn timeseries_panel(id: u32, title: &str, metric: &str, unit: &str, grid: [f64; 4]) -> Value {
    let [x, y, w, h] = grid;
    json!({
        "id": id,
        "type": "timeseries",
        "title": title,
        "gridPos": { "x": x, "y": y, "w": w, "h": h },
        "datasource": { "type": "prometheus", "uid": "${datasource}" },
        "fieldConfig": {
            "defaults": {
                "unit": unit,
                "custom": {
                    "drawStyle": "line",
                    "lineInterpolation": "smooth",
                    "fillOpacity": 15,
                    "showPoints": "never"
                }
            },
            "overrides": []
        },
        "options": {
            "legend": { "displayMode": "list", "placement": "bottom" },
            "tooltip": { "mode": "single" }
        },
        "targets": [prom_target(metric, "__auto")]
    })
}

fn gauge_panel(
    id: u32,
    title: &str,
    metric: &str,
    grid: [f64; 4],
    thresholds: Option<(f64, f64)>,
) -> Value {
    let [x, y, w, h] = grid;
    let (yellow, green) = thresholds.unwrap_or((70.0, 90.0));
    json!({
        "id": id,
        "type": "gauge",
        "title": title,
        "gridPos": { "x": x, "y": y, "w": w, "h": h },
        "datasource": { "type": "prometheus", "uid": "${datasource}" },
        "fieldConfig": {
            "defaults": {
                "min": 0,
                "max": 100,
                "unit": "none",
                "thresholds": {
                    "mode": "absolute",
                    "steps": [
                        { "color": "red", "value": null },
                        { "color": "yellow", "value": yellow },
                        { "color": "green", "value": green }
                    ]
                }
            },
            "overrides": []
        },
        "options": {
            "reduceOptions": { "calcs": ["lastNotNull"], "fields": "", "values": false },
            "showThresholdLabels": false,
            "showThresholdMarkers": true
        },
        "targets": [prom_target(metric, "SHI")]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dashboard_includes_core_metrics() {
        let doc = grafana_dashboard_json();
        let text = doc.to_string();
        for needle in [
            "streamtop_stream_health_score",
            "streamtop_segment_ttfb_seconds",
            "streamtop_bitstream_fps",
            "streamtop_cdn_cache_hits_total",
            "streamtop_cdn_cache_misses_total",
            "streamtop_virtual_buffer_seconds",
            "streamtop_drm_license_ttfb_seconds",
            "streamtop_llhls_part_duration_seconds",
            "streamtop_codec_mismatch_total",
            "streamtop_channel_dropped_total",
        ] {
            assert!(text.contains(needle), "missing {needle}");
        }
        assert_eq!(doc["dashboard"]["title"], "streamtop");
    }
}
