# streamtop

[![Crates.io](https://img.shields.io/crates/v/streamtop.svg)](https://crates.io/crates/streamtop)
[![Downloads](https://img.shields.io/crates/d/streamtop.svg)](https://crates.io/crates/streamtop)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

Live HLS, DASH, and IPTV stream diagnostics in the terminal.

<img width="1099" height="592" alt="Animation" src="https://github.com/user-attachments/assets/92b89472-ed4a-45ac-b9ff-c5f4a85fd4b8" />

## Installation

### Rust

```bash
cargo install streamtop
```

```bash
cargo install cargo-binstall
cargo binstall streamtop
```

### Windows — Scoop

```powershell
scoop bucket add streamtop https://github.com/Jorji49/streamtop
scoop install streamtop/streamtop
```

### Windows — Winget

Status: **PR [#424450](https://github.com/microsoft/winget-pkgs/pull/424450) In Review** (not merged yet). After merge:

```powershell
winget install streamtop
```

### macOS / Linux — Homebrew

```bash
brew tap Jorji49/tap
brew install streamtop
```

```bash
brew install --formula https://raw.githubusercontent.com/Jorji49/streamtop/main/Formula/streamtop.rb
```

### Arch Linux (AUR)

Status: **Template Ready** (`dist/aur/PKGBUILD` in-repo). Package is **not published** to the AUR yet — do not expect `yay -S streamtop-bin` to resolve until a maintainer uploads it.

```bash
# After AUR publish:
yay -S streamtop-bin
```

### Docker

Image ships the **CLI binary only** (no GUI, no `mpv` / `ffplay`). **Quick Play (`p`) will not work inside the container.**

```bash
docker run -it --rm ghcr.io/jorji49/streamtop:latest <URL>
```

Headless / metrics example (metrics bind defaults to `127.0.0.1:9184`):

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

### From source / release binary

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

`--probe-headers` downloads only the start of each segment (faster, enough for header and wire checks).

## What you see

| Area | Meaning |
|------|---------|
| Status | URL, LIVE / ESTIMATED, health score (SHI), video FPS, latency, CDN, buffer, `[LL-HLS]` part timing |
| Last segment | Sequence, sizes, DNS / TCP / TLS / TTFB, container type |
| ABR ladder | Bitrates, resolution, FPS, codecs — `[wire]` = from the bitstream, red = manifest vs wire mismatch |
| Charts | Latency or TTFB, download rate or transfer time |
| Log | Warnings, ads (binary SCTE-35), stalls, HTTP errors |

FPS comes from the playlist (`FRAME-RATE` / `@frameRate`) when present; otherwise from the media bitstream when it can be read.

## Commands

```bash
# Live dashboard
streamtop <URL> [--probe-headers] [-H "Key: Value"] [-A user-agent] [-i MS]

# Compare two feeds side by side
streamtop --compare <URL_1> <URL_2> --probe-headers

# Webhook alerts (Slack / Discord / any HTTP endpoint)
streamtop <URL> --webhook https://hooks.example/x --alert-on stall,shi_below_70,http_5xx

# Channel list audit → audit_report.json / .csv
streamtop ./channels.m3u --audit

# Headless pass/fail (CI) — JSON schema: schemas/summary.v1.json
streamtop <URL> --summary --summary-format json --timeout 10

# Ticket attach: curl / HAR after a short poll
streamtop <URL> --export-curl --probe-headers
streamtop <URL> --export-har incident.har --timeout 10

# Named profile from ~/.config/streamtop/config.toml (see config.example.toml)
streamtop <URL> --profile cdn

# Prometheus metrics on 127.0.0.1:9184/metrics (optional --metrics-token)
streamtop <URL> --prometheus
streamtop <URL> --prometheus 9184 --metrics-bind 0.0.0.0 --metrics-token "$STREAMTOP_METRICS_TOKEN"

# Optional DRM license / LA_URL TTFB probe
streamtop <URL> --probe-drm --summary

# Grafana dashboard JSON (import; scrape streamtop --prometheus)
streamtop --export-grafana
```

Alert kinds for `--alert-on`: `stall`, `shi_below_70`, `http_5xx`, `mismatch`, `ad_start`.

## Keys

| Key | Action |
|-----|--------|
| `q` / `Esc` / `Ctrl+C` | Quit (Esc returns to channel list when one is open) |
| `Space` | Save report under `diagnostics/` |
| `c` | Copy a curl for the last segment |
| `p` | Quick Play via `mpv` or `ffplay` (non-blocking; **not available in Docker**) |
| `r` | Reset metrics |
| `Tab` | Channel overlay |
| `?` | Help |
| `/` | Search in channel list |
| `j` / `k` | Scroll log or channel list |

## License

MIT — see [LICENSE](LICENSE).
