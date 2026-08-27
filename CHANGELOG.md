# Changelog

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
