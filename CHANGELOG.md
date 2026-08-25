# Changelog

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
