#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
HOST="${YT_DEMO_HOST:-127.0.0.1}"
PORT="${YT_DEMO_PORT:-7878}"
BASE="http://${HOST}:${PORT}"

need() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "missing required command: $1" >&2
    exit 1
  fi
}

need cargo
need curl
need npm

echo "==> Building console"
(
  cd "$ROOT/yitrace-console"
  if [ ! -d node_modules ]; then
    npm ci
  fi
  if ! VITE_API=http npm run build; then
    echo "console build failed; refreshing npm optional dependencies and retrying" >&2
    npm install
    VITE_API=http npm run build
  fi
)

echo "==> Embedding console into engine"
rm -rf "$ROOT/yitrace-engine/crates/yt-engine/console_dist"
cp -R "$ROOT/yitrace-console/dist" "$ROOT/yitrace-engine/crates/yt-engine/console_dist"

echo "==> Starting yiTrace on ${BASE}"
(
  cd "$ROOT/yitrace-engine"
  YT_BIND="${HOST}:${PORT}" cargo run -p yt-engine --example server
) &
SERVER_PID=$!

cleanup() {
  if kill -0 "$SERVER_PID" >/dev/null 2>&1; then
    kill "$SERVER_PID" >/dev/null 2>&1 || true
    wait "$SERVER_PID" >/dev/null 2>&1 || true
  fi
}
trap cleanup EXIT INT TERM

echo "==> Waiting for /v1/healthz"
for _ in $(seq 1 80); do
  if curl -fsS "${BASE}/v1/healthz" >/dev/null 2>&1; then
    break
  fi
  sleep 0.25
done
curl -fsS "${BASE}/v1/healthz" >/dev/null

echo "==> Ingesting a sample trace"
curl -fsS -XPOST "${BASE}/v1/ingest" \
  -H 'Content-Type: application/json' \
  -H 'X-Tenant-Id: 1' \
  -d '[
    {"trace_id":7001,"span_id":1,"ts":1,"seq":1,"event_type":1,"ext_span_id":"demo-7001-1","agent_name":"风控 Agent","input_text":"用户投诉疑似盗刷","logs":["开始风控研判"]},
    {"trace_id":7001,"span_id":1,"ts":2,"seq":2,"event_type":2,"ext_span_id":"demo-7001-1","status":1,"duration_ns":4200000,"output_text":"命中高风险规则，需要人工复核","logs":["疑似盗刷，已拦截"]}
  ]' >/dev/null

echo
echo "yiTrace is running:"
echo "  Console: ${BASE}/"
echo "  Health:  ${BASE}/v1/healthz"
echo
echo "Try:"
echo "  curl ${BASE}/v1/traces -H 'X-Tenant-Id: 1'"
echo "  curl -XPOST ${BASE}/v1/search -H 'Content-Type: application/json' -H 'X-Tenant-Id: 1' -d '{\"text\":\"盗刷\",\"k\":10}'"
echo

if [ "${YT_DEMO_OPEN:-0}" = "1" ]; then
  if command -v open >/dev/null 2>&1; then
    open "${BASE}/"
  elif command -v xdg-open >/dev/null 2>&1; then
    xdg-open "${BASE}/" >/dev/null 2>&1 || true
  fi
fi

echo "Press Ctrl-C to stop."
wait "$SERVER_PID"
