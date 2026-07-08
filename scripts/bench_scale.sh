#!/usr/bin/env bash
# yiTrace 规模压测入口。
#
# 默认 smoke 档：10k spans、200 次查询、release 模式。

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SPANS=10000
QUERIES=200
BATCH=512
PROFILE=release
KEEP_DATA=0
COLD_QUERIES=0
DATA_DIR=""
REPORT_DIR="$ROOT_DIR/docs/reports/scale"
REPORT_PATH=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --smoke)
      SPANS=10000
      QUERIES=200
      shift
      ;;
    --medium)
      SPANS=100000
      QUERIES=100
      shift
      ;;
    --large)
      SPANS=1000000
      QUERIES=200
      shift
      ;;
    --spans)
      SPANS="$2"
      shift 2
      ;;
    --queries)
      QUERIES="$2"
      shift 2
      ;;
    --batch)
      BATCH="$2"
      shift 2
      ;;
    --debug)
      PROFILE=debug
      shift
      ;;
    --release)
      PROFILE=release
      shift
      ;;
    --data-dir)
      DATA_DIR="$2"
      shift 2
      ;;
    --keep-data)
      KEEP_DATA=1
      shift
      ;;
    --cold-queries)
      COLD_QUERIES=1
      shift
      ;;
    --report)
      REPORT_PATH="$2"
      shift 2
      ;;
    --report-dir)
      REPORT_DIR="$2"
      shift 2
      ;;
    *)
      echo "未知参数: $1" >&2
      exit 2
      ;;
  esac
done

if [[ -z "$REPORT_PATH" ]]; then
  mkdir -p "$REPORT_DIR"
  TS="$(date -u +%Y%m%dT%H%M%SZ)"
  CACHE_LABEL="warm"
  if [[ "$COLD_QUERIES" -eq 1 ]]; then
    CACHE_LABEL="cold"
  fi
  REPORT_PATH="$REPORT_DIR/${TS}_scale-bench_${SPANS}_spans_${CACHE_LABEL}.md"
fi

cmd=(
  cargo run
  --manifest-path "$ROOT_DIR/yitrace-engine/Cargo.toml"
)

if [[ "$PROFILE" == "release" ]]; then
  cmd+=(--release)
fi

cmd+=(
  -p yt-engine
  --example scale_bench
  --
  --spans "$SPANS"
  --queries "$QUERIES"
  --batch "$BATCH"
  --report "$REPORT_PATH"
)

if [[ -n "$DATA_DIR" ]]; then
  cmd+=(--data-dir "$DATA_DIR")
fi

if [[ "$KEEP_DATA" -eq 1 ]]; then
  cmd+=(--keep-data)
fi

if [[ "$COLD_QUERIES" -eq 1 ]]; then
  cmd+=(--cold-queries)
fi

echo "==> ${cmd[*]}"
"${cmd[@]}"

echo
echo "scale bench report: $REPORT_PATH"
