//! Background jobs. The UI thread never blocks: each job runs on its own thread and reports
//! back through a channel, tagged with the job id so answers from cancelled jobs can be dropped.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::time::Instant;

use anyhow::Result;
use prowlcat_core::{ProgressHandle, Query, ScanConfig, ScanOrchestrator, ScanResult, ScanSummary, SortedResults};
use prowlcat_index::{IndexBuildConfig, IndexMeta, IndexSearcher};
use prowlcat_source::{CompressionKind, SourceEnumerator};

/// How many matches the results screen holds in memory. A search that finds more still counts
/// and exports all of them; the screen shows the first ones, in the usual order.
pub const RESULTS_KEPT: usize = 250_000;

#[derive(Debug)]
pub struct Msg {
    pub job: u64,
    pub kind: MsgKind,
}

#[derive(Debug)]
pub enum MsgKind {
    /// `results` is at most [`RESULTS_KEPT`] matches; `full` holds all of them when there are more.
    SearchDone { results: Vec<ScanResult>, summary: ScanSummary, full: Option<Box<SortedResults>> },
    Stats(CorpusStats),
    IndexBuilt { meta: IndexMeta, dir: PathBuf, elapsed_ms: u64 },
    Failed(String),
}

/// What a corpus looks like, by file kind.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CorpusStats {
    pub files: usize,
    pub bytes: u64,
    pub plain: usize,
    pub gzip: usize,
    pub zstd: usize,
    pub tar: usize,
    pub zip: usize,
}

#[derive(Debug, Clone)]
pub struct SearchSpec {
    pub queries: Vec<Query>,
    pub paths: Vec<PathBuf>,
    pub index: Option<PathBuf>,
    pub recursive: bool,
    pub threads: Option<usize>,
    pub max_matches: u64,
    pub passwords: Vec<String>,
    pub progress: ProgressHandle,
}

fn spawn(name: &str, job: u64, tx: Sender<Msg>, work: impl FnOnce() -> Result<MsgKind> + Send + 'static) {
    let builder = std::thread::Builder::new().name(format!("prowlcat-{name}"));
    let spawned = builder.spawn({
        let tx = tx.clone();
        move || {
            let kind = match catch_unwind(AssertUnwindSafe(work)) {
                Ok(Ok(kind)) => kind,
                Ok(Err(e)) => MsgKind::Failed(format!("{e:#}")),
                Err(_) => MsgKind::Failed("internal error: the worker thread panicked".into()),
            };
            // The receiver is gone only when the UI has already quit.
            let _ = tx.send(Msg { job, kind });
        }
    });
    if let Err(e) = spawned {
        let _ = tx.send(Msg { job, kind: MsgKind::Failed(format!("could not start a worker thread: {e}")) });
    }
}

pub fn spawn_search(job: u64, spec: SearchSpec, tx: Sender<Msg>) {
    spawn("scan", job, tx, move || {
        let start = Instant::now();
        // Already in the same stable order as the command line, so results read the same in both
        // modes. Matches beyond the memory budget wait in temporary files.
        let mut sorted = run_search(spec)?;
        sorted.summary_mut().elapsed_ms = start.elapsed().as_millis() as u64;
        let mut results = Vec::new();
        for r in sorted.iter().take(RESULTS_KEPT) {
            results.push(r?);
        }
        let summary = sorted.summary().clone();
        let full = (sorted.len() > results.len()).then(|| Box::new(sorted));
        Ok(MsgKind::SearchDone { results, summary, full })
    });
}

fn run_search(spec: SearchSpec) -> Result<SortedResults> {
    let defaults = ScanConfig::default();
    if let Some(dir) = &spec.index {
        let searcher = IndexSearcher::open(dir)?;
        let (results, summary) = searcher.search(&spec.queries, defaults.include_subdomains, spec.max_matches)?;
        return Ok(SortedResults::from_vec(results, summary));
    }
    let config = ScanConfig {
        paths: spec.paths,
        recursive: spec.recursive,
        threads: spec.threads.unwrap_or(defaults.threads),
        max_matches_per_query: spec.max_matches,
        passwords: spec.passwords,
        progress: Some(spec.progress),
        ..defaults
    };
    ScanOrchestrator::new(config, spec.queries).run_sorted()
}

pub fn spawn_stats(job: u64, paths: Vec<PathBuf>, recursive: bool, tx: Sender<Msg>) {
    spawn("stats", job, tx, move || {
        let entries = SourceEnumerator::new(&paths, recursive)?.collect()?;
        let count = |f: fn(CompressionKind) -> bool| entries.iter().filter(|e| f(e.compression)).count();
        Ok(MsgKind::Stats(CorpusStats {
            files: entries.len(),
            bytes: entries.iter().map(|e| e.size_bytes).sum(),
            plain: count(|k| k == CompressionKind::None),
            gzip: count(|k| k == CompressionKind::Gzip),
            zstd: count(|k| k == CompressionKind::Zstd),
            tar: count(|k| matches!(k, CompressionKind::Tar | CompressionKind::TarGz)),
            zip: count(|k| k == CompressionKind::Zip),
        }))
    });
}

pub fn spawn_index_build(
    job: u64,
    paths: Vec<PathBuf>,
    recursive: bool,
    threads: Option<usize>,
    passwords: Vec<String>,
    out: PathBuf,
    tx: Sender<Msg>,
) {
    spawn("index", job, tx, move || {
        let defaults = ScanConfig::default();
        let cfg = IndexBuildConfig {
            paths,
            recursive,
            threads: threads.unwrap_or(defaults.threads),
            mmap_threshold_mb: defaults.mmap_threshold_mb,
            passwords,
        };
        let start = Instant::now();
        let meta = prowlcat_index::build_index(&cfg, &out)?;
        Ok(MsgKind::IndexBuilt { meta, dir: out, elapsed_ms: start.elapsed().as_millis() as u64 })
    });
}

/// Read an index's totals without searching it.
pub fn index_meta(dir: &std::path::Path) -> Result<IndexMeta> {
    Ok(IndexSearcher::open(dir)?.meta().clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use prowlcat_core::{TargetParser, TypeHint};
    use std::sync::mpsc::channel;
    use std::time::Duration;

    fn corpus() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "alice@example.com:pw\nnoise\nbob@example.com:pw2\n").unwrap();
        std::fs::write(dir.path().join("b.txt.gz"), b"not really gzip").unwrap();
        dir
    }

    fn spec(paths: Vec<PathBuf>, targets: &[&str]) -> SearchSpec {
        SearchSpec {
            queries: TargetParser::new(TypeHint::Email).parse_many(targets.iter().copied()).unwrap(),
            paths,
            index: None,
            recursive: true,
            threads: Some(2),
            max_matches: 0,
            passwords: Vec::new(),
            progress: ProgressHandle::default(),
        }
    }

    fn recv(rx: &std::sync::mpsc::Receiver<Msg>) -> Msg {
        rx.recv_timeout(Duration::from_secs(20)).expect("worker answered")
    }

    #[test]
    fn search_reports_results_sorted_and_tagged_with_the_job() {
        let dir = corpus();
        let (tx, rx) = channel();
        spawn_search(7, spec(vec![dir.path().to_path_buf()], &["bob@example.com", "alice@example.com"]), tx);
        let msg = recv(&rx);
        assert_eq!(msg.job, 7);
        let MsgKind::SearchDone { results, summary, full } = msg.kind else { panic!("expected SearchDone, got {:?}", msg.kind) };
        assert!(full.is_none(), "a small result set is held entirely in memory");
        assert_eq!(summary.matches, 2);
        assert_eq!(results[0].query, "alice@example.com", "sorted by query");
    }

    #[test]
    fn live_matches_are_streamed_while_the_search_runs() {
        let dir = corpus();
        let (live_tx, live_rx) = std::sync::mpsc::sync_channel(64);
        let mut s = spec(vec![dir.path().to_path_buf()], &["alice@example.com"]);
        s.progress.live = Some(live_tx);
        let (tx, rx) = channel();
        spawn_search(1, s, tx);
        let MsgKind::SearchDone { results, .. } = recv(&rx).kind else { panic!() };
        assert_eq!(live_rx.try_iter().count(), results.len());
    }

    #[test]
    fn failures_come_back_as_messages_not_panics() {
        let (tx, rx) = channel();
        let mut s = spec(vec![PathBuf::from("/definitely/not/here")], &["a@b.com"]);
        s.index = None;
        spawn_search(2, s, tx);
        assert!(matches!(recv(&rx).kind, MsgKind::Failed(_)));
    }

    #[test]
    fn a_missing_index_is_reported() {
        let (tx, rx) = channel();
        let mut s = spec(Vec::new(), &["a@b.com"]);
        s.index = Some(PathBuf::from("/definitely/not/an/index"));
        spawn_search(3, s, tx);
        let MsgKind::Failed(msg) = recv(&rx).kind else { panic!() };
        assert!(msg.contains("not a ProwlCat index"), "{msg}");
    }

    #[test]
    fn stats_count_files_by_kind() {
        let dir = corpus();
        let (tx, rx) = channel();
        spawn_stats(4, vec![dir.path().to_path_buf()], true, tx);
        let MsgKind::Stats(s) = recv(&rx).kind else { panic!() };
        assert_eq!(s.files, 2);
        assert_eq!(s.plain, 1);
        assert_eq!(s.gzip, 1);
        assert!(s.bytes > 0);
    }

    #[test]
    fn index_build_then_search_through_the_index() {
        let dir = corpus();
        let out = tempfile::tempdir().unwrap().keep().join("idx");
        let (tx, rx) = channel();
        spawn_index_build(5, vec![dir.path().to_path_buf()], true, Some(1), Vec::new(), out.clone(), tx.clone());
        let MsgKind::IndexBuilt { meta, dir: built, .. } = recv(&rx).kind else { panic!() };
        assert_eq!(built, out);
        assert_eq!(meta.docs, 2);
        assert_eq!(index_meta(&out).unwrap().docs, 2);

        let mut s = spec(Vec::new(), &["alice@example.com"]);
        s.index = Some(out);
        spawn_search(6, s, tx);
        let MsgKind::SearchDone { summary, .. } = recv(&rx).kind else { panic!() };
        assert_eq!(summary.matches, 1);
    }
}
