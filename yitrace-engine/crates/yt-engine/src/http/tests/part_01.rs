    #[test]
    fn retention_policies_run_due_and_persist() {
        let dir = durable_temp_dir("retention-policy");
        {
            let coord = WriteCoordinator::open_durable(&dir).unwrap();
            coord.recover();
            let api = EngineJsonApi::new(coord);
            let batch = r#"[
              {"trace_id":201,"span_id":1,"ts":10,"seq":1,"event_type":1,"ext_span_id":"201-1","input_text":"old policy delete","attrs":{"project_id":"policy-demo"}},
              {"trace_id":201,"span_id":1,"ts":20,"seq":2,"event_type":2,"ext_span_id":"201-1","duration_ns":10,"output_text":"done"},
              {"trace_id":202,"span_id":1,"ts":200,"seq":1,"event_type":1,"ext_span_id":"202-1","input_text":"new policy keep","attrs":{"project_id":"policy-demo"}},
              {"trace_id":202,"span_id":1,"ts":220,"seq":2,"event_type":2,"ext_span_id":"202-1","duration_ns":10,"output_text":"done"}
            ]"#;
            let (status, body) = api.route_with_tenant("POST", "/v1/ingest", batch, Some(1));
            assert_eq!(status, 200, "{body}");
            api.coord().flush_memtable();

            let policy = r#"{
              "name":"daily-policy",
              "intervalNs":1000,
              "nextRunAtNs":100,
              "source":"policy-test",
              "reason":"ttl cleanup",
              "query":{"filter":{"projectId":"policy-demo"},"olderThanNs":50,"compact":true}
            }"#;
            let (status, body) =
                api.route_with_tenant("POST", "/v1/retention-policies", policy, Some(1));
            assert_eq!(status, 200, "{body}");
            assert!(body.contains(r#""policyId":"1""#), "{body}");
            assert!(body.contains(r#""nextRunAtNs":"100""#), "{body}");

            let (status, body) = api.route_with_tenant(
                "GET",
                "/v1/retention-policies?name=daily-policy",
                "",
                Some(1),
            );
            assert_eq!(status, 200, "{body}");
            assert!(body.contains(r#""total":1"#), "{body}");

            let (status, body) = api.route_with_tenant(
                "POST",
                "/v1/retention-policies/run-due",
                r#"{"nowNs":100,"limit":10}"#,
                Some(1),
            );
            assert_eq!(status, 200, "{body}");
            assert!(body.contains(r#""ran":1"#), "{body}");
            assert!(body.contains(r#""failed":0"#), "{body}");
            assert!(body.contains(r#""deletedTraceCount":1"#), "{body}");
            assert!(body.contains(r#""source":"policy-test""#), "{body}");
            assert!(body.contains(r#""nextRunAtNs":"1100""#), "{body}");

            let query = r#"{"filter":{"projectId":"policy-demo"}}"#;
            let (status, after) = api.route_with_tenant("POST", "/v1/trace-search", query, Some(1));
            assert_eq!(status, 200, "{after}");
            assert!(after.contains(r#""total":1"#), "{after}");
            assert!(!after.contains(r#""traceId":"201""#), "{after}");
            assert!(after.contains(r#""traceId":"202""#), "{after}");

            let (status, audits) = api.route_with_tenant(
                "GET",
                "/v1/retention-audits?source=policy-test",
                "",
                Some(1),
            );
            assert_eq!(status, 200, "{audits}");
            assert!(audits.contains(r#""total":1"#), "{audits}");
            assert!(audits.contains(r#""deletedTraceCount":1"#), "{audits}");
        }
        {
            let coord = WriteCoordinator::open_durable(&dir).unwrap();
            coord.recover();
            let api = EngineJsonApi::new(coord);
            let (status, policies) = api.route_with_tenant(
                "GET",
                "/v1/retention-policies?name=daily-policy",
                "",
                Some(1),
            );
            assert_eq!(status, 200, "{policies}");
            assert!(policies.contains(r#""total":1"#), "{policies}");
            assert!(policies.contains(r#""lastRunAtNs":"100""#), "{policies}");
            assert!(policies.contains(r#""nextRunAtNs":"1100""#), "{policies}");

            let (status, audits) = api.route_with_tenant(
                "GET",
                "/v1/retention-audits?source=policy-test",
                "",
                Some(1),
            );
            assert_eq!(status, 200, "{audits}");
            assert!(audits.contains(r#""total":1"#), "{audits}");
        }
        let _ = std::fs::remove_dir_all(dir);
    }
    #[test]
    fn golden_paths_are_tenant_isolated_and_durable() {
        let dir = durable_temp_dir("golden-paths");
        {
            let coord = WriteCoordinator::open_durable(&dir).unwrap();
            coord.recover();
            let api = EngineJsonApi::new(coord);
            let batch = r#"[
              {
                "trace_id":"gold-run-1",
                "span_id":"gold-span-1",
                "ts":100,
                "seq":1,
                "event_type":2,
                "ext_span_id":"gold-span-1",
                "status":0,
                "duration_ns":12,
                "tool_name":"planner",
                "model":"qwen",
                "provider":"openai",
                "attrs":{"project_id":"agentic-data","task_fingerprint":"refund-dispute","phase":"plan"}
              },
              {
                "trace_id":"gold-run-2",
                "span_id":"gold-span-2",
                "ts":200,
                "seq":1,
                "event_type":2,
                "ext_span_id":"gold-span-2",
                "status":0,
                "duration_ns":11,
                "tool_name":"planner",
                "model":"qwen",
                "provider":"openai",
                "attrs":{"project_id":"agentic-data","task_fingerprint":"refund-dispute","phase":"plan"}
              },
              {
                "trace_id":"gold-run-3",
                "span_id":"gold-span-3a",
                "ts":300,
                "seq":1,
                "event_type":2,
                "ext_span_id":"gold-span-3a",
                "status":0,
                "duration_ns":9,
                "tool_name":"planner",
                "model":"qwen",
                "provider":"openai",
                "attrs":{"project_id":"agentic-data","task_fingerprint":"refund-dispute","phase":"plan"}
              },
              {
                "trace_id":"gold-run-3",
                "span_id":"gold-span-3b",
                "ts":310,
                "seq":1,
                "event_type":2,
                "ext_span_id":"gold-span-3b",
                "status":0,
                "duration_ns":8,
                "tool_name":"tester",
                "model":"qwen",
                "provider":"openai",
                "attrs":{"project_id":"agentic-data","task_fingerprint":"refund-dispute","phase":"verify","validator":"unit"}
              }
            ]"#;
            let (status, body) = api.route_with_tenant("POST", "/v1/ingest", batch, Some(1));
            assert_eq!(status, 200, "{body}");

            let create = r#"{
              "sourceTraceId":"gold-run-1",
              "taskFingerprint":"refund-dispute",
              "score":960,
              "label":"fast path",
              "reason":"stable winner",
              "source":"human",
              "evalProfile":"release-gate",
              "minSampleCount":3,
              "marginScore":1001,
              "comparisonWindowNs":1000,
              "staleReasons":["manual_review"],
              "projectId":"agentic-data"
            }"#;
            let (status, body) = api.route_with_tenant("POST", "/v1/golden-paths", create, Some(1));
            assert_eq!(status, 200, "{body}");
            assert!(body.contains(r#""goldenPathId":"1""#), "{body}");
            assert!(body.contains(r#""status":"candidate""#), "{body}");
            assert!(
                body.contains(r#""trajectorySignature":"fnv1a64:"#),
                "{body}"
            );
            assert!(
                body.contains(r#""externalSourceTraceId":"gold-run-1""#),
                "{body}"
            );
            assert!(body.contains(r#""sourceTrajectory":{"#), "{body}");
            assert!(
                body.contains(r#""source_cost_usd_nanos""#)
                    || body.contains(r#""source_duration_ns""#),
                "{body}"
            );
            assert!(
                body.contains(r#""source_trajectory_step_count":1"#),
                "{body}"
            );
            assert!(body.contains(r#""project_id":"agentic-data""#), "{body}");
            assert!(body.contains(r#""model":"qwen""#), "{body}");
            assert!(body.contains(r#""provider":"openai""#), "{body}");
            assert!(body.contains(r#""evalProfile":"release-gate""#), "{body}");
            assert!(body.contains(r#""minSampleCount":"3""#), "{body}");
            assert!(body.contains(r#""marginScore":1001"#), "{body}");
            assert!(body.contains(r#""comparisonWindowNs":"1000""#), "{body}");
            assert!(
                body.contains(r#""staleReasons":["manual_review"]"#),
                "{body}"
            );

            let (status, body) = api.route_with_tenant(
                "POST",
                "/v1/golden-paths/1/status",
                r#"{"status":"confirmed","reason":"manual accept","source":"reviewer"}"#,
                Some(1),
            );
            assert_eq!(status, 200, "{body}");
            assert!(body.contains(r#""status":"confirmed""#), "{body}");
            assert!(body.contains(r#""reason":"manual accept""#), "{body}");

            let (status, body) = api.route_with_tenant(
                "POST",
                "/v1/annotations",
                r#"{"traceId":"gold-run-1","label":"best_path","score":960,"source":"human","projectId":"agentic-data"}"#,
                Some(1),
            );
            assert_eq!(status, 200, "{body}");
            let (status, body) = api.route_with_tenant(
                "POST",
                "/v1/dataset-associations",
                r#"{"datasetId":"golden-regression","itemId":"case-1","traceId":"gold-run-1","label":"pass","score":950,"projectId":"agentic-data"}"#,
                Some(1),
            );
            assert_eq!(status, 200, "{body}");

            let (status, body) = api.route_with_tenant(
                "GET",
                "/v1/golden-paths?taskFingerprint=refund-dispute&status=confirmed&projectId=agentic-data",
                "",
                Some(1),
            );
            assert_eq!(status, 200, "{body}");
            assert!(body.contains(r#""count":1"#), "{body}");

            let (status, body) = api.route_with_tenant(
                "GET",
                "/v1/golden-paths?taskFingerprint=refund-dispute&evalProfile=release-gate",
                "",
                Some(1),
            );
            assert_eq!(status, 200, "{body}");
            assert!(body.contains(r#""count":1"#), "{body}");

            let challenger = r#"{
              "sourceTraceId":"gold-run-3",
              "taskFingerprint":"refund-dispute",
              "score":930,
              "label":"challenger path",
              "challengerOf":"1",
              "evalProfile":"release-gate",
              "minSampleCount":2,
              "marginScore":20,
              "comparisonWindowNs":5000,
              "projectId":"agentic-data"
            }"#;
            let (status, body) =
                api.route_with_tenant("POST", "/v1/golden-paths", challenger, Some(1));
            assert_eq!(status, 200, "{body}");
            assert!(body.contains(r#""goldenPathId":"2""#), "{body}");
            assert!(body.contains(r#""challengerOf":"1""#), "{body}");
            assert!(body.contains(r#""evalProfile":"release-gate""#), "{body}");

            let (status, body) = api.route_with_tenant(
                "GET",
                "/v1/golden-paths?challengerOf=1&evalProfile=release-gate",
                "",
                Some(1),
            );
            assert_eq!(status, 200, "{body}");
            assert!(body.contains(r#""count":1"#), "{body}");
            assert!(body.contains(r#""goldenPathId":"2""#), "{body}");

            let (status, body) = api.route_with_tenant(
                "POST",
                "/v1/trace-trajectories",
                r#"{"filter":{"taskFingerprint":"refund-dispute","projectId":"agentic-data"},"limit":10}"#,
                Some(1),
            );
            assert_eq!(status, 200, "{body}");
            assert!(body.contains(r#""index":"materialized""#), "{body}");
            assert!(body.contains(r#""total":3"#), "{body}");
            assert!(
                body.contains(r#""trajectory":{"signature":"fnv1a64:"#),
                "{body}"
            );

            let (status, body) = api.route_with_tenant(
                "POST",
                "/v1/trace-trajectories",
                r#"{"filter":{"taskFingerprint":"refund-dispute","attrs":{"model":"qwen","provider":"openai"}},"limit":10}"#,
                Some(1),
            );
            assert_eq!(status, 200, "{body}");
            assert!(body.contains(r#""total":3"#), "{body}");
            assert!(body.contains(r#""model":"qwen""#), "{body}");

            let (status, body) = api.route_with_tenant(
                "POST",
                "/v1/path-adherence",
                r#"{"goldenPathId":"1","traceId":"gold-run-2"}"#,
                Some(1),
            );
            assert_eq!(status, 200, "{body}");
            assert!(body.contains(r#""adherence":"followed""#), "{body}");
            assert!(body.contains(r#""sameSignature":true"#), "{body}");
            assert!(body.contains(r#""sourceAvailable":true"#), "{body}");

            let (status, body) = api.route_with_tenant(
                "POST",
                "/v1/golden-paths/1/adherence",
                r#"{"traceId":"gold-run-3"}"#,
                Some(1),
            );
            assert_eq!(status, 200, "{body}");
            assert!(body.contains(r#""adherence":"extended""#), "{body}");
            assert!(body.contains(r#""sameSignature":false"#), "{body}");
            assert!(
                body.contains(r#""extraSteps":["tool:tester|phase:verify|validator:unit"]"#),
                "{body}"
            );

            let (status, body) = api.route_with_tenant(
                "POST",
                "/v1/golden-path-evidence",
                r#"{"goldenPathId":"1","candidateTraceId":"gold-run-3"}"#,
                Some(1),
            );
            assert_eq!(status, 200, "{body}");
            assert!(body.contains(r#""source":{"available":true"#), "{body}");
            assert!(body.contains(r#""annotationCount":1"#), "{body}");
            assert!(body.contains(r#""datasetAssociationCount":1"#), "{body}");
            assert!(body.contains(r#""pathAdherence":{"goldenPath""#), "{body}");
            assert!(body.contains(r#""traceDiff":{"left""#), "{body}");
            assert!(body.contains(r#""adherence":"extended""#), "{body}");

            let (status, body) = api.route_with_tenant(
                "POST",
                "/v1/golden-path-export",
                r#"{"filter":{"taskFingerprint":"refund-dispute","projectId":"agentic-data"},"limit":10}"#,
                Some(1),
            );
            assert_eq!(status, 200, "{body}");
            assert!(
                body.contains(r#""schemaVersion":"yitrace.golden_path_export.v1""#),
                "{body}"
            );
            assert!(body.contains(r#""format":"jsonl""#), "{body}");
            assert!(body.contains(r#""count":1"#), "{body}");
            assert!(body.contains(r#""recordType":"golden_path""#), "{body}");
            assert!(body.contains(r#""annotationCount":1"#), "{body}");
            assert!(body.contains(r#""datasetAssociationCount":1"#), "{body}");
            assert!(
                body.contains(r#""jsonl":"{\"schemaVersion\":\"yitrace.golden_path_export.v1\""#),
                "{body}"
            );

            let (status, body) = api.route_with_tenant(
                "POST",
                "/v1/golden-path-health",
                r#"{"goldenPathId":"1","filter":{"projectId":"agentic-data"},"limit":10,"examples":10}"#,
                Some(1),
            );
            assert_eq!(status, 200, "{body}");
            assert!(body.contains(r#""includeSource":false"#), "{body}");
            assert!(body.contains(r#""matchingTraceTotal":2"#), "{body}");
            assert!(body.contains(r#""analyzedTraceTotal":2"#), "{body}");
            assert!(body.contains(r#""followed":1"#), "{body}");
            assert!(body.contains(r#""extended":1"#), "{body}");
            assert!(body.contains(r#""usable":1.000000"#), "{body}");
            assert!(body.contains(r#""stale":true"#), "{body}");
            assert!(body.contains(r#""manual_review""#), "{body}");
            assert!(body.contains(r#""insufficient_samples""#), "{body}");
            assert!(body.contains(r#""health_below_margin""#), "{body}");
            assert!(body.contains(r#""adherence":"extended""#), "{body}");

            let (status, body) = api.route_with_tenant(
                "POST",
                "/v1/golden-paths/1/health",
                r#"{"filter":{"projectId":"agentic-data"},"includeSource":true,"limit":10}"#,
                Some(1),
            );
            assert_eq!(status, 200, "{body}");
            assert!(body.contains(r#""includeSource":true"#), "{body}");
            assert!(body.contains(r#""matchingTraceTotal":3"#), "{body}");
            assert!(body.contains(r#""followed":2"#), "{body}");

            let (status, body) =
                api.route_with_tenant("POST", "/v1/golden-paths/1/evidence", "", Some(1));
            assert_eq!(status, 200, "{body}");
            assert!(body.contains(r#""candidate":null"#), "{body}");

            let (status, body) = api.route_with_tenant(
                "GET",
                "/v1/golden-paths?taskFingerprint=refund-dispute",
                "",
                Some(2),
            );
            assert_eq!(status, 200, "{body}");
            assert!(body.contains(r#""count":0"#), "{body}");
            assert_eq!(
                api.route_with_tenant(
                    "POST",
                    "/v1/golden-paths/1/status",
                    r#"{"status":"rejected"}"#,
                    Some(2),
                )
                .0,
                404
            );
            assert_eq!(
                api.route_with_tenant(
                    "POST",
                    "/v1/path-adherence",
                    r#"{"goldenPathId":"1","traceId":"gold-run-2"}"#,
                    Some(2),
                )
                .0,
                404
            );
            assert_eq!(
                api.route_with_tenant(
                    "POST",
                    "/v1/golden-path-evidence",
                    r#"{"goldenPathId":"1","candidateTraceId":"gold-run-3"}"#,
                    Some(2),
                )
                .0,
                404
            );
            let (status, body) = api.route_with_tenant(
                "POST",
                "/v1/golden-path-export",
                r#"{"filter":{"taskFingerprint":"refund-dispute"}}"#,
                Some(2),
            );
            assert_eq!(status, 200, "{body}");
            assert!(body.contains(r#""count":0"#), "{body}");
            assert_eq!(
                api.route_with_tenant(
                    "POST",
                    "/v1/golden-path-health",
                    r#"{"goldenPathId":"1"}"#,
                    Some(2),
                )
                .0,
                404
            );
        }
        {
            let coord = WriteCoordinator::open_durable(&dir).unwrap();
            coord.recover();
            let api = EngineJsonApi::new(coord);
            let (status, body) = api.route_with_tenant(
                "GET",
                "/v1/golden-paths?taskFingerprint=refund-dispute&status=confirmed",
                "",
                Some(1),
            );
            assert_eq!(status, 200, "{body}");
            assert!(body.contains(r#""count":1"#), "{body}");
        }
        let _ = std::fs::remove_dir_all(dir);
    }
    #[test]
    fn trace_aggregate_groups_filtered_spans() {
        let s = server();
        let batch = r#"[
          {
            "trace_id":101,
            "span_id":1,
            "ts":100,
            "seq":1,
            "event_type":2,
            "ext_span_id":"101-1",
            "session_id":9001,
            "status":0,
            "duration_ns":10,
            "tool_name":"planner",
            "input_tokens":5,
            "output_tokens":10,
            "total_tokens":15,
            "cost_usd_nanos":1000,
            "attrs":{"project_id":"agentic-data","skill":"review","mode":"auto"}
          },
          {
            "trace_id":102,
            "span_id":1,
            "ts":110,
            "seq":1,
            "event_type":2,
            "ext_span_id":"102-1",
            "session_id":9002,
            "status":1,
            "duration_ns":20,
            "tool_name":"planner",
            "input_tokens":7,
            "output_tokens":8,
            "total_tokens":15,
            "cost_usd_nanos":2000,
            "attrs":{"project_id":"agentic-data","skill":"review","mode":"auto"}
          },
          {
            "trace_id":103,
            "span_id":1,
            "ts":120,
            "seq":1,
            "event_type":2,
            "ext_span_id":"103-1",
            "session_id":9003,
            "status":0,
            "duration_ns":30,
            "tool_name":"builder",
            "input_tokens":100,
            "output_tokens":100,
            "total_tokens":200,
            "cost_usd_nanos":9999,
            "attrs":{"project_id":"other","skill":"build","mode":"auto"}
          },
          {
            "trace_id":104,
            "span_id":1,
            "ts":130,
            "seq":1,
            "event_type":2,
            "ext_span_id":"104-1",
            "session_id":9004,
            "status":0,
            "duration_ns":40,
            "tool_name":"priced",
            "provider":"openai",
            "model":"gpt-4o-mini",
            "input_tokens":10,
            "cached_input_tokens":10,
            "output_tokens":10,
            "attrs":{"project_id":"priced","skill":"cost","mode":"auto"}
          }
        ]"#;
        let (status, body) = s.route_with_tenant("POST", "/v1/ingest", batch, Some(1));
        assert_eq!(status, 200, "{body}");

        let (status, body) = s.route_with_tenant(
            "POST",
            "/v1/trace-aggregate",
            r#"{"groupBy":["skill","mode"],"filter":{"attrs":{"project_id":"agentic-data"}},"sort":"count","order":"desc"}"#,
            Some(1),
        );
        assert_eq!(status, 200, "{body}");
        assert!(body.contains(r#""total":1"#), "{body}");
        assert!(body.contains(r#""spanTotal":2"#), "{body}");
        assert!(body.contains(r#""skill":"review""#), "{body}");
        assert!(body.contains(r#""mode":"auto""#), "{body}");
        assert!(body.contains(r#""spanCount":2"#), "{body}");
        assert!(body.contains(r#""traceCount":2"#), "{body}");
        assert!(body.contains(r#""errorCount":1"#), "{body}");
        assert!(body.contains(r#""sum":30"#), "{body}");
        assert!(body.contains(r#""totalTokens":30"#), "{body}");
        assert!(body.contains(r#""costUsdNanos":3000"#), "{body}");
        assert!(
            body.contains(r#""index":"attrs_postings+folded_verify""#),
            "{body}"
        );
        assert!(
            body.contains(r#""aggregationIndex":"aggregate_preaggregate_tail_overlay""#),
            "{body}"
        );
        assert!(
            body.contains(r#""spanReadIndex":"aggregate_preaggregate""#),
            "{body}"
        );
        assert!(body.contains(r#""usedSegmentRollup":false"#), "{body}");
        assert!(body.contains(r#""readModelCache":"miss""#), "{body}");

        let (status, body) = s.route_with_tenant(
            "POST",
            "/v1/trace-aggregate",
            r#"{"groupBy":["toolName"],"filter":{"status":1}}"#,
            Some(1),
        );
        assert_eq!(status, 200, "{body}");
        assert!(body.contains(r#""toolName":"planner""#), "{body}");
        assert!(body.contains(r#""spanTotal":1"#), "{body}");

        let (status, body) = s.route_with_tenant(
            "POST",
            "/v1/trace-search",
            r#"{"filter":{"projectId":"agentic-data","minCostUsdNanos":1500,"maxCostUsdNanos":2500,"minTotalTokens":10,"maxTotalTokens":20}}"#,
            Some(1),
        );
        assert_eq!(status, 200, "{body}");
        assert!(body.contains(r#""total":1"#), "{body}");
        assert!(body.contains(r#""traceId":"102""#), "{body}");
        assert!(
            body.contains(r#""index":"attrs_postings+folded_verify""#),
            "{body}"
        );

        let (status, body) = s.route_with_tenant(
            "POST",
            "/v1/trace-search",
            r#"{"filter":{"projectId":"priced","minCostUsd":0.000008,"maxCostUsd":0.000009}}"#,
            Some(1),
        );
        assert_eq!(status, 200, "{body}");
        assert!(body.contains(r#""total":1"#), "{body}");
        assert!(body.contains(r#""traceId":"104""#), "{body}");
        assert!(body.contains(r#""costUsdNanos":8250"#), "{body}");
        assert!(
            body.contains(r#""source":"estimated_model_price""#),
            "{body}"
        );
    }
