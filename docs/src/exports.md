# Exports

`--export FORMAT[:FILE]` is repeatable.

| Format | Output |
|--------|--------|
| `report-html[:PATH]` | Compliance report as HTML (default `report.html`) |
| `report-json[:PATH]` | Compliance report as JSON (default `report.json`) |
| `curl` | curl command for the last segment, printed to stdout |
| `har[:PATH]` | HAR file (default `incident.har`) |
| `incident[:PATH]` | Opens the dashboard. Press `e` to write a redacted bundle (curl, HAR, timeline, findings) to PATH. Needs a terminal. |
| `sarif[:PATH]` | SARIF 2.1.0 findings (default `streamtop.sarif`) |
| `grafana` | Grafana dashboard JSON for the Prometheus metrics, written to `streamtop-grafana.json` |

```bash
streamtop <URL> --export report-html:report.html --timeout 10
streamtop <URL> --export curl --timeout 10
streamtop <URL> --export har:incident.har --export sarif --timeout 10
streamtop --export grafana
```

All formats except `incident` run headless and work in CI. In the dashboard, `Space` writes `diagnostics/<channel>_<time>.json` and `c` copies a curl command. URLs, tokens, and secrets are redacted in all exports.
