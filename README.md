# streamtop

Live HLS, DASH, and IPTV stream diagnostics in the terminal.

<img width="1099" height="592" alt="Animation" src="https://github.com/user-attachments/assets/92b89472-ed4a-45ac-b9ff-c5f4a85fd4b8" />


## Install

```bash
cargo install --path .
```

Or build a release binary:

```bash
cargo build --release
```

Windows: if the linker fails, put [WinLibs MinGW](https://winlibs.com/) on your `PATH`, then rebuild.

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
| Status | URL, LIVE / ESTIMATED, health score (SHI), video FPS, latency, CDN, buffer |
| Last segment | Sequence, sizes, DNS / TCP / TLS / TTFB, container type |
| ABR ladder | Bitrates, resolution, FPS, codecs — `[wire]` = from the bitstream, red = manifest vs wire mismatch |
| Charts | Latency or TTFB, download rate or transfer time |
| Log | Warnings, ads (SCTE-35), stalls, HTTP errors |

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

# Headless pass/fail (CI)
streamtop <URL> --summary --timeout 10

# Prometheus metrics on :9090/metrics
streamtop <URL> --prometheus

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
| `p` | Quick Play via `mpv` or `ffplay` (non-blocking) |
| `r` | Reset metrics |
| `Tab` | Channel overlay |
| `?` | Help |
| `/` | Search in channel list |
| `j` / `k` | Scroll log or channel list |

## License

MIT — see [LICENSE](LICENSE).
