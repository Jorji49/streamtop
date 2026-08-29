//! Grafana dashboard JSON export for Prometheus metrics.

use color_eyre::eyre::{Result, WrapErr};
use serde_json::{json, Value};
use std::fs;
use std::path::Path;

pub const GRAFANA_DASHBOARD_FILENAME: &str = "streamtop-grafana.json";

/// Write Grafana dashboard JSON for streamtop `--prometheus` metrics.
pub fn export_grafana_dashboard(path: impl AsRef<Path>) -> Result<()> {
    let doc = grafana_dashboard_json();
    let text = serde_json::to_string_pretty(&doc).wrap_err("serialize grafana dashboard")?;
    fs::write(path.as_ref(), text)
        .wrap_err_with(|| format!("write {}", path.as_ref().display()))?;
    Ok(())
}

fn datasource_variable() -> Value {
    json!({
        "current": {},
        "hide": 0,
        "includeAll": false,
        "label": "Datasource",
        "multi": false,
        "name": "datasource",
        "options": [],
        "query": "prometheus",
        "refresh": 1,
        "regex": "",
        "skipUrlSync": false,
        "type": "datasource"
    })
}

/// Dashboard JSON (Grafana import format: `{ "dashboard": { ... } }`).
pub fn grafana_dashboard_json() -> Value {
    // 24-column grid: health gauges -> latency/G2G -> buffer -> CDN -> errors/ops
    let panels = vec![
        // Row 0 - health gauges
        shi_gauge_panel(1, [0.0, 0.0, 12.0, 8.0]),
        rebuffer_gauge_panel(2, [12.0, 0.0, 12.0, 8.0]),
        // Row 1 - latency / G2G
        timeseries_panel(
            3,
            "Glass-to-Glass Latency (G2G)",
            "streamtop_g2g_total_ms",
            "ms",
            [0.0, 8.0, 12.0, 8.0],
        ),
        timeseries_panel(
            4,
            "Live-edge latency",
            "streamtop_latency_seconds",
            "s",
            [12.0, 8.0, 12.0, 8.0],
        ),
        // Row 2 - TTFB / stall risk
        timeseries_panel(
            5,
            "Segment TTFB (avg)",
            "rate(streamtop_segment_ttfb_seconds_sum[1m])/rate(streamtop_segment_ttfb_seconds_count[1m])",
            "s",
            [0.0, 16.0, 12.0, 8.0],
        ),
        timeseries_panel(
            6,
            "Stall Risk Index",
            "streamtop_stall_risk_index",
            "short",
            [12.0, 16.0, 12.0, 8.0],
        ),
        // Row 3 - buffer dynamics
        timeseries_panel(
            7,
            "Virtual buffer",
            "streamtop_virtual_buffer_seconds",
            "s",
            [0.0, 24.0, 12.0, 8.0],
        ),
        timeseries_panel(
            8,
            "Bitstream FPS",
            "streamtop_bitstream_fps",
            "fps",
            [12.0, 24.0, 12.0, 8.0],
        ),
        // Row 4 - CDN
        timeseries_panel(
            9,
            "CDN cache hits",
            "streamtop_cdn_cache_hits_total",
            "short",
            [0.0, 32.0, 12.0, 8.0],
        ),
        timeseries_panel(
            10,
            "CDN cache misses",
            "streamtop_cdn_cache_misses_total",
            "short",
            [12.0, 32.0, 12.0, 8.0],
        ),
        // Row 5 - origin / HTTP errors
        timeseries_panel(
            11,
            "Origin stalls",
            "streamtop_origin_stalls_total",
            "short",
            [0.0, 40.0, 12.0, 8.0],
        ),
        timeseries_panel(
            12,
            "HTTP errors",
            "streamtop_http_errors_total",
            "short",
            [12.0, 40.0, 12.0, 8.0],
        ),
        // Row 6 - LL-HLS / ad / codec
        timeseries_panel(
            13,
            "LL-HLS enabled",
            "streamtop_ll_hls_enabled",
            "short",
            [0.0, 48.0, 8.0, 6.0],
        ),
        timeseries_panel(
            14,
            "Ad break active",
            "streamtop_ad_active",
            "short",
            [8.0, 48.0, 8.0, 6.0],
        ),
        timeseries_panel(
            15,
            "Codec mismatch total",
            "streamtop_codec_mismatch_total",
            "short",
            [16.0, 48.0, 8.0, 6.0],
        ),
        // Row 7 - DRM / LL-HLS parts
        timeseries_panel(
            16,
            "DRM license TTFB (avg)",
            "rate(streamtop_drm_license_ttfb_seconds_sum[1m])/clamp_min(rate(streamtop_drm_license_ttfb_seconds_count[1m]),1e-9)",
            "s",
            [0.0, 54.0, 12.0, 6.0],
        ),
        timeseries_panel(
            17,
            "LL-HLS part duration (avg)",
            "rate(streamtop_llhls_part_duration_seconds_sum[1m])/clamp_min(rate(streamtop_llhls_part_duration_seconds_count[1m]),1e-9)",
            "s",
            [12.0, 54.0, 12.0, 6.0],
        ),
        // Row 8 - channel drops (full width)
        timeseries_panel(
            18,
            "Channel dropped events",
            "streamtop_channel_dropped_total",
            "short",
            [0.0, 60.0, 24.0, 6.0],
        ),
        // Row 9 - synthetic QoE / TR 101 290
        timeseries_panel(
            19,
            "Synthetic QoE rebuffer risk",
            "streamtop_qoe_rebuffer_risk",
            "percent",
            [0.0, 66.0, 8.0, 6.0],
        ),
        timeseries_panel(
            20,
            "TR 101 290 P1 violations",
            "streamtop_tr101290_p1_violations_total",
            "short",
            [8.0, 66.0, 8.0, 6.0],
        ),
        timeseries_panel(
            21,
            "TR 101 290 P2 violations",
            "streamtop_tr101290_p2_violations_total",
            "short",
            [16.0, 66.0, 8.0, 6.0],
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
            "version": 3,
            "refresh": "5s",
            "time": { "from": "now-15m", "to": "now" },
            "templating": { "list": [datasource_variable()] },
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

fn shi_gauge_panel(id: u32, grid: [f64; 4]) -> Value {
    gauge_panel(
        id,
        "Stream Health Index (SHI)",
        "streamtop_stream_health_score",
        grid,
        0.0,
        100.0,
        "none",
        &[(70.0, "yellow"), (90.0, "green")],
        "SHI",
    )
}

fn rebuffer_gauge_panel(id: u32, grid: [f64; 4]) -> Value {
    gauge_panel(
        id,
        "Rebuffer Probability",
        "streamtop_rebuffer_probability_pct",
        grid,
        0.0,
        100.0,
        "percent",
        &[(10.0, "yellow"), (30.0, "red")],
        "rebuffer",
    )
}

fn gauge_panel(
    id: u32,
    title: &str,
    metric: &str,
    grid: [f64; 4],
    min: f64,
    max: f64,
    unit: &str,
    threshold_steps: &[(f64, &str)],
    legend: &str,
) -> Value {
    let [x, y, w, h] = grid;
    let mut steps = vec![json!({ "color": "green", "value": null })];
    for (value, color) in threshold_steps {
        steps.push(json!({ "color": color, "value": value }));
    }
    json!({
        "id": id,
        "type": "gauge",
        "title": title,
        "gridPos": { "x": x, "y": y, "w": w, "h": h },
        "datasource": { "type": "prometheus", "uid": "${datasource}" },
        "fieldConfig": {
            "defaults": {
                "min": min,
                "max": max,
                "unit": unit,
                "thresholds": {
                    "mode": "absolute",
                    "steps": steps
                }
            },
            "overrides": []
        },
        "options": {
            "reduceOptions": { "calcs": ["lastNotNull"], "fields": "", "values": false },
            "showThresholdLabels": false,
            "showThresholdMarkers": true
        },
        "targets": [prom_target(metric, legend)]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn panel_titles(doc: &Value) -> Vec<String> {
        doc["dashboard"]["panels"]
            .as_array()
            .expect("panels array")
            .iter()
            .filter_map(|p| p["title"].as_str().map(str::to_string))
            .collect()
    }

    fn grid_positions(doc: &Value) -> Vec<(f64, f64, f64, f64)> {
        doc["dashboard"]["panels"]
            .as_array()
            .expect("panels array")
            .iter()
            .map(|p| {
                let g = &p["gridPos"];
                (
                    g["x"].as_f64().unwrap_or(0.0),
                    g["y"].as_f64().unwrap_or(0.0),
                    g["w"].as_f64().unwrap_or(0.0),
                    g["h"].as_f64().unwrap_or(0.0),
                )
            })
            .collect()
    }

    fn rects_overlap(a: (f64, f64, f64, f64), b: (f64, f64, f64, f64)) -> bool {
        let (ax, ay, aw, ah) = a;
        let (bx, by, bw, bh) = b;
        ax < bx + bw && bx < ax + aw && ay < by + bh && by < ay + ah
    }

    #[test]
    fn dashboard_includes_core_metrics() {
        let doc = grafana_dashboard_json();
        let text = doc.to_string();
        for needle in [
            "streamtop_stream_health_score",
            "streamtop_g2g_total_ms",
            "streamtop_rebuffer_probability_pct",
            "streamtop_stall_risk_index",
            "streamtop_segment_ttfb_seconds",
            "streamtop_bitstream_fps",
            "streamtop_cdn_cache_hits_total",
            "streamtop_cdn_cache_misses_total",
            "streamtop_virtual_buffer_seconds",
            "streamtop_drm_license_ttfb_seconds",
            "streamtop_llhls_part_duration_seconds",
            "streamtop_codec_mismatch_total",
            "streamtop_channel_dropped_total",
            "streamtop_qoe_rebuffer_risk",
            "streamtop_tr101290_p1_violations_total",
            "streamtop_tr101290_p2_violations_total",
        ] {
            assert!(text.contains(needle), "missing {needle}");
        }
        assert_eq!(doc["dashboard"]["title"], "streamtop");
    }

    fn find_panel<'a>(doc: &'a Value, title: &str) -> &'a Value {
        doc["dashboard"]["panels"]
            .as_array()
            .expect("panels array")
            .iter()
            .find(|p| p["title"].as_str() == Some(title))
            .unwrap_or_else(|| panic!("panel not found: {title}"))
    }

    #[test]
    fn dashboard_v1_panel_types_and_units() {
        let doc = grafana_dashboard_json();

        let g2g = find_panel(&doc, "Glass-to-Glass Latency (G2G)");
        assert_eq!(g2g["type"], "timeseries");
        assert_eq!(g2g["fieldConfig"]["defaults"]["unit"], "ms");
        assert_eq!(
            g2g["targets"][0]["expr"].as_str(),
            Some("streamtop_g2g_total_ms")
        );

        let rebuf = find_panel(&doc, "Rebuffer Probability");
        assert_eq!(rebuf["type"], "gauge");
        assert_eq!(rebuf["fieldConfig"]["defaults"]["unit"], "percent");
        assert_eq!(rebuf["fieldConfig"]["defaults"]["min"], 0.0);
        assert_eq!(rebuf["fieldConfig"]["defaults"]["max"], 100.0);
        assert_eq!(
            rebuf["targets"][0]["expr"].as_str(),
            Some("streamtop_rebuffer_probability_pct")
        );
        let steps = rebuf["fieldConfig"]["defaults"]["thresholds"]["steps"]
            .as_array()
            .expect("threshold steps");
        assert!(steps.iter().any(|s| {
            s["color"].as_str() == Some("yellow") && s["value"].as_f64() == Some(10.0)
        }));
        assert!(steps
            .iter()
            .any(|s| { s["color"].as_str() == Some("red") && s["value"].as_f64() == Some(30.0) }));

        let stall = find_panel(&doc, "Stall Risk Index");
        assert_eq!(stall["type"], "timeseries");
        assert_eq!(stall["fieldConfig"]["defaults"]["unit"], "short");
        assert_eq!(
            stall["targets"][0]["expr"].as_str(),
            Some("streamtop_stall_risk_index")
        );
    }

    #[test]
    fn dashboard_has_datasource_variable() {
        let doc = grafana_dashboard_json();
        let list = doc["dashboard"]["templating"]["list"]
            .as_array()
            .expect("templating list");
        assert_eq!(list.len(), 1);
        assert_eq!(list[0]["name"], "datasource");
        assert_eq!(list[0]["type"], "datasource");
        assert_eq!(list[0]["query"], "prometheus");
        assert_eq!(list[0]["label"], "Datasource");
        assert_eq!(list[0]["hide"], 0);
        assert_eq!(list[0]["includeAll"], false);
        assert_eq!(list[0]["multi"], false);
        assert_eq!(list[0]["refresh"], 1);
    }

    #[test]
    fn dashboard_v1_panels_present() {
        let doc = grafana_dashboard_json();
        let titles = panel_titles(&doc);
        for expected in [
            "Stream Health Index (SHI)",
            "Rebuffer Probability",
            "Glass-to-Glass Latency (G2G)",
            "Stall Risk Index",
        ] {
            assert!(
                titles.iter().any(|t| t == expected),
                "missing panel title: {expected}"
            );
        }
    }

    #[test]
    fn dashboard_panels_use_datasource_uid() {
        let doc = grafana_dashboard_json();
        for panel in doc["dashboard"]["panels"].as_array().expect("panels") {
            assert_eq!(
                panel["datasource"]["uid"].as_str(),
                Some("${datasource}"),
                "panel {:?} missing datasource uid",
                panel["title"]
            );
        }
    }

    #[test]
    fn dashboard_grid_has_no_overlaps() {
        let doc = grafana_dashboard_json();
        let grids = grid_positions(&doc);
        for i in 0..grids.len() {
            for j in (i + 1)..grids.len() {
                assert!(
                    !rects_overlap(grids[i], grids[j]),
                    "panels {i} and {j} overlap: {:?} vs {:?}",
                    grids[i],
                    grids[j]
                );
            }
        }
    }

    #[test]
    fn export_writes_valid_json() {
        let dir = std::env::temp_dir().join("streamtop-grafana-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("dash.json");
        export_grafana_dashboard(&path).expect("export");
        let raw = std::fs::read_to_string(&path).expect("read");
        let parsed: Value = serde_json::from_str(&raw).expect("valid json");
        assert_eq!(parsed["dashboard"]["version"], 3);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
