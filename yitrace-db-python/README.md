# yitrace-db

Embedded yiTrace DB for Python agents.

`yitrace-db` is the Python equivalent of `@yitrace/db`: it embeds the Rust
yiTrace engine in the Python process and calls `EngineJsonApi` in-process. It
does not parse yiTrace files in Python, does not start an HTTP server, and does
not send traffic through a TCP socket.

## Install

For local development from this repository:

```bash
cd yitrace-db-python
python -m pip install -e .
```

Public wheels should be built with maturin per platform:

```bash
cd yitrace-db-python
python -m pip install maturin
python -m maturin build --release --interpreter "$(command -v python)"
```

Use `--interpreter` when the machine has multiple Python installs; otherwise
maturin may discover an old system Python instead of the environment you are
building for.

## Usage

```python
from yitrace_db import YiTraceDB, create_span_event_builder

db = YiTraceDB.open("./data", tenant_id=1)

events = create_span_event_builder({
    "trace_id": "run-uuid",
    "session_id": "session-uuid",
    "attrs": {
        "project_id": "agentic-data",
        "skill": "review",
        "mode": "auto",
    },
})

events.start_span(span_id="span-uuid", name="risk review", input_text="疑似盗刷")
events.log("疑似盗刷", span_id="span-uuid")
events.end_span(span_id="span-uuid", status=0, duration_ns=12_000_000, output_text="needs review")
events.ingest(db)

hits = db.search({"text": "盗刷", "k": 10, "filter": {"attrs": {"project_id": "agentic-data"}}})
span = db.span("run-uuid", "span-uuid")

db.close()
```

Use `with` to close safely:

```python
with YiTraceDB.open("./data", tenant_id=1) as db:
    print(db.search(text="盗刷", k=10))
```

The existing `yitrace` package remains the pure-Python instrumentation SDK. Use
it when you only need to emit traces to a running yiTrace service. Use
`yitrace-db` when a Python app needs an embedded local TraceDB.
