#!/usr/bin/env bash
# yiTrace eval 总入口。
#
# 默认跑“能快速卡住主要风险”的 eval：
#   - 风险矩阵 eval
#   - 真实分布式 chaos eval
#   - 主 eval harness
#   - gateway server example 编译
#   - Rust 嵌入式 DB crate 测试
#
# 可选参数：
#   --packages      额外跑 Python DB、Node DB、SDK、控制台等包级测试
#   --pack          额外跑 @yitrace/db 本地打包 + clean consumer 验证
#   --crash         额外跑 kill -9 崩溃恢复测试（默认 3 轮，可用 --crash-rounds N）
#   --heavy         等同于 --packages --pack --crash
#   --skip-rust-db  跳过 Rust 嵌入式 DB crate 测试
#   --skip-node     在 --packages/--pack 下跳过 Node 嵌入式 DB
#   --skip-python-db 在 --packages 下跳过 Python 嵌入式 DB
#   --skip-sdk      在 --packages 下跳过 Python/TypeScript SDK
#   --skip-ui       在 --packages 下跳过控制台测试和构建

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RUN_PACKAGES=0
RUN_PACK=0
RUN_CRASH=0
RUN_RUST_DB=1
RUN_NODE=1
RUN_PYTHON_DB=1
RUN_SDK=1
RUN_UI=1
CRASH_ROUNDS=3

while [[ $# -gt 0 ]]; do
  case "$1" in
    --packages)
      RUN_PACKAGES=1
      shift
      ;;
    --pack)
      RUN_PACK=1
      shift
      ;;
    --crash)
      RUN_CRASH=1
      shift
      ;;
    --heavy)
      RUN_PACKAGES=1
      RUN_PACK=1
      RUN_CRASH=1
      shift
      ;;
    --crash-rounds)
      CRASH_ROUNDS="$2"
      shift 2
      ;;
    --skip-rust-db)
      RUN_RUST_DB=0
      shift
      ;;
    --skip-node)
      RUN_NODE=0
      shift
      ;;
    --skip-python-db)
      RUN_PYTHON_DB=0
      shift
      ;;
    --skip-sdk)
      RUN_SDK=0
      shift
      ;;
    --skip-ui)
      RUN_UI=0
      shift
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
    echo "缺少目录: $1" >&2
    exit 2
  fi
}

need_dir "$ROOT_DIR/yitrace-engine"

run cargo test --offline --manifest-path "$ROOT_DIR/yitrace-engine/Cargo.toml" -p yt-engine --test risk_eval_matrix -- --test-threads=1
run cargo test --offline --manifest-path "$ROOT_DIR/yitrace-engine/Cargo.toml" -p yt-engine --test distributed_chaos_eval -- --test-threads=1
run cargo test --offline --manifest-path "$ROOT_DIR/yitrace-engine/Cargo.toml" -p yt-engine --test eval_harness -- --test-threads=1
run cargo check --offline --manifest-path "$ROOT_DIR/yitrace-engine/Cargo.toml" -p yt-engine --example gateway_server

if [[ "$RUN_RUST_DB" -eq 1 ]]; then
  need_dir "$ROOT_DIR/yitrace-db-rs"
  run cargo test --offline --manifest-path "$ROOT_DIR/yitrace-db-rs/Cargo.toml"
fi

if [[ "$RUN_PACKAGES" -eq 1 && "$RUN_SDK" -eq 1 ]]; then
  need_dir "$ROOT_DIR/yitrace-sdk/python"
  need_dir "$ROOT_DIR/yitrace-sdk/typescript"
  run python "$ROOT_DIR/yitrace-sdk/python/tests/test_sdk.py"

  pushd "$ROOT_DIR/yitrace-sdk/typescript" >/dev/null
  run npm test
  popd >/dev/null
fi

if [[ "$RUN_PACKAGES" -eq 1 && "$RUN_UI" -eq 1 ]]; then
  need_dir "$ROOT_DIR/yitrace-console"
  pushd "$ROOT_DIR/yitrace-console" >/dev/null
  run npm test
  run npm run build
  popd >/dev/null
fi

if [[ "$RUN_PACKAGES" -eq 1 && "$RUN_PYTHON_DB" -eq 1 ]]; then
  need_dir "$ROOT_DIR/yitrace-db-python"
  pushd "$ROOT_DIR/yitrace-db-python" >/dev/null
  run python -m pytest
  popd >/dev/null
fi

if [[ "$RUN_PACKAGES" -eq 1 && "$RUN_NODE" -eq 1 ]]; then
  need_dir "$ROOT_DIR/yitrace-node"
  pushd "$ROOT_DIR/yitrace-node" >/dev/null
  run npm run build
  run npm test
  popd >/dev/null
fi

if [[ "$RUN_PACK" -eq 1 && "$RUN_NODE" -eq 1 ]]; then
  need_dir "$ROOT_DIR/yitrace-node"
  pushd "$ROOT_DIR/yitrace-node" >/dev/null
  run npm run pack:verify
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
echo "eval 全部通过"
