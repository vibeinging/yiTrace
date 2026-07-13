//! 过滤属性侧车的磁盘目录。
//!
//! 行内容和 postings 分开保存。打开只读取两个目录；精确过滤按 `(field,value)` 读取一段
//! postings，点查行按 `(trace,span)` 读取一条记录。

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub(crate) type SpanKey = (u64, u64);

const MAGIC: u32 = 0x5954_4641; // "YTFA"
const VERSION: u32 = 3;
const HEADER_LEN: u64 = 8 + 9 * 8;
const ROW_REF_LEN: u64 = 28;
const POSTING_LEN: u64 = 16;
const DEFAULT_CACHE_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone, Copy)]
struct RowRef {
    key: SpanKey,
    offset: u64,
    len: u32,
}

#[derive(Clone, Copy)]
struct PostingRef {
    offset: u64,
    count: u64,
    disabled: bool,
}

pub(crate) enum PostingLookup {
    Resident(Arc<HashSet<SpanKey>>),
    Disabled,
    Missing,
}

pub(crate) struct DiskFilterCache {
    path: PathBuf,
    manifest_version: u64,
    memtable_watermark: u64,
    file: File,
    rows: Vec<RowRef>,
    posting_entries: usize,
    disabled_postings: usize,
    directory: HashMap<(String, String), PostingRef>,
    cached: HashMap<(String, String), Arc<HashSet<SpanKey>>>,
    lru: VecDeque<(String, String)>,
    cached_bytes: usize,
    cache_budget: usize,
}

impl DiskFilterCache {
    pub(crate) fn open(
        path: &Path,
        manifest_version: u64,
        memtable_watermark: u64,
    ) -> Option<Self> {
        let mut file = File::open(path).ok()?;
        let file_len = file.metadata().ok()?.len();
        if file_len < HEADER_LEN {
            return None;
        }
        let mut header = [0u8; HEADER_LEN as usize];
        file.read_exact(&mut header).ok()?;
        let mut cur = Cursor::new(&header);
        if cur.u32()? != MAGIC || cur.u32()? != VERSION {
            return None;
        }
        if cur.u64()? != manifest_version || cur.u64()? != memtable_watermark {
            return None;
        }
        let row_count = cur.u64()?;
        let posting_count = cur.u64()?;
        let rows_offset = cur.u64()?;
        let row_directory_offset = cur.u64()?;
        let postings_offset = cur.u64()?;
        let posting_directory_offset = cur.u64()?;
        let posting_directory_len = cur.u64()?;

        let row_dir_len = row_count.checked_mul(ROW_REF_LEN)?;
        let row_dir_end = row_directory_offset.checked_add(row_dir_len)?;
        let posting_dir_end = posting_directory_offset.checked_add(posting_directory_len)?;
        if rows_offset != HEADER_LEN
            || rows_offset > row_directory_offset
            || row_dir_end != postings_offset
            || postings_offset > posting_directory_offset
            || posting_dir_end != file_len
        {
            return None;
        }

        let mut row_dir = vec![0; usize::try_from(row_dir_len).ok()?];
        file.seek(SeekFrom::Start(row_directory_offset)).ok()?;
        file.read_exact(&mut row_dir).ok()?;
        let mut row_cur = Cursor::new(&row_dir);
        let mut rows = Vec::with_capacity(usize::try_from(row_count).ok()?);
        let mut previous = None;
        for _ in 0..row_count {
            let key = (row_cur.u64()?, row_cur.u64()?);
            let offset = row_cur.u64()?;
            let len = row_cur.u32()?;
            let end = offset.checked_add(u64::from(len))?;
            if previous.is_some_and(|prev| prev >= key)
                || offset < rows_offset
                || end > row_directory_offset
            {
                return None;
            }
            previous = Some(key);
            rows.push(RowRef { key, offset, len });
        }

        let mut posting_dir = vec![0; usize::try_from(posting_directory_len).ok()?];
        file.seek(SeekFrom::Start(posting_directory_offset)).ok()?;
        file.read_exact(&mut posting_dir).ok()?;
        let mut posting_cur = Cursor::new(&posting_dir);
        let mut directory = HashMap::with_capacity(usize::try_from(posting_count).ok()?);
        let mut posting_entries = 0usize;
        let mut disabled_postings = 0usize;
        for _ in 0..posting_count {
            let field = posting_cur.string_u32()?;
            let value = posting_cur.string_u32()?;
            let offset = posting_cur.u64()?;
            let count = posting_cur.u64()?;
            let disabled = posting_cur.u8()? != 0;
            let end = offset.checked_add(count.checked_mul(POSTING_LEN)?)?;
            if offset < postings_offset
                || end > posting_directory_offset
                || directory.contains_key(&(field.clone(), value.clone()))
            {
                return None;
            }
            posting_entries = posting_entries.checked_add(usize::try_from(count).ok()?)?;
            disabled_postings += usize::from(disabled);
            directory.insert(
                (field, value),
                PostingRef {
                    offset,
                    count,
                    disabled,
                },
            );
        }
        if !posting_cur.finished() {
            return None;
        }

        Some(Self {
            path: path.to_path_buf(),
            manifest_version,
            memtable_watermark,
            file,
            rows,
            posting_entries,
            disabled_postings,
            directory,
            cached: HashMap::new(),
            lru: VecDeque::new(),
            cached_bytes: 0,
            cache_budget: cache_budget(),
        })
    }

    pub(crate) fn persist_with_metadata(
        &self,
        path: &Path,
        manifest_version: u64,
        memtable_watermark: u64,
    ) -> std::io::Result<()> {
        if self.path == path
            && self.manifest_version == manifest_version
            && self.memtable_watermark == memtable_watermark
        {
            return Ok(());
        }
        let Some(parent) = path.parent() else {
            return Ok(());
        };
        std::fs::create_dir_all(parent)?;
        let tmp = path.with_extension("tmp");
        std::fs::copy(&self.path, &tmp)?;
        let mut file = OpenOptions::new().write(true).open(&tmp)?;
        file.seek(SeekFrom::Start(8))?;
        file.write_all(&manifest_version.to_le_bytes())?;
        file.write_all(&memtable_watermark.to_le_bytes())?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(tmp, path)
    }

    pub(crate) fn row_count(&self) -> usize {
        self.rows.len()
    }

    pub(crate) fn posting_count(&self) -> usize {
        self.posting_entries
    }

    pub(crate) fn disabled_posting_count(&self) -> usize {
        self.disabled_postings
    }

    pub(crate) fn row_keys(&self) -> impl Iterator<Item = SpanKey> + '_ {
        self.rows.iter().map(|row| row.key)
    }

    pub(crate) fn load_row(&mut self, key: SpanKey) -> Option<Vec<u8>> {
        let index = self.rows.binary_search_by_key(&key, |row| row.key).ok()?;
        let row = self.rows[index];
        let mut bytes = vec![0; row.len as usize];
        self.file.seek(SeekFrom::Start(row.offset)).ok()?;
        self.file.read_exact(&mut bytes).ok()?;
        Some(bytes)
    }

    pub(crate) fn lookup(&mut self, field: &str, value: &str) -> PostingLookup {
        let key = (field.to_owned(), value.to_owned());
        let Some(entry) = self.directory.get(&key).copied() else {
            return PostingLookup::Missing;
        };
        if entry.disabled {
            return PostingLookup::Disabled;
        }
        if let Some(set) = self.cached.get(&key).cloned() {
            self.touch(&key);
            return PostingLookup::Resident(set);
        }
        let Some(byte_len) = entry.count.checked_mul(POSTING_LEN) else {
            return PostingLookup::Missing;
        };
        let Ok(byte_len) = usize::try_from(byte_len) else {
            return PostingLookup::Missing;
        };
        let mut bytes = vec![0; byte_len];
        if self.file.seek(SeekFrom::Start(entry.offset)).is_err()
            || self.file.read_exact(&mut bytes).is_err()
        {
            return PostingLookup::Missing;
        }
        let mut cur = Cursor::new(&bytes);
        let mut set = HashSet::with_capacity(entry.count as usize);
        for _ in 0..entry.count {
            let Some(trace_id) = cur.u64() else {
                return PostingLookup::Missing;
            };
            let Some(span_id) = cur.u64() else {
                return PostingLookup::Missing;
            };
            set.insert((trace_id, span_id));
        }
        let set = Arc::new(set);
        if byte_len <= self.cache_budget {
            while self.cached_bytes.saturating_add(byte_len) > self.cache_budget {
                let Some(oldest) = self.lru.pop_front() else {
                    break;
                };
                if let Some(removed) = self.cached.remove(&oldest) {
                    self.cached_bytes = self
                        .cached_bytes
                        .saturating_sub(removed.len().saturating_mul(POSTING_LEN as usize));
                }
            }
            self.cached.insert(key.clone(), Arc::clone(&set));
            self.lru.push_back(key);
            self.cached_bytes = self.cached_bytes.saturating_add(byte_len);
        }
        PostingLookup::Resident(set)
    }

    fn touch(&mut self, key: &(String, String)) {
        self.lru.retain(|cached| cached != key);
        self.lru.push_back(key.clone());
    }
}

fn cache_budget() -> usize {
    std::env::var("YT_FILTER_POSTINGS_CACHE_BYTES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(DEFAULT_CACHE_BYTES)
}

pub(crate) struct PostingWrite {
    pub(crate) field: String,
    pub(crate) value: String,
    pub(crate) disabled: bool,
    pub(crate) keys: Vec<SpanKey>,
}

pub(crate) fn write_atomic<R, P>(
    path: &Path,
    manifest_version: u64,
    memtable_watermark: u64,
    rows: R,
    postings: P,
) -> std::io::Result<()>
where
    R: IntoIterator<Item = (SpanKey, Vec<u8>)>,
    P: IntoIterator<Item = std::io::Result<PostingWrite>>,
{
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    std::fs::create_dir_all(parent)?;
    let tmp = path.with_extension("tmp");
    let mut file = File::create(&tmp)?;
    file.write_all(&[0; HEADER_LEN as usize])?;

    let mut row_refs = Vec::new();
    let mut previous = None;
    for (key, bytes) in rows {
        if previous.is_some_and(|prev| prev >= key) {
            return Err(invalid("filter rows are not strictly sorted"));
        }
        previous = Some(key);
        let offset = file.stream_position()?;
        let len = u32::try_from(bytes.len()).map_err(|_| invalid("filter row is too large"))?;
        file.write_all(&bytes)?;
        row_refs.push(RowRef { key, offset, len });
    }
    let row_directory_offset = file.stream_position()?;
    for row in &row_refs {
        file.write_all(&row.key.0.to_le_bytes())?;
        file.write_all(&row.key.1.to_le_bytes())?;
        file.write_all(&row.offset.to_le_bytes())?;
        file.write_all(&row.len.to_le_bytes())?;
    }

    let postings_offset = file.stream_position()?;
    let mut posting_refs = Vec::new();
    for posting in postings {
        let posting = posting?;
        let offset = file.stream_position()?;
        for key in &posting.keys {
            file.write_all(&key.0.to_le_bytes())?;
            file.write_all(&key.1.to_le_bytes())?;
        }
        posting_refs.push((
            posting.field,
            posting.value,
            PostingRef {
                offset,
                count: posting.keys.len() as u64,
                disabled: posting.disabled,
            },
        ));
    }

    let posting_directory_offset = file.stream_position()?;
    for (field, value, posting) in &posting_refs {
        write_string(&mut file, field)?;
        write_string(&mut file, value)?;
        file.write_all(&posting.offset.to_le_bytes())?;
        file.write_all(&posting.count.to_le_bytes())?;
        file.write_all(&[u8::from(posting.disabled)])?;
    }
    let end = file.stream_position()?;

    file.seek(SeekFrom::Start(0))?;
    for value in [MAGIC, VERSION] {
        file.write_all(&value.to_le_bytes())?;
    }
    for value in [
        manifest_version,
        memtable_watermark,
        row_refs.len() as u64,
        posting_refs.len() as u64,
        HEADER_LEN,
        row_directory_offset,
        postings_offset,
        posting_directory_offset,
        end - posting_directory_offset,
    ] {
        file.write_all(&value.to_le_bytes())?;
    }
    file.sync_all()?;
    drop(file);
    std::fs::rename(tmp, path)
}

fn write_string(file: &mut File, value: &str) -> std::io::Result<()> {
    let len = u32::try_from(value.len()).map_err(|_| invalid("filter key is too large"))?;
    file.write_all(&len.to_le_bytes())?;
    file.write_all(value.as_bytes())
}

fn invalid(message: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message)
}

struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    fn take(&mut self, len: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(len)?;
        let bytes = self.bytes.get(self.pos..end)?;
        self.pos = end;
        Some(bytes)
    }

    fn u8(&mut self) -> Option<u8> {
        self.take(1)?.first().copied()
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }

    fn string_u32(&mut self) -> Option<String> {
        let len = usize::try_from(self.u32()?).ok()?;
        String::from_utf8(self.take(len)?.to_vec()).ok()
    }

    fn finished(&self) -> bool {
        self.pos == self.bytes.len()
    }
}
