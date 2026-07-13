//! BM25 持久缓存的按需读取层。
//!
//! 打开时只读取固定宽度的文档长度表和词目录。真正的 postings 在查询命中该词时才按文件
//! 偏移读取，并受固定内存预算约束。这样数据库启动和一次窄查询都不需要把完整倒排搬进内存。

use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::Arc;

pub(crate) type Doc = (u64, u64);
pub(crate) type Posting = (Doc, u32);

const MAGIC: u32 = 0x5954_424d; // "YTBM"
const VERSION: u32 = 2;
const HEADER_LEN: u64 = 8 + 9 * 8;
const DOC_RECORD_LEN: u64 = 20;
const POSTING_RECORD_LEN: u64 = 20;
const DEFAULT_CACHE_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone, Copy)]
struct PostingRef {
    offset: u64,
    count: u64,
}

#[derive(Clone, Copy)]
pub(crate) struct CacheMetadata {
    pub(crate) manifest_version: u64,
    pub(crate) memtable_watermark: u64,
    pub(crate) total_len: u64,
}

pub(crate) struct DiskBm25Cache {
    file: File,
    docs: Vec<(Doc, u32)>,
    total_len: u64,
    directory: HashMap<String, PostingRef>,
    cached: HashMap<String, Arc<Vec<Posting>>>,
    lru: VecDeque<String>,
    cached_bytes: usize,
    cache_budget: usize,
}

impl DiskBm25Cache {
    pub(crate) fn open(
        path: &Path,
        manifest_version: u64,
        memtable_watermark: u64,
    ) -> Option<Self> {
        Self::open_with_budget(
            path,
            manifest_version,
            memtable_watermark,
            postings_cache_budget(),
        )
    }

    fn open_with_budget(
        path: &Path,
        manifest_version: u64,
        memtable_watermark: u64,
        cache_budget: usize,
    ) -> Option<Self> {
        let mut file = File::open(path).ok()?;
        let file_len = file.metadata().ok()?.len();
        if file_len < HEADER_LEN {
            return None;
        }
        let mut header = [0u8; HEADER_LEN as usize];
        file.read_exact(&mut header).ok()?;
        let mut cur = SliceCursor::new(&header);
        if cur.u32()? != MAGIC || cur.u32()? != VERSION {
            return None;
        }
        if cur.u64()? != manifest_version || cur.u64()? != memtable_watermark {
            return None;
        }
        let total_len = cur.u64()?;
        let doc_count = cur.u64()?;
        let token_count = cur.u64()?;
        let docs_offset = cur.u64()?;
        let postings_offset = cur.u64()?;
        let directory_offset = cur.u64()?;
        let directory_len = cur.u64()?;

        let docs_len = doc_count.checked_mul(DOC_RECORD_LEN)?;
        let docs_end = docs_offset.checked_add(docs_len)?;
        let directory_end = directory_offset.checked_add(directory_len)?;
        if docs_offset != HEADER_LEN
            || docs_end > postings_offset
            || postings_offset > directory_offset
            || directory_end != file_len
        {
            return None;
        }

        let docs_bytes_len = usize::try_from(docs_len).ok()?;
        let mut docs_bytes = vec![0u8; docs_bytes_len];
        file.seek(SeekFrom::Start(docs_offset)).ok()?;
        file.read_exact(&mut docs_bytes).ok()?;
        let mut docs_cur = SliceCursor::new(&docs_bytes);
        let mut docs = Vec::with_capacity(usize::try_from(doc_count).ok()?);
        let mut previous = None;
        for _ in 0..doc_count {
            let doc = (docs_cur.u64()?, docs_cur.u64()?);
            let len = docs_cur.u32()?;
            if previous.is_some_and(|prev| prev >= doc) {
                return None;
            }
            previous = Some(doc);
            docs.push((doc, len));
        }

        let directory_bytes_len = usize::try_from(directory_len).ok()?;
        let mut directory_bytes = vec![0u8; directory_bytes_len];
        file.seek(SeekFrom::Start(directory_offset)).ok()?;
        file.read_exact(&mut directory_bytes).ok()?;
        let mut dir_cur = SliceCursor::new(&directory_bytes);
        let mut directory = HashMap::with_capacity(usize::try_from(token_count).ok()?);
        for _ in 0..token_count {
            let token = dir_cur.string_u32()?;
            let offset = dir_cur.u64()?;
            let count = dir_cur.u64()?;
            let bytes = count.checked_mul(POSTING_RECORD_LEN)?;
            let end = offset.checked_add(bytes)?;
            if offset < postings_offset || end > directory_offset || directory.contains_key(&token)
            {
                return None;
            }
            directory.insert(token, PostingRef { offset, count });
        }
        if !dir_cur.is_finished() {
            return None;
        }

        Some(Self {
            file,
            docs,
            total_len,
            directory,
            cached: HashMap::new(),
            lru: VecDeque::new(),
            cached_bytes: 0,
            cache_budget,
        })
    }

    pub(crate) fn doc_count(&self) -> usize {
        self.docs.len()
    }

    pub(crate) fn total_len(&self) -> u64 {
        self.total_len
    }

    pub(crate) fn doc_len(&self, doc: Doc) -> Option<u32> {
        self.docs
            .binary_search_by_key(&doc, |&(key, _)| key)
            .ok()
            .map(|index| self.docs[index].1)
    }

    pub(crate) fn docs(&self) -> &[(Doc, u32)] {
        &self.docs
    }

    pub(crate) fn token_names(&self) -> impl Iterator<Item = &String> {
        self.directory.keys()
    }

    pub(crate) fn load_postings(&mut self, token: &str) -> Option<Arc<Vec<Posting>>> {
        if let Some(postings) = self.cached.get(token).cloned() {
            self.touch(token);
            return Some(postings);
        }
        let entry = *self.directory.get(token)?;
        let byte_len = usize::try_from(entry.count.checked_mul(POSTING_RECORD_LEN)?).ok()?;
        let mut bytes = vec![0u8; byte_len];
        self.file.seek(SeekFrom::Start(entry.offset)).ok()?;
        self.file.read_exact(&mut bytes).ok()?;
        let mut cur = SliceCursor::new(&bytes);
        let mut postings = Vec::with_capacity(usize::try_from(entry.count).ok()?);
        let mut previous = None;
        let mut doc_index = 0usize;
        for _ in 0..entry.count {
            let doc = (cur.u64()?, cur.u64()?);
            let tf = cur.u32()?;
            while doc_index < self.docs.len() && self.docs[doc_index].0 < doc {
                doc_index += 1;
            }
            if tf == 0
                || previous.is_some_and(|prev| prev >= doc)
                || doc_index >= self.docs.len()
                || self.docs[doc_index].0 != doc
            {
                return None;
            }
            previous = Some(doc);
            postings.push((doc, tf));
        }
        let postings = Arc::new(postings);
        if byte_len <= self.cache_budget {
            while self.cached_bytes.saturating_add(byte_len) > self.cache_budget {
                let Some(oldest) = self.lru.pop_front() else {
                    break;
                };
                if let Some(removed) = self.cached.remove(&oldest) {
                    self.cached_bytes = self
                        .cached_bytes
                        .saturating_sub(removed.len().saturating_mul(POSTING_RECORD_LEN as usize));
                }
            }
            self.cached.insert(token.to_owned(), Arc::clone(&postings));
            self.lru.push_back(token.to_owned());
            self.cached_bytes = self.cached_bytes.saturating_add(byte_len);
        }
        Some(postings)
    }

    fn touch(&mut self, token: &str) {
        self.lru.retain(|cached| cached != token);
        self.lru.push_back(token.to_owned());
    }

    #[cfg(test)]
    fn cached_token_count(&self) -> usize {
        self.cached.len()
    }
}

fn postings_cache_budget() -> usize {
    std::env::var("YT_BM25_POSTINGS_CACHE_BYTES")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(DEFAULT_CACHE_BYTES)
}

pub(crate) fn write_atomic<D, F>(
    path: &Path,
    metadata: CacheMetadata,
    docs: D,
    doc_count: u64,
    tokens: &[String],
    mut postings_for: F,
) -> std::io::Result<()>
where
    D: IntoIterator<Item = (Doc, u32)>,
    F: FnMut(&str) -> std::io::Result<Vec<Posting>>,
{
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    std::fs::create_dir_all(parent)?;
    let tmp = path.with_extension("tmp");
    let mut file = File::create(&tmp)?;
    file.write_all(&[0u8; HEADER_LEN as usize])?;

    let mut written_docs = 0u64;
    let mut previous_doc = None;
    for (doc, len) in docs {
        if previous_doc.is_some_and(|prev| prev >= doc) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "BM25 documents are not strictly sorted",
            ));
        }
        previous_doc = Some(doc);
        file.write_all(&doc.0.to_le_bytes())?;
        file.write_all(&doc.1.to_le_bytes())?;
        file.write_all(&len.to_le_bytes())?;
        written_docs += 1;
    }
    if written_docs != doc_count {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "BM25 document count changed while saving",
        ));
    }

    let postings_offset = file.stream_position()?;
    let mut directory = Vec::with_capacity(tokens.len());
    for token in tokens {
        let postings = postings_for(token)?;
        let offset = file.stream_position()?;
        let mut previous = None;
        for &(doc, tf) in &postings {
            if tf == 0 || previous.is_some_and(|prev| prev >= doc) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "BM25 postings are not strictly sorted",
                ));
            }
            previous = Some(doc);
            file.write_all(&doc.0.to_le_bytes())?;
            file.write_all(&doc.1.to_le_bytes())?;
            file.write_all(&tf.to_le_bytes())?;
        }
        directory.push((token, offset, postings.len() as u64));
    }

    let directory_offset = file.stream_position()?;
    for (token, offset, count) in directory {
        let token_len = u32::try_from(token.len()).map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, "BM25 token is too long")
        })?;
        file.write_all(&token_len.to_le_bytes())?;
        file.write_all(token.as_bytes())?;
        file.write_all(&offset.to_le_bytes())?;
        file.write_all(&count.to_le_bytes())?;
    }
    let end = file.stream_position()?;

    file.seek(SeekFrom::Start(0))?;
    file.write_all(&MAGIC.to_le_bytes())?;
    file.write_all(&VERSION.to_le_bytes())?;
    file.write_all(&metadata.manifest_version.to_le_bytes())?;
    file.write_all(&metadata.memtable_watermark.to_le_bytes())?;
    file.write_all(&metadata.total_len.to_le_bytes())?;
    file.write_all(&doc_count.to_le_bytes())?;
    file.write_all(&(tokens.len() as u64).to_le_bytes())?;
    file.write_all(&HEADER_LEN.to_le_bytes())?;
    file.write_all(&postings_offset.to_le_bytes())?;
    file.write_all(&directory_offset.to_le_bytes())?;
    file.write_all(&(end - directory_offset).to_le_bytes())?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(tmp, path)?;
    Ok(())
}

struct SliceCursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> SliceCursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    fn take(&mut self, len: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(len)?;
        if end > self.bytes.len() {
            return None;
        }
        let value = &self.bytes[self.pos..end];
        self.pos = end;
        Some(value)
    }

    fn u32(&mut self) -> Option<u32> {
        let mut bytes = [0; 4];
        bytes.copy_from_slice(self.take(4)?);
        Some(u32::from_le_bytes(bytes))
    }

    fn u64(&mut self) -> Option<u64> {
        let mut bytes = [0; 8];
        bytes.copy_from_slice(self.take(8)?);
        Some(u64::from_le_bytes(bytes))
    }

    fn string_u32(&mut self) -> Option<String> {
        let len = usize::try_from(self.u32()?).ok()?;
        String::from_utf8(self.take(len)?.to_vec()).ok()
    }

    fn is_finished(&self) -> bool {
        self.pos == self.bytes.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "yt_bm25_disk_{name}_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn opens_metadata_and_pages_only_requested_postings() {
        let path = temp_path("page");
        let docs = vec![((1, 1), 3), ((2, 2), 4), ((3, 3), 5)];
        let tokens = vec!["a".to_owned(), "b".to_owned()];
        write_atomic(
            &path,
            CacheMetadata {
                manifest_version: 7,
                memtable_watermark: 3,
                total_len: 12,
            },
            docs.clone(),
            docs.len() as u64,
            &tokens,
            |token| match token {
                "a" => Ok(vec![((1, 1), 2), ((3, 3), 1)]),
                "b" => Ok(vec![((2, 2), 1)]),
                _ => unreachable!(),
            },
        )
        .unwrap();

        let mut cache = DiskBm25Cache::open_with_budget(&path, 7, 3, 40).unwrap();
        assert_eq!(cache.doc_count(), 3);
        assert_eq!(cache.cached_token_count(), 0);
        assert_eq!(
            &*cache.load_postings("a").unwrap(),
            &[((1, 1), 2), ((3, 3), 1)]
        );
        assert_eq!(cache.cached_token_count(), 1);
        assert_eq!(&*cache.load_postings("b").unwrap(), &[((2, 2), 1)]);
        assert_eq!(cache.cached_token_count(), 1, "40-byte budget must evict a");
        assert!(DiskBm25Cache::open(&path, 8, 3).is_none());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn rejects_truncated_cache() {
        let path = temp_path("truncated");
        std::fs::write(&path, [0u8; 16]).unwrap();
        assert!(DiskBm25Cache::open(&path, 1, 1).is_none());
        let _ = std::fs::remove_file(path);
    }
}
