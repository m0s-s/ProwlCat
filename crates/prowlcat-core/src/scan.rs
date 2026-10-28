use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::Result;
use rayon::prelude::*;
use tracing::{debug, warn};

use crate::config::ScanConfig;
use crate::sorted::{Collector, Hit, SortedResults};
use prowlcat_types::{Query, QueryKind, ScanResult, ScanSummary};
use prowlcat_decode::LineReader;
use prowlcat_match::MatchEngine;
use prowlcat_source::{SourceEntry, SourceEnumerator};

pub struct ScanOrchestrator {
    config: ScanConfig,
    queries: Vec<Query>,
}

impl ScanOrchestrator {
    pub fn new(config: ScanConfig, queries: Vec<Query>) -> Self {
        Self { config, queries }
    }

    /// Scan and return every match in memory, in scan order. Fine for small result sets; for
    /// corpora that can match millions of lines use [`run_sorted`](Self::run_sorted).
    pub fn run(&self) -> Result<(Vec<ScanResult>, ScanSummary)> {
        let entries = SourceEnumerator::new(&self.config.paths, self.config.recursive)?.collect()?;
        let engine = MatchEngine::build(&self.queries, self.config.include_subdomains)?;

        if let Some(p) = &self.config.progress {
            p.total.store(entries.iter().map(|e| e.size_bytes).sum(), Ordering::Relaxed);
        }

        let bytes_scanned = Arc::new(AtomicU64::new(0));
        let errors = Arc::new(AtomicU64::new(0));

        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(self.config.threads)
            .build()?;

        let max_matches = self.config.max_matches_per_query;
        let mmap_threshold = self.config.mmap_threshold_mb * 1024 * 1024;
        let shape = Shape { max_line: self.config.max_line_bytes, snippet: self.config.snippet_bytes };

        let results: Vec<ScanResult> = pool.install(|| {
            entries
                .par_iter()
                .flat_map(|entry| {
                    if let Some(p) = &self.config.progress {
                        if p.cancel.load(Ordering::Relaxed) {
                            return Vec::new();
                        }
                    }
                    let mut matches = Vec::new();
                    let outcome = scan_entry(
                        entry,
                        &engine,
                        mmap_threshold,
                        shape,
                        max_matches,
                        &self.config.passwords,
                        self.config.progress.as_ref(),
                        &bytes_scanned,
                        &mut |hit| {
                            let idx = hit.query_idx;
                            matches.push(hit.into_result(engine.query_raw(idx), engine.query_kind(idx), &entry.path));
                            true
                        },
                    );
                    match outcome {
                        Ok(()) => {
                            if let Some(p) = &self.config.progress {
                                p.files_done.fetch_add(1, Ordering::Relaxed);
                                p.matches.fetch_add(matches.len() as u64, Ordering::Relaxed);
                            }
                            matches
                        }
                        Err(e) => {
                            warn!(path = %entry.path.display(), error = %e, "scan failed");
                            errors.fetch_add(1, Ordering::Relaxed);
                            Vec::new()
                        }
                    }
                })
                .collect()
        });

        let summary = ScanSummary {
            targets: self.queries.len(),
            files_scanned: entries.len(),
            bytes_scanned: bytes_scanned.load(Ordering::Relaxed),
            matches: results.len(),
            elapsed_ms: 0,
            errors: errors.load(Ordering::Relaxed) as usize,
        };

        Ok((results, summary))
    }

    /// Scan with bounded memory and return the matches in the standard output order (query,
    /// then path, then line). The matches found so far are kept in at most
    /// `config.result_memory_mb` of RAM; beyond that they wait in temporary files, so the size of
    /// the result set is limited by free disk space, not by memory. The matches and their order
    /// are the ones [`run`](Self::run) followed by a stable sort would give.
    pub fn run_sorted(&self) -> Result<SortedResults> {
        let entries = Arc::new(SourceEnumerator::new(&self.config.paths, self.config.recursive)?.collect()?);
        let engine = MatchEngine::build(&self.queries, self.config.include_subdomains)?;
        let queries: Vec<(String, QueryKind)> =
            (0..engine.query_count()).map(|i| (engine.query_raw(i).to_string(), engine.query_kind(i))).collect();

        let progress = self.config.progress.as_ref();
        if let Some(p) = progress {
            p.total.store(entries.iter().map(|e| e.size_bytes).sum(), Ordering::Relaxed);
        }

        let bytes_scanned = Arc::new(AtomicU64::new(0));
        let errors = Arc::new(AtomicU64::new(0));
        let pool = rayon::ThreadPoolBuilder::new().num_threads(self.config.threads).build()?;
        let max_matches = self.config.max_matches_per_query;
        let mmap_threshold = self.config.mmap_threshold_mb * 1024 * 1024;
        let shape = Shape { max_line: self.config.max_line_bytes, snippet: self.config.snippet_bytes };
        let budget = usize::try_from(self.config.result_memory_mb.saturating_mul(1024 * 1024)).unwrap_or(usize::MAX);
        let collector = Collector::new(&entries, &queries, budget);

        pool.install(|| {
            entries.par_iter().enumerate().for_each(|(idx, entry)| {
                if progress.is_some_and(|p| p.cancel.load(Ordering::Relaxed)) || collector.failed() {
                    return;
                }
                let mut local = collector.local(idx as u32);
                let outcome = scan_entry(
                    entry,
                    &engine,
                    mmap_threshold,
                    shape,
                    max_matches,
                    &self.config.passwords,
                    progress,
                    &bytes_scanned,
                    &mut |hit| {
                        if let Some(p) = progress {
                            p.matches.fetch_add(1, Ordering::Relaxed);
                        }
                        local.push(hit)
                    },
                );
                match outcome {
                    Ok(()) => {
                        local.finish();
                        if let Some(p) = progress {
                            p.files_done.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    Err(e) => {
                        warn!(path = %entry.path.display(), error = %e, "scan failed");
                        errors.fetch_add(1, Ordering::Relaxed);
                        local.abort();
                    }
                }
            });
        });

        let summary = ScanSummary {
            targets: self.queries.len(),
            files_scanned: entries.len(),
            bytes_scanned: bytes_scanned.load(Ordering::Relaxed),
            matches: 0,
            elapsed_ms: 0,
            errors: errors.load(Ordering::Relaxed) as usize,
        };
        collector.finish(entries, queries, summary)
    }
}

/// How long lines are read and reported (see [`ScanConfig::max_line_bytes`] and
/// [`ScanConfig::snippet_bytes`]).
#[derive(Clone, Copy)]
struct Shape {
    max_line: usize,
    snippet: usize,
}

/// `ctx` bytes of context on each side of `span`, cut on character boundaries, with `…` marking
/// each side where the line goes on. Returns the text and the span inside it. A line that fits
/// in the window comes back unchanged.
fn snippet_of(text: &str, span: (usize, usize), ctx: usize) -> (String, (usize, usize)) {
    let (ms, me) = (span.0.min(text.len()), span.1.min(text.len()));
    let mut start = ms.saturating_sub(ctx);
    while !text.is_char_boundary(start) {
        start += 1;
    }
    let mut end = (me + ctx).min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    if start == 0 && end == text.len() {
        return (text.to_string(), span);
    }
    let mut out = String::with_capacity(end - start + 6);
    if start > 0 {
        out.push('…');
    }
    let shift = out.len();
    out.push_str(&text[start..end]);
    if end < text.len() {
        out.push('…');
    }
    (out, (shift + ms - start, shift + me - start))
}

#[allow(clippy::too_many_arguments)]
fn scan_entry(
    entry: &SourceEntry,
    engine: &MatchEngine,
    mmap_threshold: u64,
    shape: Shape,
    max_matches: u64,
    passwords: &[String],
    progress: Option<&crate::config::ProgressHandle>,
    bytes_scanned: &Arc<AtomicU64>,
    emit: &mut dyn FnMut(Hit) -> bool,
) -> Result<()> {
    debug!(path = %entry.path.display(), compression = ?entry.compression, "scanning file");

    let mut per_query: Vec<u64> = vec![0; engine.query_count()];

    let mut reader = LineReader::open(&entry.path, entry.compression, mmap_threshold)?
        .with_passwords(passwords)
        .with_max_line_bytes(shape.max_line);
    if let Some(p) = progress {
        reader = reader.with_progress(p.done.clone());
    }
    // Cancellation is polled every 1024 lines so the hot loop stays free of atomic loads.
    let cancel = progress.map(|p| p.cancel.clone());
    let live = progress.and_then(|p| p.live.as_ref());
    let mut stopped = false;
    reader.for_each_piece(|member, line_no, offset, skip, line: &[u8]| {
        if line_no & 0x3FF == 0 {
            if let Some(c) = &cancel {
                stopped = c.load(Ordering::Relaxed);
            }
        }
        if stopped {
            return;
        }
        bytes_scanned.fetch_add((line.len() - skip) as u64, Ordering::Relaxed);
        let hits = engine.match_line(line);
        if hits.is_empty() {
            return;
        }
        // The first `skip` bytes of a piece of a long line were also the end of the previous
        // piece; a match wholly inside them was reported there.
        let seen = if skip > 0 { String::from_utf8_lossy(&line[..skip]).len() } else { 0 };
        let text = String::from_utf8_lossy(line);
        for hit in hits {
            if skip > 0 && hit.span.1 <= seen {
                continue;
            }
            let idx = hit.query_index;
            if max_matches > 0 && per_query[idx] >= max_matches {
                continue;
            }
            per_query[idx] += 1;
            let (line_text, span) = if shape.snippet > 0 {
                snippet_of(&text, hit.span, shape.snippet)
            } else {
                (text.to_string(), hit.span)
            };
            let hit = Hit {
                query_idx: idx,
                match_kind: hit.match_kind,
                archive_entry: member.map(str::to_string).or_else(|| entry.archive_entry.clone()),
                line_number: line_no,
                byte_offset: offset,
                line_text,
                span,
                extracted: hit.extracted,
            };
            // Hits reach a watching UI as they are found, even inside one huge file. A full
            // channel or a closed receiver just means the UI is not keeping up or has left.
            if let Some(tx) = live {
                let _ = tx.try_send(hit.to_result(engine.query_raw(idx), engine.query_kind(idx), &entry.path));
            }
            if !emit(hit) {
                stopped = true;
                return;
            }
        }
    })?;

    Ok(())
}
