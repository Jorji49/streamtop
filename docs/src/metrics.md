# Metrics, agent, and alerts

## Prometheus

```bash
streamtop <URL> --prometheus            # 127.0.0.1:9184/metrics
streamtop <URL> --prometheus 9184 --metrics-bind 0.0.0.0 \
  --metrics-token "$STREAMTOP_METRICS_TOKEN"
curl -H "Authorization: Bearer $STREAMTOP_METRICS_TOKEN" http://host:9184/metrics
```

A non-loopback `--metrics-bind` requires a non-empty `--metrics-token` or `STREAMTOP_METRICS_TOKEN`. Only the `Authorization: Bearer` header is accepted; a `?token=` query is rejected.

`--prometheus` runs until Ctrl+C. It does not open the dashboard, and it cannot be combined with `--summary`, `--audit`, `--export`, `--multi-cdn`, `--compare`, `--grid`, or `--vod`.

`streamtop --export grafana` writes a dashboard JSON for these metrics.

## Multi-stream agent

```bash
streamtop --agent agent.example.toml
```

The agent polls every `[[streams]]` entry in the TOML file without a TUI and serves aggregated `/metrics`. See `agent.example.toml` in the repository.

```bash
streamtop --agent agent.toml --grid
```

`--grid` uses the same file but opens the [multi-stream grid](grid.md) instead of the metrics server. Stream ids must be unique, and each `url` must be non-empty. `--agent` does not combine with `--summary`, `--audit`, `--vod`, `--compare`, `--multi-cdn`, `--export`, or `--prometheus`; put metrics, alerts, and evidence in the config.

`otel_endpoint` in the agent file (and `--otel-endpoint` on the dashboard) exports OTLP traces and metrics about every two seconds.

## OpenTelemetry

```bash
streamtop <URL> --otel-endpoint http://127.0.0.1:4318
```

Sends OTLP traces and metric batches to `/v1/traces` and `/v1/metrics`. Private, link-local, and cloud metadata hosts are blocked unless `--allow-insecure-otel` is set.

## Webhooks

```bash
streamtop <URL> --webhook https://hooks.example/x --alert-on stall,shi_below_70,http_5xx
```

Slack, Discord, and generic HTTP targets are supported. `--alert-on` values: `stall`, `shi_below_70`, `http_5xx`, `mismatch`, `ad_start`, `ad_mismatch`. Private and metadata hosts are blocked unless `--allow-insecure-webhooks` is set (meant for tests).

## Profiles

`--profile NAME` loads headers, User-Agent, interval, and other options from `~/.config/streamtop/config.toml`. See `config.example.toml`.
