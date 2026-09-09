# streamtop v1.5.1

CDN edge classify expansion and poller module split. Summary schema v6 unchanged.

## Changes

- CDN providers: JOCDN, Medianova, Edgio, Lumen, Gcore (HIT/MISS from edge headers)
- `ManifestPoller` moved to `src/engine/poller/` (HLS/DASH/segment/http modules)
- Hermetic fixtures: AES-128 key playlist, DASH UTCTiming, CDN 302, media-sequence gap
- Docs: Limits section; TUI shows linter score (JSON SHI fields frozen)

## Install

```bash
cargo install streamtop --version 1.5.1
```

GitHub Release binaries: https://github.com/Jorji49/streamtop/releases/tag/v1.5.1
