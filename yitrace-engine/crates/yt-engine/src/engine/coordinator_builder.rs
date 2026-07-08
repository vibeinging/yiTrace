/// 引擎构造器：注入自定义检索索引（团队 jieba 分词的 BM25、自有 graph_index）后再起引擎。
/// 不传 = 用默认（bigram BM25 / 内置图式 ANN），所以现有 `WriteCoordinator::new/open/open_durable`
/// 行为不变。外部隔离 crate（如 jieba FFI）走这里把实现接进来，骨架本身仍零依赖。
///
/// ```ignore
/// // 团队 jieba 库就位后：
/// let eng = CoordinatorBuilder::new()
///     .with_tokenizer(Box::new(JiebaTokenizer::open("dict/")?)) // 只换分词层
///     .open_durable("/data/trace")?;
/// ```
#[derive(Default)]
pub struct CoordinatorBuilder {
    bm25: Option<Arc<dyn Bm25Index>>,
    graph: Option<Arc<dyn GraphIndex>>,
    /// 持久模式磁盘向量索引的参数（缓冲预算 / m / ef）。None = 默认。仅在没注入自定义 graph 时生效。
    vec_cfg: Option<DiskGraphConfig>,
}

impl CoordinatorBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// 整体替换 BM25 实现（最一般）。
    pub fn with_bm25(mut self, bm25: Arc<dyn Bm25Index>) -> Self {
        self.bm25 = Some(bm25);
        self
    }

    /// 便捷：只换 BM25 的分词器（团队 jieba 词级分词），倒排与评分仍用自有 `Bm25TextIndex`。
    pub fn with_tokenizer(self, tokenizer: Box<dyn Tokenizer>) -> Self {
        self.with_bm25(Arc::new(Bm25TextIndex::with_tokenizer(tokenizer)))
    }

    /// 替换向量 ANN 实现（接团队 graph_index 时用）。
    pub fn with_graph(mut self, graph: Arc<dyn GraphIndex>) -> Self {
        self.graph = Some(graph);
        self
    }

    /// 设持久磁盘向量索引的**缓冲预算（字节）**，如 `1 << 30` = 1GiB。仅没注入自定义 graph 时生效。
    pub fn with_vector_cache_bytes(mut self, bytes: usize) -> Self {
        self.vec_cfg = Some(self.vec_cfg.unwrap_or_default().with_cache_bytes(bytes));
        self
    }

    /// 设**建图候选列表宽度 `ef_construction`**（对齐 graph_index）：越大召回越好、建图越慢；
    /// 想要更快建图就调小（如 32），是建图速度/召回的主旋钮。默认 64。仅没注入自定义 graph 时生效。
    pub fn with_ef_construction(mut self, ef: usize) -> Self {
        self.vec_cfg = Some(self.vec_cfg.unwrap_or_default().with_ef_construction(ef));
        self
    }

    /// 设**查询候选列表宽度 `ef_search`**（对齐 `hnsw_ef_search`）：越大召回越高、查询越慢。默认 100。
    pub fn with_ef_search(mut self, ef: usize) -> Self {
        self.vec_cfg = Some(self.vec_cfg.unwrap_or_default().with_ef_search(ef));
        self
    }

    /// 设持久磁盘向量索引的完整参数（缓冲预算 / m / ef_construction / ef_search）。仅没注入自定义 graph 时生效。
    pub fn with_disk_graph_config(mut self, cfg: DiskGraphConfig) -> Self {
        self.vec_cfg = Some(cfg);
        self
    }

    /// 内存 WAL（测试/开发）。
    pub fn build(self, segments: Arc<dyn SegmentStore>) -> Arc<WriteCoordinator> {
        WriteCoordinator::build_full(
            segments,
            Wal::new(),
            Manifest::empty(),
            1,
            1,
            None,
            None,
            self.bm25,
            self.graph,
            None,
            None,
            None,
            None,
        )
    }

    /// 文件 WAL。
    pub fn open(
        self,
        segments: Arc<dyn SegmentStore>,
        wal_path: impl AsRef<std::path::Path>,
    ) -> std::io::Result<Arc<WriteCoordinator>> {
        Ok(WriteCoordinator::build_full(
            segments,
            Wal::open(wal_path)?,
            Manifest::empty(),
            1,
            1,
            None,
            None,
            self.bm25,
            self.graph,
            None,
            None,
            None,
            None,
        ))
    }

    /// 全持久化引擎（与 `WriteCoordinator::open_durable` 同语义，外加注入的索引 / 磁盘向量索引参数）。
    pub fn open_durable(
        self,
        dir: impl AsRef<std::path::Path>,
    ) -> std::io::Result<Arc<WriteCoordinator>> {
        WriteCoordinator::open_durable_inner(dir, self.bm25, self.graph, self.vec_cfg)
    }
}
