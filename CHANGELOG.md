# Changelog

## 1.1.1

- README: product overview, use cases, FAQ, 1.1.0 feature docs (TR 101 290, SEI, SRT/RTMP, QoE)
- README: SEO-friendly headings and alt text; Winget PR link -> #426121
- Cargo.toml: expanded crate description and keywords
- GitHub repo description and topics sync
- Packaging manifests bumped to 1.1.1
- E2E: mock `/health` wait loop (Windows CI race fix)
- CI: typos spellcheck job; release build gated on e2e + e2e-windows

## 1.1.0

- TR 101 290 P1/P2 MPEG-TS engine (`--tr101290`)
- Synthetic QoE player sim (`--simulate-player`, `--throttle-kbps`, `--simulated-rtt-ms`)
- SEI/HDR/CEA probe (`--probe-sei`)
- SRT/RTMP ingest routing and `ingest_stats` in summary JSON
- Summary schema v3: `tr101290`, `synthetic_qoe`, `sei_metadata`, `ingest_stats`
- Hermetic E2E harness: `tests/e2e_verify.sh`, native `tests/e2e_verify.ps1`, Python mock (HTTP/SRT/RTMP)
- SSRF: pinned OTLP/DRM GET, ingest target validation, fMP4 box size cap, OTEL span buffer bound
- Prometheus: `streamtop_qoe_rebuffer_risk`, `streamtop_tr101290_p1/p2_violations_total`
- TUI overlays: `t` TR101290, `s` SEI, `y` QoE

## 1.0.1

- CI: skip crates.io publish when the version is already on the index
- Summary JSON: fill `subtitle_drift_ms` from AvSync log lines
- README: `--vod` and `--otel-endpoint` usage

## 1.0.0

- Unified glass-to-glass latency engine: `prft` + HLS PDT + DASH publish time → `g2g_total_ms`, ingestion lag, edge propagation
- Virtual ABR buffer model: rebuffer probability, stall risk index, ladder switch / ping-pong detection
- Deep PSSH inspection from MPD and fMP4 wire probe (Widevine, PlayReady, FairPlay, ClearKey, KIDs)
- Subtitle PTS drift detector for WebVTT/TTML vs video timeline (±200ms linter threshold)
- Mock server scenarios: stall delay, out-of-order LL-HLS parts, subtitle drift, corrupt PSSH
- OpenTelemetry: W3C `traceparent` injection; spans for manifest, DNS, TCP, TLS, TTFB, wire parse, segment download, G2G
- Summary JSON schema v2 fields; Prometheus: `g2g_total_ms`, `rebuffer_probability_pct`, `stall_risk_index`
- Grafana dashboard v2: datasource variable, G2G / rebuffer / stall-risk panels, non-overlapping 24-column grid

## 0.3.5

- fMP4/MPEG-TS wire timing: sidx, trun, PTS gaps, TS CC/PCR drift, cross-segment tracker
- LL-DASH: ServiceDescription latency, availabilityTimeOffset, UTCTiming, CTE detection, production drift
- SCTE-35: UPID types, auto-return, sub-segment alignment, full segmentation descriptors
- `--vod`: one-shot VOD playlist/MPD crawl with ladder validation and summary.v1 output
- `--otel-endpoint`: OTLP/HTTP JSON trace export for DNS/TLS/TTFB/segment spans
- Hermetic mock streaming server scenarios (stall, drift, 404, corrupt fMP4, SCTE-35)
- cargo-fuzz targets: HLS, MPD, container_probe, SCTE-35

## 0.3.4

- Require `--metrics-token` (or `STREAMTOP_METRICS_TOKEN`) when `--metrics-bind` is not loopback
- DRM probe (`--probe-drm`): same SSRF rules as webhooks; HTTP redirects disabled
- Metrics auth: case-insensitive `Bearer`; empty tokens ignored
- Redact URL userinfo, fragment params, and more CDN query keys; scrub URLs in the TUI status line
- Block additional webhook metadata / `.local` hostnames
- Docker metrics example documents the required token
- CI: `cargo audit` job

## 0.3.3

- Cross-segment GOP interval from keyframe PTS (`gop_duration_sec`, `is_fixed_cadence`)
- TUI: GOP / audio wire info (IDR/Delta badges; Last Segment lines)
- Metrics: Bearer-only auth; removed `?token=`; constant-time compare
- `streamtop_channel_dropped_total` and `dropped_events` in summary / diagnostic JSON
- Scoop / Winget / Homebrew / AUR manifests for 0.3.3
- Tag-triggered crates.io publish; Docker concurrency + path filters
- Tests for GOP cadence, Bearer auth, channel drops
- Grafana panel for channel drops

## 0.3.2

- Webhook SSRF blocking; `--allow-insecure-webhooks` override
- Redact secrets in diagnostic JSON, DRM logs, audit JSON/CSV
- Compare pause ring buffer (256) with replay on resume
- Grafana: DRM TTFB, LL-HLS part duration, codec mismatch panels
- `probe_drm` in `config.toml` profiles
- Live HLS smoke: retries + soft-fail (does not block main CI)
- Tests: mock poller, compare pause buffer, audit redaction
- Fix cargo-binstall `pkg-fmt` and release asset URLs
- Sync `Cargo.lock` package version for Docker `--locked` builds
- Slack/Discord webhooks, redact helpers, CDN/DRM/SCTE detail, Prometheus histograms, compare mode

## 0.3.1

- Windows release zip no longer embeds Scoop/AUR/Winget manifests
- Winget multi-file manifests (zip + portable nested exe)
- Scoop bucket at `bucket/streamtop.json`
- README install commands match current packaging

## 0.3.0

- Quick Play (`p`): `mpv` / `ffplay` with active URL and `-H`/`-A` headers
- LL-HLS part timing and status badge
- Readable SCTE-35 command names in the log
- `--export-grafana` → `streamtop-grafana.json`
- Prometheus `streamtop_bitstream_fps`
- Packaging: deb / binstall / Homebrew / Scoop / AUR / Winget / Docker / GHCR
- Published on [crates.io](https://crates.io/crates/streamtop)

## 0.2.1

- DASH duration when Representation omits `@timescale`
- DASH Seq / Target / DVR / buffer inflated by bad duration
- Wire FPS/resolution; DNS/TCP/TLS/TTFB; binary SCTE-35; `--compare`; `--webhook`
- rustls CryptoProvider install
- CI for Linux / macOS / Windows + release artifacts
- User-facing README

## 0.2.0

- ABR FPS from HLS `FRAME-RATE` / DASH `@frameRate`
- `Space` exports `diagnostics/<channel>_<timestamp>.json`
- LL-HLS `_HLS_msn` / `_HLS_part`, PRELOAD-HINT range probe
- DASH ContentProtection DRM badges; Prometheus metrics
- Stricter `--summary` PASS rules; fixture tests
- LICENSE, CI, `.gitignore`

## 0.1.0

- Initial HLS/DASH TUI, IPTV picker, audit, summary, CDN/SHI diagnostics
