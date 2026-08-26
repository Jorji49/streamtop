# Changelog

## 0.3.3

- Cross-segment GOP interval: track keyframe PTS across consecutive segments; expose `gop_duration_sec` and `is_fixed_cadence` on wire probe / diagnostic JSON
- Wire probe: GOP and audio codec info in TUI (IDR/Delta badges, Last Segment GOP/Audio lines)
- Metrics auth: Bearer-only (`Authorization: Bearer`); query `?token=` removed; constant-time token compare
- Telemetry: `streamtop_channel_dropped_total` Prometheus counter and `dropped_events` in summary / diagnostic JSON
- Tests: GOP cadence, metrics Bearer auth, channel drop counter

## 0.3.2

- Security: webhook SSRF blocking for private/link-local/metadata destinations; `--allow-insecure-webhooks` escape hatch
- Redaction: diagnostic JSON, DRM probe logs, audit JSON/CSV
- Compare: pause ring-buffer (256) with replay on resume
- Prometheus Grafana panels: DRM license TTFB, LL-HLS part duration, codec mismatch
- Profiles: `probe_drm` in `config.toml`
- CI: live HLS smoke retries + soft-fail (does not gate main build)
- Tests: local mock poller, compare pause buffer, audit redaction/mock
- Fix cargo-binstall: valid `pkg-fmt` (`tgz`/`zip`) and release URLs that match published assets (`.tar.gz` / `.zip`)
- Sync `Cargo.lock` package version so Docker `--locked` builds succeed on GHCR publish
- Slack/Discord webhooks, redact module, CDN/DRM/SCTE depth, Prometheus histograms, compare parity

## 0.3.1

- Fix Windows release zip (no longer packs Scoop/AUR/Winget manifests into the binary archive)
- Winget multi-file manifests (`InstallerType: zip` + portable nested exe)
- Scoop bucket path (`bucket/streamtop.json`) for `scoop bucket add streamtop https://github.com/Jorji49/streamtop`
- README install commands match what actually works today

## 0.3.0

- Quick Play (`p`): launch `mpv` or `ffplay` with the active manifest URL and `-H`/`-A` headers
- LL-HLS part telemetry: sequence, duration (ms), transfer rate; status badge `[LL-HLS] part Nms`
- Binary SCTE-35 log lines use readable command names (Time Signal, Splice Insert) and segmentation types
- `streamtop --export-grafana` writes `streamtop-grafana.json` for Prometheus metrics (SHI, TTFB, FPS, CDN, buffer)
- Prometheus: `streamtop_bitstream_fps` gauge
- Packaging: cargo-deb / cargo-binstall metadata, Homebrew Formula, Scoop, AUR, Winget, Dockerfile, GHCR publish
- Published on [crates.io](https://crates.io/crates/streamtop)

## 0.2.1

- Fix DASH segment duration when Representation omits `@timescale` (inherit from AdaptationSet)
- Fix DASH Seq / Target / DVR window / virtual buffer inflated by bad duration
- Wire FPS/resolution from bitstream; DNS/TCP/TLS/TTFB; binary SCTE-35; `--compare`; `--webhook`
- rustls CryptoProvider install (no dual-backend panic)
- CI: Linux / macOS / Windows check + release artifacts
- User-facing README

## 0.2.0

- ABR ladder video FPS from HLS FRAME-RATE / DASH @frameRate
- `Space` exports `diagnostics/<channel>_<timestamp>.json` with `timeline`
- LL-HLS next `_HLS_msn` / `_HLS_part`, PRELOAD-HINT range probe
- DASH ContentProtection DRM badges; Prometheus metrics
- Stricter `--summary` PASS rules; fixture integration tests
- LICENSE, CI, `.gitignore`

## 0.1.0

- Initial HLS/DASH TUI, IPTV picker, audit, summary, CDN/SHI diagnostics
