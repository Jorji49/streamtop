# streamtop v1.4.0

Terminal HLS, DASH, IPTV, and WHEP stream diagnostics with wire probes, CI export formats, and Prometheus metrics.

## Highlights

### Transport safety

- Bounded reads on infinite chunked MPEG-TS responses (`PROBE_READ_TIMEOUT_SECS = 4s`)
- Same-origin redirect token preservation; loop detection (`ERR_HTTP_REDIRECT_LOOP`)
- SSRF pinning unchanged for webhooks, DRM, and OTLP endpoints

### CI integration

- **SARIF 2.1.0**: `--summary-format sarif` or `--export sarif:FILE` for GitHub Code Scanning
- **GitHub Actions step summary**: auto-write SHI, RTF, Part RTF, TR 101 290, ABR, and budget tables when `GITHUB_STEP_SUMMARY` is set
- **Stream budget**: `--budget-max-rtf`, `--budget-max-ttfb`, `--budget-max-cc-errors`, `--budget-max-drift` with JSON PASS/FAIL verdict
- **Unified export**: `--export report-html|report-json|curl|har|incident|grafana|sarif[:FILE]`

### Protocol expansion

- **LL-HLS parts**: `#EXT-X-PART` / `PRELOAD-HINT` part TTFB, Part RTF, `streamtop_part_dl_duration_ratio`
- **DoH timing**: `--doh-provider cloudflare|google|<URL>`, `doh_ms` in wire timing, `streamtop_dns_doh_duration_seconds`
- **WHEP probe**: sub-second SDP signaling check (201/200 answer, codecs, ICE, stream IDs)
- **AES-128-CBC probe decrypt**: in-memory `#EXT-X-KEY` fetch for encrypted TS/fMP4 wire analysis

### Transport and CDN

- **HTTP/3 (QUIC) telemetry**: reqwest `http3` + ALPN; handshake timing on h3; Prometheus `streamtop_http_version`, `streamtop_quic_handshake_seconds`, `streamtop_quic_stream_resets_total`
- **Multi-CDN skew**: `--multi-cdn URL1,URL2,...`, `--max-cdn-skew-ms`, matrix TUI, `ERR_CDN_SYNC_SKEW`
- **Transport I/O drop hint**: `ERR_TCP_IO_RESET` when connect succeeds but transfer ends with zero bytes and an I/O error
- **Zero-heap TUI**: `UiRenderCache` stores pre-built Status and Last Segment `Paragraph` widgets; draw borrows without per-frame allocation
- **Summary JSON schema v6**: `http_version`, `transfer_ms`, `multi_cdn_skew`; `ingest_stats` and `synthetic_qoe` removed

## Key CLI flags

| Flag | Purpose |
|------|---------|
| `--export FORMAT[:FILE]` | Unified export (see formats above) |
| `--doh-provider PROVIDER` | DoH lookup (`cloudflare`, `google`, or custom JSON URL) |
| `--summary-format sarif` | SARIF 2.1.0 on stdout |
| `--github-step-summary FILE` | GHA step summary markdown |
| `--budget-max-rtf RATIO` | CI budget: max segment RTF |
| `--budget-max-ttfb DURATION` | CI budget: max TTFB (`250ms`, `2s`) |
| `--budget-max-cc-errors N` | CI budget: TR 101 290 CC errors |
| `--budget-max-drift DURATION` | CI budget: subtitle A/V drift |
| `--multi-cdn URLS` | Multi-CDN skew matrix (comma-separated or `label=URL`) |
| `--max-cdn-skew-ms MS` | Skew threshold for `ERR_CDN_SYNC_SKEW` (default 3000) |

## Examples

```bash
# CI budget + SARIF + step summary
streamtop "$URL" --budget-max-rtf 1.0 --budget-max-ttfb 500ms --budget-duration 30
streamtop "$URL" --export sarif:streamtop.sarif --timeout 15

# LL-HLS with DoH
streamtop "$URL" --doh-provider cloudflare --probe-headers --summary --summary-format json

# WHEP signaling check
streamtop "https://origin.example/whep/feed"
```

## Removed

- `--simulate-player`, `--throttle-kbps`, `--simulated-rtt-ms`, and `synthetic_qoe` summary field
- `--prefer-http2` (noop flag)
- Legacy `--export-*` shims; use `--export` only
- `--anti-dpi` active raw socket stub
- Raw `srt://` / `rtmp://` ingest probes and `ingest_stats` summary field

## Quality gates

- **185+ tests** passing (`cargo test --locked`)
- **Zero clippy warnings** (`-D warnings -W clippy::pedantic -W clippy::nursery`)
- Summary JSON validated against `schemas/summary.v1.json` (schema version 6)

## Full changelog

See [CHANGELOG.md](CHANGELOG.md).
