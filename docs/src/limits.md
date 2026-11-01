# Limits

streamtop is a wire and container probe. It is not a decoder or a full transport stream analyzer.

* **No media decode.** Probes read headers and the first bytes of segments (the probe window). A stream that sends black frames or silent audio can still look healthy.
* **TR 101 290** covers P1 and P2 checks on the MPEG-TS probe window only, not an analysis of the whole multiplex. Use TSDuck for deep transport stream work.
* **Glass-to-glass** is an estimate from HLS PDT, DASH publish time, and PRFT when present. It is not player end-to-end latency.
* **WHEP** is signaling only (SDP offer and answer). There is no WebRTC media path.
* **No SRT, RTMP, UDP, or multicast input.** `srt://` and `rtmp://` URLs are rejected at startup.
* **DASH conformance** is not validated against the full specification. Use the DASH-IF Conformance tool for that.
* **Linter score** (TUI) uses four flags: RFC -15, origin stall -20, CDN MISS -5, high TTFB -10. The summary JSON keeps the older SHI field names (`schema_version` 6).
* **Quick Play** needs `mpv` or `ffplay` on the PATH and does not work in the Docker image.
