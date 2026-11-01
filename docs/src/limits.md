# Limits

streamtop is a wire and container probe. It is not a decoder or a full transport stream analyzer.

* **No media decode.** Probes read headers and the first bytes of segments (the probe window). A stream that sends black frames or silent audio can still look healthy.
* **TR 101 290** covers P1 and P2 checks on the MPEG-TS probe window only (the first 64 KB of each probed segment), not an analysis of the whole multiplex. Windows are not contiguous, so continuity counter, PCR, and PTS checks restart at each segment and errors at segment boundaries are not seen. PAT and PMT repetition is measured on the PCR clock inside the window. CRC-32 and SI tables (SDT, NIT, EIT) are not checked. Use TSDuck for deep transport stream work.
* **Glass-to-glass** is an estimate from HLS PDT, DASH segment availability time (or MPD publish time), and PRFT when present. DASH timestamps more than one hour from the local clock are ignored. It is not player end-to-end latency.
* **WHEP** is signaling only (SDP offer and answer). There is no WebRTC media path.
* **No SRT, RTMP, UDP, or multicast input.** `srt://` and `rtmp://` URLs are rejected at startup.
* **DASH live edge.** For a dynamic MPD with a SegmentTemplate, streamtop probes the newest media segment: the last `S` of a SegmentTimeline, or for `duration` templates one segment behind the last complete one by the local clock. If the local clock is off by more than a segment, those probes can return 404. Without a SegmentTemplate it probes the first Representation URL when MPD@publishTime changes.
* **DASH conformance** is not validated against the full specification. Use the DASH-IF Conformance tool for that.
* **Linter score** (TUI) uses four flags: RFC -15, origin stall -20, CDN MISS -5, high TTFB -10. The summary JSON keeps the older SHI field names (`schema_version` 7).
* **Quick Play** needs `mpv` or `ffplay` on the PATH and does not work in the Docker image.
