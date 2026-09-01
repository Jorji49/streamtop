#!/usr/bin/env bash
# End-to-end verification for streamtop v1.3.x (hermetic, no paid services).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

PASS=0
FAIL=0
TMP="${TMPDIR:-/tmp}/streamtop-e2e-$$"
mkdir -p "$TMP"
trap 'kill ${MOCK_PID:-} ${PROM_PID:-} ${AGENT_PID:-} 2>/dev/null || true; rm -rf "$TMP"' EXIT

log() { printf '[e2e] %s\n' "$*"; }
pass() { PASS=$((PASS + 1)); log "PASS: $*"; }
fail() { FAIL=$((FAIL + 1)); log "FAIL: $*" >&2; }

need_cmd() {
  command -v "$1" >/dev/null 2>&1 || {
    fail "missing required command: $1"
    exit 1
  }
}

need_cmd python3
need_cmd curl
if command -v jq >/dev/null 2>&1; then
  HAS_JQ=1
else
  HAS_JQ=0
  log "jq not found; using python3 for JSON assertions"
fi

json_get() {
  local expr="$1"
  local file="$2"
  if [[ "$HAS_JQ" -eq 1 ]]; then
    jq -r "$expr" "$file"
  else
    python3 -c "
import json, sys
doc = json.load(open(sys.argv[2], encoding='utf-8'))
cur = doc
for part in sys.argv[1].strip('.').split('.'):
    if part:
        cur = cur[part]
print(cur)
" "$expr" "$file"
  fi
}

run_summary() {
  local url="$1"
  shift
  local out="$TMP/summary.json"
  "$STREAMTOP" "$url" "$@" --summary --summary-format json --timeout 8 >"$out" 2>"$TMP/stderr.txt" || true
  if [[ ! -s "$out" ]]; then
    fail "no summary JSON for $url ($*)"
    cat "$TMP/stderr.txt" >&2 || true
    return 1
  fi
  if ! python3 "$ROOT/tests/e2e/validate_summary.py" "$out"; then
    fail "schema validation for $url"
    return 1
  fi
  echo "$out"
}

wait_mock() {
  local base="$1"
  local deadline=$((SECONDS + 60))
  while [[ $SECONDS -lt $deadline ]]; do
    if ! kill -0 "$MOCK_PID" 2>/dev/null; then
      fail "mock server died"
      cat "$TMP/mock.log" >&2 || true
      return 1
    fi
    if curl -sf --max-time 2 "${base}/health" >/dev/null; then
      return 0
    fi
    sleep 0.5
  done
  fail "mock server not ready after 60s"
  cat "$TMP/mock.log" >&2 || true
  return 1
}

wait_metrics() {
  local url="$1"
  local deadline=$((SECONDS + 30))
  while [[ $SECONDS -lt $deadline ]]; do
    if ! kill -0 "$PROM_PID" 2>/dev/null; then
      return 1
    fi
    local code
    code=$(curl -s -o /dev/null -w '%{http_code}' --max-time 2 "$url" || true)
    if [[ "$code" == "200" || "$code" == "401" ]]; then
      return 0
    fi
    sleep 0.5
  done
  return 1
}

log "Building streamtop release binary"
cargo build --release --quiet
STREAMTOP="$ROOT/target/release/streamtop"
[[ -x "$STREAMTOP" ]] || {
  fail "binary missing at $STREAMTOP"
  exit 1
}

log "Starting hermetic mock servers (HTTP/SRT/RTMP)"
python3 "$ROOT/tests/e2e/mock_all.py" >"$TMP/mock.log" 2>&1 &
MOCK_PID=$!
PROM_PID=""
AGENT_PID=""

BASE="http://127.0.0.1:8765"
wait_mock "$BASE" || exit 1

TR101_URL="${BASE}/tr101290/live.m3u8"
SEI_URL="${BASE}/sei/live.m3u8"
HLS_URL="${BASE}/live.m3u8"
LL_URL="${BASE}/ll-hls/master.m3u8"
DASH_URL="${BASE}/dash/live.mpd"
SRT_URL="srt://127.0.0.1:9000"
RTMP_URL="rtmp://127.0.0.1:1935/live/stream"

# --- 1. TR 101 290 ---
log "TR 101 290 compliance summary"
OUT=$(run_summary "$TR101_URL" --tr101290 --probe-headers) || true
if [[ -f "${OUT:-}" ]]; then
  P1=$(json_get ".tr101290.p1_violations" "$OUT")
  P2=$(json_get ".tr101290.p2_violations" "$OUT")
  if [[ "$P1" =~ ^[0-9]+$ ]] && [[ "$P2" =~ ^[0-9]+$ ]] && [[ "$P1" -gt 0 || "$P2" -gt 0 ]]; then
    pass "tr101290 violations reported (P1=$P1 P2=$P2)"
  else
    fail "expected tr101290.p1_violations or p2_violations > 0 (P1=$P1 P2=$P2)"
  fi
fi

# --- 2. SEI / HDR / captions ---
log "SEI probe summary"
OUT=$(run_summary "$SEI_URL" --probe-sei --probe-headers) || true
if [[ -f "${OUT:-}" ]]; then
  C608=$(json_get ".sei_metadata.cea608_present" "$OUT")
  HDR=$(json_get ".sei_metadata.hdr10_present" "$OUT")
  if [[ "$C608" == "True" || "$C608" == "true" ]] && [[ "$HDR" == "True" || "$HDR" == "true" ]]; then
    pass "sei_metadata captions and HDR detected"
  else
    fail "expected sei_metadata.cea608_present and hdr10_present (c608=$C608 hdr=$HDR)"
  fi
fi

# --- 3. Synthetic QoE ---
log "Synthetic QoE summary"
OUT=$(run_summary "$HLS_URL" --simulate-player --throttle-kbps 1500 --simulated-rtt-ms 120 --probe-headers) || true
if [[ -f "${OUT:-}" ]]; then
  RISK=$(json_get ".synthetic_qoe.rebuffer_risk_score" "$OUT")
  if [[ "$RISK" =~ ^[0-9]+$ ]] && [[ "$RISK" -ge 0 && "$RISK" -le 100 ]]; then
    pass "synthetic_qoe.rebuffer_risk_score=$RISK"
  else
    fail "rebuffer_risk_score out of range: $RISK"
  fi
fi

# --- 4. Legacy ingest URLs rejected ---
log "Legacy SRT URL rejection"
if "$STREAMTOP" "$SRT_URL" --summary 2>"$TMP/srt_err.txt"; then
  fail "expected srt:// to fail"
elif grep -qi "not supported" "$TMP/srt_err.txt"; then
  pass "srt:// rejected with clear error"
else
  fail "srt:// error message missing"
  cat "$TMP/srt_err.txt" >&2 || true
fi

log "Legacy RTMP URL rejection"
if "$STREAMTOP" "$RTMP_URL" --summary 2>"$TMP/rtmp_err.txt"; then
  fail "expected rtmp:// to fail"
elif grep -qi "not supported" "$TMP/rtmp_err.txt"; then
  pass "rtmp:// rejected with clear error"
else
  fail "rtmp:// error message missing"
  cat "$TMP/rtmp_err.txt" >&2 || true
fi

# --- 5. LL-HLS + DASH smoke (schema only) ---
log "LL-HLS fMP4 smoke"
OUT=$(run_summary "$LL_URL" --probe-headers) || true
if [[ -f "${OUT:-}" ]]; then
  SEG=$(json_get ".saw_segment" "$OUT")
  if [[ "$SEG" == "True" || "$SEG" == "true" ]]; then
    pass "LL-HLS saw_segment"
  else
    fail "LL-HLS did not fetch a segment"
  fi
fi

log "DASH live MPD smoke"
OUT=$(run_summary "$DASH_URL" --probe-headers --probe-drm) || true
if [[ -f "${OUT:-}" ]]; then
  pass "DASH summary schema valid"
  SV=$(json_get ".schema_version" "$OUT")
  if [[ "$SV" == "5" ]]; then
    pass "summary schema v5"
  else
    fail "expected schema_version 5, got $SV"
  fi
fi

# --- 5b. DAI / ClearKey staging (cbcs path smoke) ---
log "ClearKey cbcs staging smoke"
OUT=$(run_summary "$DASH_URL" --probe-headers --probe-drm \
  --clearkey "0123456789abcdef0123456789abcdef:fedcba9876543210fedcba9876543210") || true
if [[ -f "${OUT:-}" ]]; then
  pass "ClearKey staging summary schema valid"
fi

# --- 5c. HTML compliance report ---
log "HTML export-report"
REPORT="$TMP/test_report.html"
"$STREAMTOP" "$HLS_URL" --export-report "$REPORT" --timeout 5 >/dev/null 2>&1 || true
if [[ -s "$REPORT" ]] && head -n 1 "$REPORT" | grep -q '<!DOCTYPE html>'; then
  pass "export-report HTML structure"
else
  fail "export-report missing or invalid HTML"
fi
SIDE="$TMP/test_report.incident.json"
if [[ -s "$SIDE" ]]; then
  pass "export-report incident sidecar"
else
  fail "export-report incident sidecar missing"
fi

# --- 5d. Multi-stream agent metrics ---
log "Agent fleet metrics"
AGENT_PORT=19184
AGENT_CFG="$TMP/agent.toml"
cat >"$AGENT_CFG" <<EOF
metrics_bind = "127.0.0.1"
metrics_port = $AGENT_PORT

[[streams]]
id = "hls"
url = "$HLS_URL"
interval_ms = 500

[[streams]]
id = "dash"
url = "$DASH_URL"
interval_ms = 500
EOF
"$STREAMTOP" --agent "$AGENT_CFG" >/dev/null 2>&1 &
AGENT_PID=$!
sleep 5
AGENT_METRICS=$(curl -s --max-time 3 "http://127.0.0.1:${AGENT_PORT}/metrics" || true)
if echo "$AGENT_METRICS" | grep -q 'streamtop_agent_streams_active'; then
  pass "agent aggregated metrics endpoint"
else
  fail "agent metrics missing streamtop_agent_streams_active"
fi
if echo "$AGENT_METRICS" | grep -q 'stream_id="hls"'; then
  pass "agent stream_id label hls"
else
  fail "agent missing stream_id label"
fi
kill "$AGENT_PID" 2>/dev/null || true
wait "$AGENT_PID" 2>/dev/null || true
AGENT_PID=""

# --- 6. Webhook SSRF gate ---
log "Webhook SSRF protection"
set +e
"$STREAMTOP" "$HLS_URL" --webhook "http://169.254.169.254/latest/meta-data" --timeout 2 >/dev/null 2>&1
RC=$?
set -e
if [[ "$RC" -ne 0 ]]; then
  pass "metadata webhook blocked (exit $RC)"
else
  fail "metadata webhook should be blocked"
fi

log "Invalid alert list rejection"
set +e
"$STREAMTOP" "$HLS_URL" --webhook "${BASE}/webhook" --allow-insecure-webhooks \
  --alert-on typo --timeout 1 >/dev/null 2>&1
RC=$?
set -e
[[ "$RC" -ne 0 ]] && pass "invalid --alert-on rejected" || fail "invalid --alert-on accepted"

log "VOD crawl and incident exports"
"$STREAMTOP" --vod "$HLS_URL" --summary --summary-format json >/dev/null 2>&1 \
  && pass "VOD crawl command" || fail "VOD crawl command"
HAR="$TMP/incident.har"
"$STREAMTOP" "$HLS_URL" --export-har "$HAR" --timeout 2 >/dev/null 2>&1 || true
[[ -s "$HAR" ]] && pass "HAR export" || fail "HAR export"

# --- 7. Prometheus auth + metrics ---
log "Prometheus metrics auth"
METRICS_PORT=$((20000 + RANDOM % 20000))
METRICS_URL="http://127.0.0.1:${METRICS_PORT}/metrics"
"$STREAMTOP" "$HLS_URL" \
  --simulate-player --tr101290 \
  --prometheus "$METRICS_PORT" --metrics-token "test-token" \
  --probe-headers >/dev/null 2>&1 &
PROM_PID=$!
wait_metrics "$METRICS_URL" || {
  fail "metrics endpoint not ready"
  exit 1
}

CODE=$(curl -s -o /dev/null -w '%{http_code}' "$METRICS_URL")
if [[ "$CODE" == "401" ]]; then
  pass "metrics 401 without token"
else
  fail "expected metrics 401 without token, got $CODE"
fi

METRICS=$(curl -s -H "Authorization: Bearer test-token" "$METRICS_URL")
CODE=$(curl -s -o /dev/null -w '%{http_code}' -H "Authorization: Bearer test-token" "$METRICS_URL")
if [[ "$CODE" == "200" ]]; then
  pass "metrics 200 with bearer token"
else
  fail "expected metrics 200 with token, got $CODE"
fi

echo "$METRICS" | grep -q "streamtop_qoe_rebuffer_risk" && pass "metric streamtop_qoe_rebuffer_risk present" || fail "missing streamtop_qoe_rebuffer_risk"
echo "$METRICS" | grep -q "streamtop_tr101290_p1_violations_total" && pass "metric streamtop_tr101290_p1_violations_total present" || fail "missing tr101290 p1 metric"
echo "$METRICS" | grep -q "streamtop_inband_emsg_total" && pass "metric streamtop_inband_emsg_total present" || fail "missing inband emsg metric"
echo "$METRICS" | grep -q "streamtop_ad_mismatch_total" && pass "metric streamtop_ad_mismatch_total present" || fail "missing ad mismatch metric"
echo "$METRICS" | grep -q "streamtop_clearkey_decrypt_ok" && pass "metric streamtop_clearkey_decrypt_ok present" || fail "missing clearkey metric"

kill "$PROM_PID" 2>/dev/null || true
wait "$PROM_PID" 2>/dev/null || true
PROM_PID=""

log "Results: $PASS passed, $FAIL failed"
if [[ "$FAIL" -gt 0 ]]; then
  exit 1
fi
exit 0
