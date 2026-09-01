# Changelog

## [1.4.0] - 2026-09-01

### Added

- DNS-over-HTTPS resolver (`--doh-provider cloudflare|google|<URL>`): pure-Rust JSON API lookup, `doh_ms` wire timing, Prometheus `streamtop_dns_doh_duration_seconds`, reason code `ERR_DOH_RESOLUTION_FAILED`
- WHEP signaling probe: HTTP POST SDP offer, parse 200/201 answer (TTFB, codecs, ICE candidates, `a=msid` stream IDs); no media decode
- SARIF v2.1.0 export: `--summary-format sarif` and `--export sarif[:FILE]` with `ruleId` mapped to `DiagnosticReasonCode`
- GitHub Actions step summary: `--github-step-summary FILE` or auto-write via `GITHUB_STEP_SUMMARY` on `--summary` and budget runs (SHI, RTF, Part RTF, TR 101 290 P1/P2, ABR ladder, budget verdict)
- LL-HLS part telemetry: `#EXT-X-PART` / `PRELOAD-HINT` part TTFB, download ms, Part RTF; Prometheus `streamtop_part_dl_duration_ratio`; `ERR_PART_RTF_STALL` on stall
- In-memory AES-128-CBC probe decryption for `#EXT-X-KEY` segments (TR 101 290 and SCTE-35 wire analysis without H.264/AAC decoders)
- Unified export CLI: `--export FORMAT[:FILE]` for `report-html`, `report-json`, `curl`, `har`, `incident`, `grafana`, `sarif` (repeatable)
- Stream budget mode: `--budget-max-rtf`, `--budget-max-ttfb`, `--budget-max-cc-errors`, `--budget-max-drift`, `--budget-duration`
- Summary JSON v5 optional fields: `http_version`, `transfer_ms`, `quic_handshake_ms`, `multi_cdn_skew`
- HTTP/2 ALPN transport telemetry via reqwest; `NetworkTiming` adds `transfer_ms`, `http_version`, QUIC fields
- Prometheus: `streamtop_http_version{version}`, `streamtop_quic_handshake_seconds`, `streamtop_quic_stream_resets_total`
- Multi-CDN skew matrix: `--multi-cdn URL1,URL2,...`, `--max-cdn-skew-ms`, reason code `ERR_CDN_SYNC_SKEW`
- Zero-alloc TUI header path: `UiRenderCache` rebuilt on events, not draw ticks
- Middlebox heuristics wired: `ERR_DPI_TCP_RESET`, redirect cycle/limit via `ERR_HTTP_REDIRECT_LOOP`

### Changed

- Export flags consolidated under `--export`; legacy `--export-*` aliases hidden and emit deprecation warnings
- Bounded stream reader hardening for infinite chunked MPEG-TS responses (`PROBE_READ_TIMEOUT_SECS`)
- HTTP redirect policy detects cycles and hop limits; auth headers preserved on valid hops
- ABR ladder alignment linter (`ERR_ABR_VARIANT_MISALIGNMENT`)
- Summary JSON schema version 5 (was v4 in pre-release notes)

### Deprecated

- Legacy export flags: `--export-curl`, `--export-har`, `--export-report`, `--export-grafana`, `--export-incident`
- Raw `srt://` and `rtmp://` ingest probes (warning logged; WHEP preferred)

### Removed

- Experimental active `--anti-dpi` raw socket stub; `middlebox.rs` retained as passive heuristics only

## [1.3.2] - 2026-08-30

- Real-Time Factor (RTF): download-to-duration ratio on each segment (`dl_to_dur_ratio`)
- Stall risk states: Normal (<0.70x), Elevated (0.70x-1.00x), Draining (>1.00x)
- TUI Status and Last Segment panels show color-coded RTF indicator
- Prometheus gauge `streamtop_segment_dl_duration_ratio`
- Headless summary JSON, agent logs, and diagnostic exports include `dl_to_dur_ratio`

## [1.3.1] - 2026-08-30

- Inband DASH `emsg` box parsing and DAI cross-layer validation
- Apple FairPlay `cbcs` 1:9 sub-sample staging decryption (ClearKey probe)
- Self-contained HTML incident and compliance export (`--export-report`)
- Interactive compiled regex modal overlay in TUI (`/`)
- Multi-stream headless agent daemon (`streamtop --agent config.toml`)
- Zero-panic refactor: `slice_util` bounds-safe indexing, pedantic clippy clean, ~40 clone sites removed

## 1.3.0

- Inband DASH `emsg` SCTE-35 extraction and DAI cross-layer validation
- FairPlay `cbcs` 1:9 staging ClearKey pattern decrypt (pure Rust AES-CBC)
- Self-contained HTML/JSON compliance reports (`--export-report`)
- TUI regex log filter modal (`/` overlay with live syntax validation)
- Multi-stream headless agent (`--agent agent.toml`) with aggregated `/metrics`
- OTLP metric batches on `/v1/metrics`; Grafana dashboard v4 (DAI, ClearKey, agent)
- Summary schema v4; `streamtop_inband_emsg_total`, `streamtop_ad_mismatch_total` metrics
- Agent stream cap (64), bounded 512-event channels, streaming HTML report writer
- E2E harness covers export-report, agent fleet, schema v4, cbcs staging
- CI release gates: promote-release verifies check/e2e/audit/typos jobs; Docker tag push removed

## 1.1.2

- Webhook delivery is bounded and invalid `--alert-on` values fail fast
- Separate insecure destination flags for webhooks, OTLP, and ingest
- Audit terminal output redacts signed URLs and error text
- Summary schema v3 is fully validated in Linux and Windows E2E
- QoE and TR 101 290 panels added to the Grafana dashboard
- Deterministic release archives, SHA256SUMS, and package hash verification
- CI supply-chain hardening: locked builds, pinned actions, audit/fuzz gates, SBOM and provenance
- Expanded CLI E2E, profile options, security policy, CODEOWNERS, and dependency updates

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
