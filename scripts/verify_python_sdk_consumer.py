#!/usr/bin/env python3
"""Verify the Python yitrace SDK from a clean consumer environment."""
from __future__ import annotations

import os
import shutil
import subprocess
import sys
import tempfile
import textwrap
import venv
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SDK_DIR = ROOT / "yitrace-sdk" / "python"


def run(cmd: list[str], *, cwd: Path | None = None, env: dict[str, str] | None = None) -> None:
    print(f"\n==> {' '.join(cmd)}")
    subprocess.run(cmd, cwd=cwd, env=env, check=True)


def venv_python(env_dir: Path) -> Path:
    if os.name == "nt":
        return env_dir / "Scripts" / "python.exe"
    return env_dir / "bin" / "python"


def main() -> int:
    if not SDK_DIR.exists():
        raise SystemExit(f"missing Python SDK directory: {SDK_DIR}")
    work = Path(tempfile.mkdtemp(prefix="yitrace-python-sdk-consumer-"))
    try:
        env_dir = work / ".venv"
        venv.EnvBuilder(with_pip=True, clear=True).create(env_dir)
        py = venv_python(env_dir)
        run([str(py), "-m", "pip", "install", "--no-deps", str(SDK_DIR)])
        script = work / "verify.py"
        script.write_text(
            textwrap.dedent(
                """
                from yitrace import CollectingExporter, Tracer, YiTraceClient, connect

                client = connect(url="http://127.0.0.1:7878", tenant_id=1)
                assert isinstance(client, YiTraceClient)

                exporter = CollectingExporter()
                tracer = Tracer(exporter=exporter, node_id=1)
                with tracer.trace("clean consumer") as trace:
                    with trace.span("span") as span:
                        span.log("疑似盗刷")
                assert [event.event_type.value for event in exporter.events] == [1, 4, 2]

                try:
                    connect(path="./data")
                except RuntimeError as err:
                    message = str(err)
                    assert "pip install yitrace-db" in message
                    assert "pip install 'yitrace[db]'" in message
                else:
                    raise AssertionError("connect(path=...) must explain the missing yitrace-db package")
                """
            ),
            encoding="utf-8",
        )
        run([str(py), str(script)], cwd=work)
        print(f"\nVerified Python yitrace SDK in clean consumer: {work}")
    finally:
        shutil.rmtree(work, ignore_errors=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
