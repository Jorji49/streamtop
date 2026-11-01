# Comparison with other tools

streamtop overlaps with several established tools. None of them is a replacement for the others; they answer different questions. This page tries to be factual about where each one fits. Corrections are welcome as issues or pull requests.

## Summary

| | streamtop | TSDuck | ffprobe | threefive | iptv-checker |
|---|---|---|---|---|---|
| Main job | Live HLS/DASH monitoring over time | MPEG-TS analysis and processing | Container and codec inspection | SCTE-35 decode and encode | Check links in IPTV playlists |
| Language | Rust | C++ | C (FFmpeg) | Python | Node.js |
| Interface | TUI and CLI | CLI (`tsp` plugins and utilities) | CLI | CLI and library | CLI and library |
| HLS / DASH playlists | Polls live playlists continuously | HLS input plugin | Reads them as inputs | Parses SCTE-35 in HLS; DASH SCTE-214 support | Uses ffprobe per link |
| MPEG-TS depth | Probe window only (TR 101 290 P1/P2) | Whole-stream analysis, PSI/SI tables, PCR and continuity checks, many plugins | Demux and decode | SCTE-35 sections in TS | None |
| UDP / multicast / SRT | No | Yes | Yes (via FFmpeg protocols) | UDP and multicast for TS | No |
| Media decode | No | No (transport level) | Yes | No | Via ffprobe |
| SCTE-35 | Decodes HLS tags and DASH `emsg` live, flags mismatches; `streamtop scte35` decodes one cue | Splice monitoring and injection plugins | Exposes data streams, no SCTE-35 semantics | Full decode and encode, the main purpose | No |
| CDN / HTTP timing | DNS, TCP, TLS, TTFB, CDN HIT/MISS per segment | No | No | No | No |
| Prometheus / OTLP | Yes | No | No | No | No |
| CI verdicts | PASS/FAIL JSON, SARIF, budgets | Exit codes from scripts | Exit codes from scripts | No | Results written to an output directory |

## When to use which

**Use TSDuck** when the problem is inside the transport stream: PSI/SI tables, PID mapping, PCR and continuity analysis over the whole stream, multicast or DVB input, remultiplexing, or SCTE-35 injection. TSDuck works at the MPEG-TS level and is far deeper there than streamtop. streamtop only inspects the first bytes of each segment.

**Use ffprobe** when you need exact codec and container parameters from one file or URL, or anything that needs decoding. ffprobe reads a snapshot; it does not track a live playlist over time, and it does not report HTTP timing or CDN behavior.

**Use threefive** when the job is SCTE-35 itself: decoding cues from TS, HLS, base64, hex, or XML, encoding new cues, or injecting them. streamtop decodes the cues it finds while monitoring, and `streamtop scte35` decodes a single Base64 or hex cue, but it does not encode, inject, or verify the CRC.

**Use iptv-checker** when you want per-channel status plus ffprobe stream metadata (codecs, resolution) for a playlist, with retries, delays, and proxy support. `streamtop check` probes reachability and segment TTFB without ffprobe, prints a table, JSON, or CSV, and can write the working entries to a new M3U with `--alive-out`.

**Use streamtop** when you are watching a live HLS or DASH channel over minutes or hours and want to see segment timing, CDN edge behavior, stalls, ABR ladder versus bitstream mismatches, and ad markers in one terminal view, or when you want that as Prometheus metrics or a CI gate.

## Other tools

* [Eyevinn hls-monitor](https://github.com/Eyevinn/hls-monitor): HTTP service that monitors HLS streams for errors.
* [DASH-IF Conformance](https://github.com/Dash-Industry-Forum/DASH-IF-Conformance): the reference conformance validator for MPEG-DASH. Use it for spec conformance; streamtop does not validate full DASH conformance.
* Apple `mediastreamvalidator`: the reference HLS validator, macOS only.
