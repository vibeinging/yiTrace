#!/usr/bin/env bash
# 在派生索引已经 fsync、还没原子替换正式文件时杀进程，再验证能从原始数据恢复。

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ENGINE_DIR="$ROOT_DIR/yitrace-engine"
BIN="$ENGINE_DIR/target/release/examples/scale_bench"
DATA_DIR="$(mktemp -d)"
LOG_DIR="$DATA_DIR/logs"
SPANS="${YT_SIDECAR_CRASH_SPANS:-2000}"
PID=""

cleanup() {
  if [[ -n "$PID" ]] && kill -0 "$PID" 2>/dev/null; then
    kill -9 "$PID" 2>/dev/null || true
    wait "$PID" 2>/dev/null || true
  fi
  rm -rf "$DATA_DIR"
}
trap cleanup EXIT

mkdir -p "$LOG_DIR"

echo "==> 构建派生索引崩溃测试进程"
cargo build --offline --manifest-path "$ENGINE_DIR/Cargo.toml" \
  -p yt-engine --example scale_bench --release --features test-failpoints

echo "==> 生成 $SPANS 个 span"
"$BIN" --phase generate --spans "$SPANS" --queries 1 --batch 500 \
  --data-dir "$DATA_DIR/db" --report "$LOG_DIR/generate.md"

wait_for_marker() {
  local marker="$1"
  local log="$2"
  local attempt
  for attempt in $(seq 1 300); do
    if [[ -f "$marker" ]]; then
      return 0
    fi
    if ! kill -0 "$PID" 2>/dev/null; then
      echo "测试进程在到达停顿点前退出" >&2
      cat "$log" >&2
      return 1
    fi
    sleep 0.1
  done
  echo "等待派生索引停顿点超时: $marker" >&2
  cat "$log" >&2
  return 1
}

crash_and_recover() {
  local kind="$1"
  local file="$2"
  local query="$3"
  local marker="$DATA_DIR/${kind}.marker"
  local crash_log="$LOG_DIR/${kind}-crash.log"
  local target="$DATA_DIR/db/$file"

  echo "==> 在 $kind 原子替换前 kill -9"
  rm -f "$target" "$DATA_DIR/db/${file%.*}.tmp" "$marker"
  YT_TEST_SIDECAR_BEFORE_RENAME="$kind" \
    YT_TEST_SIDECAR_MARKER="$marker" \
    "$BIN" --phase query --queries 1 --only-query "$query" \
      --data-dir "$DATA_DIR/db" --report "$LOG_DIR/${kind}-interrupted.md" \
      >"$crash_log" 2>&1 &
  PID=$!
  wait_for_marker "$marker" "$crash_log"
  kill -9 "$PID"
  wait "$PID" 2>/dev/null || true
  PID=""

  if [[ -f "$target" ]]; then
    echo "$kind 正式文件在原子替换前已经可见" >&2
    exit 1
  fi

  echo "==> 从 WAL/segment 恢复 $kind"
  "$BIN" --phase query --queries 1 --only-query "$query" \
    --data-dir "$DATA_DIR/db" --report "$LOG_DIR/${kind}-recovery.md"
  if [[ ! -s "$target" ]]; then
    echo "$kind 没有完成重建" >&2
    exit 1
  fi
}

crash_and_recover bm25 bm25.dat search_common_text
crash_and_recover segment_bloom segment_bloom.dat search_common_text
crash_and_recover filter_attrs filter_attrs.dat search_common_text_project
crash_and_recover trace_rollup trace_rollup.dat trace_aggregate_rollup

echo "派生索引 kill -9 恢复全部通过"
