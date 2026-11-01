# FAQ

**How is streamtop different from ffprobe or VLC?**
ffprobe inspects a single file or URL snapshot, and VLC plays the stream. streamtop polls live playlists, tracks segment health over time, shows SCTE-35 markers and linter score trends, and exports Prometheus metrics and CI summary JSON. See [Comparison](comparison.md).

**Does it work headless in CI?**
Yes. Use `--summary --summary-format json --timeout N` for PASS/FAIL output, `--summary-format sarif` or `--export sarif:FILE` for Code Scanning, and `--budget-max-*` for threshold gates. See [Headless and CI](ci.md).

**Which protocols are supported?**
HLS (including LL-HLS parts), MPEG-DASH, IPTV channel lists (M3U, JSON, YAML), and WHEP HTTP endpoints. Wire probes cover fMP4, MPEG-TS, ADTS, and H.264/H.265 elementary streams without a decoder.

**Can it check which channels in my IPTV playlist work?**
Yes, with `--audit`. See [IPTV playlist checking](iptv.md).

**Is it safe to expose Prometheus metrics?**
The default bind is loopback. A non-loopback bind requires `--metrics-token` or `STREAMTOP_METRICS_TOKEN`, and requests must send `Authorization: Bearer`.

**Are tokens in my URLs written to disk?**
Exports and diagnostics redact URLs, query tokens, and secrets.

**Does it need FFmpeg?**
No. Only Quick Play (`p`) uses `mpv` or `ffplay` if they are installed.

**Where do I report a bug?**
[GitHub issues](https://github.com/Jorji49/streamtop/issues). Questions and ideas can go to [Discussions](https://github.com/Jorji49/streamtop/discussions).
