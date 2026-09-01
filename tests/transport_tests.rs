//! Transport, multi-CDN, and TUI cache tests for v1.4.0.

use streamtop::engine::multi_cdn::{compute_skew_from_snapshots, parse_multi_cdn};
use streamtop::engine::redirect::RedirectLoopError;
use streamtop::models::{HttpVersion, MultiCdnEdgeSnapshot, NetworkTiming};
use streamtop::ui::render_cache::UiRenderCache;

#[test]
fn http_version_metric_labels() {
    assert_eq!(HttpVersion::H3.as_metric_label(), "h3");
    assert_eq!(HttpVersion::H2.as_metric_label(), "h2");
    assert_eq!(HttpVersion::H1.as_metric_label(), "h1.1");
}

#[test]
fn redirect_loop_reason_code() {
    assert_eq!(
        RedirectLoopError::reason_code().as_str(),
        "ERR_HTTP_REDIRECT_LOOP"
    );
}

#[test]
fn multi_cdn_skew_threshold() {
    let edges = vec![
        MultiCdnEdgeSnapshot {
            label: "a".into(),
            url: "https://a/x".into(),
            media_sequence: Some(10),
            pdt_offset_ms: Some(1000),
            ..MultiCdnEdgeSnapshot::default()
        },
        MultiCdnEdgeSnapshot {
            label: "b".into(),
            url: "https://b/x".into(),
            media_sequence: Some(13),
            pdt_offset_ms: Some(4000),
            ..MultiCdnEdgeSnapshot::default()
        },
    ];
    let report = compute_skew_from_snapshots(&edges);
    assert_eq!(report.max_skew_ms, 3000);
}

#[test]
fn parse_multi_cdn_comma_list() {
    let urls = parse_multi_cdn("https://a.example/m3u8,https://b.example/m3u8").expect("parse");
    assert_eq!(urls.len(), 2);
}

#[test]
fn render_cache_starts_dirty_for_first_paint() {
    let cache = UiRenderCache::default();
    assert!(cache.is_dirty());
}

#[test]
fn network_timing_transfer_field() {
    let t = NetworkTiming {
        ttfb_ms: 50,
        transfer_ms: Some(120),
        http_version: Some(HttpVersion::H2),
        ..NetworkTiming::default()
    };
    let line = t.display_line();
    assert!(line.contains("HTTP: h2"));
    assert!(line.contains("Xfer: 120ms"));
}
