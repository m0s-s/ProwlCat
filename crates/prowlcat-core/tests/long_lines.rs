//! Very long lines: the line cap and the snippet change how a match is reported, never which
//! matches are found.

use std::io::Write;

use prowlcat_core::{ScanConfig, ScanOrchestrator, ScanResult, TargetParser, TypeHint};

/// One logical line of about `len` bytes: fields of every kind, separated the way SQL rows and
/// combo lists separate them, with the emails we look for scattered through it, some of them
/// close to where a cut is likely to fall. `example.com` is rare in it, on purpose: finding a
/// domain in a line costs a pass over the whole line per occurrence.
fn long_line(len: usize) -> (String, Vec<String>) {
    let mut x: u64 = 0xdead_beef_cafe_f00d;
    let mut next = move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    let seps = [" ", ",", "),(", "', '", ";", " | ", "\t"];
    let mut s = String::from("INSERT INTO t VALUES (");
    let mut planted = Vec::new();
    let mut i = 0u64;
    while s.len() < len {
        i += 1;
        let field = match next() % 6 {
            0 => {
                let e = format!("target{}@corp.test", planted.len());
                planted.push(e.clone());
                e
            }
            1 if i % 40 == 1 => format!("https://shop{}.example.com/p", next() % 100),
            1 => format!("user{i}@mail.example.org"),
            2 => format!("{}.{}.{}.{}", next() % 256, next() % 256, next() % 256, next() % 256),
            3 => format!("pw{}é漢", next() % 10_000),
            _ => format!("https://shop{}.example.net/x", next() % 100),
        };
        s.push_str(&field);
        s.push_str(seps[(next() % seps.len() as u64) as usize]);
    }
    (s, planted)
}

fn scan(dir: &std::path::Path, max_line: usize, snippet: usize, mmap_mb: u64, planted: &[String]) -> Vec<ScanResult> {
    let mut targets: Vec<String> = planted.iter().take(40).cloned().collect();
    targets.push("example.com".into());
    let queries = TargetParser::new(TypeHint::Auto).parse_many(targets).unwrap();
    let config = ScanConfig {
        paths: vec![dir.to_path_buf()],
        threads: 2,
        mmap_threshold_mb: mmap_mb,
        max_line_bytes: max_line,
        snippet_bytes: snippet,
        ..ScanConfig::default()
    };
    let sorted = ScanOrchestrator::new(config, queries).run_sorted().unwrap();
    sorted.iter().map(|r| r.unwrap()).collect()
}

/// What identifies a match independently of how much of the line is shown.
fn keys(results: &[ScanResult]) -> Vec<String> {
    let mut v: Vec<String> = results
        .iter()
        .map(|r| format!("{}|{:?}|{}|{:?}", r.query, r.source.line_number, r.match_kind as u8, r.extracted))
        .collect();
    v.sort();
    v
}

/// The matches as the same line-and-value pairs, however many times each was reported. A domain
/// found three times in a line is reported three times (the engine has always done that), and
/// how many times depends on how many pieces the line was cut into.
fn distinct(results: &[ScanResult]) -> Vec<String> {
    let mut v = keys(results);
    v.dedup();
    v
}

fn emails(results: &[ScanResult]) -> Vec<ScanResult> {
    results.iter().filter(|r| r.query.contains('@')).cloned().collect()
}

fn corpus(len: usize) -> (tempfile::TempDir, Vec<String>) {
    let dir = tempfile::tempdir().unwrap();
    let (line, planted) = long_line(len);
    let mut f = std::fs::File::create(dir.path().join("dump.sql")).unwrap();
    writeln!(f, "short first line with target0@example.com").unwrap();
    writeln!(f, "{line}").unwrap();
    f.write_all(b"last line, no newline: target1@example.com").unwrap();
    (dir, planted)
}

#[test]
fn a_line_cap_finds_exactly_the_same_matches() {
    let (dir, planted) = corpus(1_200_000);
    assert!(planted.len() > 100);
    let whole = scan(dir.path(), 0, 0, 0, &planted);
    assert!(whole.len() > 100, "{}", whole.len());
    assert!(whole.iter().any(|r| r.query == "example.com"), "the domain is in the line");
    for cap in [4096, 5000, 8192, 65_536] {
        for mmap_mb in [0, 1] {
            let pieces = scan(dir.path(), cap, 0, mmap_mb, &planted);
            assert_eq!(keys(&emails(&pieces)), keys(&emails(&whole)), "emails, cap {cap}, mmap {mmap_mb}");
            assert_eq!(distinct(&pieces), distinct(&whole), "cap {cap}, mmap {mmap_mb}");
            assert!(pieces.iter().all(|r| r.line_text.len() <= cap), "a piece longer than the cap");
            for r in &pieces {
                let t = &r.line_text[r.matched_span.0..r.matched_span.1];
                assert!(!t.is_empty(), "span of {} inside its piece", r.query);
            }
        }
    }
}

#[test]
fn a_snippet_shows_the_match_with_its_context_only() {
    let (dir, planted) = corpus(300_000);
    let whole = scan(dir.path(), 0, 0, 0, &planted);
    let snip = scan(dir.path(), 0, 30, 0, &planted);
    assert_eq!(keys(&snip), keys(&whole), "the matches do not change");
    for (w, s) in whole.iter().zip(&snip) {
        assert_eq!((&w.query, w.source.line_number), (&s.query, s.source.line_number));
        assert!(s.line_text.len() <= w.matched_span.1 - w.matched_span.0 + 2 * 30 + 6 + 3, "{}", s.line_text.len());
        let shown = &s.line_text[s.matched_span.0..s.matched_span.1];
        assert_eq!(shown, &w.line_text[w.matched_span.0..w.matched_span.1], "the span still points at the match");
    }
    // A line that fits in the window is untouched.
    let first = snip.iter().find(|r| r.source.line_number == Some(0)).unwrap();
    assert_eq!(first.line_text, "short first line with target0@example.com");
}

#[test]
fn cap_and_snippet_together() {
    let (dir, planted) = corpus(400_000);
    let whole = scan(dir.path(), 0, 0, 0, &planted);
    let both = scan(dir.path(), 8192, 40, 0, &planted);
    assert_eq!(keys(&emails(&both)), keys(&emails(&whole)));
    assert_eq!(distinct(&both), distinct(&whole));
    assert!(both.iter().all(|r| r.line_text.len() < 400));
}
