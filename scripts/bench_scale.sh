#!/usr/bin/env bash
# yiTrace 规模压测入口。
#
# 默认 smoke 档：10k spans、200 次查询、release 模式。

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SPANS=10000
QUERIES=200
BATCH=512
SEED=11400714819323198485
PROFILE=release
KEEP_DATA=0
COLD_QUERIES=0
VERIFY_SEARCH=0
VERIFY_SOURCE_INDEX=0
DATA_DIR=""
AUTO_DATA_DIR=0
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
    --seed)
      SEED="$2"
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
    --verify-search)
      VERIFY_SEARCH=1
      shift
      ;;
    --verify-source-index)
      VERIFY_SOURCE_INDEX=1
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

runner=(
  cargo run
  --manifest-path "$ROOT_DIR/yitrace-engine/Cargo.toml"
)

if [[ "$PROFILE" == "release" ]]; then
  runner+=(--release)
fi

runner+=(
  -p yt-engine
  --example scale_bench
  --
)

common=(
  --spans "$SPANS"
  --queries "$QUERIES"
  --batch "$BATCH"
  --seed "$SEED"
)

if [[ "$COLD_QUERIES" -eq 1 ]]; then
  if [[ -z "$DATA_DIR" ]]; then
    DATA_DIR="$(mktemp -d "${TMPDIR:-/tmp}/yitrace-scale.XXXXXX")"
    AUTO_DATA_DIR=1
    trap 'rm -rf "$DATA_DIR"' EXIT
  fi
  if [[ "$REPORT_PATH" == *.md ]]; then
    GENERATE_REPORT="${REPORT_PATH%.md}_generate.md"
  else
    GENERATE_REPORT="${REPORT_PATH}.generate.md"
  fi

  generate_cmd=(
    "${runner[@]}"
    --phase generate
    "${common[@]}"
    --data-dir "$DATA_DIR"
    --report "$GENERATE_REPORT"
    --keep-data
  )
  query_cmd=(
    "${runner[@]}"
    --phase query
    "${common[@]}"
    --data-dir "$DATA_DIR"
    --report "$REPORT_PATH"
  )
  if [[ "$VERIFY_SEARCH" -eq 1 ]]; then
    query_cmd+=(--verify-search)
  fi
  if [[ "$VERIFY_SOURCE_INDEX" -eq 1 ]]; then
    query_cmd+=(--verify-source-index)
  fi

  echo "==> ${generate_cmd[*]}"
  "${generate_cmd[@]}"
  echo "==> ${query_cmd[*]}"
  "${query_cmd[@]}"

  if [[ "$KEEP_DATA" -eq 0 && "$AUTO_DATA_DIR" -eq 1 ]]; then
    rm -rf "$DATA_DIR"
    trap - EXIT
  elif [[ "$KEEP_DATA" -eq 1 && "$AUTO_DATA_DIR" -eq 1 ]]; then
    trap - EXIT
  fi
  echo
  echo "scale bench generation report: $GENERATE_REPORT"
  echo "scale bench query report: $REPORT_PATH"
  exit 0
fi

cmd=(
  "${runner[@]}"
  --phase full
  "${common[@]}"
  --report "$REPORT_PATH"
)
if [[ -n "$DATA_DIR" ]]; then
  cmd+=(--data-dir "$DATA_DIR")
fi
if [[ "$KEEP_DATA" -eq 1 ]]; then
  cmd+=(--keep-data)
fi
if [[ "$VERIFY_SEARCH" -eq 1 ]]; then
  cmd+=(--verify-search)
fi
if [[ "$VERIFY_SOURCE_INDEX" -eq 1 ]]; then
  cmd+=(--verify-source-index)
fi

echo "==> ${cmd[*]}"
"${cmd[@]}"

echo
echo "scale bench report: $REPORT_PATH"
