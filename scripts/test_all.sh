#!/usr/bin/env bash
# yiTrace 日常测试总入口。
#
# 默认跑主线必需测试：
#   - Rust 引擎离线测试
#   - package-mode eval（Python/TypeScript SDK + Node/Python/Rust 嵌入式 DB）
#   - 控制台数据层测试 + 构建
#
# 可选参数：
#   --skip-node       跳过 Node 嵌入式 DB
#   --skip-python-db  跳过 Python 嵌入式 DB
#   --skip-rust-db    跳过 Rust 嵌入式 DB
#   --skip-ui         跳过控制台
#   --crash           额外跑 kill -9 崩溃恢复测试（默认 3 轮，可用 --crash-rounds N）

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RUN_NODE=1
RUN_PYTHON_DB=1
RUN_RUST_DB=1
RUN_UI=1
RUN_CRASH=0
CRASH_ROUNDS=3

while [[ $# -gt 0 ]]; do
  case "$1" in
    --skip-node)
      RUN_NODE=0
      shift
      ;;
    --skip-python-db)
      RUN_PYTHON_DB=0
      shift
      ;;
    --skip-rust-db)
      RUN_RUST_DB=0
      shift
      ;;
    --skip-ui)
      RUN_UI=0
      shift
      ;;
    --crash)
      RUN_CRASH=1
      shift
      ;;
    --crash-rounds)
      CRASH_ROUNDS="$2"
      shift 2
      ;;
    *)
      echo "未知参数: $1" >&2
      exit 2
      ;;
  esac
done

run() {
  echo
  echo "==> $*"
  "$@"
}

need_dir() {
  if [[ ! -d "$1" ]]; then
    echo "跳过，缺少目录: $1" >&2
    return 1
  fi
  return 0
}

need_dir "$ROOT_DIR/yitrace-engine" >/dev/null
run cargo test --offline --manifest-path "$ROOT_DIR/yitrace-engine/Cargo.toml"

PACKAGE_ARGS=()
if [[ "$RUN_PYTHON_DB" -eq 0 ]]; then
  PACKAGE_ARGS+=("--skip-python-db")
fi
if [[ "$RUN_RUST_DB" -eq 0 ]]; then
  PACKAGE_ARGS+=("--skip-rust-db")
fi
if [[ "$RUN_NODE" -eq 0 ]]; then
  PACKAGE_ARGS+=("--skip-node")
fi
run "$ROOT_DIR/scripts/package_mode_eval.sh" "${PACKAGE_ARGS[@]}"

if [[ "$RUN_UI" -eq 1 ]] && need_dir "$ROOT_DIR/yitrace-console"; then
  pushd "$ROOT_DIR/yitrace-console" >/dev/null
  run npm test
  run npm run build
  popd >/dev/null
fi

if [[ "$RUN_CRASH" -eq 1 ]]; then
  pushd "$ROOT_DIR/yitrace-engine" >/dev/null
  run cargo build -p yt-engine --example server_durable --release
  popd >/dev/null
  pushd "$ROOT_DIR" >/dev/null
  run ./tests/crash_recovery_kill9.sh "$CRASH_ROUNDS"
  popd >/dev/null
fi

echo
echo "全部测试通过"
