struct FanoutReport {
    shard_count: usize,
    ok_shards: usize,
    failed_shards: Vec<FanoutShardFailure>,
}

impl FanoutReport {
    fn all_ok(shard_count: usize) -> Self {
        Self {
            shard_count,
            ok_shards: shard_count,
            failed_shards: Vec::new(),
        }
    }

    fn from_parts(
        shard_count: usize,
        ok_shards: usize,
        failed_shards: Vec<FanoutShardFailure>,
    ) -> Self {
        Self {
            shard_count,
            ok_shards,
            failed_shards,
        }
    }

    fn degraded(&self) -> bool {
        !self.failed_shards.is_empty()
    }

    fn json_fields(&self) -> String {
        let failures = self
            .failed_shards
            .iter()
            .map(FanoutShardFailure::json)
            .collect::<Vec<_>>()
            .join(",");
        format!(
            r#","shardCount":{},"okShards":{},"degraded":{},"failedShards":[{}]"#,
            self.shard_count,
            self.ok_shards,
            self.degraded(),
            failures
        )
    }
}

struct FanoutShardFailure {
    shard_id: String,
    status: u16,
    error: String,
}

impl FanoutShardFailure {
    fn json(&self) -> String {
        format!(
            r#"{{"shardId":"{}","status":{},"error":"{}"}}"#,
            json_escape(&self.shard_id),
            self.status,
            json_escape(&self.error)
        )
    }
}
