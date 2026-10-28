//! Optional Tantivy index for repeated queries over the same corpus (`prowlcat index`, `search --index`).
//!
//! Only lines containing at least one email, domain or IP are indexed, and each is stored verbatim
//! with its location. A search uses the index to pick candidate lines and then re-runs the regular
//! [`MatchEngine`] on them, so hits, spans and extracted values are identical to a full scan.
//! Plain-text (`--loose`) queries cannot be answered from term indexes and are rejected.

mod terms;

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv6Addr};
use std::ops::Bound;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{bail, Context, Result};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use tantivy::collector::DocSetCollector;
use tantivy::query::{BooleanQuery, Occur, Query as TantivyQuery, RangeQuery, TermQuery};
use tantivy::schema::{Field, IndexRecordOption, IpAddrOptions, Schema, Value, STORED, STRING};
use tantivy::{doc, DocAddress, Index, TantivyDocument, Term};
use tracing::{debug, warn};

use prowlcat_decode::LineReader;
use prowlcat_match::MatchEngine;
use prowlcat_source::SourceEnumerator;
use prowlcat_types::{Query, QueryKind, ScanResult, ScanSummary, SourceLocation};

use terms::extract_terms;

const META_FILE: &str = "prowlcat-index.json";
const WRITER_HEAP: usize = 256 * 1024 * 1024;

#[derive(Clone, Copy)]
struct Fields {
    tok: Field,
    ip: Field,
    path: Field,
    entry: Field,
    line_no: Field,
    offset: Field,
    line: Field,
}

fn schema() -> (Schema, Fields) {
    let mut b = Schema::builder();
    let ip_opts = IpAddrOptions::default().set_indexed().set_fast();
    let fields = Fields {
        tok: b.add_text_field("tok", STRING),
        ip: b.add_ip_addr_field("ip", ip_opts),
        path: b.add_text_field("path", STORED),
        entry: b.add_text_field("entry", STORED),
        line_no: b.add_u64_field("line_no", STORED),
        offset: b.add_u64_field("offset", STORED),
        line: b.add_text_field("line", STORED),
    };
    (b.build(), fields)
}

/// Totals recorded when the index was built.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IndexMeta {
    pub files: usize,
    pub bytes: u64,
    pub lines: u64,
    pub docs: u64,
    pub errors: usize,
}

#[derive(Debug, Clone)]
pub struct IndexBuildConfig {
    pub paths: Vec<PathBuf>,
    pub recursive: bool,
    pub threads: usize,
    pub mmap_threshold_mb: u64,
    pub passwords: Vec<String>,
    /// Read SQLite databases, BSON dumps and JSON arrays record by record (see `LineReader`).
    pub structured: bool,
}

fn ipv6(ip: IpAddr) -> Ipv6Addr {
    match ip {
        IpAddr::V4(v4) => v4.to_ipv6_mapped(),
        IpAddr::V6(v6) => v6,
    }
}

/// Build an index of `cfg.paths` in the (new or empty) directory `out`.
pub fn build_index(cfg: &IndexBuildConfig, out: &Path) -> Result<IndexMeta> {
    if out.join(META_FILE).exists() {
        bail!("{} already holds an index; remove it first", out.display());
    }
    std::fs::create_dir_all(out).with_context(|| format!("create {}", out.display()))?;
    if std::fs::read_dir(out)?.next().is_some() {
        bail!("{} is not empty", out.display());
    }

    let entries = SourceEnumerator::new(&cfg.paths, cfg.recursive)?.collect()?;
    let (schema, f) = schema();
    let index = Index::create_in_dir(out, schema)?;
    let writer = index.writer_with_num_threads(cfg.threads.max(1), WRITER_HEAP)?;

    let (lines, docs, errors) = (AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0));
    let mmap_threshold = cfg.mmap_threshold_mb * 1024 * 1024;
    let pool = rayon::ThreadPoolBuilder::new().num_threads(cfg.threads.max(1)).build()?;

    pool.install(|| {
        entries.par_iter().for_each(|entry| {
            let mut tokens = HashSet::new();
            let mut ips = HashSet::new();
            let mut run = || -> Result<()> {
                let mut reader =
                    LineReader::open(&entry.path, entry.compression, mmap_threshold)?
                        .with_passwords(&cfg.passwords)
                        .with_structured(cfg.structured);
                let path = entry.path.to_string_lossy().into_owned();
                reader.for_each_line(|member, line_no, offset, line| {
                    lines.fetch_add(1, Ordering::Relaxed);
                    extract_terms(line, &mut tokens, &mut ips);
                    if tokens.is_empty() && ips.is_empty() {
                        return;
                    }
                    let entry_name = member.map(str::to_string);
                    let mut d = doc!(
                        f.path => path.as_str(),
                        f.line_no => line_no,
                        f.offset => offset,
                        f.line => String::from_utf8_lossy(line).as_ref(),
                    );
                    if let Some(name) = &entry_name {
                        d.add_text(f.entry, name);
                    }
                    for t in &tokens {
                        d.add_text(f.tok, t);
                    }
                    for ip in &ips {
                        d.add_ip_addr(f.ip, ipv6(*ip));
                    }
                    match writer.add_document(d) {
                        Ok(_) => {
                            docs.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(e) => warn!(error = %e, "failed to index line"),
                    }
                })
            };
            if let Err(e) = run() {
                warn!(path = %entry.path.display(), error = %e, "indexing failed");
                errors.fetch_add(1, Ordering::Relaxed);
            }
        });
    });

    let mut writer = writer;
    writer.commit()?;
    writer.wait_merging_threads()?;

    let meta = IndexMeta {
        files: entries.len(),
        bytes: entries.iter().map(|e| e.size_bytes).sum(),
        lines: lines.load(Ordering::Relaxed),
        docs: docs.load(Ordering::Relaxed),
        errors: errors.load(Ordering::Relaxed) as usize,
    };
    std::fs::write(out.join(META_FILE), serde_json::to_vec_pretty(&meta)?)?;
    Ok(meta)
}

pub struct IndexSearcher {
    index: Index,
    fields: Fields,
    meta: IndexMeta,
}

impl IndexSearcher {
    pub fn open(dir: &Path) -> Result<Self> {
        let meta_bytes = std::fs::read(dir.join(META_FILE))
            .with_context(|| format!("{} is not a ProwlCat index", dir.display()))?;
        let meta = serde_json::from_slice(&meta_bytes)?;
        let index = Index::open_in_dir(dir)?;
        let (_, fields) = schema();
        Ok(Self { index, fields, meta })
    }

    pub fn meta(&self) -> &IndexMeta {
        &self.meta
    }

    /// Answer `queries` from the index. Results match a full scan of the indexed corpus.
    pub fn search(
        &self,
        queries: &[Query],
        include_subdomains: bool,
        max_matches: u64,
    ) -> Result<(Vec<ScanResult>, ScanSummary)> {
        if let Some(q) = queries.iter().find(|q| q.kind == QueryKind::Text) {
            bail!("text query {:?} cannot use --index (term indexes only cover emails, domains, IPs and CIDRs)", q.raw);
        }
        let f = self.fields;
        let engine = MatchEngine::build(queries, include_subdomains)?;
        let searcher = self.index.reader()?.searcher();

        // Candidate lines, each with the queries the index nominated it for.
        let mut candidates: HashMap<DocAddress, Vec<usize>> = HashMap::new();
        for (qi, q) in queries.iter().enumerate() {
            let tantivy_query = self.candidate_query(q);
            for addr in searcher.search(&*tantivy_query, &DocSetCollector)? {
                candidates.entry(addr).or_default().push(qi);
            }
        }
        debug!(candidates = candidates.len(), "index candidates");

        let mut results = Vec::new();
        for (addr, wanted) in candidates {
            let d: TantivyDocument = searcher.doc(addr)?;
            let text = |field: Field| d.get_first(field).and_then(|v| v.as_str());
            let num = |field: Field| d.get_first(field).and_then(|v| v.as_u64());
            let (Some(path), Some(line)) = (text(f.path), text(f.line)) else { continue };
            for hit in engine.match_line(line.as_bytes()) {
                if !wanted.contains(&hit.query_index) {
                    continue;
                }
                results.push(ScanResult {
                    query: engine.query_raw(hit.query_index).to_string(),
                    kind: engine.query_kind(hit.query_index),
                    match_kind: hit.match_kind,
                    source: SourceLocation {
                        path: PathBuf::from(path),
                        archive_entry: text(f.entry).map(str::to_string),
                        line_number: num(f.line_no),
                        byte_offset: num(f.offset).unwrap_or(0),
                    },
                    line_text: line.to_string(),
                    matched_span: hit.span,
                    extracted: hit.extracted,
                });
            }
        }

        // Deterministic order so the per-query cap keeps the same matches on every run.
        results.sort_by(|a, b| {
            (&a.query, &a.source.path, &a.source.archive_entry, a.source.line_number).cmp(&(
                &b.query,
                &b.source.path,
                &b.source.archive_entry,
                b.source.line_number,
            ))
        });
        if max_matches > 0 {
            let mut seen: HashMap<String, u64> = HashMap::new();
            results.retain(|r| {
                let n = seen.entry(r.query.clone()).or_default();
                *n += 1;
                *n <= max_matches
            });
        }

        let summary = ScanSummary {
            targets: queries.len(),
            files_scanned: self.meta.files,
            bytes_scanned: self.meta.bytes,
            matches: results.len(),
            elapsed_ms: 0,
            errors: self.meta.errors,
        };
        Ok((results, summary))
    }

    fn candidate_query(&self, q: &Query) -> Box<dyn TantivyQuery> {
        let f = self.fields;
        match (q.kind, &q.network, &q.address) {
            (QueryKind::Cidr, Some(net), _) => {
                let lo = Term::from_field_ip_addr(f.ip, ipv6(net.network()));
                let hi = Term::from_field_ip_addr(f.ip, ipv6(net.broadcast()));
                Box::new(RangeQuery::new(Bound::Included(lo), Bound::Included(hi)))
            }
            (QueryKind::Ip, _, Some(addr)) => {
                Box::new(TermQuery::new(Term::from_field_ip_addr(f.ip, ipv6(*addr)), IndexRecordOption::Basic))
            }
            _ => {
                let tok = Term::from_field_text(f.tok, &q.normalized.to_ascii_lowercase());
                let by_token: Box<dyn TantivyQuery> = Box::new(TermQuery::new(tok, IndexRecordOption::Basic));
                if q.kind == QueryKind::Ip {
                    // An IP query whose address failed to parse still matches as a token.
                    return Box::new(BooleanQuery::new(vec![(Occur::Should, by_token)]));
                }
                by_token
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query(raw: &str, kind: QueryKind) -> Query {
        let normalized = raw.to_ascii_lowercase();
        let network = (kind == QueryKind::Cidr).then(|| normalized.parse().unwrap());
        let address = (kind == QueryKind::Ip).then(|| normalized.parse().unwrap());
        Query { raw: raw.into(), normalized, kind, network, address }
    }

    fn corpus() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("a.txt"),
            "bob@example.com:hunter2\nnoise line\nalice@mail.example.com:pw\n10.1.2.3 admin\n192.168.5.5 other\n",
        )
        .unwrap();
        dir
    }

    fn build(corpus: &Path, out: &Path) -> IndexMeta {
        let cfg = IndexBuildConfig {
            paths: vec![corpus.to_path_buf()],
            recursive: true,
            threads: 2,
            mmap_threshold_mb: 0,
            passwords: Vec::new(),
            structured: true,
        };
        build_index(&cfg, out).unwrap()
    }

    #[test]
    fn indexed_search_matches_expectations() {
        let corpus = corpus();
        let out = tempfile::tempdir().unwrap();
        let index_dir = out.path().join("idx");
        let meta = build(corpus.path(), &index_dir);
        assert_eq!(meta.lines, 5);
        assert_eq!(meta.docs, 4, "the noise line is not indexed");

        let s = IndexSearcher::open(&index_dir).unwrap();
        let q = |r: &str, k| vec![query(r, k)];

        let (hits, _) = s.search(&q("bob@example.com", QueryKind::Email), true, 0).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].source.line_number, Some(0));

        let (hits, _) = s.search(&q("example.com", QueryKind::Domain), true, 0).unwrap();
        // Same result as a full scan: subdomains are included, so `@mail.example.com` counts too.
        assert_eq!(hits.len(), 2);

        let (hits, _) = s.search(&q("mail.example.com", QueryKind::Domain), true, 0).unwrap();
        assert_eq!(hits.len(), 1);

        let (hits, _) = s.search(&q("10.0.0.0/8", QueryKind::Cidr), true, 0).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].source.line_number, Some(3));

        let (hits, _) = s.search(&q("192.168.5.5", QueryKind::Ip), true, 0).unwrap();
        assert_eq!(hits.len(), 1);

        let (hits, _) = s.search(&q("10.0.0.0/8", QueryKind::Cidr), true, 1).unwrap();
        assert_eq!(hits.len(), 1, "max_matches caps per query");

        assert!(s.search(&q("hunter", QueryKind::Text), true, 0).is_err());
    }

    #[test]
    fn refuses_to_overwrite_an_index() {
        let corpus = corpus();
        let out = tempfile::tempdir().unwrap();
        let index_dir = out.path().join("idx");
        build(corpus.path(), &index_dir);
        let cfg = IndexBuildConfig {
            paths: vec![corpus.path().to_path_buf()],
            recursive: true,
            threads: 1,
            mmap_threshold_mb: 0,
            passwords: Vec::new(),
            structured: true,
        };
        assert!(build_index(&cfg, &index_dir).is_err());
    }
}
