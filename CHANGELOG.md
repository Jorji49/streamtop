# Changelog

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
