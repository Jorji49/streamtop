# streamtop v1.4.0 Technical Report

## Scope

Final stabilization for production monitoring: HTTP/3 transport telemetry, zero-heap TUI rendering, full reason-code wiring, and removal of legacy SRT/RTMP ingest.

## Architecture invariants

- Zero-decode: wire timing and container headers only (MPEG-TS, fMP4, ADTS); no H.264/HEVC/AAC decoders
- `#![forbid(unsafe_code)]` on all crates; no `.unwrap()` / `.expect()` in production `src/`
- Bounded I/O: `EVENT_CHANNEL_CAPACITY = 512`, `try_send` drop counters, bounded stream readers
- No stubs: no `TODO`, `FIXME`, `STUB`, or `unimplemented!()` in `src/`

## Removed

- `src/engine/ingest_probe.rs` and SRT/RTMP routing
- `IngestStats`, `StreamEvent::Ingest`, `ingest_stats` in summary JSON schema
- `--allow-insecure-ingest` CLI flag
- Active anti-DPI socket fragmentation; `middlebox.rs` remains passive read-only heuristics

## HTTP/3 transport telemetry

- reqwest built with `http3` feature; `.cargo/config.toml` sets `reqwest_unstable`
- ALPN detection: h3, h2, http/1.1 via `HttpVersion::from_reqwest`
- QUIC fields on `NetworkTiming`: handshake ms, 0-RTT flag, stream resets
- Prometheus: `streamtop_http_version{version}`, `streamtop_quic_handshake_seconds`, `streamtop_quic_stream_resets_total`
- Summary JSON v5: `http_version`, `transfer_ms`, `quic_handshake_ms`

## Zero-heap TUI

- `UiRenderCache` stores pre-built `Paragraph<'static>` widgets for Status and Last Segment panels
- Rebuilt on `StreamEvent`, not on 30 FPS draw ticks
- `layout::draw_header` and `draw_segment_panel` borrow cached widgets with no per-frame `clone()` or `format!`

## Reason codes

All `DiagnosticReasonCode` variants emit stable `ERR_*` labels across TUI logs, JSON/SARIF export, and Prometheus where applicable:

- Redirect loop: `ERR_HTTP_REDIRECT_LOOP`
- DPI reset: `ERR_DPI_TCP_RESET` (poller + middlebox heuristics)
- CDN skew: `ERR_CDN_SYNC_SKEW`
- Budget gates, TR 101 290, ABR, DoH, AES key, stall risk, part RTF

## Verification

```bash
cargo test --locked --all-targets
cargo clippy --all-targets -- -D warnings -W clippy::pedantic -W clippy::nursery
```

Hermetic E2E: `tests/e2e_verify.sh`, `tests/e2e_verify.ps1` (legacy URL rejection, schema v5).
