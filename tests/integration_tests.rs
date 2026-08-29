//! Integration tests: input router classification against fixtures.

mod mock_server;

use std::fs;
use std::path::PathBuf;

use mock_server::MockScenario;
use streamtop::engine::dash::{classify_content_protection, parse_dash_mpd};
use streamtop::engine::g2g::compute_g2g;
use streamtop::engine::linter::{lint_subtitle_drift, next_blocking_targets, scan_ll_hls};
use streamtop::engine::playlist_parser::{detect_and_parse, ParsedInput};
use streamtop::engine::pssh::scan_pssh_boxes;
use streamtop::engine::subtitle_probe::{compute_subtitle_drift, probe_subtitle_payload};
use url::Url;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

fn load(name: &str) -> Vec<u8> {
    fs::read(fixture(name)).unwrap_or_else(|e| panic!("load {name}: {e}"))
}

#[test]
fn router_hls_master_is_single_stream() {
    let body = load("hls_master.m3u8");
    let parsed = detect_and_parse("https://cdn.example/master.m3u8", &body, None).unwrap();
    assert!(
        matches!(parsed, ParsedInput::SingleStream { .. }),
        "expected SingleStream for HLS master"
    );
}

#[test]
fn router_hls_media_with_targetduration_is_single_stream() {
    let body = load("hls_media.m3u8");
    let parsed = detect_and_parse("https://cdn.example/index.m3u8", &body, None).unwrap();
    assert!(matches!(parsed, ParsedInput::SingleStream { .. }));
}

#[test]
fn router_ll_hls_media_is_single_stream() {
    let body = load("ll_hls_media.m3u8");
    let parsed = detect_and_parse("https://cdn.example/ll.m3u8", &body, None).unwrap();
    assert!(matches!(parsed, ParsedInput::SingleStream { .. }));

    let text = String::from_utf8_lossy(&body);
    let ll = scan_ll_hls(&text);
    assert!(ll.is_ll_hls);
    assert!(ll.can_block_reload);
    assert!(ll.has_preload_hint);
    assert!(ll.part_count >= 2);
    assert_eq!(ll.last_part_sequence, Some(ll.part_count));
    assert_eq!(ll.last_part_duration_ms, Some(333));
    let badge = ll.header_badge().unwrap();
    assert!(badge.contains("[LL-HLS]"));
    assert!(badge.contains("333ms"));
    assert!(badge.contains("seq="));

    let (msn, part) = next_blocking_targets(100, 1, ll.part_count);
    assert_eq!(msn, 100);
    assert_eq!(part, Some(u64::from(ll.part_count)));
}

#[test]
fn router_iptv_m3u_is_channel_list_never_hls() {
    let body = load("iptv_channels.m3u");
    let parsed = detect_and_parse(
        "https://raw.githubusercontent.com/example/tr.m3u",
        &body,
        None,
    )
    .unwrap();
    match parsed {
        ParsedInput::IptvChannels { channels, .. } => {
            assert_eq!(channels.len(), 2);
            assert!(channels[0].name.contains("TRT"));
        }
        ParsedInput::SingleStream { .. } => panic!("IPTV must not become SingleStream"),
    }
}

#[test]
fn router_dash_mpd_is_single_stream() {
    let body = load("dash_live.mpd");
    let parsed = detect_and_parse(
        "https://cdn.example/live.mpd",
        &body,
        Some("application/dash+xml"),
    )
    .unwrap();
    assert!(matches!(parsed, ParsedInput::SingleStream { .. }));
}

#[test]
fn dash_fixture_parses_widevine_content_protection() {
    let body = load("dash_live.mpd");
    let xml = String::from_utf8_lossy(&body);
    let base = Url::parse("https://cdn.example/live.mpd").unwrap();
    let summary = parse_dash_mpd(&xml, &base).unwrap();
    assert!(summary.type_live);
    assert!(summary.drm.present);
    assert_eq!(summary.drm.method.as_deref(), Some("Widevine"));
    assert!(!summary.variants.is_empty());
    // Representation omits timescale; must inherit 90000 from AdaptationSet → 10s
    assert!((summary.segment_duration_hint_secs - 10.0).abs() < 0.01);
    assert_eq!(summary.variants[0].frame_rate, Some(25.0));

    let d = classify_content_protection("urn:uuid:9a04f079-9840-4286-ab92-e65be0885f95").unwrap();
    assert_eq!(d.method.as_deref(), Some("PlayReady"));
}

#[test]
fn g2g_correlates_prft_and_pdt() {
    use chrono::{TimeZone, Utc};
    let prft = 1_700_000_000_000u64;
    let pdt = Utc.timestamp_millis_opt(prft as i64 + 500).single();
    let m = compute_g2g(Some(prft), pdt.as_ref(), None, Some(80), prft + 3_000);
    assert_eq!(m.ingestion_lag_ms, Some(500));
    assert_eq!(m.edge_propagation_ms, Some(80));
    assert_eq!(m.g2g_total_ms, Some(3_000));
}

#[tokio::test]
async fn mock_chaos_subtitle_drift_integrates() {
    let server = mock_server::MockStreamServer::start_with(MockScenario::SubtitleDrift).await;
    let vtt_url = format!("{}/sub.vtt", server.base_url);
    let body = reqwest::Client::new()
        .get(&vtt_url)
        .send()
        .await
        .expect("get")
        .bytes()
        .await
        .expect("bytes");
    let probe = probe_subtitle_payload(&body);
    let sync = compute_subtitle_drift(&probe, Some(1000));
    assert!(sync.desync_warning);
    assert!(!lint_subtitle_drift(&sync).is_empty());
}

#[test]
fn corrupt_pssh_scan_marks_invalid() {
    let server_body = mock_server::corrupt_pssh_segment();
    let info = scan_pssh_boxes(server_body.as_bytes());
    assert_eq!(info.entries.len(), 1);
    assert!(!info.entries[0].valid);
}

#[tokio::test]
async fn vod_scans_mock_hls_playlist() {
    use std::process::ExitCode;

    use streamtop::engine::summary::SummaryFormat;
    use streamtop::engine::vod::run_vod;
    use streamtop::ui::app::SessionOpts;

    let server = mock_server::MockStreamServer::start_hls().await;
    let url = format!("{}/live.m3u8", server.base_url);
    let session = SessionOpts {
        headers: vec![],
        user_agent: None,
        interval_ms: None,
        probe_headers: true,
        probe_drm: false,
        webhook_url: None,
        alert_on: String::new(),
        allow_insecure_webhooks: false,
        otel_endpoint: None,
        tr101290: false,
        probe_sei: false,
        simulate_player: false,
        throttle_kbps: None,
        simulated_rtt_ms: None,
    };
    let exit = run_vod(url, session, SummaryFormat::Json)
        .await
        .expect("vod crawl");
    assert_eq!(exit, ExitCode::SUCCESS);
}

#[test]
fn tr101290_engine_on_mock_fixture() {
    use streamtop::engine::tr101290::Tr101290Engine;

    let mut engine = Tr101290Engine::new();
    let report = engine.ingest(&mock_server::tr101290_broken_ts(), 1_000);
    assert!(
        report.p1_violations > 0 || report.p2_violations > 0,
        "expected TR 101 290 violations"
    );
}

#[test]
fn sei_fixture_captions_and_hdr() {
    use streamtop::engine::sei_probe::probe_sei;
    use streamtop::models::ContainerKind;

    let bytes = mock_server::sei_caption_hdr_ts();
    let r = probe_sei(&bytes, ContainerKind::Ts);
    assert!(r.cea608_present, "cea608: {r:?}");
    assert!(r.hdr10_present, "hdr10: {r:?}");
}
