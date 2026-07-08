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


def venv_script(env_dir: Path, name: str) -> Path:
    if os.name == "nt":
        return env_dir / "Scripts" / f"{name}.exe"
    return env_dir / "bin" / name


def main() -> int:
    if not SDK_DIR.exists():
        raise SystemExit(f"missing Python SDK directory: {SDK_DIR}")
    work = Path(tempfile.mkdtemp(prefix="yitrace-python-sdk-consumer-"))
    try:
        env_dir = work / ".venv"
        venv.EnvBuilder(with_pip=True, clear=True).create(env_dir)
        py = venv_python(env_dir)
        run([str(py), "-m", "pip", "install", "--no-deps", str(SDK_DIR)])
        run([str(venv_script(env_dir, "yitrace")), "--help"])
        script = work / "verify.py"
        script.write_text(
            textwrap.dedent(
                """
                from yitrace import CollectingExporter, NoopExporter, Tracer, YiTraceClient, connect, init_yitrace, shutdown_yitrace

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

                runtime = init_yitrace(path="./data", fail_open=True, register_atexit=False)
                assert runtime.enabled is False
                assert isinstance(runtime.exporter, NoopExporter)
                with runtime.tracer.trace("fail open") as trace:
                    with trace.span("span") as span:
                        span.log("ignored")
                runtime.close()
                assert runtime.exporter.dropped_count() == 3
                shutdown_yitrace()
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
