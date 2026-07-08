from __future__ import annotations

import json
import sys
import tempfile
from pathlib import Path

import pytest

from yitrace_db import YiTraceDB, create_span_event_builder

SDK_PYTHON_DIR = Path(__file__).resolve().parents[2] / "yitrace-sdk" / "python"
if SDK_PYTHON_DIR.exists():
    sys.path.insert(0, str(SDK_PYTHON_DIR))


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

            trace_search = db.trace_search(
                {"text": "盗刷", "filter": {"projectId": "agentic-data", "skill": "python-db"}}
            )
            assert trace_search["total"] == 1
            assert trace_search["items"][0]["externalTraceId"] == "run-python"
            assert trace_search["readPlan"]["source"] == "filter_index"
            assert trace_search["readPlan"]["usedFilterIndex"] is True
            assert trace_search["readPlan"]["candidateSpanKeys"] == 1

            aggregate = db.trace_aggregate(
                {"filter": {"projectId": "agentic-data"}, "groupBy": ["skill"]}
            )
            assert aggregate["items"][0]["key"]["skill"] == "python-db"
            assert aggregate["items"][0]["spanCount"] == 1
            assert aggregate["readPlan"]["usedFilterIndex"] is True

            storage = db.storage_stats(
                {"filter": {"projectId": "agentic-data"}, "groupBy": ["skill"]}
            )
            assert storage["total"]["traceCount"] == 1
            assert storage["total"]["spanCount"] == 1
            assert storage["total"]["estimatedBytes"] > 0
            assert storage["readPlan"]["usedFilterIndex"] is True

            db.ingest(
                [
                    {
                        "trace_id": "python-task-a",
                        "span_id": "plan",
                        "ts": 200,
                        "seq": 1,
                        "event_type": 2,
                        "ext_span_id": "python-task-a-plan",
                        "agent_name": "planner",
                        "status": 0,
                        "duration_ns": 100,
                        "attrs": {
                            "project_id": "agentic-data",
                            "skill": "python-db",
                            "task_fingerprint": "python-task",
                            "loop_id": "python-loop",
                            "validation_status": "pass",
                        },
                    },
                    {
                        "trace_id": "python-task-a",
                        "span_id": "tool",
                        "parent_span_id": "plan",
                        "ts": 210,
                        "seq": 1,
                        "event_type": 2,
                        "ext_span_id": "python-task-a-tool",
                        "tool_name": "sql.check",
                        "status": 0,
                        "duration_ns": 50,
                        "attrs": {
                            "project_id": "agentic-data",
                            "skill": "python-db",
                            "task_fingerprint": "python-task",
                            "loop_id": "python-loop",
                            "validation_status": "pass",
                        },
                    },
                    {
                        "trace_id": "python-task-b",
                        "span_id": "plan",
                        "ts": 220,
                        "seq": 1,
                        "event_type": 2,
                        "ext_span_id": "python-task-b-plan",
                        "agent_name": "planner",
                        "status": 1,
                        "duration_ns": 300,
                        "attrs": {
                            "project_id": "agentic-data",
                            "skill": "python-db",
                            "task_fingerprint": "python-task",
                            "loop_id": "python-loop",
                            "validation_status": "fail",
                        },
                    },
                ]
            )

            trajectories = db.trace_trajectories(
                {"filter": {"projectId": "agentic-data", "taskFingerprint": "python-task"}}
            )
            assert trajectories["total"] == 2

            groups = db.trajectory_groups(
                {"filter": {"projectId": "agentic-data", "taskFingerprint": "python-task"}}
            )
            assert groups["total"] == 2

            diff = db.trace_diff("python-task-a", "python-task-b")
            assert diff["sameSignature"] is False

            loops = db.loops(projectId="agentic-data", taskFingerprint="python-task")
            assert loops["total"] == 1
            assert loops["items"][0]["loopId"] == "python-loop"

            loop = db.loop("python-loop")
            assert loop["summary"]["traceCount"] == 2

            task_pass = db.task_traces("python-task", validationStatus="pass")
            assert task_pass["total"] == 1
            assert task_pass["items"][0]["summary"]["externalTraceId"] == "python-task-a"

            annotation = db.annotate(
                traceId="run-python",
                spanId="span-python",
                label="best_path",
                score=950,
                reason="human confirmed",
                source="pytest",
                projectId="agentic-data",
                attrs={"skill": "python-db"},
            )
            assert annotation["annotationId"] == "1"
            assert annotation["externalTraceId"] == "run-python"
            assert annotation["attrs"]["project_id"] == "agentic-data"

            annotations = db.annotations(projectId="agentic-data", skill="python-db", label="best_path")
            assert annotations["total"] == 1

            updated = db.update_annotation(
                annotation["annotationId"],
                {"status": "resolved", "reviewer": "qa", "attrs": {"mode": "eval"}},
            )
            assert updated["status"] == "resolved"
            assert updated["attrs"]["mode"] == "eval"
            assert updated["attrs"]["project_id"] == "agentic-data"

            deleted = db.delete_annotation(annotation["annotationId"], reviewer="qa", reason="stale")
            assert deleted["status"] == "deleted"
            assert db.annotations(projectId="agentic-data", label="best_path")["total"] == 0
            with_deleted = db.annotations(projectId="agentic-data", label="best_path", includeDeleted=True)
            assert with_deleted["total"] == 1
            assert with_deleted["items"][0]["reason"] == "stale"

            link = db.link_dataset_item(
                datasetId="python-regression",
                itemId="case-1",
                traceId="run-python",
                spanId="span-python",
                split="eval",
                label="pass",
                score=900,
                projectId="agentic-data",
                attrs={"skill": "python-db"},
            )
            assert link["associationId"] == "1"
            assert link["externalSpanId"] == "span-python"

            links = db.dataset_associations(datasetId="python-regression", projectId="agentic-data")
            assert links["total"] == 1
            assert links["items"][0]["itemId"] == "case-1"

            assert db.annotations(projectId="agentic-data", includeDeleted=True, tenant_id=999)["total"] == 0

            db.ingest(
                [
                    {
                        "trace_id": "python-retention-keep",
                        "span_id": "span",
                        "ts": 400,
                        "seq": 1,
                        "event_type": 2,
                        "ext_span_id": "python-retention-keep-span",
                        "status": 0,
                        "duration_ns": 10,
                        "attrs": {"project_id": "python-retention", "skill": "cleanup"},
                    },
                    {
                        "trace_id": "python-retention-delete",
                        "span_id": "span",
                        "ts": 410,
                        "seq": 1,
                        "event_type": 2,
                        "ext_span_id": "python-retention-delete-span",
                        "status": 0,
                        "duration_ns": 10,
                        "attrs": {"project_id": "python-retention", "skill": "cleanup"},
                    },
                ]
            )
            db.flush()
            db.ingest(
                [
                    {
                        "trace_id": "python-retention-hot",
                        "span_id": "span",
                        "ts": 420,
                        "seq": 1,
                        "event_type": 2,
                        "ext_span_id": "python-retention-hot-span",
                        "status": 0,
                        "duration_ns": 10,
                        "attrs": {"project_id": "python-retention", "skill": "cleanup"},
                    }
                ]
            )
            db.annotate(traceId="python-retention-keep", label="keep", source="retention-test", projectId="python-retention")
            retention_query = {
                "filter": {"projectId": "python-retention"},
                "deleteBeforeTs": 1000,
                "protect": {"annotations": True, "datasetAssociations": True, "snapshots": True, "evalLinks": True, "pathMemory": True},
                "requestedBy": "pytest-retention",
                "reason": "ttl",
            }
            plan = db.retention_plan(retention_query)
            assert plan["dryRun"] is True
            assert plan["candidates"]["traceCount"] == 3
            assert any("annotation" in reasons for reasons in plan["protectedReasons"].values())
            assert len(plan["deletableTraceIds"]) == 2

            applied = db.apply_retention(retention_query)
            assert applied["applied"] is True
            assert len(applied["applyResult"]["deletedTraceIds"]) == 1
            assert len(applied["applyResult"]["skippedLiveTraceIds"]) == 1
            remaining = db.trace_search({"filter": {"projectId": "python-retention"}, "limit": 10})
            assert remaining["total"] == 2
            remaining_ids = {item["externalTraceId"] for item in remaining["items"]}
            assert remaining_ids == {"python-retention-keep", "python-retention-hot"}
            audits = db.retention_audits(source="pytest-retention")
            assert audits["total"] == 1
            assert audits["items"][0]["counts"]["deletedTraceCount"] == 1

            policy = db.create_retention_policy(
                {
                    "name": "python-retention-policy",
                    "intervalNs": 1000,
                    "nextRunAtNs": 1,
                    "query": {
                        "filter": {"projectId": "python-retention"},
                        "deleteBeforeTs": 1000,
                        "protect": {"annotations": True},
                        "requestedBy": "python-policy",
                    },
                    "source": "python-policy",
                    "reason": "ttl",
                }
            )
            assert policy["policyId"] == "1"
            due = db.run_retention_policies({"nowNs": 2, "limit": 1})
            assert due["ran"] == 1
            assert due["items"][0]["ok"] is True

        with YiTraceDB.open(tmp, tenant_id=1) as reopened:
            recovered_annotations = reopened.annotations(projectId="agentic-data", includeDeleted=True)
            assert recovered_annotations["total"] == 1
            assert recovered_annotations["items"][0]["status"] == "deleted"
            recovered_links = reopened.dataset_associations(datasetId="python-regression")
            assert recovered_links["total"] == 1
            assert recovered_links["items"][0]["itemId"] == "case-1"
            recovered_audits = reopened.retention_audits()
            assert recovered_audits["total"] == 2
            recovered_policies = reopened.retention_policies(name="python-retention-policy")
            assert recovered_policies["total"] == 1
            assert recovered_policies["items"][0]["lastRunAtNs"] == "2"


def test_python_sdk_server_embedded_exporters_write_real_db():
    from yitrace import BufferedDbExporter, SpoolConsumer, SpoolDbExporter, Tracer, init_yitrace

    with tempfile.TemporaryDirectory() as tmp:
        data_dir = Path(tmp) / "db"
        helper_data_dir = Path(tmp) / "helper-db"
        spool_dir = Path(tmp) / "spool"
        with YiTraceDB.open(data_dir, tenant_id=1) as db:
            buffered = BufferedDbExporter(
                db,
                tenant_id=1,
                max_batch=16,
                flush_interval=0.01,
                register_atexit=False,
            )
            tracer = Tracer(exporter=buffered, node_id=1)
            with tracer.trace("buffered trace", tenant_id=1) as trace:
                with trace.span("buffered span") as span:
                    span.log("buffered 疑似盗刷")
            tracer.close()
            assert buffered.sent_count() == 3

            spool = SpoolDbExporter(spool_dir, tenant_id=1, max_batch=16, fsync=False)
            spool_tracer = Tracer(exporter=spool, node_id=1)
            with spool_tracer.trace("spool trace", tenant_id=1) as trace:
                with trace.span("spool span") as span:
                    span.log("spool 疑似盗刷")
            spool_tracer.close()
            assert spool.written_count() == 3
            assert len(list((spool_dir / "ready").iterdir())) == 1

            consumer = SpoolConsumer(db, spool_dir)
            assert consumer.consume_once() == 3
            assert consumer.consumed_count() == 3
            assert not list((spool_dir / "ready").iterdir())

            hits = db.search({"text": "盗刷", "k": 10})
            messages = "\n".join(
                message
                for hit in hits
                for message in hit.get("logs", [])
            )
            assert "buffered 疑似盗刷" in messages
            assert "spool 疑似盗刷" in messages

        runtime = init_yitrace(
            path=helper_data_dir,
            tenant_id=1,
            node_id=1,
            flush_interval=0.01,
            register_atexit=False,
        )
        with runtime.tracer.trace("helper trace", tenant_id=1) as trace:
            with trace.span("helper span") as span:
                span.log("helper 疑似盗刷")
        runtime.close()
        with YiTraceDB.open(helper_data_dir, tenant_id=1) as db:
            hits = db.search({"text": "盗刷", "k": 10})
            helper_messages = "\n".join(
                message
                for hit in hits
                for message in hit.get("logs", [])
            )
            assert "helper 疑似盗刷" in helper_messages


def test_python_embedded_db_rejects_double_writer_lock():
    with tempfile.TemporaryDirectory() as tmp:
        db = YiTraceDB.open(tmp, tenant_id=1)
        try:
            lock_text = Path(tmp, ".yitrace.lock").read_text(encoding="utf-8")
            lock_info = json.loads(lock_text)
            assert lock_info["pid"] > 0
            assert lock_info["data_dir"] == tmp
            with pytest.raises(RuntimeError, match="already open or locked") as err:
                YiTraceDB.open(tmp, tenant_id=1)
            message = str(err.value)
            assert "existing lock owner" in message
            assert '"pid"' in message
            assert tmp in message
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


def test_python_fastapi_router_reuses_embedded_db_route_boundary():
    fastapi = pytest.importorskip("fastapi")
    testclient = pytest.importorskip("fastapi.testclient")
    from yitrace_db.fastapi import create_yitrace_router

    with tempfile.TemporaryDirectory() as tmp:
        with YiTraceDB.open(tmp) as db:
            app = fastapi.FastAPI()
            app.include_router(create_yitrace_router(db), prefix="/yitrace")
            client = testclient.TestClient(app)

            builder = create_span_event_builder(
                {
                    "trace_id": "router-run",
                    "session_id": "router-session",
                    "attrs": {"project_id": "router-project", "skill": "fastapi"},
                }
            )
            builder.start_span(span_id="router-span", name="fastapi route", input_text="疑似盗刷")
            builder.log("疑似盗刷", span_id="router-span")
            builder.end_span(span_id="router-span", status=0, duration_ns=1)

            ingest = client.post("/yitrace/v1/ingest", json=builder.events(), headers={"X-Tenant-Id": "42"})
            assert ingest.status_code == 200
            assert ingest.json()["ingested"] == 3

            search = client.post(
                "/yitrace/v1/search",
                json={"text": "盗刷", "filter": {"attrs": {"project_id": "router-project"}}},
                headers={"X-Tenant-Id": "42"},
            )
            assert search.status_code == 200
            assert search.json()[0]["external_trace_id"] == "router-run"

            other_tenant = client.post(
                "/yitrace/v1/search",
                json={"text": "盗刷", "filter": {"attrs": {"project_id": "router-project"}}},
                headers={"X-Tenant-Id": "43"},
            )
            assert other_tenant.status_code == 200
            assert other_tenant.json() == []


def test_python_cli_rejects_multi_worker_embedded_serve():
    from yitrace_db.cli import _parse_bind, main

    assert _parse_bind("0.0.0.0:7878") == ("0.0.0.0", 7878)
    with tempfile.TemporaryDirectory() as tmp:
        with pytest.raises(SystemExit):
            main(["serve", "--data-dir", tmp, "--workers", "2"])
