impl WriteCoordinator {

    /// 生产可观测（§3.1）：聚合所有关键运行态，供 /metrics 端点输出。
    /// 返回的字符串是 Prometheus 文本格式（每行一个 metric + 注释），零依赖、好排查。
    /// 返回 owned String，调用者直接写进 HTTP body。
    pub fn metrics(&self) -> String {
        let mut out = String::with_capacity(2048);
        let version = self.current.version();
        let segments = self.current.manifest().segments.len();
        let memtable_rows = self.memtable_len();
        let dead = self.dead_count();
        let active_readers = self.current.active_reader_count();
        let committed_tail = self.current.committed_tail();
        let flush_threshold = self.flush_threshold.load(Ordering::Relaxed);
        let filter_attrs = self.filter_attrs.lock().unwrap().len();
        let (
            attr_posting_keys,
            attr_posting_entries,
            attr_posting_estimated_bytes,
            attr_posting_singleton_keys,
            attr_posting_small_vec_keys,
            attr_posting_hashset_keys,
            attr_posting_interned_keys,
            attr_posting_interned_values,
            attr_posting_incomplete_keys,
        ) = {
            let postings = self.attr_postings.lock().unwrap();
            let keys = postings.exact.len() + postings.array_items.len();
            let mut singleton_keys = 0usize;
            let mut small_vec_keys = 0usize;
            let mut hashset_keys = 0usize;
            for list in postings.exact.values().chain(postings.array_items.values()) {
                if list.is_singleton() {
                    singleton_keys += 1;
                } else if list.is_small() {
                    small_vec_keys += 1;
                } else if list.is_hashset() {
                    hashset_keys += 1;
                }
            }
            (
                keys,
                postings.indexed_entries,
                postings.estimated_bytes,
                singleton_keys,
                small_vec_keys,
                hashset_keys,
                postings.attr_keys.len(),
                postings.attr_values.len(),
                postings.incomplete_keys.len(),
            )
        };
        let (
            attr_sidecar_segments,
            attr_sidecar_exact_terms,
            attr_sidecar_array_terms,
            attr_sidecar_incomplete_keys,
        ) = self.seg_attr_directory.lock().unwrap().stats();
        let (
            attr_sidecar_cache_entries,
            attr_sidecar_cache_bytes,
            attr_sidecar_cache_byte_budget,
            attr_sidecar_cache_hits,
            attr_sidecar_cache_misses,
            attr_sidecar_cache_loads,
            attr_sidecar_cache_evictions,
        ) = self.seg_attr_cache.lock().unwrap().stats();
        let fold_cache_entries = self.seg_fold_cache.lock().unwrap().map.len();
        let bloom_count = self.seg_key_bloom.lock().unwrap().len();
        let trace_rollup_profile_stats = self.trace_aggregate_rollup_profile_stats();
        let datasets = self.datasets.lock().unwrap().len();
        let annotations = self.annotations.lock().unwrap().len();
        let dataset_associations = self.dataset_associations.lock().unwrap().len();
        let retention_audits = self.retention_audits.lock().unwrap().len();
        let retention_policies = self.retention_policies.lock().unwrap().len();

        // 确定性 manifest 版本（每次 commit +1）。
        out.push_str("# HELP yt_manifest_version Manifest 版本号（每次 commit +1）。\n");
        out.push_str("# TYPE yt_manifest_version gauge\n");
        out.push_str(&format!("yt_manifest_version {version}\n\n"));

        out.push_str(
            "# HELP yt_format_version 数据格式版本（persist::FORMAT_VER，升级迁移用）。\n",
        );
        out.push_str("# TYPE yt_format_version gauge\n");
        out.push_str(&format!("yt_format_version {}\n\n", persist::FORMAT_VER));

        out.push_str("# HELP yt_segments_live 活跃段数（含 sealed/live/compacting）。\n");
        out.push_str("# TYPE yt_segments_live gauge\n");
        out.push_str(&format!("yt_segments_live {segments}\n\n"));

        out.push_str("# HELP yt_memtable_rows 活内存表行数。\n");
        out.push_str("# TYPE yt_memtable_rows gauge\n");
        out.push_str(&format!("yt_memtable_rows {memtable_rows}\n\n"));

        out.push_str(
            "# HELP yt_segments_dead 待回收 dead 段数（compaction 摘下、等水位满足删）。\n",
        );
        out.push_str("# TYPE yt_segments_dead gauge\n");
        out.push_str(&format!("yt_segments_dead {dead}\n\n"));

        out.push_str("# HELP yt_readers_active 活跃快照读者数（pin 了某版本的）。\n");
        out.push_str("# TYPE yt_readers_active gauge\n");
        out.push_str(&format!("yt_readers_active {active_readers}\n\n"));

        out.push_str("# HELP yt_wal_committed_tail 已确认的最大 WAL LSN。\n");
        out.push_str("# TYPE yt_wal_committed_tail counter\n");
        out.push_str(&format!("yt_wal_committed_tail {committed_tail}\n\n"));

        out.push_str("# HELP yt_flush_threshold 内存表自动刷盘阈值（行数）。\n");
        out.push_str("# TYPE yt_flush_threshold gauge\n");
        out.push_str(&format!("yt_flush_threshold {flush_threshold}\n\n"));

        out.push_str("# HELP yt_filter_attrs 检索过滤属性边车条目数。\n");
        out.push_str("# TYPE yt_filter_attrs gauge\n");
        out.push_str(&format!("yt_filter_attrs {filter_attrs}\n\n"));

        out.push_str("# HELP yt_attr_posting_keys attrs 倒排索引 key 数。\n");
        out.push_str("# TYPE yt_attr_posting_keys gauge\n");
        out.push_str(&format!("yt_attr_posting_keys {attr_posting_keys}\n\n"));

        out.push_str(
            "# HELP yt_attr_posting_singleton_keys 使用单元素紧凑结构的 attrs 倒排 key 数。\n",
        );
        out.push_str("# TYPE yt_attr_posting_singleton_keys gauge\n");
        out.push_str(&format!(
            "yt_attr_posting_singleton_keys {attr_posting_singleton_keys}\n\n"
        ));

        out.push_str(
            "# HELP yt_attr_posting_small_vec_keys 使用小型有序 Vec 的 attrs 倒排 key 数。\n",
        );
        out.push_str("# TYPE yt_attr_posting_small_vec_keys gauge\n");
        out.push_str(&format!(
            "yt_attr_posting_small_vec_keys {attr_posting_small_vec_keys}\n\n"
        ));

        out.push_str("# HELP yt_attr_posting_hashset_keys 升级为 HashSet 的 attrs 倒排 key 数。\n");
        out.push_str("# TYPE yt_attr_posting_hashset_keys gauge\n");
        out.push_str(&format!(
            "yt_attr_posting_hashset_keys {attr_posting_hashset_keys}\n\n"
        ));

        out.push_str("# HELP yt_attr_posting_interned_keys attrs 倒排索引字段名字典条目数。\n");
        out.push_str("# TYPE yt_attr_posting_interned_keys gauge\n");
        out.push_str(&format!(
            "yt_attr_posting_interned_keys {attr_posting_interned_keys}\n\n"
        ));

        out.push_str("# HELP yt_attr_posting_interned_values attrs 倒排索引字段值字典条目数。\n");
        out.push_str("# TYPE yt_attr_posting_interned_values gauge\n");
        out.push_str(&format!(
            "yt_attr_posting_interned_values {attr_posting_interned_values}\n\n"
        ));

        out.push_str("# HELP yt_attr_posting_entries attrs 倒排索引 entry 数。\n");
        out.push_str("# TYPE yt_attr_posting_entries gauge\n");
        out.push_str(&format!(
            "yt_attr_posting_entries {attr_posting_entries}\n\n"
        ));

        out.push_str("# HELP yt_attr_posting_entry_budget attrs 倒排索引 entry 预算上限。\n");
        out.push_str("# TYPE yt_attr_posting_entry_budget gauge\n");
        out.push_str(&format!(
            "yt_attr_posting_entry_budget {ATTR_POSTINGS_MAX_ENTRIES}\n\n"
        ));

        out.push_str("# HELP yt_attr_posting_estimated_bytes attrs 倒排索引近似内存占用字节数。\n");
        out.push_str("# TYPE yt_attr_posting_estimated_bytes gauge\n");
        out.push_str(&format!(
            "yt_attr_posting_estimated_bytes {attr_posting_estimated_bytes}\n\n"
        ));

        out.push_str(
            "# HELP yt_attr_posting_estimated_byte_budget attrs 倒排索引近似内存预算字节数。\n",
        );
        out.push_str("# TYPE yt_attr_posting_estimated_byte_budget gauge\n");
        out.push_str(&format!(
            "yt_attr_posting_estimated_byte_budget {ATTR_POSTINGS_MAX_ESTIMATED_BYTES}\n\n"
        ));

        out.push_str(
            "# HELP yt_attr_posting_incomplete_keys 因预算或策略降级为慢路径的 attrs key 数。\n",
        );
        out.push_str("# TYPE yt_attr_posting_incomplete_keys gauge\n");
        out.push_str(&format!(
            "yt_attr_posting_incomplete_keys {attr_posting_incomplete_keys}\n\n"
        ));

        out.push_str("# HELP yt_attr_sidecar_segments 已注册 attrs segment sidecar 的段数。\n");
        out.push_str("# TYPE yt_attr_sidecar_segments gauge\n");
        out.push_str(&format!(
            "yt_attr_sidecar_segments {attr_sidecar_segments}\n\n"
        ));

        out.push_str("# HELP yt_attr_sidecar_exact_terms attrs sidecar exact term 数。\n");
        out.push_str("# TYPE yt_attr_sidecar_exact_terms gauge\n");
        out.push_str(&format!(
            "yt_attr_sidecar_exact_terms {attr_sidecar_exact_terms}\n\n"
        ));

        out.push_str("# HELP yt_attr_sidecar_array_terms attrs sidecar array includes term 数。\n");
        out.push_str("# TYPE yt_attr_sidecar_array_terms gauge\n");
        out.push_str(&format!(
            "yt_attr_sidecar_array_terms {attr_sidecar_array_terms}\n\n"
        ));

        out.push_str(
            "# HELP yt_attr_sidecar_incomplete_keys attrs sidecar 中按 segment 标记 incomplete 的 key 数。\n",
        );
        out.push_str("# TYPE yt_attr_sidecar_incomplete_keys gauge\n");
        out.push_str(&format!(
            "yt_attr_sidecar_incomplete_keys {attr_sidecar_incomplete_keys}\n\n"
        ));

        out.push_str("# HELP yt_attr_sidecar_cache_entries attrs sidecar LRU cache 条目数。\n");
        out.push_str("# TYPE yt_attr_sidecar_cache_entries gauge\n");
        out.push_str(&format!(
            "yt_attr_sidecar_cache_entries {attr_sidecar_cache_entries}\n\n"
        ));

        out.push_str(
            "# HELP yt_attr_sidecar_cache_bytes attrs sidecar LRU cache 近似驻留字节数。\n",
        );
        out.push_str("# TYPE yt_attr_sidecar_cache_bytes gauge\n");
        out.push_str(&format!(
            "yt_attr_sidecar_cache_bytes {attr_sidecar_cache_bytes}\n\n"
        ));

        out.push_str(
            "# HELP yt_attr_sidecar_cache_byte_budget attrs sidecar LRU cache 字节预算。\n",
        );
        out.push_str("# TYPE yt_attr_sidecar_cache_byte_budget gauge\n");
        out.push_str(&format!(
            "yt_attr_sidecar_cache_byte_budget {attr_sidecar_cache_byte_budget}\n\n"
        ));

        out.push_str("# HELP yt_attr_sidecar_cache_hits attrs sidecar LRU cache 命中次数。\n");
        out.push_str("# TYPE yt_attr_sidecar_cache_hits counter\n");
        out.push_str(&format!(
            "yt_attr_sidecar_cache_hits {attr_sidecar_cache_hits}\n\n"
        ));

        out.push_str("# HELP yt_attr_sidecar_cache_misses attrs sidecar LRU cache 未命中次数。\n");
        out.push_str("# TYPE yt_attr_sidecar_cache_misses counter\n");
        out.push_str(&format!(
            "yt_attr_sidecar_cache_misses {attr_sidecar_cache_misses}\n\n"
        ));

        out.push_str("# HELP yt_attr_sidecar_cache_loads attrs sidecar 被加载进 cache 的次数。\n");
        out.push_str("# TYPE yt_attr_sidecar_cache_loads counter\n");
        out.push_str(&format!(
            "yt_attr_sidecar_cache_loads {attr_sidecar_cache_loads}\n\n"
        ));

        out.push_str("# HELP yt_attr_sidecar_cache_evictions attrs sidecar LRU cache 驱逐次数。\n");
        out.push_str("# TYPE yt_attr_sidecar_cache_evictions counter\n");
        out.push_str(&format!(
            "yt_attr_sidecar_cache_evictions {attr_sidecar_cache_evictions}\n\n"
        ));

        out.push_str("# HELP yt_fold_cache_entries 段折叠缓存条目数（解码后的段）。\n");
        out.push_str("# TYPE yt_fold_cache_entries gauge\n");
        out.push_str(&format!("yt_fold_cache_entries {fold_cache_entries}\n\n"));

        out.push_str("# HELP yt_seg_bloom_count 段级 key Bloom 条目数。\n");
        out.push_str("# TYPE yt_seg_bloom_count gauge\n");
        out.push_str(&format!("yt_seg_bloom_count {bloom_count}\n\n"));

        out.push_str("# HELP yt_trace_rollup_cached_segments 已载入内存的 trace rollup segment 数。\n");
        out.push_str("# TYPE yt_trace_rollup_cached_segments gauge\n");
        out.push_str(&format!(
            "yt_trace_rollup_cached_segments {}\n\n",
            trace_rollup_profile_stats.cached_segments
        ));

        out.push_str("# HELP yt_trace_rollup_cached_rows 已载入内存的 trace rollup span row 数。\n");
        out.push_str("# TYPE yt_trace_rollup_cached_rows gauge\n");
        out.push_str(&format!(
            "yt_trace_rollup_cached_rows {}\n\n",
            trace_rollup_profile_stats.cached_rows
        ));

        out.push_str(
            "# HELP yt_trace_rollup_storage_profile_families 已载入 storageStats 预聚合 profile 族数量。\n",
        );
        out.push_str("# TYPE yt_trace_rollup_storage_profile_families gauge\n");
        out.push_str(&format!(
            "yt_trace_rollup_storage_profile_families {}\n\n",
            trace_rollup_profile_stats.storage_profile_families
        ));

        out.push_str(
            "# HELP yt_trace_rollup_storage_profile_buckets 已载入 storageStats 预聚合 bucket 数量。\n",
        );
        out.push_str("# TYPE yt_trace_rollup_storage_profile_buckets gauge\n");
        out.push_str(&format!(
            "yt_trace_rollup_storage_profile_buckets {}\n\n",
            trace_rollup_profile_stats.storage_profile_buckets
        ));

        out.push_str(
            "# HELP yt_trace_rollup_aggregate_profile_families 已载入 traceAggregate 预聚合 profile 族数量。\n",
        );
        out.push_str("# TYPE yt_trace_rollup_aggregate_profile_families gauge\n");
        out.push_str(&format!(
            "yt_trace_rollup_aggregate_profile_families {}\n\n",
            trace_rollup_profile_stats.aggregate_profile_families
        ));

        out.push_str(
            "# HELP yt_trace_rollup_aggregate_profile_buckets 已载入 traceAggregate 预聚合 bucket 数量。\n",
        );
        out.push_str("# TYPE yt_trace_rollup_aggregate_profile_buckets gauge\n");
        out.push_str(&format!(
            "yt_trace_rollup_aggregate_profile_buckets {}\n\n",
            trace_rollup_profile_stats.aggregate_profile_buckets
        ));

        out.push_str("# HELP yt_datasets 评测数据集数。\n");
        out.push_str("# TYPE yt_datasets gauge\n");
        out.push_str(&format!("yt_datasets {datasets}\n\n"));

        out.push_str("# HELP yt_annotations 业务 annotation 条目数。\n");
        out.push_str("# TYPE yt_annotations gauge\n");
        out.push_str(&format!("yt_annotations {annotations}\n\n"));

        out.push_str(
            "# HELP yt_dataset_associations 外部 dataset item 与 trace/span 的关联条目数。\n",
        );
        out.push_str("# TYPE yt_dataset_associations gauge\n");
        out.push_str(&format!(
            "yt_dataset_associations {dataset_associations}\n\n"
        ));

        out.push_str("# HELP yt_retention_audits retention/apply 执行审计记录数。\n");
        out.push_str("# TYPE yt_retention_audits gauge\n");
        out.push_str(&format!("yt_retention_audits {retention_audits}\n\n"));

        out.push_str("# HELP yt_retention_policies 已保存的 retention policy 数。\n");
        out.push_str("# TYPE yt_retention_policies gauge\n");
        out.push_str(&format!("yt_retention_policies {retention_policies}\n"));

        out
    }

    /// 当前引擎支持的数据格式版本（persist::FORMAT_VER）。
    pub fn format_version() -> u32 {
        persist::FORMAT_VER
    }

    /// 检查数据目录的 manifest 版本：返回 (磁盘上的版本, 引擎支持的版本)。
    /// 两者相等 = 兼容；磁盘 < 引擎 = 需迁移；磁盘 > 引擎 = 需新引擎。
    /// 无 manifest = 新目录（返回 (0, FORMAT_VER)）。
    pub fn check_format(dir: impl AsRef<std::path::Path>) -> (u32, u32) {
        let manifest_path = dir.as_ref().join("manifest.dat");
        match std::fs::read(&manifest_path) {
            Ok(bytes) => {
                // 文件布局：[crc32 u32][MAGIC u32][FORMAT_VER u32]...
                // 跳过 4 字节 crc 前缀读 magic + version。
                if bytes.len() < 12 {
                    return (0, persist::FORMAT_VER);
                }
                let magic = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
                if magic != 0x5654_4D46 {
                    return (0, persist::FORMAT_VER); // 损坏或非本格式
                }
                let disk_ver = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
                (disk_ver, persist::FORMAT_VER)
            }
            Err(_) => (0, persist::FORMAT_VER), // 无文件 = 新目录
        }
    }

    /// **迁移骨架**（§3.4）：把数据目录从 `from_ver` 升级到当前引擎版本。
    ///
    /// 当前 FORMAT_VER=1，无历史老版本数据，所以 from_ver 只可能是 1（无操作）或损坏（报错）。
    /// 真实迁移工具的逻辑（版本 1→2、2→3…）会在引入格式变更时逐版本实现，沿这个签名扩展。
    pub fn migrate(dir: impl AsRef<std::path::Path>) -> std::io::Result<()> {
        let (disk, current) = Self::check_format(&dir);
        match disk.cmp(&current) {
            std::cmp::Ordering::Equal => {
                olog::log(
                    olog::Level::Info,
                    "migrate",
                    &[("status", &"already current"), ("ver", &disk)],
                );
                Ok(())
            }
            std::cmp::Ordering::Less => {
                olog::log(
                    olog::Level::Error,
                    "migrate",
                    &[
                        ("status", &"old version not yet supported"),
                        ("disk", &disk),
                        ("engine", &current),
                    ],
                );
                Err(std::io::Error::new(
                    std::io::ErrorKind::Unsupported,
                    format!(
                        "从格式版本 {} 迁移到 {} 尚未实现（当前引擎无历史老版本数据）",
                        disk, current
                    ),
                ))
            }
            std::cmp::Ordering::Greater => {
                olog::log(
                    olog::Level::Error,
                    "migrate",
                    &[
                        ("status", &"data newer than engine"),
                        ("disk", &disk),
                        ("engine", &current),
                    ],
                );
                Err(std::io::Error::new(
                    std::io::ErrorKind::Unsupported,
                    format!(
                        "数据格式版本 {} 比引擎支持的 {} 新，需升级引擎",
                        disk, current
                    ),
                ))
            }
        }
    }
}
