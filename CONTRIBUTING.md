# Contributing

## Scope

streamtop is a wire-probe CLI for HLS, DASH, IPTV, and WHEP. Prefer objective
measurements (manifest/segment timing, TR 101 290, container probes, metrics)
over speculative simulation or chat integrations unless there is a clear
operational need.

## Development setup

```bash
rustup toolchain install stable
cargo test --locked
cargo clippy --all-targets --all-features -- -D warnings -W clippy::pedantic -W clippy::nursery
cargo fmt --all --check
```

Optional hermetic E2E:

```bash
# Linux/macOS
./tests/e2e_verify.sh target/debug/streamtop
# Windows
./tests/e2e_verify.ps1 -Streamtop target/debug/streamtop.exe
```

## Code rules

- `#![forbid(unsafe_code)]`. No `.unwrap()` / `.expect()` in non-test `src/`.
- Bounded channels only; hot paths avoid unnecessary `.clone()` / heap churn.
- Validate untrusted bytes with `.get(..)` or `slice_util::subslice_len`.
- Comments only for protocol quirks, bitmasks, concurrency, or SSRF mitigations.
- Follow standard rustdoc conventions: keep doc comments concise, factual, and focused on invariant/protocol details.

## Pull requests

1. Open an issue first for non-trivial behavior changes.
2. Keep diffs surgical: one concern per PR when practical.
3. Update tests for parser/engine changes (valid and truncated/corrupt inputs).
4. If you touch packaging (`bucket/`, `dist/`, `Formula/`), keep version and
   hashes aligned with `Cargo.toml`.
5. Fill out the PR template checklist.

## Commit messages

Single-line subjects, repo style:

```
fix: grafana panel grid
chore: scoop hash
```

## Security

Do not file public issues for undisclosed vulnerabilities. See [SECURITY.md](SECURITY.md).

## Conduct

By participating you agree to the [Code of Conduct](CODE_OF_CONDUCT.md).
