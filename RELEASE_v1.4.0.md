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

### Pinnacle transport and CDN

- **HTTP/2 ALPN telemetry**: `NetworkTiming` adds `transfer_ms`, `http_version`; Prometheus `streamtop_http_version`, `streamtop_quic_handshake_seconds`, `streamtop_quic_stream_resets_total`
- **Multi-CDN skew**: `--multi-cdn URL1,URL2,...`, `--max-cdn-skew-ms`, matrix TUI, `ERR_CDN_SYNC_SKEW`
- **Middlebox heuristics wired**: `ERR_DPI_TCP_RESET`, redirect cycle/limit `ERR_HTTP_REDIRECT_LOOP`
- **TUI render cache**: header text rebuilt on events, not 30 FPS draw ticks
- **Summary JSON schema v5**: `http_version`, `transfer_ms`, `multi_cdn_skew`

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
| `--prefer-http2` | Enable reqwest HTTP/2 ALPN stack |

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

## Deprecated

- `--export-curl`, `--export-har`, `--export-report`, `--export-grafana`, `--export-incident` -> use `--export`
- `srt://` / `rtmp://` ingest URLs -> prefer WHEP

## Removed

- `--anti-dpi` active raw socket stub

## Quality gates

- **179 tests** passing (`cargo test --locked --all-targets`, 1 RTMP mock ignored)
- **Zero clippy warnings** (`-D warnings -W clippy::pedantic -W clippy::nursery`)
- Summary JSON validated against `schemas/summary.v1.json` (schema version 4)

## Full changelog

See [CHANGELOG.md](CHANGELOG.md).
