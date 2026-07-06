from __future__ import annotations

import tempfile

import pytest

from yitrace_db import YiTraceDB, create_span_event_builder


def test_python_embedded_db_ingests_searches_and_reads_span_detail():
    with tempfile.TemporaryDirectory() as tmp:
        with YiTraceDB.open(tmp, tenant_id=1) as db:
            builder = create_span_event_builder(
                {
                    "trace_id": "run-python",
                    "session_id": "session-python",
                    "attrs": {"project_id": "agentic-data", "skill": "python-db", "mode": "test"},
                }
            )
            builder.start_span(
                span_id="span-python",
                name="risk review",
                agent_name="risk-agent",
                input_text="疑似盗刷订单",
                input_tokens=100,
            )
            builder.log("疑似盗刷", span_id="span-python")
            builder.end_span(
                span_id="span-python",
                status=0,
                duration_ns=12_000,
                output_text="needs review",
                output_tokens=20,
            )
            result = builder.ingest(db)
            assert result["ingested"] == 3

            hits = db.search({"text": "盗刷", "filter": {"attrs": {"project_id": "agentic-data", "skill": "python-db"}}})
            assert len(hits) == 1
            assert hits[0]["external_trace_id"] == "run-python"

            sessions = db.sessions(attrs={"project_id": "agentic-data", "skill": "python-db"})
            assert sessions["items"]
            assert sessions["items"][0]["externalSessionId"] == "session-python"

            span = db.span("run-python", "span-python")
            assert span["externalSpanId"] == "span-python"
            messages = [message for event in span["logEvents"] for message in event["messages"]]
            assert "疑似盗刷" in messages


def test_python_embedded_db_rejects_double_writer_lock():
    with tempfile.TemporaryDirectory() as tmp:
        db = YiTraceDB.open(tmp, tenant_id=1)
        try:
            with pytest.raises(RuntimeError, match="already open or locked"):
                YiTraceDB.open(tmp, tenant_id=1)
        finally:
            db.close()

        reopened = YiTraceDB.open(tmp, tenant_id=1)
        reopened.close()


def test_python_embedded_db_general_route_json_and_closed_errors():
    with tempfile.TemporaryDirectory() as tmp:
        db = YiTraceDB.open({"dataDir": tmp, "tenantId": 1})
        try:
            body = db.route_json("POST", "/v1/search", {"text": "missing", "k": 1})
            assert body == []
        finally:
            db.close()

        with pytest.raises(RuntimeError, match="closed"):
            db.search(text="missing")
