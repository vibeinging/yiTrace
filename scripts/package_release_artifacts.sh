#!/usr/bin/env bash
# Build release package artifacts in the same way the tag-only GitHub Action does.
#
# This script is intentionally local-runner friendly: run it before creating a
# tag so the GitHub Action only repeats a path that has already passed locally.

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT_DIR="${YITRACE_RELEASE_DIST:-"$ROOT_DIR/dist/tag-package"}"
MODE="all"
TARGET=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --sdk-only)
      MODE="sdk"
      shift
      ;;
    --native-only)
      MODE="native"
      shift
      ;;
    --target)
      TARGET="${2:-}"
      if [[ -z "$TARGET" ]]; then
        echo "--target requires a value" >&2
        exit 2
      fi
      MODE="target"
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

want() {
  local target="$1"
  case "$MODE" in
    all)
      return 0
      ;;
    sdk)
      [[ "$target" == "python-sdk" || "$target" == "typescript-sdk" || "$target" == "rust-sdk" || "$target" == "rust-db-source" ]]
      ;;
    native)
      [[ "$target" == "python-db" || "$target" == "node-db" ]]
      ;;
    target)
      [[ "$target" == "$TARGET" ]]
      ;;
    *)
      return 1
      ;;
  esac
}

copy_dir_files() {
  local from="$1"
  local to="$2"
  mkdir -p "$to"
  find "$from" -maxdepth 1 -type f -exec cp {} "$to"/ \;
}

rm -rf "$OUT_DIR"
mkdir -p "$OUT_DIR"

echo "Packaging yiTrace release artifacts into $OUT_DIR"
echo "Mode: $MODE"
if [[ -n "$TARGET" ]]; then
  echo "Target: $TARGET"
fi

if [[ "$MODE" == "all" || "$MODE" == "sdk" ]]; then
  run "$ROOT_DIR/scripts/package_mode_eval.sh" --skip-node --skip-python-db --skip-rust-db
fi

if want python-sdk; then
  if [[ "$MODE" == "target" ]]; then
    run python "$ROOT_DIR/yitrace-sdk/python/tests/test_sdk.py"
    run python "$ROOT_DIR/scripts/verify_python_sdk_consumer.py"
  fi

  echo
  echo "==> Python SDK wheel/sdist"
  pushd "$ROOT_DIR/yitrace-sdk/python" >/dev/null
  rm -rf dist
  run python -m build
  copy_dir_files "$PWD/dist" "$OUT_DIR/python-sdk"
  popd >/dev/null
fi

if want python-db; then
  echo
  echo "==> Python embedded DB wheel"
  pushd "$ROOT_DIR/yitrace-db-python" >/dev/null
  # MATURIN_BUILD_ARGS is used by CI to request manylinux wheels on Linux.
  MATURIN_ARGS=()
  if [[ -n "${MATURIN_BUILD_ARGS:-}" ]]; then
    # shellcheck disable=SC2206
    MATURIN_ARGS=(${MATURIN_BUILD_ARGS})
    run python -m maturin build --release "${MATURIN_ARGS[@]}" --out "$OUT_DIR/python-db"
  else
    run python -m maturin build --release --out "$OUT_DIR/python-db"
  fi
  popd >/dev/null
fi

if want typescript-sdk; then
  if [[ "$MODE" == "target" ]]; then
    pushd "$ROOT_DIR/yitrace-sdk/typescript" >/dev/null
    run npm test
    popd >/dev/null
  fi

  echo
  echo "==> TypeScript SDK npm tarball"
  pushd "$ROOT_DIR/yitrace-sdk/typescript" >/dev/null
  run npm run build
  mkdir -p "$OUT_DIR/typescript-sdk"
  run npm pack --pack-destination "$OUT_DIR/typescript-sdk"
  popd >/dev/null
fi

if want rust-sdk; then
  if [[ "$MODE" == "target" ]]; then
    run cargo test --offline --manifest-path "$ROOT_DIR/yitrace-sdk/rust/Cargo.toml"
  fi

  echo
  echo "==> Rust SDK crate"
  run cargo package --manifest-path "$ROOT_DIR/yitrace-sdk/rust/Cargo.toml" --allow-dirty
  mkdir -p "$OUT_DIR/rust-sdk"
  cp "$ROOT_DIR"/yitrace-sdk/rust/target/package/yitrace-*.crate "$OUT_DIR/rust-sdk"/
fi

if want rust-db-source; then
  echo
  echo "==> Rust embedded DB source bundle"
  mkdir -p "$OUT_DIR/rust-db"
  (
    cd "$ROOT_DIR"
    COPYFILE_DISABLE=1 tar \
      --exclude "*/target" \
      --exclude "*/node_modules" \
      --exclude "*/.pytest_cache" \
      --exclude "*/__pycache__" \
      -czf "$OUT_DIR/rust-db/yitrace-db-rs-0.1.0-source.tar.gz" \
      yitrace-db-rs \
      yitrace-engine
  )
fi

if want node-db; then
  echo
  echo "==> Node embedded DB local native tarballs"
  pushd "$ROOT_DIR/yitrace-node" >/dev/null
  run npm run build
  run npm run pack:verify
  mkdir -p "$OUT_DIR/node-db"
  cp dist/*.tgz dist/pack-manifest.json "$OUT_DIR/node-db"/
  popd >/dev/null
fi

echo
echo "==> Checksums"
(
  cd "$OUT_DIR"
  if ! find . -type f ! -name SHA256SUMS.txt | grep -q .; then
    echo "No artifacts were produced for mode=$MODE target=${TARGET:-all}" >&2
    exit 1
  fi
  python - <<'PY'
from __future__ import annotations

import hashlib
from pathlib import Path

rows = []
for path in sorted(Path(".").rglob("*")):
    if not path.is_file() or path.name == "SHA256SUMS.txt":
        continue
    digest = hashlib.sha256(path.read_bytes()).hexdigest()
    rows.append(f"{digest}  {path.as_posix()}")
Path("SHA256SUMS.txt").write_text("\n".join(rows) + ("\n" if rows else ""), encoding="utf-8")
PY
)

echo
echo "Release artifacts ready:"
find "$OUT_DIR" -maxdepth 3 -type f | sort
