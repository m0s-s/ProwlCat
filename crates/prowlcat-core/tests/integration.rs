//! Integration smoke tests against bundled fixtures.

use std::path::PathBuf;

use prowlcat_core::{ScanConfig, ScanOrchestrator, TargetParser, TypeHint};

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(name)
}

#[test]
fn search_email_in_fixture() {
    let parser = TargetParser::new(TypeHint::Auto);
    let queries = parser.parse_many(["user@example.com"]).unwrap();

    let config = ScanConfig {
        paths: vec![fixture_path("sample_breach.txt")],
        recursive: false,
        threads: 2,
        ..ScanConfig::default()
    };

    let (results, summary) = ScanOrchestrator::new(config, queries).run().unwrap();
    assert!(summary.matches >= 1, "expected at least one email match");
    assert!(results.iter().any(|r| r.query.contains("user@example.com")));
}

#[test]
fn search_cidr_literal_and_membership() {
    let parser = TargetParser::new(TypeHint::Cidr);
    let queries = parser.parse_many(["10.0.0.0/8"]).unwrap();

    let config = ScanConfig {
        paths: vec![fixture_path("sample_breach.txt")],
        recursive: false,
        threads: 2,
        ..ScanConfig::default()
    };

    let (results, _) = ScanOrchestrator::new(config, queries).run().unwrap();
    use prowlcat_types::MatchKind;
    assert!(
        results.iter().any(|r| r.match_kind == MatchKind::CidrLiteral),
        "expected literal CIDR match"
    );
    assert!(
        results.iter().any(|r| r.match_kind == MatchKind::IpInNetwork),
        "expected IP-in-network match for 10.23.1.5"
    );
}

#[test]
fn search_domain_excludes_false_positive() {
    let parser = TargetParser::new(TypeHint::Domain);
    let queries = parser.parse_many(["acme.example"]).unwrap();

    let config = ScanConfig {
        paths: vec![fixture_path("sample_breach.txt")],
        recursive: false,
        threads: 2,
        ..ScanConfig::default()
    };

    let (results, _) = ScanOrchestrator::new(config, queries).run().unwrap();
    assert!(!results.is_empty());
    assert!(
        results
            .iter()
            .all(|r| !r.line_text.starts_with("notacme.example")),
        "acme.example must not match notacme.example line"
    );
}

#[test]
fn search_plain_text_substring() {
    let parser = TargetParser::new(TypeHint::Text);
    let queries = parser.parse_many(["Winter2024"]).unwrap();

    let config = ScanConfig {
        paths: vec![fixture_path("sample_breach.txt")],
        recursive: false,
        threads: 2,
        ..ScanConfig::default()
    };

    let (results, summary) = ScanOrchestrator::new(config, queries).run().unwrap();
    assert_eq!(summary.matches, 1);
    assert!(results[0].line_text.contains("Winter2024"));
}

// ---- M2: zstd, tar, tar.gz, zip, mmap ----

use std::io::Write;

const SAMPLE: &str = "alice@example.com:hunter2\nnobody@else.org:pw\nbob@example.com:letmein\n";

fn scratch_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("prowlcat-test-{}-{}", tag, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn scan(path: PathBuf, mmap_mb: u64) -> Vec<prowlcat_types::ScanResult> {
    let queries = TargetParser::new(TypeHint::Email)
        .parse_many(["alice@example.com", "bob@example.com"])
        .unwrap();
    let config = ScanConfig {
        paths: vec![path],
        recursive: false,
        threads: 2,
        mmap_threshold_mb: mmap_mb,
        ..ScanConfig::default()
    };
    let (results, summary) = ScanOrchestrator::new(config, queries).run().unwrap();
    assert_eq!(summary.errors, 0);
    results
}

fn tar_bytes() -> Vec<u8> {
    let mut b = tar::Builder::new(Vec::new());
    for (name, body) in [("a/one.txt", SAMPLE), ("two.txt", "bob@example.com:x\n")] {
        let mut h = tar::Header::new_gnu();
        h.set_size(body.len() as u64);
        h.set_mode(0o644);
        h.set_cksum();
        b.append_data(&mut h, name, body.as_bytes()).unwrap();
    }
    b.into_inner().unwrap()
}

#[test]
fn zstd_file() {
    let dir = scratch_dir("zst");
    let p = dir.join("dump.txt.zst");
    std::fs::write(&p, zstd::encode_all(SAMPLE.as_bytes(), 0).unwrap()).unwrap();
    let r = scan(p, 64);
    assert_eq!(r.len(), 2);
    assert!(r.iter().all(|m| m.source.archive_entry.is_none()));
}

#[test]
fn tar_and_tar_gz_report_member_names() {
    let dir = scratch_dir("tar");
    let plain = dir.join("dump.tar");
    std::fs::write(&plain, tar_bytes()).unwrap();
    let r = scan(plain, 64);
    assert_eq!(r.len(), 3);
    assert!(r.iter().any(|m| m.source.archive_entry.as_deref() == Some("a/one.txt")));
    assert!(r.iter().any(|m| m.source.archive_entry.as_deref() == Some("two.txt")));

    let gz = dir.join("dump.tar.gz");
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    enc.write_all(&tar_bytes()).unwrap();
    std::fs::write(&gz, enc.finish().unwrap()).unwrap();
    assert_eq!(scan(gz, 64).len(), 3);
}

#[test]
fn zip_members() {
    let dir = scratch_dir("zip");
    let p = dir.join("dump.zip");
    let mut w = zip::ZipWriter::new(std::fs::File::create(&p).unwrap());
    let opts = zip::write::SimpleFileOptions::default();
    w.start_file("inner/leak.txt", opts).unwrap();
    w.write_all(SAMPLE.as_bytes()).unwrap();
    w.finish().unwrap();
    let r = scan(p, 64);
    assert_eq!(r.len(), 2);
    assert!(r.iter().all(|m| m.source.archive_entry.as_deref() == Some("inner/leak.txt")));
}

#[test]
fn mmap_matches_streaming_results() {
    let dir = scratch_dir("mmap");
    let p = dir.join("dump.txt");
    std::fs::write(&p, SAMPLE).unwrap();
    // threshold 0 disables mmap
    let streamed = scan(p.clone(), 0);
    // mmap_threshold_mb is whole MiB, so exercise the mapped path through the decode crate directly (1 byte).
    let mut reader =
        prowlcat_decode::LineReader::open(&p, prowlcat_source::CompressionKind::None, 1).unwrap();
    let mut lines = Vec::new();
    reader.for_each_line(|_, n, o, l| lines.push((n, o, l.to_vec()))).unwrap();
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[1].1, "alice@example.com:hunter2\n".len() as u64);
    assert_eq!(streamed.len(), 2);
}

// ---- password-protected zip ----

fn encrypted_zip(dir: &std::path::Path, name: &str) -> PathBuf {
    let p = dir.join(name);
    let mut w = zip::ZipWriter::new(std::fs::File::create(&p).unwrap());
    let opts = zip::write::SimpleFileOptions::default();
    // The zip crate can only write AES; legacy ZipCrypto shares the same decrypt API path.
    let opts = opts.with_aes_encryption(zip::AesMode::Aes256, "s3cret");
    w.start_file("locked.txt", opts).unwrap();
    w.write_all(SAMPLE.as_bytes()).unwrap();
    w.finish().unwrap();
    p
}

fn scan_with(path: PathBuf, passwords: &[&str]) -> usize {
    let queries = TargetParser::new(TypeHint::Email)
        .parse_many(["alice@example.com", "bob@example.com"])
        .unwrap();
    let config = ScanConfig {
        paths: vec![path],
        recursive: false,
        threads: 1,
        passwords: passwords.iter().map(|s| s.to_string()).collect(),
        ..ScanConfig::default()
    };
    ScanOrchestrator::new(config, queries).run().unwrap().0.len()
}

#[test]
fn encrypted_zip_needs_correct_password() {
    let dir = scratch_dir("zipenc");
    let p = encrypted_zip(&dir, "aes.zip");
    assert_eq!(scan_with(p.clone(), &[]), 0, "no password");
    assert_eq!(scan_with(p.clone(), &["wrong"]), 0, "wrong password");
    assert_eq!(scan_with(p, &["wrong", "s3cret"]), 2, "password list");
}

// ---- M3: progress ----

#[test]
fn progress_reaches_total_for_every_format() {
    use prowlcat_core::ProgressHandle;
    use std::sync::atomic::Ordering;

    let dir = scratch_dir("progress");
    let plain = dir.join("a.txt");
    std::fs::write(&plain, SAMPLE.repeat(100)).unwrap();
    let gz = dir.join("b.txt.gz");
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    enc.write_all(SAMPLE.repeat(100).as_bytes()).unwrap();
    std::fs::write(&gz, enc.finish().unwrap()).unwrap();
    std::fs::write(dir.join("c.txt.zst"), zstd::encode_all(SAMPLE.as_bytes(), 0).unwrap()).unwrap();
    std::fs::write(dir.join("d.tar"), tar_bytes()).unwrap();
    let zp = dir.join("e.zip");
    let mut w = zip::ZipWriter::new(std::fs::File::create(&zp).unwrap());
    w.start_file("x.txt", zip::write::SimpleFileOptions::default()).unwrap();
    w.write_all(SAMPLE.as_bytes()).unwrap();
    w.finish().unwrap();

    for mmap_mb in [0u64, 1] {
        // mmap_mb is whole MiB so 1 does not map these small files; the decode-level
        // mmap top-up is covered separately below.
        let handle = ProgressHandle::default();
        let queries = TargetParser::new(TypeHint::Email).parse_many(["alice@example.com"]).unwrap();
        let config = ScanConfig {
            paths: vec![dir.clone()],
            threads: 2,
            mmap_threshold_mb: mmap_mb,
            progress: Some(handle.clone()),
            ..ScanConfig::default()
        };
        ScanOrchestrator::new(config, queries).run().unwrap();
        let total = handle.total.load(Ordering::Relaxed);
        assert!(total > 0);
        assert_eq!(handle.done.load(Ordering::Relaxed), total, "done must equal total");
        assert_eq!(handle.files_done.load(Ordering::Relaxed), 5);
    }
}

#[test]
fn progress_tops_up_for_mmap() {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;

    let dir = scratch_dir("progress-mmap");
    let p = dir.join("big.txt");
    std::fs::write(&p, SAMPLE.repeat(10)).unwrap();
    let size = std::fs::metadata(&p).unwrap().len();
    let counter = Arc::new(AtomicU64::new(0));
    let mut reader = prowlcat_decode::LineReader::open(&p, prowlcat_source::CompressionKind::None, 1)
        .unwrap()
        .with_progress(counter.clone());
    reader.for_each_line(|_, _, _, _| {}).unwrap();
    assert_eq!(counter.load(Ordering::Relaxed), size);
}

// ---- live matches and cancellation (used by the TUI) ----

#[test]
fn live_sink_receives_every_match() {
    use prowlcat_core::ProgressHandle;

    let dir = scratch_dir("live");
    std::fs::write(dir.join("a.txt"), SAMPLE).unwrap();
    std::fs::write(dir.join("b.txt"), SAMPLE).unwrap();

    let (tx, rx) = std::sync::mpsc::sync_channel(1024);
    let handle = ProgressHandle { live: Some(tx), ..ProgressHandle::default() };
    let queries = TargetParser::new(TypeHint::Email)
        .parse_many(["alice@example.com", "bob@example.com"])
        .unwrap();
    let config = ScanConfig {
        paths: vec![dir],
        threads: 2,
        progress: Some(handle),
        ..ScanConfig::default()
    };
    let (results, summary) = ScanOrchestrator::new(config, queries).run().unwrap();

    // The orchestrator owns the only sender once `run` returns, so dropping it ends the stream.
    let live: Vec<_> = rx.try_iter().collect();
    assert_eq!(summary.matches, 4);
    assert_eq!(live.len(), results.len(), "live stream mirrors the result list");
}

#[test]
fn cancel_flag_stops_the_scan() {
    use prowlcat_core::ProgressHandle;
    use std::sync::atomic::Ordering;

    let dir = scratch_dir("cancel");
    std::fs::write(dir.join("a.txt"), SAMPLE.repeat(50)).unwrap();

    let handle = ProgressHandle::default();
    handle.cancel.store(true, Ordering::Relaxed);
    let queries = TargetParser::new(TypeHint::Email).parse_many(["alice@example.com"]).unwrap();
    let config = ScanConfig {
        paths: vec![dir],
        threads: 1,
        progress: Some(handle),
        ..ScanConfig::default()
    };
    let (results, summary) = ScanOrchestrator::new(config, queries).run().unwrap();
    assert!(results.is_empty(), "a pre-cancelled scan finds nothing");
    assert_eq!(summary.errors, 0, "cancelling is not an error");
}

// ---- bounded-memory, ordered results (run_sorted) ----

/// A corpus of many formats and files, so ordering has real work to do.
fn mixed_corpus(tag: &str) -> PathBuf {
    let dir = scratch_dir(tag);
    for i in 0..12 {
        let body = format!("{SAMPLE}noise {i}\n{}", SAMPLE.repeat(i % 4 + 1));
        std::fs::write(dir.join(format!("f{:02}.txt", 11 - i)), body).unwrap();
    }
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    enc.write_all(SAMPLE.repeat(5).as_bytes()).unwrap();
    std::fs::write(dir.join("g.txt.gz"), enc.finish().unwrap()).unwrap();
    std::fs::write(dir.join("t.tar"), tar_bytes()).unwrap();
    let mut w = zip::ZipWriter::new(std::fs::File::create(dir.join("z.zip")).unwrap());
    for name in ["m1.txt", "m2.txt"] {
        w.start_file(name, zip::write::SimpleFileOptions::default()).unwrap();
        w.write_all(SAMPLE.as_bytes()).unwrap();
    }
    w.finish().unwrap();
    dir
}

fn config_for(dir: &std::path::Path, max_matches: u64, memory_mb: u64) -> ScanConfig {
    ScanConfig {
        paths: vec![dir.to_path_buf()],
        threads: 4,
        max_matches_per_query: max_matches,
        result_memory_mb: memory_mb,
        ..ScanConfig::default()
    }
}

fn queries() -> Vec<prowlcat_types::Query> {
    TargetParser::new(TypeHint::Auto)
        .parse_many(["bob@example.com", "alice@example.com", "example.com"])
        .unwrap()
}

fn as_json(results: &[prowlcat_types::ScanResult]) -> Vec<String> {
    results.iter().map(|r| serde_json::to_string(r).unwrap()).collect()
}

#[test]
fn sorted_run_gives_exactly_the_stable_sorted_list() {
    let dir = mixed_corpus("sorted-eq");
    for max in [0u64, 2] {
        let (mut expected, summary) = ScanOrchestrator::new(config_for(&dir, max, 256), queries()).run().unwrap();
        expected.sort_by(|a, b| {
            a.query
                .cmp(&b.query)
                .then(a.source.path.cmp(&b.source.path))
                .then(a.source.line_number.cmp(&b.source.line_number))
        });
        assert!(expected.len() > 20, "the corpus must produce plenty of matches");

        // 256 MiB keeps everything in memory; 0 pushes every batch through temporary files.
        for memory_mb in [256u64, 0] {
            let sorted = ScanOrchestrator::new(config_for(&dir, max, memory_mb), queries()).run_sorted().unwrap();
            let got: Vec<_> = sorted.iter().collect::<Result<_, _>>().unwrap();
            assert_eq!(as_json(&got), as_json(&expected), "max {max}, memory {memory_mb} MiB");
            assert_eq!(sorted.summary().matches, summary.matches);
            assert_eq!(sorted.summary().files_scanned, summary.files_scanned);
            assert_eq!(sorted.summary().bytes_scanned, summary.bytes_scanned);
            assert_eq!(sorted.spilled_runs() > 0, memory_mb == 0, "spill only when the budget is exhausted");
        }
    }
}

#[test]
fn sorted_results_can_be_read_more_than_once() {
    let dir = mixed_corpus("sorted-twice");
    let sorted = ScanOrchestrator::new(config_for(&dir, 0, 0), queries()).run_sorted().unwrap();
    let first: Vec<_> = sorted.iter().collect::<Result<_, _>>().unwrap();
    let second: Vec<_> = sorted.iter().collect::<Result<_, _>>().unwrap();
    assert_eq!(as_json(&first), as_json(&second));
    assert_eq!(first.len(), sorted.len());
}

#[test]
fn sorted_run_reports_progress_and_honours_cancel() {
    use prowlcat_core::ProgressHandle;
    use std::sync::atomic::Ordering;

    let dir = mixed_corpus("sorted-progress");
    let handle = ProgressHandle::default();
    let mut config = config_for(&dir, 0, 256);
    config.progress = Some(handle.clone());
    let sorted = ScanOrchestrator::new(config, queries()).run_sorted().unwrap();
    assert_eq!(handle.matches.load(Ordering::Relaxed), sorted.len() as u64);
    assert_eq!(handle.done.load(Ordering::Relaxed), handle.total.load(Ordering::Relaxed));
    assert_eq!(handle.files_done.load(Ordering::Relaxed), sorted.summary().files_scanned as u64);

    let cancelled = ProgressHandle::default();
    cancelled.cancel.store(true, Ordering::Relaxed);
    let mut config = config_for(&dir, 0, 256);
    config.progress = Some(cancelled);
    let sorted = ScanOrchestrator::new(config, queries()).run_sorted().unwrap();
    assert!(sorted.is_empty());
    assert_eq!(sorted.summary().errors, 0);
}

#[test]
fn default_progress_changes_nothing() {
    use prowlcat_core::ProgressHandle;

    let dir = scratch_dir("noop-progress");
    std::fs::write(dir.join("a.txt"), SAMPLE.repeat(30)).unwrap();
    let run = |progress: Option<ProgressHandle>| {
        let queries = TargetParser::new(TypeHint::Email).parse_many(["alice@example.com"]).unwrap();
        let config = ScanConfig { paths: vec![dir.clone()], threads: 2, progress, ..ScanConfig::default() };
        ScanOrchestrator::new(config, queries).run().unwrap().1.matches
    };
    assert_eq!(run(None), run(Some(ProgressHandle::default())));
}
