# Compare and multi-CDN

## Side by side

```bash
streamtop --compare <URL_1> <URL_2>
```

Two streams in one TUI, for example origin and edge, or two encoder outputs. Keys: `Space` pause/resume, `d` detail, `l` log focus, `c` curl, `h` or `e` HAR, `Tab` switch pane.

## Multi-CDN skew

```bash
streamtop --multi-cdn akamai=https://a.example/live.m3u8,cloudflare=https://b.example/live.m3u8
streamtop --multi-cdn https://a.example/live.m3u8,https://b.example/live.m3u8 --summary --timeout 15
```

Polls every edge concurrently and builds a live-edge skew matrix from media sequence numbers and PDT. When the skew passes `--max-cdn-skew-ms` (default 3000) it reports `ERR_CDN_SYNC_SKEW`. With `--summary` it prints JSON and exits non-zero when the threshold is exceeded.
