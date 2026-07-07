    struct FailingShardClient;

    impl ShardClient for FailingShardClient {
        fn route_with_tenant(
            &self,
            _method: &str,
            _path: &str,
            _body: &str,
            _tenant: Option<u64>,
        ) -> Result<(u16, String), ShardClientError> {
            Err(ShardClientError::unavailable("test shard route unavailable"))
        }

        fn ingest_wire_for_tenant(
            &self,
            _records: Vec<crate::WireRecord>,
            _tenant: Option<u64>,
        ) -> Result<(), ShardClientError> {
            Err(ShardClientError::unavailable("test shard ingest unavailable"))
        }

        fn search_hits(
            &self,
            _request: &SearchJsonRequest,
        ) -> Result<Vec<(FoldedSpan, f32)>, ShardClientError> {
            Err(ShardClientError::unavailable("test shard search unavailable"))
        }

        fn replication_status(&self) -> crate::ReplicationStatus {
            empty_replication_status()
        }
    }

    struct FailingTraceStorage {
        coord: Arc<WriteCoordinator>,
        shards: Vec<ShardBackend>,
    }

    impl FailingTraceStorage {
        fn new(shard_count: usize) -> Self {
            let coord = WriteCoordinator::new(Arc::new(InMemorySegmentStore::default()));
            let shards = (0..shard_count)
                .map(|idx| ShardBackend {
                    id: ShardId::new(format!("failing-shard-{idx}")),
                    coord: Arc::clone(&coord),
                    client: Arc::new(FailingShardClient),
                    replicas: Vec::new(),
                })
                .collect();
            Self { coord, shards }
        }
    }

    impl TraceStorage for FailingTraceStorage {
        fn mode(&self) -> StorageMode {
            StorageMode::InProcessCluster
        }

        fn primary_coord(&self) -> &Arc<WriteCoordinator> {
            &self.coord
        }

        fn shards(&self) -> &[ShardBackend] {
            &self.shards
        }

        fn ingest_records_for_tenant(&self, _recs: Vec<crate::WireRecord>, _tenant: Option<u64>) {}

        fn shard_index_for_record(&self, _tenant: Option<u64>, _rec: &crate::WireRecord) -> usize {
            0
        }

        fn remember_owner(
            &self,
            _tenant: Option<u64>,
            _trace_id: u64,
            _session_id: Option<u64>,
            _idx: usize,
        ) {
        }

        fn trace_owner_index(&self, _tenant: Option<u64>, _trace_id: u64) -> Option<usize> {
            None
        }

        fn session_owner_index(&self, _tenant: Option<u64>, _session_id: u64) -> Option<usize> {
            None
        }

        fn trace_detail_owner_index(&self, _tenant: Option<u64>, _trace_id: u64) -> Option<usize> {
            None
        }

        fn session_detail_owner_index(
            &self,
            _tenant: Option<u64>,
            _session_id: u64,
        ) -> Option<usize> {
            None
        }
    }

    #[test]
    fn cluster_search_reports_all_shard_client_failures() {
        let api = EngineJsonApi {
            storage: Arc::new(FailingTraceStorage::new(2)),
            snapshot_leases: Arc::new(super::snapshot_helpers::SnapshotLeaseBook::default()),
            read_model_cache: Arc::new(std::sync::Mutex::new(super::ReadModelCache::default())),
        };
        let (status, body) = api.route(
            "POST",
            "/v1/search",
            r#"{"text":"盗刷","k":10,"includeFanout":true}"#,
        );
        assert_eq!(status, 503, "{body}");
        assert!(body.contains(r#""error":"all shards unavailable""#), "{body}");
        assert!(body.contains(r#""queryMode":"fanout_merge""#), "{body}");
        assert!(body.contains(r#""shardCount":2"#), "{body}");
        assert!(body.contains(r#""okShards":0"#), "{body}");
        assert!(body.contains(r#""degraded":true"#), "{body}");
        assert!(body.contains(r#""shardId":"failing-shard-0""#), "{body}");
        assert!(body.contains(r#""shardId":"failing-shard-1""#), "{body}");
        assert!(body.contains(r#""status":503"#), "{body}");
    }

    #[test]
    fn route_metrics_reports_prometheus_format() {
        // §3.1：/v1/metrics 输出 Prometheus 文本格式，含关键运行态指标。
        let s = server();
        // 灌点数据，让 memtable_rows > 0、committed_tail 推进。
        s.route("POST", "/v1/ingest", BATCH);
        let (status, body) = s.route("GET", "/v1/metrics", "");
        assert_eq!(status, 200);
        // Prometheus 格式特征：有 # HELP / # TYPE 注释、metric 行。
        assert!(body.contains("# HELP "), "应有 HELP 注释:\n{body}");
        assert!(body.contains("# TYPE "), "应有 TYPE 注释:\n{body}");
        // 关键指标都在。
        assert!(
            body.contains("yt_manifest_version"),
            "缺 manifest 版本:\n{body}"
        );
        assert!(body.contains("yt_memtable_rows"), "缺内存表行数:\n{body}");
        assert!(body.contains("yt_wal_committed_tail"), "缺 WAL 尾:\n{body}");
        assert!(body.contains("yt_segments_live"), "缺活跃段数:\n{body}");
        assert!(body.contains("yt_readers_active"), "缺活跃读者:\n{body}");
        // 灌过数据 → committed_tail > 0。
        assert!(
            body.lines()
                .any(|l| l.starts_with("yt_wal_committed_tail ") && !l.ends_with(" 0")),
            "灌数据后 committed_tail 应 > 0:\n{body}"
        );
    }
    #[test]
    fn annotations_and_dataset_associations_are_tenant_isolated_and_durable() {
        let dir = durable_temp_dir("metadata");
        {
            let coord = WriteCoordinator::open_durable(&dir).unwrap();
            coord.recover();
            let api = EngineJsonApi::new(coord);
            let trace = r#"[{
              "trace_id":"run-uuid",
              "span_id":"span-uuid",
              "ts":100,
              "seq":1,
              "event_type":2,
              "ext_span_id":"span-uuid",
              "session_id":"session-uuid",
              "status":0,
              "duration_ns":42,
              "agent_name":"builder-agent",
              "input_text":"builder 输入",
              "output_text":"builder 输出",
              "attrs":{"project_id":"agentic-data","skill":"review"}
            }]"#;
            let (status, body) = api.route_with_tenant("POST", "/v1/ingest", trace, Some(1));
            assert_eq!(status, 200, "{body}");

            let annotation = r#"{
              "traceId":"run-uuid",
              "spanId":"span-uuid",
              "target":"span",
              "label":"best_path",
              "score":920,
              "reason":"人工确认这次路径最短",
              "source":"human",
              "projectId":"agentic-data",
              "skill":"review"
            }"#;
            let (status, body) =
                api.route_with_tenant("POST", "/v1/annotations", annotation, Some(1));
            assert_eq!(status, 200, "{body}");
            assert!(body.contains(r#""externalTraceId":"run-uuid""#), "{body}");
            assert!(body.contains(r#""externalSpanId":"span-uuid""#), "{body}");

            let link = r#"{
              "datasetId":"best-path-regression",
              "itemId":"case-1",
              "traceId":"run-uuid",
              "spanId":"span-uuid",
              "snapshotId":"snap-1",
              "snapshotHash":"fnv1a64:abc",
              "evalRunId":"eval-1",
              "split":"train",
              "label":"pass",
              "score":920,
              "projectId":"agentic-data",
              "skill":"review"
            }"#;
            let (status, body) =
                api.route_with_tenant("POST", "/v1/dataset-associations", link, Some(1));
            assert_eq!(status, 200, "{body}");
            assert!(
                body.contains(r#""datasetId":"best-path-regression""#),
                "{body}"
            );

            let other =
                r#"{"traceId":"run-uuid","label":"wrong_tenant","projectId":"agentic-data"}"#;
            assert_eq!(
                api.route_with_tenant("POST", "/v1/annotations", other, Some(2))
                    .0,
                200
            );
            let second_annotation =
                r#"{"traceId":"run-uuid","label":"needs_review","projectId":"agentic-data"}"#;
            assert_eq!(
                api.route_with_tenant("POST", "/v1/annotations", second_annotation, Some(1))
                    .0,
                200
            );
            let third_annotation =
                r#"{"traceId":"run-uuid","label":"bad_answer","projectId":"agentic-data"}"#;
            assert_eq!(
                api.route_with_tenant("POST", "/v1/annotations", third_annotation, Some(1))
                    .0,
                200
            );

            let second_link = r#"{
              "datasetId":"best-path-regression",
              "itemId":"case-2",
              "traceId":"run-uuid",
              "spanId":"span-uuid",
              "label":"review",
              "projectId":"agentic-data"
            }"#;
            assert_eq!(
                api.route_with_tenant("POST", "/v1/dataset-associations", second_link, Some(1))
                    .0,
                200
            );
            let third_link = r#"{
              "datasetId":"best-path-regression",
              "itemId":"case-3",
              "traceId":"run-uuid",
              "spanId":"span-uuid",
              "label":"fail",
              "projectId":"agentic-data"
            }"#;
            assert_eq!(
                api.route_with_tenant("POST", "/v1/dataset-associations", third_link, Some(1))
                    .0,
                200
            );

            let (status, body) = api.route_with_tenant(
                "GET",
                "/v1/annotations?traceId=run-uuid&projectId=agentic-data",
                "",
                Some(1),
            );
            assert_eq!(status, 200, "{body}");
            assert!(body.contains(r#""count":3"#), "{body}");
            assert!(body.contains(r#""label":"best_path""#), "{body}");
            assert!(!body.contains("wrong_tenant"), "{body}");

            let (status, body) = api.route_with_tenant(
                "GET",
                "/v1/annotations?traceId=run-uuid&projectId=agentic-data&limit=2",
                "",
                Some(1),
            );
            assert_eq!(status, 200, "{body}");
            assert!(body.contains(r#""count":3"#), "{body}");
            assert!(body.contains(r#""pageCount":2"#), "{body}");
            assert!(body.contains(r#""nextCursor":2"#), "{body}");
            assert!(body.contains(r#""label":"bad_answer""#), "{body}");
            assert!(body.contains(r#""label":"needs_review""#), "{body}");
            assert!(!body.contains(r#""label":"best_path""#), "{body}");

            let (status, body) = api.route_with_tenant(
                "GET",
                "/v1/annotations?traceId=run-uuid&projectId=agentic-data&cursor=2&limit=2",
                "",
                Some(1),
            );
            assert_eq!(status, 200, "{body}");
            assert!(body.contains(r#""pageCount":1"#), "{body}");
            assert!(body.contains(r#""nextCursor":null"#), "{body}");
            assert!(body.contains(r#""label":"best_path""#), "{body}");

            let (status, body) = api.route_with_tenant(
                "GET",
                "/v1/dataset-associations?datasetId=best-path-regression&limit=2",
                "",
                Some(1),
            );
            assert_eq!(status, 200, "{body}");
            assert!(body.contains(r#""count":3"#), "{body}");
            assert!(body.contains(r#""pageCount":2"#), "{body}");
            assert!(body.contains(r#""nextCursor":2"#), "{body}");
            assert!(body.contains(r#""itemId":"case-3""#), "{body}");
            assert!(body.contains(r#""itemId":"case-2""#), "{body}");
            assert!(!body.contains(r#""itemId":"case-1""#), "{body}");

            let (status, body) = api.route_with_tenant(
                "POST",
                "/v1/trace-search",
                r#"{"filter":{"annotation":{"label":"best_path","source":"human","scoreMin":900}}}"#,
                Some(1),
            );
            assert_eq!(status, 200, "{body}");
            assert!(body.contains(r#""total":1"#), "{body}");
            assert!(body.contains(r#""externalSpanId":"span-uuid""#), "{body}");

            let (status, body) = api.route_with_tenant(
                "POST",
                "/v1/trace-search",
                r#"{"filter":{"dataset":{"datasetId":"best-path-regression","itemId":"case-1","evalRunId":"eval-1","scoreMin":900}}}"#,
                Some(1),
            );
            assert_eq!(status, 200, "{body}");
            assert!(body.contains(r#""total":1"#), "{body}");

            let (status, body) = api.route_with_tenant(
                "GET",
                "/v1/traces?annotationLabel=best_path&annotationScoreMin=900",
                "",
                Some(1),
            );
            assert_eq!(status, 200, "{body}");
            assert!(body.contains(r#""external_trace_id":"run-uuid""#), "{body}");

            let (status, body) = api.route_with_tenant(
                "GET",
                "/v1/sessions?datasetId=best-path-regression&datasetLabel=pass",
                "",
                Some(1),
            );
            assert_eq!(status, 200, "{body}");
            assert!(
                body.contains(r#""externalSessionId":"session-uuid""#),
                "{body}"
            );

            let (status, body) = api.route_with_tenant(
                "POST",
                "/v1/trace-search",
                r#"{"filter":{"annotationLabel":"missing"}}"#,
                Some(1),
            );
            assert_eq!(status, 200, "{body}");
            assert!(body.contains(r#""total":0"#), "{body}");

            let (status, body) = api.route_with_tenant(
                "PATCH",
                "/v1/annotations/1",
                r#"{"status":"resolved","reviewer":"four","reason":"review accepted","attrs":{"review_round":1}}"#,
                Some(1),
            );
            assert_eq!(status, 200, "{body}");
            assert!(body.contains(r#""status":"resolved""#), "{body}");
            assert!(body.contains(r#""reviewer":"four""#), "{body}");
            assert!(body.contains(r#""review_round":1"#), "{body}");
            assert!(body.contains(r#""updatedAtNs":"#), "{body}");

            let (status, body) = api.route_with_tenant(
                "GET",
                "/v1/annotations?traceId=run-uuid&label=best_path&status=resolved",
                "",
                Some(1),
            );
            assert_eq!(status, 200, "{body}");
            assert!(body.contains(r#""count":1"#), "{body}");
            assert!(body.contains(r#""reviewer":"four""#), "{body}");

            let (status, body) = api.route_with_tenant(
                "PATCH",
                "/v1/annotations/1",
                r#"{"status":"rejected"}"#,
                Some(2),
            );
            assert_eq!(status, 404, "{body}");

            let (status, body) = api.route_with_tenant(
                "DELETE",
                "/v1/annotations/1",
                r#"{"reviewer":"four","reason":"superseded"}"#,
                Some(1),
            );
            assert_eq!(status, 200, "{body}");
            assert!(body.contains(r#""status":"deleted""#), "{body}");
            assert!(body.contains(r#""reason":"superseded""#), "{body}");

            let (status, body) = api.route_with_tenant(
                "GET",
                "/v1/annotations?traceId=run-uuid&label=best_path",
                "",
                Some(1),
            );
            assert_eq!(status, 200, "{body}");
            assert!(body.contains(r#""count":0"#), "{body}");

            let (status, body) = api.route_with_tenant(
                "GET",
                "/v1/annotations?traceId=run-uuid&label=best_path&status=deleted",
                "",
                Some(1),
            );
            assert_eq!(status, 200, "{body}");
            assert!(body.contains(r#""count":1"#), "{body}");
            assert!(body.contains(r#""status":"deleted""#), "{body}");

            let (status, body) = api.route_with_tenant(
                "POST",
                "/v1/trace-search",
                r#"{"filter":{"annotation":{"label":"best_path"}}}"#,
                Some(1),
            );
            assert_eq!(status, 200, "{body}");
            assert!(body.contains(r#""total":0"#), "{body}");

            let (status, body) = api.route_with_tenant(
                "POST",
                "/v1/trace-search",
                r#"{"filter":{"annotation":{"label":"best_path","status":"deleted"}}}"#,
                Some(1),
            );
            assert_eq!(status, 200, "{body}");
            assert!(body.contains(r#""total":1"#), "{body}");
        }
        {
            let coord = WriteCoordinator::open_durable(&dir).unwrap();
            coord.recover();
            let api = EngineJsonApi::new(coord);
            let (status, body) = api.route_with_tenant(
                "GET",
                "/v1/annotations?traceId=run-uuid&label=best_path",
                "",
                Some(1),
            );
            assert_eq!(status, 200, "{body}");
            assert!(body.contains(r#""count":0"#), "{body}");

            let (status, body) = api.route_with_tenant(
                "GET",
                "/v1/annotations?traceId=run-uuid&label=best_path&status=deleted",
                "",
                Some(1),
            );
            assert_eq!(status, 200, "{body}");
            assert!(body.contains(r#""count":1"#), "{body}");
            assert!(body.contains(r#""status":"deleted""#), "{body}");
            assert!(body.contains(r#""reviewer":"four""#), "{body}");

            let (status, body) = api.route_with_tenant(
                "GET",
                "/v1/dataset-associations?datasetId=best-path-regression&itemId=case-1",
                "",
                Some(1),
            );
            assert_eq!(status, 200, "{body}");
            assert!(body.contains(r#""count":1"#), "{body}");
            assert!(body.contains(r#""snapshotHash":"fnv1a64:abc""#), "{body}");
            assert!(body.contains(r#""project_id":"agentic-data""#), "{body}");

            let (status, body) = api.route_with_tenant(
                "POST",
                "/v1/trace-search",
                r#"{"filter":{"datasetId":"best-path-regression","datasetLabel":"pass"}}"#,
                Some(1),
            );
            assert_eq!(status, 200, "{body}");
            assert!(body.contains(r#""total":1"#), "{body}");
        }
        let _ = std::fs::remove_dir_all(dir);
    }
    #[test]
    fn storage_stats_and_retention_plan_protect_metadata_before_apply() {
        let dir = durable_temp_dir("storage-retention");
        {
            let coord = WriteCoordinator::open_durable(&dir).unwrap();
            coord.recover();
            let api = EngineJsonApi::new(coord);
            let batch = r#"[
              {"trace_id":101,"span_id":1,"ts":10,"seq":1,"event_type":1,"ext_span_id":"101-1","input_text":"old delete","attrs":{"project_id":"retention-demo","task_fingerprint":"case-a"}},
              {"trace_id":101,"span_id":1,"ts":20,"seq":2,"event_type":2,"ext_span_id":"101-1","duration_ns":10,"output_text":"done"},
              {"trace_id":102,"span_id":1,"ts":30,"seq":1,"event_type":1,"ext_span_id":"102-1","input_text":"old protected","attrs":{"project_id":"retention-demo","task_fingerprint":"case-a"}},
              {"trace_id":102,"span_id":1,"ts":40,"seq":2,"event_type":2,"ext_span_id":"102-1","duration_ns":10,"output_text":"done"},
              {"trace_id":103,"span_id":1,"ts":200,"seq":1,"event_type":1,"ext_span_id":"103-1","input_text":"new keep","attrs":{"project_id":"retention-demo","task_fingerprint":"case-a"}},
              {"trace_id":103,"span_id":1,"ts":220,"seq":2,"event_type":2,"ext_span_id":"103-1","duration_ns":10,"output_text":"done"}
            ]"#;
            let (status, body) = api.route_with_tenant("POST", "/v1/ingest", batch, Some(1));
            assert_eq!(status, 200, "{body}");
            api.coord().flush_memtable();

            let (status, body) = api.route_with_tenant(
                "POST",
                "/v1/annotations",
                r#"{"traceId":102,"target":"trace","label":"manual_keep"}"#,
                Some(1),
            );
            assert_eq!(status, 200, "{body}");

            let query = r#"{"filter":{"projectId":"retention-demo"},"groupBy":["projectId"]}"#;
            let (status, stats) =
                api.route_with_tenant("POST", "/v1/storage-stats", query, Some(1));
            assert_eq!(status, 200, "{stats}");
            assert!(stats.contains(r#""traceCount":3"#), "{stats}");
            assert!(stats.contains(r#""groupBy":["project_id"]"#), "{stats}");
            assert!(
                stats.contains(r#""project_id":"retention-demo""#),
                "{stats}"
            );
            assert!(stats.contains(r#""annotations":1"#), "{stats}");

            let retention_query =
                r#"{"filter":{"projectId":"retention-demo"},"deleteBeforeTs":100}"#;
            let (status, plan) =
                api.route_with_tenant("POST", "/v1/retention-plan", retention_query, Some(1));
            assert_eq!(status, 200, "{plan}");
            assert!(plan.contains(r#""dryRun":true"#), "{plan}");
            assert!(plan.contains(r#""candidates":{"traceCount":2"#), "{plan}");
            assert!(plan.contains(r#""protected":{"traceCount":1"#), "{plan}");
            assert!(plan.contains(r#""deletable":{"traceCount":1"#), "{plan}");
            assert!(plan.contains(r#""102":["annotation"]"#), "{plan}");

            let retention_apply_query = r#"{"filter":{"projectId":"retention-demo"},"deleteBeforeTs":100,"compact":true,"requestedBy":"test-policy","reason":"ttl cleanup"}"#;
            let (status, applied) = api.route_with_tenant(
                "POST",
                "/v1/retention/apply",
                retention_apply_query,
                Some(1),
            );
            assert_eq!(status, 200, "{applied}");
            assert!(applied.contains(r#""applied":true"#), "{applied}");
            assert!(applied.contains(r#""deletedTraceCount":1"#), "{applied}");
            assert!(
                applied.contains(r#""deletedTraceIds":["101"]"#),
                "{applied}"
            );
            assert!(
                applied.contains(r#""compactResult":{"beforeLiveSegmentCount":1"#),
                "{applied}"
            );
            assert!(
                applied.contains(r#""compactedSegmentCount":1"#),
                "{applied}"
            );
            assert!(
                applied.contains(r#""droppedDeletedRowCount":2"#),
                "{applied}"
            );
            assert!(
                applied.contains(r#""rewrittenLiveRowCount":4"#),
                "{applied}"
            );
            assert!(applied.contains(r#""audit":{"auditId":"1""#), "{applied}");
            assert!(applied.contains(r#""source":"test-policy""#), "{applied}");
            assert!(applied.contains(r#""reason":"ttl cleanup""#), "{applied}");
            assert!(
                applied.contains(r#""traceIds":{"deletable":["101"],"deleted":["101"]"#),
                "{applied}"
            );

            let (status, after) = api.route_with_tenant("POST", "/v1/trace-search", query, Some(1));
            assert_eq!(status, 200, "{after}");
            assert!(after.contains(r#""total":2"#), "{after}");
            assert!(!after.contains(r#""traceId":"101""#), "{after}");
            assert!(after.contains(r#""traceId":"102""#), "{after}");
            assert!(after.contains(r#""traceId":"103""#), "{after}");

            let (status, audits) = api.route_with_tenant(
                "GET",
                "/v1/retention-audits?source=test-policy",
                "",
                Some(1),
            );
            assert_eq!(status, 200, "{audits}");
            assert!(audits.contains(r#""total":1"#), "{audits}");
            assert!(audits.contains(r#""deletedTraceCount":1"#), "{audits}");
            assert!(audits.contains(r#""sampleTruncated":false"#), "{audits}");

            let (status, audits) = api.route_with_tenant(
                "POST",
                "/v1/retention-audits",
                r#"{"filter":{"source":"test-policy"},"limit":10}"#,
                Some(1),
            );
            assert_eq!(status, 200, "{audits}");
            assert!(audits.contains(r#""auditId":"1""#), "{audits}");

            let (status, audits) = api.route_with_tenant(
                "GET",
                "/v1/retention-audits?source=test-policy",
                "",
                Some(2),
            );
            assert_eq!(status, 200, "{audits}");
            assert!(audits.contains(r#""total":0"#), "{audits}");
        }
        {
            let coord = WriteCoordinator::open_durable(&dir).unwrap();
            coord.recover();
            let api = EngineJsonApi::new(coord);
            let (status, audits) = api.route_with_tenant(
                "GET",
                "/v1/retention-audits?source=test-policy",
                "",
                Some(1),
            );
            assert_eq!(status, 200, "{audits}");
            assert!(audits.contains(r#""total":1"#), "{audits}");
            assert!(audits.contains(r#""auditId":"1""#), "{audits}");
            assert!(audits.contains(r#""deletedTraceCount":1"#), "{audits}");
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn storage_stats_read_model_cache_hits_and_invalidates_on_ingest() {
        let dir = durable_temp_dir("storage-stats-cache");
        let coord = WriteCoordinator::open_durable(&dir).unwrap();
        coord.recover();
        let api = EngineJsonApi::new(coord);

        let first_batch = r#"[
          {"trace_id":701,"span_id":1,"ts":10,"seq":1,"event_type":1,"ext_span_id":"701-1","input_text":"cache first","attrs":{"project_id":"cache-demo"}},
          {"trace_id":701,"span_id":1,"ts":20,"seq":2,"event_type":2,"ext_span_id":"701-1","duration_ns":10,"output_text":"done"}
        ]"#;
        let (status, body) = api.route_with_tenant("POST", "/v1/ingest", first_batch, Some(7));
        assert_eq!(status, 200, "{body}");
        api.coord().flush_memtable();

        let query = r#"{"filter":{"projectId":"cache-demo"},"groupBy":["projectId"]}"#;
        let (status, first) =
            api.route_with_tenant("POST", "/v1/storage-stats", query, Some(7));
        assert_eq!(status, 200, "{first}");
        assert!(first.contains(r#""traceCount":1"#), "{first}");
        assert!(
            first.contains(r#""spanReadIndex":"storage_preaggregate""#),
            "{first}"
        );
        assert!(
            first.contains(r#""storagePreaggregateProfile":["project_id"]"#),
            "{first}"
        );
        assert!(first.contains(r#""readModelCache":"miss""#), "{first}");

        let (status, second) =
            api.route_with_tenant("POST", "/v1/storage-stats", query, Some(7));
        assert_eq!(status, 200, "{second}");
        assert!(second.contains(r#""traceCount":1"#), "{second}");
        assert!(second.contains(r#""readModelCache":"hit""#), "{second}");

        let identity_query =
            r#"{"filter":{"projectId":"cache-demo","traceId":701},"groupBy":["projectId"]}"#;
        let (status, identity) =
            api.route_with_tenant("POST", "/v1/storage-stats", identity_query, Some(7));
        assert_eq!(status, 200, "{identity}");
        assert!(
            identity.contains(r#""spanReadIndex":"folded_scan""#),
            "{identity}"
        );
        assert!(
            identity.contains(r#""rollupFallbackReason":"rollup_blocked""#),
            "{identity}"
        );

        let row_rollup_query = r#"{"filter":{"projectId":"cache-demo"},"groupBy":["callSite"]}"#;
        let (status, row_rollup) =
            api.route_with_tenant("POST", "/v1/storage-stats", row_rollup_query, Some(7));
        assert_eq!(status, 200, "{row_rollup}");
        assert!(
            row_rollup.contains(r#""spanReadIndex":"storage_segment_rollup""#),
            "{row_rollup}"
        );

        let second_batch = r#"[
          {"trace_id":702,"span_id":1,"ts":30,"seq":1,"event_type":1,"ext_span_id":"702-1","input_text":"cache second","attrs":{"project_id":"cache-demo"}},
          {"trace_id":702,"span_id":1,"ts":40,"seq":2,"event_type":2,"ext_span_id":"702-1","duration_ns":10,"output_text":"done"}
        ]"#;
        let (status, body) = api.route_with_tenant("POST", "/v1/ingest", second_batch, Some(7));
        assert_eq!(status, 200, "{body}");

        let (status, after_ingest) =
            api.route_with_tenant("POST", "/v1/storage-stats", query, Some(7));
        assert_eq!(status, 200, "{after_ingest}");
        assert!(after_ingest.contains(r#""traceCount":2"#), "{after_ingest}");
        assert!(
            after_ingest.contains(r#""readModelCache":"miss""#),
            "{after_ingest}"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn storage_stats_preaggregate_matches_folded_scan_for_basic_totals() {
        let dir = durable_temp_dir("storage-stats-rollup-parity");
        let coord = WriteCoordinator::open_durable(&dir).unwrap();
        coord.recover();
        let api = EngineJsonApi::new(coord);

        let batch = r#"[
          {"trace_id":901,"span_id":1,"session_id":91,"ts":10,"seq":1,"event_type":1,"ext_span_id":"901-1","input_text":"risk input","attrs":{"project_id":"rollup-parity","skill":"review"}},
          {"trace_id":901,"span_id":1,"session_id":91,"ts":20,"seq":2,"event_type":2,"ext_span_id":"901-1","duration_ns":10,"status":0,"output_text":"risk output","logs":["first done"]},
          {"trace_id":901,"span_id":2,"session_id":91,"ts":30,"seq":1,"event_type":1,"ext_span_id":"901-2","tool_name":"validator","input_text":"tool input","attrs":{"project_id":"rollup-parity","skill":"review"}},
          {"trace_id":901,"span_id":2,"session_id":91,"ts":40,"seq":2,"event_type":2,"ext_span_id":"901-2","duration_ns":20,"status":2,"output_text":"tool output","logs":["tool failed"]}
        ]"#;
        let (status, body) = api.route_with_tenant("POST", "/v1/ingest", batch, Some(9));
        assert_eq!(status, 200, "{body}");
        api.coord().flush_memtable();

        let fast_query = r#"{"filter":{"projectId":"rollup-parity"},"groupBy":["projectId"]}"#;
        let (status, fast) =
            api.route_with_tenant("POST", "/v1/storage-stats", fast_query, Some(9));
        assert_eq!(status, 200, "{fast}");
        assert!(
            fast.contains(r#""spanReadIndex":"storage_preaggregate""#),
            "{fast}"
        );

        let fallback_query =
            r#"{"filter":{"projectId":"rollup-parity","traceId":901},"groupBy":["projectId"]}"#;
        let (status, fallback) =
            api.route_with_tenant("POST", "/v1/storage-stats", fallback_query, Some(9));
        assert_eq!(status, 200, "{fallback}");
        assert!(
            fallback.contains(r#""spanReadIndex":"folded_scan""#),
            "{fallback}"
        );

        for path in [
            &["total", "traceCount"][..],
            &["total", "spanCount"],
            &["total", "sessionCount"],
            &["total", "eventCount"],
            &["total", "errorSpanCount"],
            &["total", "bytes", "inputText"],
            &["total", "bytes", "outputText"],
            &["total", "bytes", "logs"],
            &["total", "bytes", "attrs"],
            &["total", "bytes", "fields"],
            &["total", "bytes", "estimated"],
        ] {
            assert_eq!(
                test_json_u64(&fast, path),
                test_json_u64(&fallback, path),
                "path {path:?}"
            );
        }
        assert_eq!(test_json_u64(&fast, &["total", "traceCount"]), 1);
        assert_eq!(test_json_u64(&fast, &["total", "spanCount"]), 2);
        assert_eq!(test_json_u64(&fast, &["total", "eventCount"]), 4);
        assert_eq!(test_json_u64(&fast, &["total", "errorSpanCount"]), 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn trace_search_read_model_cache_hits_and_invalidates_on_ingest() {
        let dir = durable_temp_dir("trace-search-cache");
        let coord = WriteCoordinator::open_durable(&dir).unwrap();
        coord.recover();
        let api = EngineJsonApi::new(coord);

        let first_batch = r#"[
          {"trace_id":801,"span_id":1,"ts":10,"seq":1,"event_type":1,"ext_span_id":"801-1","input_text":"trace cache first","attrs":{"project_id":"trace-cache"}},
          {"trace_id":801,"span_id":1,"ts":20,"seq":2,"event_type":2,"ext_span_id":"801-1","duration_ns":10,"output_text":"done"}
        ]"#;
        let (status, body) = api.route_with_tenant("POST", "/v1/ingest", first_batch, Some(8));
        assert_eq!(status, 200, "{body}");
        api.coord().flush_memtable();

        let query = r#"{"filter":{"projectId":"trace-cache"},"limit":10}"#;
        let (status, first) = api.route_with_tenant("POST", "/v1/trace-search", query, Some(8));
        assert_eq!(status, 200, "{first}");
        assert!(first.contains(r#""total":1"#), "{first}");
        assert!(first.contains(r#""readModelCache":"miss""#), "{first}");

        let (status, second) = api.route_with_tenant("POST", "/v1/trace-search", query, Some(8));
        assert_eq!(status, 200, "{second}");
        assert!(second.contains(r#""total":1"#), "{second}");
        assert!(second.contains(r#""readModelCache":"hit""#), "{second}");

        let second_batch = r#"[
          {"trace_id":802,"span_id":1,"ts":30,"seq":1,"event_type":1,"ext_span_id":"802-1","input_text":"trace cache second","attrs":{"project_id":"trace-cache"}},
          {"trace_id":802,"span_id":1,"ts":40,"seq":2,"event_type":2,"ext_span_id":"802-1","duration_ns":10,"output_text":"done"}
        ]"#;
        let (status, body) = api.route_with_tenant("POST", "/v1/ingest", second_batch, Some(8));
        assert_eq!(status, 200, "{body}");

        let (status, after_ingest) =
            api.route_with_tenant("POST", "/v1/trace-search", query, Some(8));
        assert_eq!(status, 200, "{after_ingest}");
        assert!(after_ingest.contains(r#""total":2"#), "{after_ingest}");
        assert!(
            after_ingest.contains(r#""readModelCache":"miss""#),
            "{after_ingest}"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn retention_plan_protects_snapshot_eval_and_path_memory_refs() {
        let dir = durable_temp_dir("retention-derived-protect");
        {
            let coord = WriteCoordinator::open_durable(&dir).unwrap();
            coord.recover();
            let api = EngineJsonApi::new(coord);
            let batch = r#"[
              {"trace_id":201,"span_id":1,"ts":10,"seq":1,"event_type":1,"ext_span_id":"201-1","input_text":"old delete a","attrs":{"project_id":"retention-derived"}},
              {"trace_id":201,"span_id":1,"ts":20,"seq":2,"event_type":2,"ext_span_id":"201-1","duration_ns":10,"output_text":"done"},
              {"trace_id":202,"span_id":1,"ts":10,"seq":1,"event_type":1,"ext_span_id":"202-1","input_text":"old delete b","attrs":{"project_id":"retention-derived"}},
              {"trace_id":202,"span_id":1,"ts":20,"seq":2,"event_type":2,"ext_span_id":"202-1","duration_ns":10,"output_text":"done"},
              {"trace_id":203,"span_id":1,"ts":10,"seq":1,"event_type":1,"ext_span_id":"203-1","input_text":"snapshot keep","attrs":{"project_id":"retention-derived"}},
              {"trace_id":203,"span_id":1,"ts":20,"seq":2,"event_type":2,"ext_span_id":"203-1","duration_ns":10,"output_text":"done"},
              {"trace_id":204,"span_id":1,"ts":10,"seq":1,"event_type":1,"ext_span_id":"204-1","input_text":"eval keep","attrs":{"project_id":"retention-derived"}},
              {"trace_id":204,"span_id":1,"ts":20,"seq":2,"event_type":2,"ext_span_id":"204-1","duration_ns":10,"output_text":"done"},
              {"trace_id":205,"span_id":1,"ts":10,"seq":1,"event_type":1,"ext_span_id":"205-1","input_text":"path memory keep","attrs":{"project_id":"retention-derived"}},
              {"trace_id":205,"span_id":1,"ts":20,"seq":2,"event_type":2,"ext_span_id":"205-1","duration_ns":10,"output_text":"done"}
            ]"#;
            let (status, body) = api.route_with_tenant("POST", "/v1/ingest", batch, Some(1));
            assert_eq!(status, 200, "{body}");
            api.coord().flush_memtable();

            let (status, body) = api.route_with_tenant(
                "POST",
                "/v1/dataset-associations",
                r#"{"datasetId":"snapshots","itemId":"snap-203","traceId":203,"snapshotId":"snap-203","snapshotHash":"fnv1a64:203"}"#,
                Some(1),
            );
            assert_eq!(status, 200, "{body}");
            let (status, body) = api.route_with_tenant(
                "POST",
                "/v1/dataset-associations",
                r#"{"datasetId":"eval-regression","itemId":"eval-204","traceId":204,"evalRunId":"eval-run-1"}"#,
                Some(1),
            );
            assert_eq!(status, 200, "{body}");
            let (status, body) = api.route_with_tenant(
                "POST",
                "/v1/annotations",
                r#"{"traceId":205,"target":"trace","label":"path_memory","pathMemoryId":"pm-1"}"#,
                Some(1),
            );
            assert_eq!(status, 200, "{body}");

            let query = r#"{"filter":{"projectId":"retention-derived"},"deleteBeforeTs":100,"protect":{"annotations":false,"datasetAssociations":false,"goldenPaths":false}}"#;
            let (status, plan) =
                api.route_with_tenant("POST", "/v1/retention-plan", query, Some(1));
            assert_eq!(status, 200, "{plan}");
            assert!(plan.contains(r#""candidates":{"traceCount":5"#), "{plan}");
            assert!(plan.contains(r#""protected":{"traceCount":3"#), "{plan}");
            assert!(plan.contains(r#""deletable":{"traceCount":2"#), "{plan}");
            assert!(plan.contains(r#""snapshots":true"#), "{plan}");
            assert!(plan.contains(r#""evalLinks":true"#), "{plan}");
            assert!(plan.contains(r#""pathMemory":true"#), "{plan}");
            assert!(plan.contains(r#""snapshotRefs":1"#), "{plan}");
            assert!(plan.contains(r#""evalLinks":1"#), "{plan}");
            assert!(plan.contains(r#""pathMemoryRefs":1"#), "{plan}");
            assert!(plan.contains(r#""203":["snapshot"]"#), "{plan}");
            assert!(plan.contains(r#""204":["evalLink"]"#), "{plan}");
            assert!(plan.contains(r#""205":["pathMemory"]"#), "{plan}");
            assert!(
                plan.contains(r#""deletableTraceIds":["201","202"]"#),
                "{plan}"
            );

            let apply_query = r#"{"filter":{"projectId":"retention-derived"},"deleteBeforeTs":100,"protect":{"annotations":false,"datasetAssociations":false,"goldenPaths":false},"requestedBy":"derived-protect-test"}"#;
            let (status, applied) =
                api.route_with_tenant("POST", "/v1/retention/apply", apply_query, Some(1));
            assert_eq!(status, 200, "{applied}");
            assert!(applied.contains(r#""deletedTraceCount":2"#), "{applied}");
            assert!(
                applied.contains(r#""deletedTraceIds":["201","202"]"#),
                "{applied}"
            );
            assert!(applied.contains(r#""snapshots":true"#), "{applied}");
            assert!(applied.contains(r#""evalLinks":true"#), "{applied}");
            assert!(applied.contains(r#""pathMemory":true"#), "{applied}");

            let (status, remaining) = api.route_with_tenant(
                "POST",
                "/v1/trace-search",
                r#"{"filter":{"projectId":"retention-derived"}}"#,
                Some(1),
            );
            assert_eq!(status, 200, "{remaining}");
            assert!(remaining.contains(r#""total":3"#), "{remaining}");
        }
        let _ = std::fs::remove_dir_all(dir);
    }
