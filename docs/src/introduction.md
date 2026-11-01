# streamtop

streamtop is a Rust CLI and terminal UI for monitoring live video streams. Point it at an HLS playlist (`.m3u8`), an MPEG-DASH manifest (`.mpd`), an IPTV channel list (`.m3u`, `.json`, `.yaml`), or a WHEP HTTP endpoint. It shows segment timing, codec data read from the wire, ad markers, and health scores in the terminal, without a browser or GUI player.

![streamtop dashboard](https://github.com/user-attachments/assets/f4fbb3e2-f572-4003-bb5d-b22cbba52a80)

Typical uses:

* Debug CDN and origin issues on a live channel (TTFB, HTTP status, cache HIT/MISS, stalls).
* Check encoder output against the manifest (resolution, FPS, GOP, audio).
* See SCTE-35 ad markers in HLS tags and DASH `emsg` boxes.
* Check which channels in an IPTV playlist are working.
* Gate CI on a stream with PASS/FAIL JSON, SARIF, or budget thresholds.
* Scrape Prometheus metrics or send OTLP traces from a live probe.

```bash
cargo install streamtop
streamtop "https://example.com/master.m3u8"
```

Source code, issues, and releases: [github.com/Jorji49/streamtop](https://github.com/Jorji49/streamtop). License: MIT.
