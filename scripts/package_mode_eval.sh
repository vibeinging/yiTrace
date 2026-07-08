#!/usr/bin/env bash
# yiTrace package-mode eval.
#
# Covers the public package shapes that users install or embed:
#   - Python yitrace facade: connect(url/path), DbExporter, HTTP client
#   - TypeScript tracing SDK
#   - Rust tracing SDK
#   - Python yitrace-db embedded DB, FastAPI router, serve worker guard
#   - Rust yitrace-db embedded crate
#   - Node @yitrace/db embedded package

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RUN_NODE=1
RUN_PYTHON_DB=1
RUN_RUST_DB=1
RUN_SDK=1

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
    --skip-sdk)
      RUN_SDK=0
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

if [[ "$RUN_SDK" -eq 1 ]]; then
  if need_dir "$ROOT_DIR/yitrace-sdk/python"; then
    run python "$ROOT_DIR/yitrace-sdk/python/tests/test_sdk.py"
    run python "$ROOT_DIR/scripts/verify_python_sdk_consumer.py"
  fi

  if need_dir "$ROOT_DIR/yitrace-sdk/typescript"; then
    pushd "$ROOT_DIR/yitrace-sdk/typescript" >/dev/null
    run npm test
    popd >/dev/null
  fi

  if need_dir "$ROOT_DIR/yitrace-sdk/rust"; then
    run cargo test --offline --manifest-path "$ROOT_DIR/yitrace-sdk/rust/Cargo.toml"
  fi
fi

if [[ "$RUN_PYTHON_DB" -eq 1 ]]; then
  if need_dir "$ROOT_DIR/yitrace-db-python"; then
    pushd "$ROOT_DIR/yitrace-db-python" >/dev/null
    run python -m pytest
    popd >/dev/null
  fi
fi

if [[ "$RUN_RUST_DB" -eq 1 ]]; then
  if need_dir "$ROOT_DIR/yitrace-db-rs"; then
    run cargo test --offline --manifest-path "$ROOT_DIR/yitrace-db-rs/Cargo.toml"
  fi
fi

if [[ "$RUN_NODE" -eq 1 ]]; then
  if need_dir "$ROOT_DIR/yitrace-node"; then
    pushd "$ROOT_DIR/yitrace-node" >/dev/null
    run npm run build
    run npm test
    popd >/dev/null
  fi
fi

echo
echo "package-mode eval 全部通过"
