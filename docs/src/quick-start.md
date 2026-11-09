# Quick start

```bash
# HLS live stream
streamtop "https://example.com/master.m3u8"

# MPEG-DASH
streamtop "https://example.com/manifest.mpd"

# IPTV channel list: opens a channel picker
streamtop ./channels.m3u

# MPEG-TS TR 101 290 checks and SEI metadata
streamtop "https://example.com/live.m3u8" --tr101290 --probe-sei

# Headless PASS/FAIL for CI
streamtop "https://example.com/master.m3u8" --summary --summary-format json --timeout 10

# Several live streams in one grid
streamtop --grid URL1 URL2 URL3
```

Segment fetches are range probes by default: streamtop requests only the first bytes of each segment, which is enough for header, container, and timing checks. Pass `--full-segment` to download whole segments (up to 32 MB) instead.

## Common options

| Option | Meaning |
|--------|---------|
| `-H "Key: Value"` | Extra HTTP header, repeatable |
| `-A <UA>` | User-Agent |
| `-i <MS>` | Manifest poll interval in milliseconds |
| `--profile <NAME>` | Load a named profile from `~/.config/streamtop/config.toml` (see `config.example.toml`) |
| `--full-segment` | Download full segments instead of range probes |
| `--tr101290` | ETSI TR 101 290 P1/P2 checks on the MPEG-TS probe window |
| `--probe-sei` | H.264/H.265 SEI: captions (CEA-608/708) and HDR metadata |
| `--probe-drm` | License server / `EXT-X-KEY` / DASH `LA_URL` TTFB |
| `--clearkey KID:KEY` | Staging ClearKey for encrypted probe checks |
| `--doh-provider cloudflare\|google\|<URL>` | DNS-over-HTTPS timing |

Run `streamtop --help` for the full list.
