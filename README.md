<div align="center">

<img width="96" alt="streamtop logo" src="https://github.com/user-attachments/assets/95b0df39-8404-4229-a071-7876ba6f3fde" />

# streamtop

**Terminal HLS, DASH, and IPTV stream monitor. Real-time health checks, wire probes, and production metrics from the command line.**

<a title="This tool is Tool Of The Week on Terminal Trove, The HOME of all things in the terminal" href="https://terminaltrove.com"><img src="https://cdn.terminaltrove.com/media/badges/tool_of_the_week/png/terminal_trove_tool_of_the_week_gold_on_black_bg.png" alt="Terminal Trove Tool Of The Week" height="50" /></a>


[![Awesome Ratatui](https://img.shields.io/badge/awesome-ratatui-e43716?style=for-the-badge&logo=rust&logoColor=white)](https://github.com/ratatui/awesome-ratatui)

[![Crates.io](https://img.shields.io/crates/v/streamtop?style=flat-square&color=007ec6&labelColor=1c1c1c)](https://crates.io/crates/streamtop)
[![Release](https://img.shields.io/github/v/release/Jorji49/streamtop?label=release&style=flat-square&color=007ec6&labelColor=1c1c1c)](https://github.com/Jorji49/streamtop/releases/latest)
[![Downloads](https://img.shields.io/crates/d/streamtop?style=flat-square&color=2ea44f&labelColor=1c1c1c)](https://crates.io/crates/streamtop)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue?style=flat-square&labelColor=1c1c1c)](LICENSE)

<img width="1200" alt="streamtop HLS DASH IPTV terminal dashboard demo" src="https://github.com/user-attachments/assets/f4fbb3e2-f572-4003-bb5d-b22cbba52a80" />

</div>

## What is streamtop?

**streamtop** is a Rust CLI and terminal UI for monitoring live video streams. Point it at an HLS playlist (`.m3u8`), MPEG-DASH manifest (`.mpd`), IPTV channel list (`.m3u`), or WHEP HTTP endpoint and get segment timing, codec wire data, ad markers, and health scores without opening a browser or GUI player.

Debug CDN issues, validate encoder output, compare origin vs edge, run CI smoke tests on manifests, or scrape Prometheus metrics from a live probe.

WHEP HTTP endpoints are the supported path for WebRTC egress signaling probes. Legacy `srt://` and `rtmp://` URLs are rejected at startup.

## Features

### Protocols and inputs

* **HLS** (`.m3u8`): live, LL-HLS `#EXT-X-PART` / `PRELOAD-HINT`, part TTFB and Part RTF, `#EXT-X-PROGRAM-DATE-TIME`, media playlists
* **MPEG-DASH** (`.mpd`): live and VOD, ServiceDescription latency, UTCTiming, ContentProtection / PSSH
* **IPTV / catalogs** (`.m3u`, `.json`, `.yaml`): channel picker, search, playlist audit to JSON/CSV
* **WHEP**: HTTP POST SDP offer, parse 200/201 answer (signaling TTFB, codecs, ICE candidates, stream IDs)

### Wire and container probes

* **`--probe-headers`**: fetch only the first bytes of each segment for fast TTFB and header checks
* **GOP / FPS / resolution**: manifest vs bitstream comparison; mismatch badges in the TUI
* **Audio**: ADTS, fMP4, MPEG-TS PMT from the probe window
* **TR 101 290** (`--tr101290`): MPEG-TS P1/P2 checks (sync, continuity, PCR, PAT/PMT)
* **AES-128-CBC probe**: in-memory `#EXT-X-KEY` fetch and decrypt for encrypted TS/fMP4 wire analysis (no full decoder)
* **SEI / HDR** (`--probe-sei`): side metadata from H.264/H.265 elementary streams
* **DRM** (`--probe-drm`): key-server / LA_URL TTFB with SSRF-safe pinned GET

### Live operations

* **LL-HLS part telemetry**: per-part TTFB, download ms, Part RTF (`part_dl_duration_ratio`); Prometheus `streamtop_part_dl_duration_ratio`
* **DNS-over-HTTPS** (`--doh-provider cloudflare|google|<URL>`): DoH JSON lookup; `doh_ms` in wire timing and summary JSON
* **HTTP version / timing**: `NetworkTiming` reports DNS/TCP/TLS/TTFB/transfer ms and negotiated `http_version`
* **Multi-CDN skew** (`--multi-cdn URL1,URL2,...`): concurrent edge polling, live-edge seq/PDT skew matrix, `ERR_CDN_SYNC_SKEW`
* **SCTE-35 / DAI**: manifest cues, inband DASH `emsg`, cross-layer mismatch detection
* **Staging ClearKey** (`--clearkey KID:KEY`): cenc CTR and FairPlay cbcs pattern probe
* **Glass-to-glass latency**: PRFT, HLS PDT, DASH publish time -> `g2g_total_ms`
* **Measured buffer model**: rebuffer probability and stall risk from observed download-to-duration ratios
* **Split-screen compare**: two URLs side by side in one TUI
* **Quick Play** (`p`): launch `mpv` or `ffplay` with active headers

### Export and observability

* **Unified export** (`--export FORMAT[:FILE]`): `report-html`, `report-json`, `curl`, `har`, `incident`, `grafana`, `sarif` (repeatable)
* **Incident export**: redacted curl, `.har`, diagnostic JSON (`Space` / `--export incident` / `e`)
* **Compliance report**: `--export report-html:report.html` or `--export report-json:report.json`
* **SARIF 2.1.0**: `--summary-format sarif` or `--export sarif:streamtop.sarif` for GitHub Code Scanning
* **GitHub Actions step summary**: `--github-step-summary FILE` or auto-write when `GITHUB_STEP_SUMMARY` is set (`--summary`, budget mode)
* **Headless CI**: stable `streamtop.summary.v1` JSON contract (field version **6**), `--timeout`, PASS/FAIL rules
* **Stream budget** (`--budget-max-rtf`, `--budget-max-ttfb`, `--budget-max-cc-errors`, `--budget-max-drift`): threshold assertions with JSON verdict
* **Multi-stream agent** (`--agent agent.example.toml`): fleet polling with aggregated `/metrics`
* **Prometheus** `/metrics` on `:9184` (Bearer token required on non-loopback bind)
* **OpenTelemetry**: OTLP traces + metric batches (`/v1/traces`, `/v1/metrics`)
* **Grafana**: `--export grafana` -> dashboard JSON
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

Validated, awaiting merge: [microsoft/winget-pkgs#427437](https://github.com/microsoft/winget-pkgs/pull/427437).

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
docker run -it --rm ghcr.io/jorji49/streamtop:v1.4.0 <URL>
docker run -it --rm ghcr.io/jorji49/streamtop:latest <URL>
```

Metrics on a non-loopback bind require a token:

```bash
docker run --rm -p 9184:9184 \
  -e STREAMTOP_METRICS_TOKEN=change-me \
  ghcr.io/jorji49/streamtop:v1.4.0 \
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
| Last segment | Seq, sizes, DNS / TCP / TLS / DoH / TTFB, container, GOP interval, audio |
| ABR ladder | Bitrate, resolution, FPS, codecs. `[wire]` is from the bitstream; red = manifest vs wire mismatch |
| Charts | Latency or TTFB; download rate or transfer time |
| Log | Warnings, ads (SCTE-35), stalls, HTTP errors |

Overlay keys: `t` TR 101 290, `s` SEI/HDR.

FPS prefers playlist `FRAME-RATE` / `@frameRate`, otherwise the bitstream when available. GOP interval comes from keyframe PTS across segments (Fixed or Variable). Audio codec / rate / channels come from ADTS, fMP4, or MPEG-TS PMT in the probe window.

## Usage

```bash
# Dashboard
streamtop <URL> [--probe-headers] [-H "Key: Value"] [-A user-agent] [-i MS]

# Side-by-side compare
streamtop --compare <URL_1> <URL_2> --probe-headers

# Multi-CDN skew matrix (TUI) or headless JSON with --summary
streamtop --multi-cdn akamai=https://a.example/live.m3u8,cloudflare=https://b.example/live.m3u8 --max-cdn-skew-ms 3000
streamtop --multi-cdn https://a.example/live.m3u8,https://b.example/live.m3u8 --summary --timeout 15

# Webhooks (Slack / Discord / HTTP). Private and metadata hosts blocked by default.
streamtop <URL> --webhook https://hooks.example/x --alert-on stall,shi_below_70,http_5xx
streamtop <URL> --webhook http://127.0.0.1:9999/hook --allow-insecure-webhooks

# Channel list audit -> audit_report.json / .csv
streamtop ./channels.m3u --audit

# Headless PASS/FAIL (CI). Stable contract: streamtop.summary.v1; field version: 6
streamtop <URL> --summary --summary-format json --timeout 10

# SARIF 2.1.0 for GitHub Code Scanning
streamtop <URL> --summary --summary-format sarif --timeout 10

# GitHub Actions step summary (SHI, RTF, TR 101 290, ABR, budget table)
streamtop <URL> --summary --github-step-summary "$GITHUB_STEP_SUMMARY" --timeout 10
# When GITHUB_STEP_SUMMARY is set, --summary and budget mode write it automatically.

# Stream budget assertions (JSON verdict on stdout; non-zero exit on breach)
streamtop <URL> --budget-max-rtf 1.0 --budget-max-ttfb 500ms --budget-duration 30

# Unified export (repeatable)
streamtop <URL> --export report-html:report.html --timeout 10
streamtop <URL> --export report-json:report.json --timeout 10
streamtop <URL> --export curl --probe-headers --timeout 10
streamtop <URL> --export har:incident.har --timeout 10
streamtop <URL> --export sarif:streamtop.sarif --timeout 10
streamtop --export grafana

# LL-HLS with DoH timing and Prometheus scrape
streamtop <URL> --doh-provider cloudflare --probe-headers --prometheus

# WHEP signaling probe (JSON report; no media decode)
streamtop "https://origin.example/whep/feed"

# Headless background agent: multi-stream monitoring without a TUI (see agent.example.toml)
streamtop --agent agent.example.toml

# VOD playlist crawl
streamtop --vod <URL> --summary

# OTEL trace export
streamtop <URL> --otel-endpoint http://127.0.0.1:4318

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

# Encrypted HLS with staging ClearKey + TR 101 290
streamtop <URL> --clearkey KID:KEY --tr101290 --probe-headers
```

### CI examples

```bash
# Budget thresholds + SARIF findings + GHA step summary
streamtop "$STREAM_URL" \
  --budget-max-rtf 1.0 \
  --budget-max-ttfb 500ms \
  --budget-max-cc-errors 0 \
  --budget-duration 30 \
  --github-step-summary "${GITHUB_STEP_SUMMARY:-step.md}"

streamtop "$STREAM_URL" --export sarif:streamtop.sarif --timeout 15

# Live LL-HLS origin check with DoH and part RTF metrics
streamtop "$LL_HLS_URL" \
  --doh-provider google \
  --probe-headers \
  --summary --summary-format json \
  --timeout 12

# WHEP endpoint smoke test (signaling only)
streamtop "https://webrtc.example/live/whep" --timeout 5
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
| `t` | TR 101 290 overlay |
| `s` | SEI / HDR / caption overlay |
| `j` / `k` | Scroll log or channel list |

Compare mode: `Space` pause/resume, `d` detail, `l` log focus, `c` curl, `h` HAR, `Tab` switch pane.
`e` also exports HAR in compare mode.

## Headless verdict

`--summary` returns PASS only when the stream is LIVE, SHI is at least 85, no critical RFC errors or origin stalls were observed, the last HTTP status is 200/206, and at least one segment was fetched. Any failed condition returns FAIL and a non-zero exit code. The schema file is `schemas/summary.v1.json`; `schema_version` is currently `6`.

## FAQ

**How is streamtop different from ffprobe or VLC?**  
ffprobe inspects a single file or URL snapshot. streamtop polls live playlists, tracks segment health over time, surfaces SCTE-35 and SHI trends, and exports Prometheus metrics and CI-friendly summary JSON.

**Does it work headless in CI?**  
Yes. Use `--summary --summary-format json --timeout N` for PASS/FAIL output, `--summary-format sarif` or `--export sarif:FILE` for Code Scanning, and `--budget-max-*` for threshold gates. Hermetic E2E tests live in `tests/e2e_verify.sh` and `tests/e2e_verify.ps1`.

**Which streaming protocols are supported?**  
HLS (including LL-HLS parts), MPEG-DASH, IPTV M3U lists, and WHEP HTTP egress. Wire probes cover fMP4, MPEG-TS, ADTS, and elementary H.264/H.265 without invoking full decoders.

**Is it safe to expose Prometheus metrics?**  
Bind to loopback by default. For remote scrape targets, set `--metrics-token` or `STREAMTOP_METRICS_TOKEN`; Bearer auth is required on non-loopback binds.

## License

[MIT](LICENSE).
