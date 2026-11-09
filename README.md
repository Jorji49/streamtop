<div align="center">

<img width="96" alt="streamtop logo" src="https://github.com/user-attachments/assets/95b0df39-8404-4229-a071-7876ba6f3fde" />

# streamtop

**Terminal HLS, DASH, and IPTV stream monitor with wire probes, health checks, SCTE-35, TR 101 290, and Prometheus metrics.**

<a title="This tool is Tool Of The Week on Terminal Trove, The HOME of all things in the terminal" href="https://terminaltrove.com/streamtop/"><img src="https://cdn.terminaltrove.com/media/badges/tool_of_the_week/png/terminal_trove_tool_of_the_week_gold_on_black_bg.png" alt="Terminal Trove Tool Of The Week" height="50" /></a>

[![Awesome Ratatui](https://img.shields.io/badge/awesome-ratatui-e43716?style=for-the-badge&logo=rust&logoColor=white)](https://github.com/ratatui/awesome-ratatui)
[![Awesome Rust](https://img.shields.io/badge/awesome-rust-dea584?style=for-the-badge&logo=rust&logoColor=white)](https://github.com/rust-unofficial/awesome-rust)

[![Crates.io](https://img.shields.io/crates/v/streamtop?style=flat-square&color=007ec6&labelColor=1c1c1c)](https://crates.io/crates/streamtop)
[![Release](https://img.shields.io/github/v/release/Jorji49/streamtop?label=release&style=flat-square&color=007ec6&labelColor=1c1c1c)](https://github.com/Jorji49/streamtop/releases/latest)
[![Downloads](https://img.shields.io/crates/d/streamtop?style=flat-square&color=2ea44f&labelColor=1c1c1c)](https://crates.io/crates/streamtop)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue?style=flat-square&labelColor=1c1c1c)](LICENSE)


<img width="1200" alt="streamtop HLS DASH IPTV terminal dashboard demo" src="https://github.com/user-attachments/assets/f4fbb3e2-f572-4003-bb5d-b22cbba52a80" />

</div>

**[Documentation](https://jorji49.github.io/streamtop/)** | [Install](https://jorji49.github.io/streamtop/install.html) | [IPTV checking](https://jorji49.github.io/streamtop/iptv.html) | [CI](https://jorji49.github.io/streamtop/ci.html) | [Comparison](https://jorji49.github.io/streamtop/comparison.html) | [Changelog](CHANGELOG.md)

streamtop is a Rust CLI and terminal UI for monitoring live video streams. Point it at an HLS playlist (`.m3u8`), an MPEG-DASH manifest (`.mpd`), an IPTV channel list (`.m3u`, `.json`, `.yaml`), or a WHEP endpoint. It shows segment timing, codec data read from the wire, SCTE-35 ad markers, CDN behavior, and health scores without a browser or GUI player.

## Install

```bash
cargo install streamtop                          # or: cargo binstall streamtop
brew tap Jorji49/tap && brew install streamtop   # macOS / Linux
scoop bucket add streamtop https://github.com/Jorji49/streamtop && scoop install streamtop
docker run -it --rm ghcr.io/jorji49/streamtop:latest <URL>
```

Prebuilt binaries for Linux (x86_64 and arm64, glibc and musl), macOS (Apple Silicon and Intel), and Windows x86_64 are on [GitHub Releases](https://github.com/Jorji49/streamtop/releases/latest). Arch Linux, winget, and source builds are covered in the [install guide](https://jorji49.github.io/streamtop/install.html). `streamtop completions <bash|zsh|fish|elvish|powershell>` prints a completion script and `streamtop man` prints the man page.

## Usage

```bash
# Live dashboard for HLS or DASH
streamtop "https://example.com/master.m3u8"
streamtop "https://example.com/manifest.mpd" --tr101290 --probe-sei

# IPTV list: channel picker
streamtop ./channels.m3u

# Several streams in one grid (80x24 scrolls). Enter opens one stream.
streamtop --grid URL1 URL2 URL3
streamtop --agent agent.toml --grid

# Save a redacted evidence bundle when a stall or continuity error fires
streamtop URL --evidence-dir ./evidence --evidence-segments 2

# Check every entry: table, JSON or CSV, a playlist of working ones, exit 1 on dead
streamtop check ./channels.m3u --alive-out working.m3u --fail-on-dead

# Decode one SCTE-35 cue (Base64, hex, or an HLS tag line)
streamtop scte35 "/DAvAAAAAAAA///wFAVIAACPf+/+c2nALv4AUsz1AAAAAAAKAAhDVUVJAAABNWLbowo="

# Headless PASS/FAIL for CI (JSON or SARIF)
streamtop <URL> --summary --summary-format json --timeout 10

# Prometheus /metrics on 127.0.0.1:9184
streamtop <URL> --prometheus
```

## What it checks

* HLS (including LL-HLS parts) and MPEG-DASH, live and VOD
* Segment DNS / TCP / TLS / TTFB timing and CDN HIT/MISS for 12 CDNs
* ABR ladder versus the bitstream: resolution, FPS, GOP, audio
* TR 101 290 P1/P2 on MPEG-TS, SEI captions and HDR metadata
* IPTV playlist checks with `streamtop check`
* SCTE-35 in HLS tags and DASH `emsg`, with mismatch detection, and `streamtop scte35` for single cues
* Side-by-side compare, a multi-stream grid, and multi-CDN live-edge skew
* Exports: HTML/JSON reports, HAR, curl, SARIF, Grafana dashboard
* Prometheus, OpenTelemetry, webhooks, a multi-stream agent, and opt-in alarm evidence bundles

streamtop reads headers and the first bytes of each segment. It does not decode media, and it does not take SRT, RTMP, or UDP input. See [Limits](https://jorji49.github.io/streamtop/limits.html) and how it compares with [TSDuck, ffprobe, threefive, and iptv-checker](https://jorji49.github.io/streamtop/comparison.html).

## Contributing

Bug reports and pull requests are welcome. See [CONTRIBUTING.md](CONTRIBUTING.md). Questions and ideas go to [Discussions](https://github.com/Jorji49/streamtop/discussions).

## License

[MIT](LICENSE).
