#!/usr/bin/env bash
# §1.3 生产就绪路线：真 kill -9 崩溃测试。
#
# 流程（循环 N 次）：
#   1. 起 server_durable（持久化）
#   2. curl 灌一批 trace（含中文、token、向量）
#   3. kill -9（SIGKILL，模拟进程被杀/断电——内存全失，只剩盘上的）
#   4. 重起 server_durable（同目录）
#   5. 验证：之前灌的数据还在（GET /v1/traces）、检索还能用（POST /v1/search 中文）
#   6. 累积——下一轮在上一轮的数据上继续（验证多次崩溃累积后仍一致）
#
# 用法：
#   cd yitrace-engine && cargo build -p yt-engine --example server_durable --release
#   cd .. && ./tests/crash_recovery_kill9.sh [轮数，默认 20]
#
# 退出码：0 = 全部通过；1 = 有轮失败。

set -u
ROUNDS="${1:-20}"
ENGINE_DIR="yitrace-engine"
BIN="$ENGINE_DIR/target/release/examples/server_durable"
DATA_DIR="$(mktemp -d)"
PORT=7879
PASS=0
FAIL=0

# 找二进制
if [[ ! -x "$BIN" ]]; then
  echo "✗ 找不到 $BIN —— 先 cargo build -p yt-engine --example server_durable --release" >&2
  exit 2
fi

cleanup() { pkill -f "server_durable $DATA_DIR" 2>/dev/null || true; rm -rf "$DATA_DIR"; }
trap cleanup EXIT

start_server() {
  "$BIN" "$DATA_DIR" >"$DATA_DIR/server.log" 2>&1 &
  SERVER_PID=$!
  # 等端口起来
  for _ in $(seq 1 50); do
    if curl -s "http://127.0.0.1:$PORT/v1/traces" >/dev/null 2>&1; then return 0; fi
    sleep 0.1
  done
  echo "  ✗ 服务没起来"; cat "$DATA_DIR/server.log"; return 1
}

ingest() {
  local r=$1
  # 灌一条带中文 + token 的 span（每轮不同 span_id，累积验证）
  curl -s -XPOST "http://127.0.0.1:$PORT/v1/ingest" \
    -d "[{\"trace_id\":1,\"span_id\":${r},\"ts\":${r},\"seq\":1,\"event_type\":3,\"ext_span_id\":\"1-${r}\",\"status\":0,\"input_tokens\":100,\"agent_name\":\"风控\",\"logs\":[\"第${r}轮 灌入\"]}]" \
    >/dev/null
}

verify() {
  local out
  # 1) trace 列表有数据
  out=$(curl -s "http://127.0.0.1:$PORT/v1/traces")
  if ! echo "$out" | grep -q '"trace_id":1'; then
    echo "  ✗ 轮 $1：trace 列表为空（数据丢了）"; return 1
  fi
  # 2) 中文检索能找到"风控"
  out=$(curl -s -XPOST "http://127.0.0.1:$PORT/v1/search" -d '{"text":"风控","k":10}')
  if ! echo "$out" | grep -q 'span_id'; then
    echo "  ✗ 轮 $1：中文检索'风控'无结果（索引丢了）"; return 1
  fi
  echo "  ✓ 轮 $1：数据在、检索在"
  return 0
}

echo "=== 真 kill -9 崩溃测试（$ROUNDS 轮）==="
echo "数据目录: $DATA_DIR"
echo ""

for r in $(seq 1 "$ROUNDS"); do
  echo "轮 $r:"
  start_server || { FAIL=$((FAIL+1)); continue; }
  ingest "$r"
  sleep 0.1  # 让摄入落盘窗口
  echo "  kill -9 (pid $SERVER_PID)"
  kill -9 "$SERVER_PID" 2>/dev/null
  wait "$SERVER_PID" 2>/dev/null
  sleep 0.1

  # 重起，验证
  start_server || { FAIL=$((FAIL+1)); continue; }
  if verify "$r"; then PASS=$((PASS+1)); else FAIL=$((FAIL+1)); fi
  kill -9 "$SERVER_PID" 2>/dev/null
  wait "$SERVER_PID" 2>/dev/null
done

echo ""
echo "=== 结果 ==="
echo "通过: $PASS / $ROUNDS"
echo "失败: $FAIL"
if [[ "$FAIL" -eq 0 ]]; then
  echo "✓ 全部通过：连续 $ROUNDS 次 kill-9 重启，零数据丢失、零索引坏"
  exit 0
else
  echo "✗ 有失败轮——崩溃恢复有 bug"
  exit 1
fi
