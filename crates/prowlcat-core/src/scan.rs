use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::Result;
use rayon::prelude::*;
use tracing::{debug, warn};

use crate::config::ScanConfig;
use prowlcat_types::{Query, ScanResult, ScanSummary};
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

        let results: Vec<ScanResult> = pool.install(|| {
            entries
                .par_iter()
                .flat_map(|entry| {
                    match scan_entry(entry, &engine, mmap_threshold, max_matches, &self.config.passwords, self.config.progress.as_ref(), &bytes_scanned) {
                        Ok(matches) => {
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
}

fn scan_entry(
    entry: &SourceEntry,
    engine: &MatchEngine,
    mmap_threshold: u64,
    max_matches: u64,
    passwords: &[String],
    progress: Option<&crate::config::ProgressHandle>,
    bytes_scanned: &Arc<AtomicU64>,
) -> Result<Vec<ScanResult>> {
    debug!(path = %entry.path.display(), compression = ?entry.compression, "scanning file");

    let mut out = Vec::new();
    let mut per_query: Vec<u64> = vec![0; engine.query_count()];

    let mut reader = LineReader::open(&entry.path, entry.compression, mmap_threshold)?.with_passwords(passwords);
    if let Some(p) = progress {
        reader = reader.with_progress(p.done.clone());
    }
    reader.for_each_line(|member, line_no, offset, line: &[u8]| {
        bytes_scanned.fetch_add(line.len() as u64, Ordering::Relaxed);
        let hits = engine.match_line(line);
        for hit in hits {
            let idx = hit.query_index;
            if max_matches > 0 && per_query[idx] >= max_matches {
                continue;
            }
            per_query[idx] += 1;
            out.push(ScanResult {
                query: engine.query_raw(idx).to_string(),
                kind: engine.query_kind(idx),
                match_kind: hit.match_kind,
                source: prowlcat_types::SourceLocation {
                    path: entry.path.clone(),
                    archive_entry: member.map(str::to_string).or_else(|| entry.archive_entry.clone()),
                    line_number: Some(line_no),
                    byte_offset: offset,
                },
                line_text: String::from_utf8_lossy(line).into_owned(),
                matched_span: hit.span,
                extracted: hit.extracted,
            });
        }
    })?;

    Ok(out)
}
