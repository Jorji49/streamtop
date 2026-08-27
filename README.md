# streamtop

[![Awesome Ratatui](https://img.shields.io/badge/awesome-ratatui-ff4400?logo=rust&logoColor=white)](https://github.com/ratatui/awesome-ratatui)
[![Crates.io](https://img.shields.io/crates/v/streamtop.svg)](https://crates.io/crates/streamtop)
[![Downloads](https://img.shields.io/crates/d/streamtop.svg)](https://crates.io/crates/streamtop)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

Live HLS, DASH, and IPTV stream diagnostics in the terminal.

<img width="1200" height="560" alt="Animation" src="https://github.com/user-attachments/assets/92b89472-ed4a-45ac-b9ff-c5f4a85fd4b8" />

## Installation

### Rust

```bash
cargo install streamtop
```

```bash
cargo install cargo-binstall
cargo binstall streamtop
```

### Windows (Scoop)

```powershell
scoop bucket add streamtop https://github.com/Jorji49/streamtop
scoop install streamtop/streamtop
```

### Windows (Winget)

Package submission is in review ([PR #424450](https://github.com/microsoft/winget-pkgs/pull/424450)). After merge:

```powershell
winget install streamtop
```

### macOS / Linux (Homebrew)

```bash
brew tap Jorji49/tap
brew install streamtop
```

```bash
brew install --formula https://raw.githubusercontent.com/Jorji49/streamtop/main/Formula/streamtop.rb
```

### Arch Linux (AUR)

A packaging template lives at `dist/aur/PKGBUILD`. The package is not published to the AUR yet.

### Docker

The image contains the CLI binary only (no `mpv` / `ffplay`). Quick Play is unavailable in the container.

```bash
docker run -it --rm ghcr.io/jorji49/streamtop:latest <URL>
```

```bash
docker run --rm -p 9184:9184 ghcr.io/jorji49/streamtop:latest \
  <URL> --prometheus --metrics-bind 0.0.0.0
```

### Debian / Ubuntu

```bash
cargo install cargo-deb
cargo deb
sudo dpkg -i target/debian/streamtop_*.deb
```

### From source

Release binaries: [latest release](https://github.com/Jorji49/streamtop/releases/latest)

```bash
git clone https://github.com/Jorji49/streamtop.git
cd streamtop
cargo install --path .
```

## Quick start

```bash
streamtop "https://example.com/master.m3u8"
streamtop "https://example.com/manifest.mpd" --probe-headers
streamtop "./channels.m3u"
```

`--probe-headers` downloads only the start of each segment (faster; enough for header and wire checks).

## What you see

| Area | Meaning |
|------|---------|
| Status | URL, LIVE / ESTIMATED, health score (SHI), FPS, GOP/audio badges, latency, CDN, buffer, LL-HLS timing |
| Last segment | Sequence, sizes, DNS / TCP / TLS / TTFB, container type, GOP interval, audio wire info |
| ABR ladder | Bitrates, resolution, FPS, codecs. `[wire]` is from the bitstream; red marks manifest vs wire mismatch |
| Charts | Latency or TTFB, download rate or transfer time |
| Log | Warnings, ads (SCTE-35), stalls, HTTP errors |

FPS comes from the playlist (`FRAME-RATE` / `@frameRate`) when present; otherwise from the bitstream when readable. GOP interval is estimated from keyframe PTS across consecutive segments (Fixed vs Variable cadence). Audio codec, sample rate, and channels come from ADTS, fMP4, or MPEG-TS PMT when present in the probe window.

## Commands

```bash
# Live dashboard
streamtop <URL> [--probe-headers] [-H "Key: Value"] [-A user-agent] [-i MS]

# Compare two feeds
streamtop --compare <URL_1> <URL_2> --probe-headers

# Webhook alerts (Slack / Discord / HTTP). Private and metadata hosts are blocked by default.
streamtop <URL> --webhook https://hooks.example/x --alert-on stall,shi_below_70,http_5xx
# Local webhook testing only:
streamtop <URL> --webhook http://127.0.0.1:9999/hook --allow-insecure-webhooks

# Channel list audit -> audit_report.json / .csv
streamtop ./channels.m3u --audit

# Headless pass/fail (CI). JSON schema: schemas/summary.v1.json
streamtop <URL> --summary --summary-format json --timeout 10

# Ticket attach: curl / HAR (secrets redacted)
streamtop <URL> --export-curl --probe-headers
streamtop <URL> --export-har incident.har --timeout 10

# Named profile from ~/.config/streamtop/config.toml (see config.example.toml)
streamtop <URL> --profile cdn

# Prometheus on 127.0.0.1:9184/metrics
streamtop <URL> --prometheus
streamtop <URL> --prometheus 9184 --metrics-bind 0.0.0.0 --metrics-token "$STREAMTOP_METRICS_TOKEN"
# Scrape with: curl -H "Authorization: Bearer $STREAMTOP_METRICS_TOKEN" http://host:9184/metrics
# Query ?token= is not supported (Bearer header only).

# Optional DRM license / LA_URL TTFB probe
streamtop <URL> --probe-drm --summary

# Grafana dashboard JSON
streamtop --export-grafana
```

Alert kinds for `--alert-on`: `stall`, `shi_below_70`, `http_5xx`, `mismatch`, `ad_start`.

## Keys

| Key | Action |
|-----|--------|
| `q` / `Esc` / `Ctrl+C` | Quit (Esc returns to channel list when open) |
| `Space` | Save report under `diagnostics/` (URLs and secrets redacted) |
| `c` | Copy a curl for the last segment (redacted) |
| `p` | Quick Play via `mpv` or `ffplay` (not available in Docker) |
| `r` | Reset metrics |
| `Tab` | Channel overlay |
| `?` | Help |
| `/` | Search in channel list |
| `j` / `k` | Scroll log or channel list |

Compare mode: `Space` pause/resume (ring buffer), `d` detail, `l` log focus, `c` curl, `h` HAR, `Tab` focus pane.

## License

MIT. See [LICENSE](LICENSE).
