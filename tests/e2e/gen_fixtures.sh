#!/usr/bin/env bash
# Generate local test feeds with ffmpeg (optional). Falls back to built-in Rust/Python mocks.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${ROOT}/tests/fixtures/generated"
mkdir -p "$OUT"

if ! command -v ffmpeg >/dev/null 2>&1; then
  echo "ffmpeg not found; E2E uses tests/e2e/mock_all.py and tests/mock_server/fixtures.rs"
  exit 0
fi

echo "Generating HLS ladder -> ${OUT}/hls"
ffmpeg -hide_banner -loglevel error -y \
  -f lavfi -i "testsrc2=size=1280x720:rate=25" \
  -f lavfi -i "sine=frequency=440:sample_rate=48000" \
  -t 6 -c:v libx264 -pix_fmt yuv420p -profile:v main -g 50 -keyint_min 50 \
  -c:a aac -b:a 128k \
  -f hls -hls_time 2 -hls_list_size 3 -hls_flags independent_segments \
  -master_pl_name master.m3u8 \
  -var_stream_map "v:0,a:0 v:1,a:0" \
  -map 0:v -map 1:a -s:v:0 640x360 -b:v:0 800k \
  -map 0:v -map 1:a -s:v:1 1280x720 -b:v:1 2500k \
  "${OUT}/hls/stream_%v.m3u8" || true

echo "Generating LL-HLS fMP4 partials -> ${OUT}/ll-hls"
ffmpeg -hide_banner -loglevel error -y \
  -f lavfi -i "testsrc2=size=1280x720:rate=30" \
  -f lavfi -i "sine=frequency=880:sample_rate=48000" \
  -t 4 -c:v libx264 -pix_fmt yuv420p -g 30 \
  -c:a aac \
  -f hls -hls_time 1 -hls_list_size 4 \
  -hls_segment_type fmp4 -hls_fmp4_init_filename init.m4s \
  "${OUT}/ll-hls/media.m3u8" || true

echo "Generating MPEG-TS with errors -> ${OUT}/broken.ts"
# Baseline TS; TR101290 violations are injected by mock_server/fixtures.rs for hermetic tests.
ffmpeg -hide_banner -loglevel error -y \
  -f lavfi -i "testsrc2=size=640x360:rate=25" \
  -t 2 -c:v libx264 -pix_fmt yuv420p -f mpegts "${OUT}/broken.ts" || true

echo "Generating HDR10 HEVC (optional) -> ${OUT}/hdr.hevc"
ffmpeg -hide_banner -loglevel error -y \
  -f lavfi -i "testsrc2=size=1280x720:rate=25" \
  -t 2 -c:v libx265 -pix_fmt yuv420p10le -x265-params "hdr-opt=1:repeat-headers=1" \
  "${OUT}/hdr.hevc" || true

echo "Done. Generated assets under ${OUT} (optional; E2E mocks do not require these)."
