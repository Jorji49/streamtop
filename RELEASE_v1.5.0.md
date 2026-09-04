# streamtop v1.5.0

Hardened wire-probe CLI: pruned speculative QoE simulation, legacy export shims, and noop flags. Summary schema v6.

## Changes

- Removed `--simulate-player` / throttle / RTT and `synthetic_qoe` from summary, metrics, and TUI
- Removed legacy `--export-*` shims and CLI aliases (`--metrics-port`, `range-probe`, `matrix`, `headless`)
- Removed noop `--prefer-http2`
- Reason code `ERR_TCP_IO_RESET` replaces `ERR_DPI_TCP_RESET`
- Docker build includes `.cargo/config.toml` for HTTP/3 (`reqwest_unstable`)
- Docs trimmed: no persona table, no SEO keyword block

## Install

```bash
cargo install streamtop --version 1.5.0
```

GitHub Release binaries: https://github.com/Jorji49/streamtop/releases/tag/v1.5.0
