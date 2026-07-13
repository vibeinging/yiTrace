#!/usr/bin/env python3
"""Check that every yiTrace release artifact uses one version."""
from __future__ import annotations

import argparse
import json
from pathlib import Path
import sys

try:
    import tomllib
except ImportError as err:  # pragma: no cover - release runners use Python 3.12
    raise SystemExit("check_release_versions.py requires Python 3.11+") from err


ROOT = Path(__file__).resolve().parents[1]


def read_json(path: Path) -> dict:
    return json.loads(path.read_text(encoding="utf-8"))


def read_toml(path: Path) -> dict:
    with path.open("rb") as file:
        return tomllib.load(file)


def cargo_package_version(path: Path, package_name: str) -> str:
    lock = read_toml(path)
    matches = [pkg["version"] for pkg in lock.get("package", []) if pkg.get("name") == package_name]
    if len(matches) != 1:
        raise ValueError(f"{path}: expected one {package_name!r} package, found {len(matches)}")
    return matches[0]


def main() -> int:
    parser = argparse.ArgumentParser()
    expected_group = parser.add_mutually_exclusive_group()
    expected_group.add_argument("--expected", help="expected release version; defaults to @yitrace/db")
    expected_group.add_argument("--tag", help="release tag, for example v0.1.3 or v0.1.3-only-node-db")
    args = parser.parse_args()

    node_package = read_json(ROOT / "yitrace-node/package.json")
    tag_version = None
    if args.tag:
        if not args.tag.startswith("v"):
            raise SystemExit(f"release tag must start with v: {args.tag}")
        tag_version = args.tag[1:].split("-only-", 1)[0]
    expected = args.expected or tag_version or node_package["version"]
    versions: dict[str, str] = {
        "@yitrace/db": node_package["version"],
        "yitrace-db-node crate": read_toml(ROOT / "yitrace-node/Cargo.toml")["package"]["version"],
        "yitrace-db-node Cargo.lock": cargo_package_version(
            ROOT / "yitrace-node/Cargo.lock", "yitrace-db-node"
        ),
        "@yitrace/trace-sdk": read_json(ROOT / "yitrace-sdk/typescript/package.json")["version"],
        "TypeScript SDK package-lock": read_json(
            ROOT / "yitrace-sdk/typescript/package-lock.json"
        )["packages"][""]["version"],
        "Python yitrace": read_toml(ROOT / "yitrace-sdk/python/pyproject.toml")["project"]["version"],
        "Python yitrace-db": read_toml(ROOT / "yitrace-db-python/pyproject.toml")["project"]["version"],
        "Python native crate": read_toml(ROOT / "yitrace-db-python/Cargo.toml")["package"]["version"],
        "Python native Cargo.lock": cargo_package_version(
            ROOT / "yitrace-db-python/Cargo.lock", "yitrace-db-python"
        ),
        "Rust yitrace SDK": read_toml(ROOT / "yitrace-sdk/rust/Cargo.toml")["package"]["version"],
        "Rust yitrace SDK Cargo.lock": cargo_package_version(
            ROOT / "yitrace-sdk/rust/Cargo.lock", "yitrace"
        ),
        "Rust yitrace-db": read_toml(ROOT / "yitrace-db-rs/Cargo.toml")["package"]["version"],
        "Rust yitrace-db Cargo.lock": cargo_package_version(
            ROOT / "yitrace-db-rs/Cargo.lock", "yitrace-db"
        ),
    }

    node_lock = read_json(ROOT / "yitrace-node/package-lock.json")
    versions["@yitrace/db package-lock"] = node_lock["packages"][""]["version"]

    platform_packages = sorted((ROOT / "yitrace-node/npm").glob("*/package.json"))
    expected_optional_names = set()
    for path in platform_packages:
        package = read_json(path)
        versions[package["name"]] = package["version"]
        expected_optional_names.add(package["name"])

    optional = node_package.get("optionalDependencies", {})
    if set(optional) != expected_optional_names:
        missing = sorted(expected_optional_names - set(optional))
        extra = sorted(set(optional) - expected_optional_names)
        raise SystemExit(f"optional package list mismatch: missing={missing}, extra={extra}")
    for name, version in optional.items():
        versions[f"{name} optionalDependency"] = version
        lock_entry = node_lock["packages"].get(f"node_modules/{name}")
        if not lock_entry:
            raise SystemExit(f"package-lock is missing optional package {name}")
        versions[f"{name} package-lock"] = lock_entry["version"]

    python_sdk = read_toml(ROOT / "yitrace-sdk/python/pyproject.toml")["project"]
    db_extra = python_sdk.get("optional-dependencies", {}).get("db", [])
    expected_db_requirement = f"yitrace-db=={expected}"
    if db_extra != [expected_db_requirement]:
        raise SystemExit(
            f"Python SDK db extra must be [{expected_db_requirement!r}], found {db_extra!r}"
        )

    mismatches = [(name, version) for name, version in versions.items() if version != expected]
    if mismatches:
        print(f"release version mismatch; expected {expected}", file=sys.stderr)
        for name, version in mismatches:
            print(f"- {name}: {version}", file=sys.stderr)
        return 1

    print(f"yiTrace release versions are consistent: {expected} ({len(versions)} checks)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
