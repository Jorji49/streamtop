# streamtop

[![Awesome Ratatui](https://img.shields.io/badge/awesome-ratatui-e43716?style=for-the-badge&logo=rust&logoColor=white)](https://github.com/ratatui/awesome-ratatui)

[![Crates.io](https://img.shields.io/crates/v/streamtop?style=flat-square&color=007ec6&labelColor=1c1c1c)](https://crates.io/crates/streamtop)
[![Release](https://img.shields.io/github/v/release/Jorji49/streamtop?label=release&style=flat-square&color=007ec6&labelColor=1c1c1c)](https://github.com/Jorji49/streamtop/releases/latest)
[![Downloads](https://img.shields.io/crates/d/streamtop?style=flat-square&color=2ea44f&labelColor=1c1c1c)](https://crates.io/crates/streamtop)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue?style=flat-square&labelColor=1c1c1c)](LICENSE)

Terminal diagnostics for live **HLS**, **DASH**, and **IPTV** streams.

<img width="1200" alt="demo" src="https://github.com/user-attachments/assets/a98017bd-429c-43fb-8a14-2c13fb4257cf" />

Latest release: **[v1.0.1](https://github.com/Jorji49/streamtop/releases/tag/v1.0.1)** (`streamtop --version`).

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

In review: [microsoft/winget-pkgs#425258](https://github.com/microsoft/winget-pkgs/pull/425258).

```powershell
winget install streamtop
```

### Homebrew

```bash
brew tap Jorji49/tap
brew install streamtop
```

Formula in this repo:

```bash
brew install --formula https://raw.githubusercontent.com/Jorji49/streamtop/main/Formula/streamtop.rb
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
docker run -it --rm ghcr.io/jorji49/streamtop:v1.0.1 <URL>
docker run -it --rm ghcr.io/jorji49/streamtop:latest <URL>
```

Metrics on a non-loopback bind require a token:

```bash
docker run --rm -p 9184:9184 \
  -e STREAMTOP_METRICS_TOKEN=change-me \
  ghcr.io/jorji49/streamtop:v1.0.1 \
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
streamtop "https://example.com/master.m3u8"
streamtop "https://example.com/manifest.mpd" --probe-headers
streamtop "./channels.m3u"
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

# Headless PASS/FAIL (CI). Schema: schemas/summary.v1.json
streamtop <URL> --summary --summary-format json --timeout 10

# VOD playlist crawl
streamtop <URL> --vod --summary

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

`--alert-on` values: `stall`, `shi_below_70`, `http_5xx`, `mismatch`, `ad_start`.

Non-loopback `--metrics-bind` requires a non-empty `--metrics-token` or `STREAMTOP_METRICS_TOKEN`.

## Keys

| Key | Action |
|-----|--------|
| `q` / `Esc` / `Ctrl+C` | Quit (`Esc` leaves the channel list when open) |
| `Space` | Write `diagnostics/…` report (URLs and secrets redacted) |
| `c` | Copy curl for the last segment (redacted) |
| `p` | Play with `mpv` or `ffplay` (not in Docker) |
| `r` | Reset metrics |
| `Tab` | Channel overlay |
| `?` | Help |
| `/` | Search channel list |
| `j` / `k` | Scroll log or channel list |

Compare mode: `Space` pause/resume, `d` detail, `l` log focus, `c` curl, `h` HAR, `Tab` switch pane.

## License

[MIT](LICENSE).
