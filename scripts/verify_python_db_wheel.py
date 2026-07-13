#!/usr/bin/env python3
"""Install a built yitrace-db wheel in a clean environment and use every public entrypoint."""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import urllib.request
import venv


def run(command: list[str], **kwargs) -> subprocess.CompletedProcess[str]:
    print("==>", " ".join(command), flush=True)
    return subprocess.run(command, check=True, text=True, **kwargs)


def venv_command(root: Path, name: str) -> Path:
    directory = "Scripts" if os.name == "nt" else "bin"
    suffix = ".exe" if os.name == "nt" else ""
    return root / directory / f"{name}{suffix}"


def choose_wheel(wheel_dir: Path) -> Path:
    wheels = sorted(wheel_dir.glob("yitrace_db-*.whl"))
    if len(wheels) != 1:
        raise SystemExit(f"expected one yitrace-db wheel in {wheel_dir}, found {len(wheels)}")
    return wheels[0].resolve()


def free_port() -> int:
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return int(sock.getsockname()[1])


def wait_for_health(port: int, process: subprocess.Popen[str]) -> None:
    url = f"http://127.0.0.1:{port}/v1/healthz"
    deadline = time.monotonic() + 20
    while time.monotonic() < deadline:
        if process.poll() is not None:
            stdout, stderr = process.communicate()
            raise RuntimeError(f"yitrace-db serve exited early\nstdout:\n{stdout}\nstderr:\n{stderr}")
        try:
            with urllib.request.urlopen(url, timeout=1) as response:
                if response.status == 200 and json.load(response) == {"ok": True}:
                    return
        except Exception:
            time.sleep(0.1)
    raise RuntimeError(f"timed out waiting for {url}")


def post_json(port: int, path: str, payload: object) -> object:
    request = urllib.request.Request(
        f"http://127.0.0.1:{port}{path}",
        data=json.dumps(payload).encode("utf-8"),
        headers={"Content-Type": "application/json", "X-Tenant-Id": "42"},
        method="POST",
    )
    with urllib.request.urlopen(request, timeout=5) as response:
        assert response.status == 200
        return json.load(response)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--wheel-dir", type=Path, required=True)
    args = parser.parse_args()
    wheel = choose_wheel(args.wheel_dir)

    work = Path(tempfile.mkdtemp(prefix="yitrace-python-db-wheel-"))
    try:
        environment = work / "venv"
        venv.EnvBuilder(with_pip=True).create(environment)
        python = venv_command(environment, "python")
        cli = venv_command(environment, "yitrace-db")
        run([str(python), "-m", "pip", "install", "--disable-pip-version-check", f"{wheel}[server]"])

        consumer = work / "consumer.py"
        consumer.write_text(
            """
from pathlib import Path
import tempfile

from yitrace_db import YiTraceDB, create_span_event_builder

with tempfile.TemporaryDirectory() as tmp:
    path = Path(tmp)
    with YiTraceDB.open(path, tenant_id=42) as db:
        builder = create_span_event_builder({
            "trace_id": "wheel-run",
            "session_id": "wheel-session",
            "attrs": {"project_id": "wheel-project", "skill": "release"},
        })
        builder.start_span(span_id="wheel-span", name="wheel smoke", input_text="疑似盗刷")
        builder.log("疑似盗刷", span_id="wheel-span")
        builder.end_span(span_id="wheel-span", status=0, duration_ns=7)
        db.ingest(builder.events(), tenant_id=42)
        assert len(db.search(text="盗刷", k=10)) == 1

    with YiTraceDB.open(path, tenant_id=42) as db:
        hits = db.search(text="盗刷", k=10)
        assert len(hits) == 1
        assert hits[0]["external_trace_id"] == "wheel-run"
""".lstrip(),
            encoding="utf-8",
        )
        run([str(python), str(consumer)], cwd=work)
        run([str(cli), "--help"], cwd=work, capture_output=True)

        port = free_port()
        server_data = work / "server-data"
        process = subprocess.Popen(
            [str(cli), "serve", "--data-dir", str(server_data), "--bind", f"127.0.0.1:{port}"],
            cwd=work,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        try:
            wait_for_health(port, process)
            ingest = post_json(
                port,
                "/v1/ingest",
                [{
                    "trace_id": "server-wheel-run",
                    "span_id": "server-wheel-span",
                    "session_id": "server-wheel-session",
                    "ts": 1,
                    "seq": 1,
                    "event_type": 3,
                    "ext_span_id": "server-wheel-span",
                    "status": 0,
                    "attrs": {"project_id": "wheel-server", "skill": "release"},
                    "logs": ["CLI server 盗刷验证"],
                }],
            )
            assert ingest["ingested"] == 1
            hits = post_json(
                port,
                "/v1/search",
                {"text": "盗刷", "filter": {"attrs": {"project_id": "wheel-server"}}},
            )
            assert len(hits) == 1
            assert hits[0]["external_trace_id"] == "server-wheel-run"
        finally:
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=10)
        print(f"Verified yitrace-db wheel in clean consumer: {wheel.name}")
        return 0
    finally:
        shutil.rmtree(work, ignore_errors=True)


if __name__ == "__main__":
    raise SystemExit(main())
