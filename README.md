<div align="center">

<img width="96" alt="streamtop logo" src="https://github.com/user-attachments/assets/95b0df39-8404-4229-a071-7876ba6f3fde" />

# streamtop

**Terminal HLS, DASH, and IPTV stream monitor. Real-time health checks, wire probes, and production metrics from the command line.**

[![Awesome Ratatui](https://img.shields.io/badge/awesome-ratatui-e43716?style=for-the-badge&logo=rust&logoColor=white)](https://github.com/ratatui/awesome-ratatui)

[![Crates.io](https://img.shields.io/crates/v/streamtop?style=flat-square&color=007ec6&labelColor=1c1c1c)](https://crates.io/crates/streamtop)
[![Release](https://img.shields.io/github/v/release/Jorji49/streamtop?label=release&style=flat-square&color=007ec6&labelColor=1c1c1c)](https://github.com/Jorji49/streamtop/releases/latest)
[![Downloads](https://img.shields.io/crates/d/streamtop?style=flat-square&color=2ea44f&labelColor=1c1c1c)](https://crates.io/crates/streamtop)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue?style=flat-square&labelColor=1c1c1c)](LICENSE)

<img width="1200" alt="streamtop HLS DASH IPTV terminal dashboard demo" src="https://github.com/user-attachments/assets/f4fbb3e2-f572-4003-bb5d-b22cbba52a80" />

</div>

## What is streamtop?

**streamtop** is a Rust CLI and terminal UI for monitoring live video streams. Point it at an HLS playlist (`.m3u8`), MPEG-DASH manifest (`.mpd`), IPTV channel list (`.m3u`), SRT listener, or RTMP URL and get segment timing, codec wire data, ad markers, and health scores without opening a browser or GUI player.

Use it to debug CDN issues, validate encoder output, compare origin vs edge, run CI smoke tests on manifests, or watch production feeds with Prometheus and OpenTelemetry hooks.

## Use cases

| Role | Typical task |
|------|----------------|
| Broadcast / OTT engineer | Check GOP cadence, audio sync, LL-HLS part timing, SCTE-35 ad cues |
| CDN / SRE | Track TTFB, DNS/TCP/TLS breakdown, glass-to-glass latency, rebuffer risk |
| Developer / QA | Headless `--summary` PASS/FAIL in CI, VOD crawl, HAR/curl export on incidents |
| NOC | Dual-pane `--compare`, webhooks to Slack/Discord, live SHI and stall alerts |

## Features

### Protocols and inputs

* **HLS** (`.m3u8`): live, LL-HLS parts, PRELOAD-HINT, `#EXT-X-PROGRAM-DATE-TIME`, media playlists
* **MPEG-DASH** (`.mpd`): live and VOD, ServiceDescription latency, UTCTiming, ContentProtection / PSSH
* **IPTV / catalogs** (`.m3u`, `.json`, `.yaml`): channel picker, search, playlist audit to JSON/CSV
* **Ingest**: SRT and RTMP URL routing with ingest stats in summary JSON

### Wire and container probes

* **`--probe-headers`**: fetch only the first bytes of each segment for fast TTFB and header checks
* **GOP / FPS / resolution**: manifest vs bitstream comparison; mismatch badges in the TUI
* **Audio**: ADTS, fMP4, MPEG-TS PMT from the probe window
* **TR 101 290** (`--tr101290`): MPEG-TS P1/P2 checks (sync, continuity, PCR, PAT/PMT)
* **SEI / HDR** (`--probe-sei`): side metadata from H.264/H.265 elementary streams
* **DRM** (`--probe-drm`): key-server / LA_URL TTFB with SSRF-safe pinned GET

### Live operations

* **SCTE-35 / DAI**: manifest cues, inband DASH `emsg`, cross-layer mismatch detection
* **Staging ClearKey** (`--clearkey KID:KEY`): cenc CTR and FairPlay **cbcs** 1:9 pattern probe
* **Glass-to-glass latency**: PRFT, HLS PDT, DASH publish time -> `g2g_total_ms`
* **Virtual buffer model**: rebuffer probability, stall risk index, ABR ladder switch detection
* **Synthetic QoE** (`--simulate-player`): player sim with throttle and simulated RTT
* **Split-screen compare**: two URLs side by side in one TUI
* **Quick Play** (`p`): launch `mpv` or `ffplay` with active headers

### Export and observability

* **Incident export**: redacted curl, `.har`, diagnostic JSON (`Space` / `--export-har` / `e`)
* **Compliance report** (`--export-report`): single-file HTML or JSON dashboard
* **Headless CI**: stable `streamtop.summary.v1` JSON contract (field version **4**), `--timeout`, PASS/FAIL rules
* **Multi-stream agent** (`--agent agent.example.toml`): fleet polling with aggregated `/metrics`
* **Prometheus** `/metrics` on `:9184` (Bearer token required on non-loopback bind)
* **OpenTelemetry**: OTLP traces + metric batches (`/v1/traces`, `/v1/metrics`)
* **Grafana**: `--export-grafana` -> dashboard JSON v4 (DAI, ClearKey, agent panels)
* **Webhooks**: Slack, Discord, generic HTTP on stall, SHI, 5xx, mismatch, ad start

## Install

### cargo

```bash
cargo install streamtop
# or: cargo install cargo-binstall && cargo binstall streamtop
```

### Scoop (Windows)

```powershell
scoop bucket add streamtop https://github.com/Jorji49/streamtop
scoop install streamtop/streamtop
```

### Winget (Windows)

Validated, awaiting merge: [microsoft/winget-pkgs#426121](https://github.com/microsoft/winget-pkgs/pull/426121).

```powershell
winget install Jorji49.streamtop
```

### Homebrew

```bash
brew tap Jorji49/tap
brew install streamtop
```

### Arch (binary package)

AUR submission is not listed yet. Use the packaging mirror:

```bash
git clone https://github.com/Jorji49/streamtop-bin.git
cd streamtop-bin
makepkg -si
```

Source: `dist/aur/PKGBUILD`.

### Docker

```bash
docker run -it --rm ghcr.io/jorji49/streamtop:v1.3.0 <URL>
docker run -it --rm ghcr.io/jorji49/streamtop:latest <URL>
```

Metrics on a non-loopback bind require a token:

```bash
docker run --rm -p 9184:9184 \
  -e STREAMTOP_METRICS_TOKEN=change-me \
  ghcr.io/jorji49/streamtop:v1.3.0 \
  <URL> --prometheus --metrics-bind 0.0.0.0 \
  --metrics-token "$STREAMTOP_METRICS_TOKEN"
```

### Debian package

```bash
cargo install cargo-deb
cargo deb
sudo dpkg -i target/debian/streamtop_*.deb
```

### From source

```bash
git clone https://github.com/Jorji49/streamtop.git
cd streamtop
cargo install --path .
```

Binaries: [GitHub Releases](https://github.com/Jorji49/streamtop/releases/latest).

## Quick start

```bash
# HLS live stream
streamtop "https://example.com/master.m3u8"

# MPEG-DASH with fast wire probe
streamtop "https://example.com/manifest.mpd" --probe-headers

# IPTV channel list
streamtop "./channels.m3u"

# MPEG-TS TR 101 290 + SEI metadata
streamtop "https://example.com/live.ts.m3u8" --tr101290 --probe-sei
```

`--probe-headers` requests only the first bytes of each segment (faster; enough for headers and wire checks).

## UI overview

| Area | Contents |
|------|----------|
| Status | URL, LIVE / ESTIMATED, SHI, FPS, GOP / audio badges, latency, CDN, buffer, G2G, LL-HLS |
| Last segment | Seq, sizes, DNS / TCP / TLS / TTFB, container, GOP interval, audio |
| ABR ladder | Bitrate, resolution, FPS, codecs. `[wire]` is from the bitstream; red = manifest vs wire mismatch |
| Charts | Latency or TTFB; download rate or transfer time |
| Log | Warnings, ads (SCTE-35), stalls, HTTP errors |

Overlay keys: `t` TR 101 290, `s` SEI/HDR, `y` synthetic QoE.

FPS prefers playlist `FRAME-RATE` / `@frameRate`, otherwise the bitstream when available. GOP interval comes from keyframe PTS across segments (Fixed or Variable). Audio codec / rate / channels come from ADTS, fMP4, or MPEG-TS PMT in the probe window.

## Usage

```bash
# Dashboard
streamtop <URL> [--probe-headers] [-H "Key: Value"] [-A user-agent] [-i MS]

# Side-by-side compare
streamtop --compare <URL_1> <URL_2> --probe-headers

# Webhooks (Slack / Discord / HTTP). Private and metadata hosts blocked by default.
streamtop <URL> --webhook https://hooks.example/x --alert-on stall,shi_below_70,http_5xx
streamtop <URL> --webhook http://127.0.0.1:9999/hook --allow-insecure-webhooks

# Channel list audit -> audit_report.json / .csv
streamtop ./channels.m3u --audit

# Headless PASS/FAIL (CI). Stable contract: streamtop.summary.v1; field version: 4
streamtop <URL> --summary --summary-format json --timeout 10

# HTML / JSON compliance report
streamtop <URL> --export-report report.html --timeout 10

# Multi-stream headless agent (see agent.example.toml)
streamtop --agent agent.example.toml

# VOD playlist crawl
streamtop --vod <URL> --summary

# OTEL trace export
streamtop <URL> --otel-endpoint http://127.0.0.1:4318

# Curl / HAR for the last segment (secrets redacted)
streamtop <URL> --export-curl --probe-headers
streamtop <URL> --export-har incident.har --timeout 10

# Profile from ~/.config/streamtop/config.toml (see config.example.toml)
streamtop <URL> --profile cdn

# Prometheus /metrics (default bind 127.0.0.1:9184)
streamtop <URL> --prometheus
streamtop <URL> --prometheus 9184 --metrics-bind 0.0.0.0 \
  --metrics-token "$STREAMTOP_METRICS_TOKEN"
# curl -H "Authorization: Bearer $STREAMTOP_METRICS_TOKEN" http://host:9184/metrics
# Query ?token= is not accepted; Bearer header only.

# DRM key / LA_URL TTFB (SSRF-filtered; no redirects)
streamtop <URL> --probe-drm --summary

# Grafana dashboard JSON -> streamtop-grafana.json
streamtop --export-grafana
```

`--alert-on` values: `stall`, `shi_below_70`, `http_5xx`, `mismatch`, `ad_start`, `ad_mismatch`.

Non-loopback `--metrics-bind` requires a non-empty `--metrics-token` or `STREAMTOP_METRICS_TOKEN`.

## Keyboard shortcuts

| Key | Action |
|-----|--------|
| `q` / `Esc` / `Ctrl+C` | Quit (`Esc` leaves the channel list when open) |
| `Space` | Write `diagnostics/…` report (URLs and secrets redacted) |
| `c` | Copy curl for the last segment (redacted) |
| `p` | Play with `mpv` or `ffplay` (not in Docker) |
| `r` | Reset metrics |
| `Tab` | Channel overlay |
| `?` | Help |
| `/` | Regex log filter modal (Enter lock, Esc clear) |
| `f` / `F` | Cycle preset log filter / clear regex filter |
| `j` / `k` | Scroll log or channel list |

Compare mode: `Space` pause/resume, `d` detail, `l` log focus, `c` curl, `h` HAR, `Tab` switch pane.
`e` also exports HAR in compare mode. Prometheus mode is not available for SRT/RTMP ingest URLs; use the TUI or `--summary`.

## Headless verdict

`--summary` returns PASS only when the stream is LIVE, SHI is at least 85, no critical RFC errors or origin stalls were observed, the last HTTP status is 200/206, and at least one segment was fetched. Any failed condition returns FAIL and a non-zero exit code. The schema file is `schemas/summary.v1.json`; `schema_version` is currently `4`.

## FAQ

**How is streamtop different from ffprobe or VLC?**  
ffprobe inspects a single file or URL snapshot. streamtop polls live playlists, tracks segment health over time, surfaces SCTE-35 and SHI trends, and exports Prometheus metrics and CI-friendly summary JSON.

**Does it work headless in CI?**  
Yes. Use `--summary --summary-format json --timeout N` for PASS/FAIL output. Hermetic E2E tests live in `tests/e2e_verify.sh` and `tests/e2e_verify.ps1`.

**Which streaming protocols are supported?**  
HLS, MPEG-DASH, IPTV M3U lists, SRT ingest URLs, and RTMP ingest URLs. Wire probes cover fMP4, MPEG-TS, ADTS, and elementary H.264/H.265.

**Is it safe to expose Prometheus metrics?**  
Bind to loopback by default. For remote scrape targets, set `--metrics-token` or `STREAMTOP_METRICS_TOKEN`; Bearer auth is required on non-loopback binds.

## Related searches

HLS stream monitor, MPEG-DASH diagnostics tool, IPTV analyzer CLI, live stream health check, LL-HLS latency monitor, SCTE-35 ad detection, TR 101 290 MPEG-TS analyzer, CDN TTFB probe, OTT stream QA, Rust terminal video diagnostics.

## License

[MIT](LICENSE).
