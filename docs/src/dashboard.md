# Dashboard and keys

## Layout

| Area | Contents |
|------|----------|
| Status | URL, LIVE / ESTIMATED, linter score, FPS, GOP and audio badges, latency, CDN, buffer, G2G, LL-HLS |
| Last segment | Sequence, sizes, DNS / TCP / TLS / DoH / TTFB, container, GOP interval, audio |
| ABR ladder | Bitrate, resolution, FPS, codecs. `[wire]` values come from the bitstream; red means manifest and wire disagree |
| Charts | Latency or TTFB; download rate or transfer time |
| Log | Warnings, SCTE-35 ad markers, stalls, HTTP errors |

FPS comes from the playlist `FRAME-RATE` or `@frameRate` when present, otherwise from the bitstream. GOP interval comes from keyframe PTS across segments (fixed or variable). Audio codec, rate, and channels come from ADTS, fMP4, or the MPEG-TS PMT in the probe window.

## What is probed

* **HLS**: live and VOD media playlists, LL-HLS `#EXT-X-PART` and `PRELOAD-HINT` with per-part TTFB and download-to-duration ratio, `#EXT-X-PROGRAM-DATE-TIME`.
* **MPEG-DASH**: live and VOD, ServiceDescription latency, UTCTiming, ContentProtection and PSSH.
* **Containers**: fMP4, MPEG-TS, ADTS, and H.264/H.265 elementary streams, read from the probe window without a decoder.
* **AES-128**: `#EXT-X-KEY` fetch and in-memory decrypt so encrypted TS/fMP4 can be inspected.
* **SCTE-35**: HLS tags (`EXT-X-DATERANGE`, `EXT-X-CUE-OUT`, `EXT-OATCLS-SCTE35`, `EXT-X-SCTE35`), DASH inband `emsg`, and mismatches between the two.
* **CDN edge**: HIT/MISS from edge headers for Cloudflare, CloudFront, Akamai, Fastly, BunnyCDN, Azure CDN, Google Cloud CDN, JOCDN, Medianova, Edgio, Lumen, and Gcore.
* **Glass-to-glass estimate**: from PRFT, HLS PDT, or the DASH segment availability time (MPD publish time as a fallback). This is an estimate, not player end-to-end latency.
* **Buffer model**: rebuffer probability from observed download-to-duration ratios.

## Keys

| Key | Action |
|-----|--------|
| `q` / `Esc` / `Ctrl+C` | Quit (`Esc` leaves the channel list when it is open) |
| `Space` | Write `diagnostics/<channel>_<time>.json` (URLs and secrets redacted) |
| `c` | Copy a curl command for the last segment (redacted) |
| `p` | Play with `mpv` or `ffplay` using the active headers |
| `r` | Reset metrics |
| `Tab` | Channel overlay |
| `?` | Help |
| `/` | Regex log filter (Enter to lock, Esc to clear) |
| `f` / `F` | Cycle preset log filter / clear it |
| `t` | TR 101 290 overlay |
| `s` | SEI / HDR / caption overlay |
| `j` / `k` | Scroll the log or channel list |

Compare mode: `Space` pause/resume, `d` detail, `l` log focus, `c` curl, `h` or `e` HAR, `Tab` switch pane.

The grid (`streamtop --grid URL1 URL2`, or several URLs) uses the same dashboard after you press Enter on a tile. `q` or Esc leaves that dashboard and returns to the grid. See [Grid and evidence](grid.md).
