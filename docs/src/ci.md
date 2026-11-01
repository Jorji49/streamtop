# Headless and CI

## PASS/FAIL summary

```bash
streamtop <URL> --summary --summary-format json --timeout 10
```

`--summary` listens for `--timeout` seconds (default 8) and prints a verdict. It returns PASS only when:

* the stream is LIVE,
* the linter score is at least 85,
* no critical RFC errors or origin stalls were seen,
* the last HTTP status was 200 or 206,
* at least one segment was fetched.

Any failed condition returns FAIL and a non-zero exit code. The JSON contract is `streamtop.summary.v1`; the schema is `schemas/summary.v1.json` and `schema_version` is currently 6.

Formats: `text` (default), `json`, `sarif`.

## SARIF for GitHub Code Scanning

```bash
streamtop <URL> --summary --summary-format sarif --timeout 10 > streamtop.sarif
# or
streamtop <URL> --export sarif:streamtop.sarif --timeout 10
```

## GitHub Actions step summary

```bash
streamtop <URL> --summary --github-step-summary "$GITHUB_STEP_SUMMARY" --timeout 10
```

When `GITHUB_STEP_SUMMARY` is set, `--summary` and budget mode write it automatically. The summary includes the linter score, download-to-duration ratio, TR 101 290 counts, the ABR ladder, and the budget table.

## Stream budgets

Budgets are threshold assertions. streamtop prints a JSON verdict and exits non-zero on a breach.

```bash
streamtop "$STREAM_URL" \
  --budget-max-rtf 1.0 \
  --budget-max-ttfb 500ms \
  --budget-max-cc-errors 0 \
  --budget-max-drift 2000ms \
  --budget-duration 30
```

| Flag | Fails when |
|------|-----------|
| `--budget-max-rtf` | download time divided by segment duration exceeds the ratio |
| `--budget-max-ttfb` | segment TTFB exceeds the value (`250ms`, `2s`) |
| `--budget-max-cc-errors` | TR 101 290 continuity counter errors exceed N |
| `--budget-max-drift` | A/V or subtitle drift exceeds the value |
| `--budget-duration` | probe window in seconds (default 30) |

## VOD crawl

```bash
streamtop --vod <URL> --summary
```

Crawls the playlist or MPD tree once without live polling.

## WHEP smoke test

```bash
streamtop "https://webrtc.example/live/whep" --timeout 5
```

Sends an SDP offer and parses the answer (TTFB, codecs, ICE candidates). Signaling only; no media path.

## Example workflow step

```yaml
- name: Stream check
  run: |
    cargo install streamtop
    streamtop "$STREAM_URL" --summary --summary-format json --timeout 15
```

Hermetic end-to-end tests for streamtop itself are in `tests/e2e_verify.sh` and `tests/e2e_verify.ps1`.
