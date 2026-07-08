// ───────────────────────── 引擎用：惰性磁盘图索引（首个向量定维度） ─────────────────────────

/// 引擎 `open_durable` 用的磁盘图索引包装：维度由**首个 embedding** 决定（或重开时从元页读回），
/// 在此之前（还没向量）搜索返回空。这样引擎不必预先知道向量维度。
pub struct DurableGraphIndex {
    dir: PathBuf,
    cfg: DiskGraphConfig,
    inner: Mutex<Option<std::sync::Arc<DiskGraphIndex>>>,
}

impl DurableGraphIndex {
    /// 在 `dir` 下放磁盘图索引。已有元页则立即打开（维度从盘读回）；没有则等首个向量来定维度。
    pub fn open(dir: impl AsRef<Path>, cfg: DiskGraphConfig) -> Self {
        let dir = dir.as_ref().to_path_buf();
        let inner = if dir.join("meta").exists() {
            DiskGraphIndex::open(&dir, 0, cfg)
                .ok()
                .map(std::sync::Arc::new)
        } else {
            None
        };
        Self {
            dir,
            cfg,
            inner: Mutex::new(inner),
        }
    }

    fn handle(&self) -> Option<std::sync::Arc<DiskGraphIndex>> {
        self.inner.lock().unwrap().clone()
    }
}

impl GraphIndex for DurableGraphIndex {
    fn index_embedding(&self, trace_id: u64, span_id: u64, embedding: Vec<f32>) {
        // 首个向量定维度、建索引；之后复用。锁只护"取/建句柄"，建图本身在句柄上做（句柄内部已同步）。
        let idx = {
            let mut g = self.inner.lock().unwrap();
            if g.is_none() {
                match DiskGraphIndex::open(&self.dir, embedding.len(), self.cfg) {
                    Ok(i) => *g = Some(std::sync::Arc::new(i)),
                    Err(_) => return,
                }
            }
            g.clone()
        };
        if let Some(i) = idx {
            i.index_embedding(trace_id, span_id, embedding);
        }
    }

    fn search(
        &self,
        query: &[f32],
        k: usize,
        filter: &dyn Fn(u64, u64) -> bool,
    ) -> Vec<(u64, u64, f32)> {
        match self.handle() {
            Some(i) => i.search(query, k, filter),
            None => Vec::new(),
        }
    }

    fn flush(&self) {
        if let Some(i) = self.handle() {
            i.flush();
        }
    }
}
