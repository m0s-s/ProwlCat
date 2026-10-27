//! Ordered, memory-bounded collection of scan results.
//!
//! A scan over a very large breach can match hundreds of millions of lines. Holding every
//! `ScanResult` in a `Vec` (and sorting it afterwards) makes the memory use grow with the number
//! of matches until the operating system kills the process. Here matches are kept in a compact
//! form, and once they pass a budget they are sorted and written to an anonymous temporary file
//! (a "run"). Reading the results back is a k-way merge of the runs, so memory stays at the budget
//! plus a few kilobytes per run, whatever the number of matches.
//!
//! The order is exactly the one the command line has always produced: a stable sort by query
//! text, then file path, then line number, with ties kept in scan order.

use std::cmp::{Ordering, Reverse};
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::fs::File;
use std::io::{self, BufRead, BufReader, BufWriter, Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::{Context, Result};
use bincode::Options;
use serde::{Deserialize, Serialize};
use tracing::warn;

use prowlcat_source::SourceEntry;
use prowlcat_types::{ExtractedValue, MatchKind, QueryKind, ScanResult, ScanSummary, SourceLocation};

/// A worker hands its matches over once it holds this many (estimated) bytes.
const LOCAL_FLUSH: usize = 2 * 1024 * 1024;
const RUN_READ_BUFFER: usize = 32 * 1024;

/// A match without the parts every match of a file shares (path) or of a query (text, kind).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Hit {
    pub query_idx: usize,
    pub match_kind: MatchKind,
    pub archive_entry: Option<String>,
    pub line_number: u64,
    pub byte_offset: u64,
    pub line_text: String,
    pub span: (usize, usize),
    pub extracted: Vec<ExtractedValue>,
}

impl Hit {
    /// Rough heap footprint, used only to decide when to spill.
    fn weight(&self) -> usize {
        let extracted: usize = self
            .extracted
            .iter()
            .map(|e| match e {
                ExtractedValue::Email(s)
                | ExtractedValue::Domain(s)
                | ExtractedValue::Ip(s)
                | ExtractedValue::Cidr(s)
                | ExtractedValue::Text(s) => s.len() + 32,
            })
            .sum();
        std::mem::size_of::<Item>()
            + self.line_text.len()
            + self.archive_entry.as_ref().map_or(0, String::len)
            + extracted
            + 32
    }

    pub(crate) fn into_result(self, query: &str, kind: QueryKind, path: &Path) -> ScanResult {
        ScanResult {
            query: query.to_string(),
            kind,
            match_kind: self.match_kind,
            source: SourceLocation {
                path: path.to_path_buf(),
                archive_entry: self.archive_entry,
                line_number: Some(self.line_number),
                byte_offset: self.byte_offset,
            },
            line_text: self.line_text,
            matched_span: self.span,
            extracted: self.extracted,
        }
    }

    pub(crate) fn to_result(&self, query: &str, kind: QueryKind, path: &Path) -> ScanResult {
        self.clone().into_result(query, kind, path)
    }
}

/// Position of a match in the output. Field order is the sort order: query text, file path, line,
/// then scan order (file index, sequence in that file) to keep ties stable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct Key {
    query: u32,
    path: u32,
    line: u64,
    pub entry: u32,
    seq: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Item {
    key: Key,
    hit: Hit,
}

/// Rank of each element under `cmp`, equal elements sharing a rank.
fn dense_ranks<T>(items: &[T], cmp: impl Fn(&T, &T) -> Ordering) -> Vec<u32> {
    let mut order: Vec<usize> = (0..items.len()).collect();
    order.sort_by(|&a, &b| cmp(&items[a], &items[b]));
    let mut ranks = vec![0u32; items.len()];
    let mut rank = 0u32;
    for (n, &i) in order.iter().enumerate() {
        if n > 0 && cmp(&items[order[n - 1]], &items[i]) != Ordering::Equal {
            rank += 1;
        }
        ranks[i] = rank;
    }
    ranks
}

/// A sorted batch of matches parked in an anonymous temporary file (deleted on close).
///
/// Many runs share one file, one after the other, so a very large search holds a few dozen file
/// descriptors rather than one per run (the default limit is often 1024).
struct Run {
    file: Arc<File>,
    start: u64,
    len: u64,
}

/// A temporary file is closed to new runs once it holds this much.
const SPILL_FILE_MAX: u64 = 1 << 30;

struct Inner {
    mem: Vec<Item>,
    mem_bytes: usize,
    runs: Vec<Run>,
    /// File new runs are appended to, and how much it holds.
    current: Option<(Arc<File>, u64)>,
    total: u64,
    /// Matches of a file that were handed over before the file was finished.
    partial: HashMap<u32, u64>,
    /// Files that failed after handing over part of their matches; those matches are ignored,
    /// as a failed file has always contributed nothing.
    dropped: HashSet<u32>,
    spill_failed: bool,
}

/// Shared by all workers of one scan.
pub(crate) struct Collector {
    inner: Mutex<Inner>,
    budget: usize,
    query_rank: Vec<u32>,
    path_rank: Vec<u32>,
}

impl Collector {
    pub(crate) fn new(entries: &[SourceEntry], queries: &[(String, QueryKind)], budget: usize) -> Self {
        Self {
            inner: Mutex::new(Inner {
                mem: Vec::new(),
                mem_bytes: 0,
                runs: Vec::new(),
                current: None,
                total: 0,
                partial: HashMap::new(),
                dropped: HashSet::new(),
                spill_failed: false,
            }),
            budget,
            query_rank: dense_ranks(queries, |a, b| a.0.cmp(&b.0)),
            path_rank: dense_ranks(entries, |a, b| a.path.cmp(&b.path)),
        }
    }

    pub(crate) fn local(&self, entry: u32) -> Local<'_> {
        Local { collector: self, entry, buf: Vec::new(), bytes: 0, seq: 0 }
    }

    fn add(&self, batch: Vec<Item>, bytes: usize, partial_of: Option<u32>) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.total += batch.len() as u64;
        if let Some(entry) = partial_of {
            *inner.partial.entry(entry).or_default() += batch.len() as u64;
        }
        inner.mem.extend(batch);
        inner.mem_bytes += bytes;
        if inner.mem_bytes > self.budget && !inner.spill_failed {
            // Workers wait on this lock while the run is written, which is the point: the scan
            // slows down instead of the memory growing.
            if let Err(e) = spill(&mut inner) {
                warn!(error = %e, "could not write results to disk; keeping them in memory");
                inner.spill_failed = true;
            }
        }
    }

    fn discard(&self, entry: u32) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(n) = inner.partial.remove(&entry) {
            inner.total -= n;
            inner.dropped.insert(entry);
        }
    }

    pub(crate) fn finish(
        self,
        entries: Arc<Vec<SourceEntry>>,
        queries: Vec<(String, QueryKind)>,
        mut summary: ScanSummary,
    ) -> SortedResults {
        let mut inner = self.inner.into_inner().unwrap_or_else(|e| e.into_inner());
        inner.mem.sort_unstable_by_key(|i| i.key);
        summary.matches = inner.total as usize;
        SortedResults {
            summary,
            backing: Backing::Spilled(Box::new(Spilled {
                mem: inner.mem,
                runs: inner.runs,
                dropped: inner.dropped,
                entries,
                queries,
            })),
        }
    }
}

/// Where result runs are parked. They must not land in RAM: on many Linux systems `/tmp` is a
/// tmpfs, and a large search would fill it and exhaust memory, which is what the runs exist to
/// avoid. `PROWLCAT_TMP_DIR` wins; otherwise the system temporary directory is used unless it is
/// RAM-backed, in which case `/var/tmp` and then `~/.cache/prowlcat` are tried.
fn spill_dir() -> Option<&'static Path> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(|| {
        if let Some(dir) = std::env::var_os("PROWLCAT_TMP_DIR").filter(|d| !d.is_empty()) {
            return Some(PathBuf::from(dir));
        }
        let system = std::env::temp_dir();
        if !ram_backed(&system) {
            return None;
        }
        let cache = std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".cache")))
            .map(|c| c.join("prowlcat"));
        let chosen = [Some(PathBuf::from("/var/tmp")), cache].into_iter().flatten().find(|d| {
            std::fs::create_dir_all(d).is_ok() && !ram_backed(d) && tempfile::tempfile_in(d).is_ok()
        });
        match &chosen {
            Some(d) => warn!(from = %system.display(), to = %d.display(), "temporary directory is in RAM; parking results elsewhere"),
            None => warn!(dir = %system.display(), "temporary directory is in RAM; set PROWLCAT_TMP_DIR to a disk"),
        }
        chosen
    })
    .as_deref()
}

/// True when `path` lives on a tmpfs or ramfs mount (Linux only).
fn ram_backed(path: &Path) -> bool {
    let Ok(mounts) = std::fs::read_to_string("/proc/self/mounts") else { return false };
    let Ok(path) = path.canonicalize() else { return false };
    mounts
        .lines()
        .filter_map(|l| {
            let mut f = l.split_whitespace();
            let (_, mount, fstype) = (f.next()?, f.next()?, f.next()?);
            path.starts_with(mount).then_some((mount.len(), fstype))
        })
        .max_by_key(|(len, _)| *len)
        .is_some_and(|(_, t)| matches!(t, "tmpfs" | "ramfs"))
}

fn spill(inner: &mut Inner) -> Result<()> {
    inner.mem.sort_unstable_by_key(|i| i.key);
    let (file, start) = match inner.current.take() {
        Some((file, len)) if len < SPILL_FILE_MAX => (file, len),
        _ => {
            let file = match spill_dir() {
                Some(dir) => tempfile::tempfile_in(dir),
                None => tempfile::tempfile(),
            }
            .context("create a temporary file for results")?;
            (Arc::new(file), 0)
        }
    };
    // On failure `current` stays empty, so the next spill starts a fresh file and whatever was
    // half written here is never read.
    {
        let mut out = BufWriter::with_capacity(1 << 20, &*file);
        for item in &inner.mem {
            spill_codec().serialize_into(&mut out, item).context("write a result to disk")?;
        }
        out.flush()?;
    }
    let end = (&*file).stream_position().context("measure the temporary file")?;
    inner.current = Some((file.clone(), end));
    inner.runs.push(Run { file, start, len: end - start });
    inner.mem = Vec::new();
    inner.mem_bytes = 0;
    Ok(())
}

/// One worker's view of the collector while it scans a file.
pub(crate) struct Local<'a> {
    collector: &'a Collector,
    entry: u32,
    buf: Vec<Item>,
    bytes: usize,
    seq: u64,
}

impl Local<'_> {
    pub(crate) fn push(&mut self, hit: Hit) {
        let c = self.collector;
        let key = Key {
            query: c.query_rank[hit.query_idx],
            path: c.path_rank[self.entry as usize],
            line: hit.line_number,
            entry: self.entry,
            seq: self.seq,
        };
        self.seq += 1;
        self.bytes += hit.weight();
        self.buf.push(Item { key, hit });
        if self.bytes >= LOCAL_FLUSH {
            self.flush(true);
        }
    }

    fn flush(&mut self, partial: bool) {
        if self.buf.is_empty() {
            return;
        }
        let batch = std::mem::take(&mut self.buf);
        let bytes = std::mem::take(&mut self.bytes);
        self.collector.add(batch, bytes, partial.then_some(self.entry));
    }

    /// The file was read to the end.
    pub(crate) fn finish(mut self) {
        self.flush(false);
    }

    /// The file failed: whatever it found is ignored.
    pub(crate) fn abort(self) {
        self.collector.discard(self.entry);
    }
}

// ---- reading results back ---------------------------------------------------------------------

/// Scan results in output order, held in memory or spilled to disk. Reading them is repeatable.
pub struct SortedResults {
    summary: ScanSummary,
    backing: Backing,
}

enum Backing {
    Ready(Vec<ScanResult>),
    Spilled(Box<Spilled>),
}

struct Spilled {
    mem: Vec<Item>,
    runs: Vec<Run>,
    dropped: HashSet<u32>,
    entries: Arc<Vec<SourceEntry>>,
    queries: Vec<(String, QueryKind)>,
}

impl SortedResults {
    /// Wrap results that are already in memory (for example from an index), putting them in the
    /// standard output order.
    pub fn from_vec(mut results: Vec<ScanResult>, summary: ScanSummary) -> Self {
        results.sort_by(|a, b| {
            a.query
                .cmp(&b.query)
                .then(a.source.path.cmp(&b.source.path))
                .then(a.source.line_number.cmp(&b.source.line_number))
        });
        Self { summary, backing: Backing::Ready(results) }
    }

    pub fn summary(&self) -> &ScanSummary {
        &self.summary
    }

    pub fn summary_mut(&mut self) -> &mut ScanSummary {
        &mut self.summary
    }

    /// Number of matches.
    pub fn len(&self) -> usize {
        self.summary.matches
    }

    pub fn is_empty(&self) -> bool {
        self.summary.matches == 0
    }

    /// How many temporary files the results were spilled into (0 when they all fit in memory).
    pub fn spilled_runs(&self) -> usize {
        match &self.backing {
            Backing::Ready(_) => 0,
            Backing::Spilled(s) => s.runs.len(),
        }
    }

    /// The matches in output order. Each call starts from the beginning.
    pub fn iter(&self) -> Iter<'_> {
        match &self.backing {
            Backing::Ready(v) => Iter(Kind::Ready(v.iter())),
            Backing::Spilled(s) => match Merge::new(s) {
                Ok(m) => Iter(Kind::Merge(Box::new(m))),
                Err(e) => Iter(Kind::Failed(Some(e))),
            },
        }
    }
}

impl std::fmt::Debug for SortedResults {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SortedResults")
            .field("matches", &self.summary.matches)
            .field("spilled_runs", &self.spilled_runs())
            .finish()
    }
}

pub struct Iter<'a>(Kind<'a>);

enum Kind<'a> {
    Ready(std::slice::Iter<'a, ScanResult>),
    Merge(Box<Merge<'a>>),
    Failed(Option<anyhow::Error>),
}

impl Iterator for Iter<'_> {
    type Item = Result<ScanResult>;

    fn next(&mut self) -> Option<Self::Item> {
        match &mut self.0 {
            Kind::Ready(it) => it.next().cloned().map(Ok),
            Kind::Merge(m) => m.next(),
            Kind::Failed(e) => e.take().map(Err),
        }
    }
}

/// Positioned reads, so several readers can walk the same temporary file independently.
struct At<'a> {
    file: &'a File,
    pos: u64,
    end: u64,
}

impl Read for At<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let room = (self.end - self.pos).min(buf.len() as u64) as usize;
        if room == 0 {
            return Ok(0);
        }
        let n = read_at(self.file, &mut buf[..room], self.pos)?;
        self.pos += n as u64;
        Ok(n)
    }
}

#[cfg(unix)]
fn read_at(file: &File, buf: &mut [u8], pos: u64) -> io::Result<usize> {
    std::os::unix::fs::FileExt::read_at(file, buf, pos)
}

#[cfg(windows)]
fn read_at(file: &File, buf: &mut [u8], pos: u64) -> io::Result<usize> {
    std::os::windows::fs::FileExt::seek_read(file, buf, pos)
}

struct RunReader<'a> {
    reader: BufReader<At<'a>>,
}

impl RunReader<'_> {
    fn next(&mut self) -> Result<Option<Item>> {
        if self.reader.fill_buf().context("read results back from disk")?.is_empty() {
            return Ok(None);
        }
        Ok(Some(spill_codec().deserialize_from(&mut self.reader).context("decode a spilled result")?))
    }
}

/// Compact binary encoding of the items in a run (variable-length integers, no field names).
fn spill_codec() -> impl bincode::Options {
    bincode::DefaultOptions::new()
}

enum Source<'a> {
    Mem(std::slice::Iter<'a, Item>),
    Run(RunReader<'a>),
}

enum Head<'a> {
    Ref(&'a Item),
    Own(Item),
}

impl Head<'_> {
    fn item(&self) -> &Item {
        match self {
            Head::Ref(i) => i,
            Head::Own(i) => i,
        }
    }
}

/// K-way merge of the in-memory tail and every spilled run.
struct Merge<'a> {
    s: &'a Spilled,
    sources: Vec<Source<'a>>,
    heads: Vec<Option<Head<'a>>>,
    heap: BinaryHeap<Reverse<(Key, usize)>>,
    failed: bool,
}

impl<'a> Merge<'a> {
    fn new(s: &'a Spilled) -> Result<Self> {
        let mut sources = vec![Source::Mem(s.mem.iter())];
        for run in &s.runs {
            sources.push(Source::Run(RunReader {
                reader: BufReader::with_capacity(RUN_READ_BUFFER, At { file: &run.file, pos: run.start, end: run.start + run.len }),
            }));
        }
        let mut m = Self {
            s,
            heads: (0..sources.len()).map(|_| None).collect(),
            sources,
            heap: BinaryHeap::new(),
            failed: false,
        };
        for i in 0..m.sources.len() {
            m.advance(i)?;
        }
        Ok(m)
    }

    fn advance(&mut self, i: usize) -> Result<()> {
        let head = match &mut self.sources[i] {
            Source::Mem(it) => it.next().map(Head::Ref),
            Source::Run(r) => r.next()?.map(Head::Own),
        };
        if let Some(h) = &head {
            self.heap.push(Reverse((h.item().key, i)));
        }
        self.heads[i] = head;
        Ok(())
    }

    fn next(&mut self) -> Option<Result<ScanResult>> {
        loop {
            if self.failed {
                return None;
            }
            let Reverse((_, i)) = self.heap.pop()?;
            let head = self.heads[i].take()?;
            if let Err(e) = self.advance(i) {
                self.failed = true;
                return Some(Err(e));
            }
            let item = head.item();
            if self.s.dropped.contains(&item.key.entry) {
                continue;
            }
            let (query, kind) = &self.s.queries[item.hit.query_idx];
            let path = &self.s.entries[item.key.entry as usize].path;
            return Some(Ok(item.hit.to_result(query, *kind, path)));
        }
    }
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "linux")]
    #[test]
    fn ram_backed_detects_tmpfs() {
        assert!(!ram_backed(Path::new("/")));
        assert!(!ram_backed(Path::new("/nonexistent/path")));
        if Path::new("/dev/shm").exists() {
            assert!(ram_backed(Path::new("/dev/shm")));
        }
    }

    use super::*;
    use prowlcat_source::SourceEntry;
    use std::path::PathBuf;

    fn entry(path: &str) -> SourceEntry {
        SourceEntry {
            path: PathBuf::from(path),
            compression: prowlcat_source::CompressionKind::None,
            size_bytes: 0,
            archive_entry: None,
        }
    }

    fn hit(query_idx: usize, line: u64, text: &str) -> Hit {
        Hit {
            query_idx,
            match_kind: MatchKind::Exact,
            archive_entry: None,
            line_number: line,
            byte_offset: line * 10,
            line_text: text.to_string(),
            span: (0, 1),
            extracted: vec![ExtractedValue::Text(text.to_string())],
        }
    }

    /// Feed the same hits through a collector with the given budget.
    fn collect(budget: usize) -> SortedResults {
        let entries = Arc::new(vec![entry("/b/file2"), entry("/a/file1"), entry("/a/file1")]);
        let queries = vec![("zeta".to_string(), QueryKind::Text), ("alpha".to_string(), QueryKind::Text)];
        let c = Collector::new(&entries, &queries, budget);
        // Files finish in a different order from their index, as with parallel workers.
        for (e, hits) in [(2u32, vec![(1usize, 5u64), (0, 1)]), (0, vec![(0, 9), (1, 2), (1, 3)]), (1, vec![(1, 7)])] {
            let mut l = c.local(e);
            for (q, line) in hits {
                l.push(hit(q, line, &format!("e{e}q{q}l{line}")));
            }
            l.finish();
        }
        c.finish(entries, queries, ScanSummary::default())
    }

    fn order(r: &SortedResults) -> Vec<String> {
        r.iter().map(|x| x.unwrap().line_text).collect()
    }

    #[test]
    fn order_is_query_then_path_then_line() {
        let r = collect(usize::MAX);
        assert_eq!(r.spilled_runs(), 0);
        assert_eq!(r.len(), 6);
        // "alpha" (q1) before "zeta" (q0); /a/file1 (entries 1 and 2) before /b/file2 (entry 0);
        // within one query and path, by line.
        assert_eq!(order(&r), ["e2q1l5", "e1q1l7", "e0q1l2", "e0q1l3", "e2q0l1", "e0q0l9"]);
    }

    #[test]
    fn spilling_changes_nothing_but_memory() {
        let in_memory = order(&collect(usize::MAX));
        for budget in [0usize, 200, 600] {
            let r = collect(budget);
            assert_eq!(order(&r), in_memory, "budget {budget}");
        }
        assert!(collect(0).spilled_runs() > 0, "a zero budget forces runs onto disk");
    }

    #[test]
    fn reading_is_repeatable() {
        let r = collect(0);
        assert_eq!(order(&r), order(&r));
    }

    #[test]
    fn a_failed_file_contributes_nothing_even_after_a_partial_hand_over() {
        let entries = Arc::new(vec![entry("/a")]);
        let queries = vec![("q".to_string(), QueryKind::Text)];
        let c = Collector::new(&entries, &queries, usize::MAX);
        let mut l = c.local(0);
        l.push(hit(0, 1, "x"));
        l.flush(true);
        l.push(hit(0, 2, "y"));
        l.abort();
        let r = c.finish(entries, queries, ScanSummary::default());
        assert_eq!(r.len(), 0);
        assert!(order(&r).is_empty());
    }

    #[test]
    fn from_vec_uses_the_standard_order() {
        let mk = |q: &str, p: &str, line: u64| ScanResult {
            query: q.into(),
            kind: QueryKind::Text,
            match_kind: MatchKind::Exact,
            source: SourceLocation { path: p.into(), archive_entry: None, line_number: Some(line), byte_offset: 0 },
            line_text: format!("{q}{p}{line}"),
            matched_span: (0, 0),
            extracted: Vec::new(),
        };
        let r = SortedResults::from_vec(vec![mk("b", "x", 1), mk("a", "y", 2), mk("a", "x", 9), mk("a", "x", 3)], ScanSummary::default());
        assert_eq!(order(&r), ["ax3", "ax9", "ay2", "bx1"]);
    }
}
