#!/usr/bin/env bash
# yiTrace 主线 eval 总入口。
#
# 默认只跑单机/嵌入式主线能承受的风险用例：
#   - 风险矩阵 eval
#   - 主 eval harness
#   - Rust 引擎离线测试
#
# 可选参数：
#   --packages       额外跑 package-mode eval 和控制台测试
#   --pack           额外跑 @yitrace/db 本地打包验证
#   --crash          额外跑 kill -9 崩溃恢复测试（默认 3 轮，可用 --crash-rounds N）
#   --heavy          等同于 --packages --pack --crash
#   --skip-engine    跳过 Rust 引擎全量离线测试，只跑 eval 测试
#   --skip-node      在 --packages/--pack 下跳过 Node 嵌入式 DB
#   --skip-python-db 在 --packages 下跳过 Python 嵌入式 DB
#   --skip-rust-db   在 --packages 下跳过 Rust 嵌入式 DB crate
#   --skip-sdk       在 --packages 下跳过 Python/TypeScript SDK
#   --skip-ui        在 --packages 下跳过控制台测试和构建

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RUN_ENGINE=1
RUN_PACKAGES=0
RUN_PACK=0
RUN_CRASH=0
RUN_NODE=1
RUN_PYTHON_DB=1
RUN_RUST_DB=1
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
    --skip-engine)
      RUN_ENGINE=0
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
    --skip-rust-db)
      RUN_RUST_DB=0
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
    echo "跳过，缺少目录: $1" >&2
    return 1
  fi
  return 0
}

need_dir "$ROOT_DIR/yitrace-engine" >/dev/null

run cargo test --offline --manifest-path "$ROOT_DIR/yitrace-engine/Cargo.toml" -p yt-engine --test risk_eval_matrix -- --test-threads=1
run cargo test --offline --manifest-path "$ROOT_DIR/yitrace-engine/Cargo.toml" -p yt-engine --test eval_harness -- --test-threads=1

if [[ "$RUN_ENGINE" -eq 1 ]]; then
  run cargo test --offline --manifest-path "$ROOT_DIR/yitrace-engine/Cargo.toml"
fi

if [[ "$RUN_PACKAGES" -eq 1 ]]; then
  PACKAGE_ARGS=()
  if [[ "$RUN_SDK" -eq 0 ]]; then
    PACKAGE_ARGS+=("--skip-sdk")
  fi
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

  if [[ "$RUN_UI" -eq 1 ]]; then
    if need_dir "$ROOT_DIR/yitrace-console"; then
      pushd "$ROOT_DIR/yitrace-console" >/dev/null
      run npm test
      run npm run build
      popd >/dev/null
    fi
  fi
fi

if [[ "$RUN_PACK" -eq 1 && "$RUN_NODE" -eq 1 ]]; then
  if need_dir "$ROOT_DIR/yitrace-node"; then
    pushd "$ROOT_DIR/yitrace-node" >/dev/null
    run npm run pack:verify
    popd >/dev/null
  fi
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
