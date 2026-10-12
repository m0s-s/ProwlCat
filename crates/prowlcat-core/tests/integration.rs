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
