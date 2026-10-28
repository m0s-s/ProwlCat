//! End-to-end tests: the real `prowlcat` binary, run as a user would run it, on a corpus built
//! here. They are the release gate: a tag must not be pushed unless they pass, and the release
//! workflow runs them again against the packaged binary.
//!
//! `PROWLCAT_E2E_BIN` points them at another binary (the packaged one); otherwise the one
//! cargo just built is used. `PROWLCAT_E2E_EXPECT_VERSION` makes `--version` be checked too.
//!
//! Nothing here depends on the working directory, the platform, or the temporary directory's
//! name: searches run with the corpus as the current directory and relative paths.

use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::Value;

fn bin() -> PathBuf {
    std::env::var_os("PROWLCAT_E2E_BIN").map(PathBuf::from).unwrap_or_else(|| env!("CARGO_BIN_EXE_prowlcat").into())
}

struct Out {
    code: i32,
    stdout: String,
    stderr: String,
}

fn run_env(dir: &Path, args: &[&str], envs: &[(&str, &str)]) -> Out {
    let mut cmd = Command::new(bin());
    cmd.args(args)
        .current_dir(dir)
        .env("PROWLCAT_UI", "cli")
        .env("PROWLCAT_STATE_DIR", dir.join(".state"))
        .env_remove("PROWLCAT_LOG_FILE")
        .env_remove("PROWLCAT_RESULT_MEM_MB")
        .env_remove("PROWLCAT_TMP_DIR")
        .env_remove("PROWLCAT_RAR_TOOL")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // The RAR tests use a program only if the one running them names it.
    if let Some(tool) = std::env::var_os("PROWLCAT_E2E_RAR_TOOL") {
        cmd.env("PROWLCAT_RAR_TOOL", tool);
    }
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let o = cmd.output().expect("run prowlcat");
    Out {
        code: o.status.code().expect("exit code"),
        stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
    }
}

fn run(dir: &Path, args: &[&str]) -> Out {
    run_env(dir, args, &[])
}

/// Search and return the matches as parsed NDJSON, plus the exit code.
fn search(dir: &Path, args: &[&str]) -> (i32, Vec<Value>) {
    search_env(dir, args, &[])
}

fn search_env(dir: &Path, args: &[&str], envs: &[(&str, &str)]) -> (i32, Vec<Value>) {
    let out_file = dir.join(".out.ndjson");
    let _ = fs::remove_file(&out_file);
    let mut a = vec!["search"];
    a.extend_from_slice(args);
    a.extend_from_slice(&["--no-progress", "-o", ".out.ndjson"]);
    let o = run_env(dir, &a, envs);
    let text = fs::read_to_string(&out_file).unwrap_or_default();
    let rows = text.lines().map(|l| serde_json::from_str(l).expect("valid NDJSON line")).collect();
    (o.code, rows)
}

fn raw_ndjson(dir: &Path, args: &[&str], envs: &[(&str, &str)]) -> String {
    let out_file = dir.join(".out.ndjson");
    let _ = fs::remove_file(&out_file);
    let mut a = vec!["search"];
    a.extend_from_slice(args);
    a.extend_from_slice(&["--no-progress", "-o", ".out.ndjson"]);
    run_env(dir, &a, envs);
    fs::read_to_string(&out_file).unwrap_or_default()
}

fn lines_of(rows: &[Value]) -> BTreeSet<String> {
    rows.iter().map(|r| r["line_text"].as_str().unwrap().to_string()).collect()
}

fn path_of(row: &Value) -> String {
    row["source"]["path"].as_str().unwrap().replace('\\', "/")
}

// ---- corpus ----

const SHARED: &str = "\
alice@example.com:pw1
bob@mail.example.com:pw2
https://example.com/login:zed:pw3
https://shop.example.org/cart
notexample.com:neg1
example.community:neg2
carol@example.org:pw4
10.1.2.3 admin
192.168.5.5 other
203.0.113.42 listening
2001:db8::1 v6 host
Winter2024 is a text needle
";

fn gz(data: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    e.write_all(data).unwrap();
    e.finish().unwrap()
}

fn tar_of(members: &[(&str, &str)]) -> Vec<u8> {
    let mut b = tar::Builder::new(Vec::new());
    for (name, body) in members {
        let mut h = tar::Header::new_gnu();
        h.set_size(body.len() as u64);
        h.set_mode(0o644);
        h.set_cksum();
        b.append_data(&mut h, name, body.as_bytes()).unwrap();
    }
    b.into_inner().unwrap()
}

fn zip_of(members: &[(&str, &str)], password: Option<&str>) -> Vec<u8> {
    let mut z = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (name, body) in members {
        let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        let opts = match password {
            Some(p) => opts.with_aes_encryption(zip::AesMode::Aes256, p),
            None => opts,
        };
        z.start_file(*name, opts).unwrap();
        z.write_all(body.as_bytes()).unwrap();
    }
    z.finish().unwrap().into_inner()
}

/// One file per container kind, each holding one email nobody else has.
fn corpus() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    fs::write(p.join("plain.txt"), format!("plain@example.com:p\n{SHARED}")).unwrap();
    fs::write(p.join("gzipped.txt.gz"), gz(b"gzip@example.com:p\n")).unwrap();
    fs::write(p.join("zstd.txt.zst"), zstd::encode_all(&b"zstd@example.com:p\n"[..], 1).unwrap()).unwrap();
    let tar = tar_of(&[("inner/a.txt", "tar@example.com:p\n"), ("inner/b.txt", "other line\n")]);
    fs::write(p.join("bundle.tar"), &tar).unwrap();
    fs::write(p.join("bundle.tar.gz"), gz(&tar)).unwrap();
    fs::write(p.join("open.zip"), zip_of(&[("z/one.txt", "zip@example.com:p\n")], None)).unwrap();
    fs::write(p.join("locked.zip"), zip_of(&[("secret.txt", "locked@example.com:p\n")], Some("s3cret"))).unwrap();
    fs::write(p.join("passwords.txt"), "wrong\ns3cret\n").unwrap();
    dir
}

// ---- tests ----

#[test]
fn version_matches_the_package() {
    let dir = tempfile::tempdir().unwrap();
    let o = run(dir.path(), &["--version"]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    let expect = std::env::var("PROWLCAT_E2E_EXPECT_VERSION").unwrap_or_else(|_| env!("CARGO_PKG_VERSION").to_string());
    let expect = expect.trim_start_matches('v');
    assert!(o.stdout.contains(expect), "--version is {:?}, expected {expect}", o.stdout);
}

#[test]
fn every_container_is_searched() {
    let c = corpus();
    let d = c.path();
    // (target, file the match must come from, archive member if any)
    let cases: &[(&str, &str, Option<&str>)] = &[
        ("plain@example.com", "plain.txt", None),
        ("gzip@example.com", "gzipped.txt.gz", None),
        ("zstd@example.com", "zstd.txt.zst", None),
        ("tar@example.com", "bundle.tar", Some("inner/a.txt")),
        ("tar@example.com", "bundle.tar.gz", Some("inner/a.txt")),
        ("zip@example.com", "open.zip", Some("z/one.txt")),
    ];
    for (target, file, member) in cases {
        let (code, rows) = search(d, &[target, "-p", file]);
        assert_eq!(code, 0, "{target} in {file}");
        assert_eq!(rows.len(), 1, "{target} in {file}");
        assert!(path_of(&rows[0]).ends_with(file), "{:?}", rows[0]);
        assert_eq!(rows[0]["source"]["archive_entry"].as_str(), *member, "{target} in {file}");
    }
}

#[test]
fn a_directory_search_finds_each_container_once() {
    let c = corpus();
    let (code, rows) = search(c.path(), &["tar@example.com", "-p", ".", "-r"]);
    assert_eq!(code, 0);
    let files: BTreeSet<String> = rows.iter().map(path_of).map(|p| p.rsplit('/').next().unwrap().to_string()).collect();
    assert_eq!(files, BTreeSet::from(["bundle.tar".to_string(), "bundle.tar.gz".to_string()]));
}

#[test]
fn encrypted_zip_needs_a_candidate_password() {
    let c = corpus();
    let d = c.path();
    let (code, rows) = search(d, &["locked@example.com", "-p", "locked.zip"]);
    assert_eq!((code, rows.len()), (1, 0), "no password");
    let (code, rows) = search(d, &["locked@example.com", "-p", "locked.zip", "--password", "wrong"]);
    assert_eq!((code, rows.len()), (1, 0), "wrong password");
    let (code, rows) = search(d, &["locked@example.com", "-p", "locked.zip", "--password-file", "passwords.txt"]);
    assert_eq!((code, rows.len()), (0, 1), "password file");
    assert_eq!(rows[0]["source"]["archive_entry"], "secret.txt");
}

#[test]
fn domain_search_covers_subdomains_and_urls_and_nothing_longer() {
    let c = corpus();
    let (code, rows) = search(c.path(), &["example.com", "-t", "domain", "-p", "plain.txt"]);
    assert_eq!(code, 0);
    let got = lines_of(&rows);
    for line in ["plain@example.com:p", "alice@example.com:pw1", "bob@mail.example.com:pw2", "https://example.com/login:zed:pw3"] {
        assert!(got.contains(line), "missing {line}: {got:?}");
    }
    for line in ["notexample.com:neg1", "example.community:neg2", "https://shop.example.org/cart", "carol@example.org:pw4"] {
        assert!(!got.contains(line), "false positive {line}: {got:?}");
    }
}

#[test]
fn email_ip_cidr_and_text_targets() {
    let c = corpus();
    let d = c.path();
    let one = |args: &[&str]| lines_of(&search(d, args).1);

    assert_eq!(one(&["bob@mail.example.com", "-p", "plain.txt"]), BTreeSet::from(["bob@mail.example.com:pw2".to_string()]));
    assert_eq!(one(&["203.0.113.42", "-t", "ip", "-p", "plain.txt"]), BTreeSet::from(["203.0.113.42 listening".to_string()]));
    assert_eq!(one(&["2001:db8::1", "-t", "ip", "-p", "plain.txt"]), BTreeSet::from(["2001:db8::1 v6 host".to_string()]));
    assert_eq!(one(&["10.0.0.0/8", "-t", "cidr", "-p", "plain.txt"]), BTreeSet::from(["10.1.2.3 admin".to_string()]));
    assert_eq!(one(&["192.168.0.0/16", "-t", "cidr", "-p", "plain.txt"]), BTreeSet::from(["192.168.5.5 other".to_string()]));
    assert_eq!(one(&["winter2024", "-t", "text", "-p", "plain.txt"]), BTreeSet::from(["Winter2024 is a text needle".to_string()]));
    assert_eq!(one(&["WINTER2024", "--loose", "-p", "plain.txt"]), BTreeSet::from(["Winter2024 is a text needle".to_string()]));
}

#[test]
fn several_targets_and_a_targets_file_agree() {
    let c = corpus();
    let d = c.path();
    fs::write(d.join("targets.txt"), "alice@example.com\nbob@mail.example.com\n").unwrap();
    let a = raw_ndjson(d, &["alice@example.com", "bob@mail.example.com", "-p", "plain.txt"], &[]);
    let b = raw_ndjson(d, &["-f", "targets.txt", "-p", "plain.txt"], &[]);
    assert_eq!(a.lines().count(), 2);
    assert_eq!(a, b);
}

#[test]
fn exit_codes_mean_found_none_error() {
    let c = corpus();
    let d = c.path();
    assert_eq!(search(d, &["alice@example.com", "-p", "plain.txt"]).0, 0);
    assert_eq!(search(d, &["nobody@example.com", "-p", "plain.txt"]).0, 1);
    assert_eq!(search(d, &["alice@example.com", "-p", "does-not-exist.txt"]).0, 2);
    assert_eq!(run(d, &["search", "10.0.0.0/99", "-t", "cidr", "-p", "plain.txt", "--no-progress"]).code, 2);
    assert_eq!(run(d, &["search", "-p", "plain.txt", "--no-progress"]).code, 2, "no target");
}

#[test]
fn output_formats() {
    let c = corpus();
    let d = c.path();
    let base = ["search", "example.com", "-t", "domain", "-p", "plain.txt", "--no-progress"];

    let table = run(d, &base);
    assert_eq!(table.code, 0);
    assert!(table.stdout.contains("[domain] example.com") && table.stdout.contains("└─"), "{}", table.stdout);

    let mut a = base.to_vec();
    a.extend_from_slice(&["-o", "o.json"]);
    assert_eq!(run(d, &a).code, 0);
    let json = fs::read_to_string(d.join("o.json")).unwrap();
    assert_eq!(json.trim_end().lines().count(), 1, "compact JSON is one line");
    let v: Value = serde_json::from_str(&json).unwrap();
    let n = v["results"].as_array().unwrap().len();
    assert!(n >= 4);
    assert_eq!(v["summary"]["matches"], n);

    let mut a = base.to_vec();
    a.extend_from_slice(&["-o", "o.ndjson"]);
    run(d, &a);
    assert_eq!(fs::read_to_string(d.join("o.ndjson")).unwrap().lines().count(), n);

    let mut a = base.to_vec();
    a.extend_from_slice(&["-o", "o.csv"]);
    run(d, &a);
    let csv = fs::read_to_string(d.join("o.csv")).unwrap();
    assert!(csv.starts_with("query,kind,match_kind,path,line,byte_offset,context"), "{csv}");
    assert_eq!(csv.lines().count(), n + 1);
}

#[test]
fn threads_memory_budget_and_spill_directory_never_change_the_results() {
    let c = corpus();
    let d = c.path();
    let spill = tempfile::tempdir().unwrap();
    let spill_dir = spill.path().to_str().unwrap();
    let base = ["example.com", "alice@example.com", "-p", ".", "-r", "--format", "ndjson"];
    let reference = raw_ndjson(d, &[&base[..], &["-j", "1"]].concat(), &[("PROWLCAT_RESULT_MEM_MB", "4096")]);
    assert!(reference.lines().count() >= 8, "{reference}");
    for (threads, mem) in [("4", "4096"), ("1", "0"), ("4", "0"), ("3", "1")] {
        let got = raw_ndjson(
            d,
            &[&base[..], &["-j", threads]].concat(),
            &[("PROWLCAT_RESULT_MEM_MB", mem), ("PROWLCAT_TMP_DIR", spill_dir)],
        );
        assert_eq!(got, reference, "threads {threads}, budget {mem} MB");
    }
}

#[test]
fn a_spill_that_cannot_be_written_stops_the_search_with_an_error() {
    let c = corpus();
    let d = c.path();
    let nowhere = d.join("no-such-dir").join("deeper");
    let o = run_env(
        d,
        &["search", "example.com", "-t", "domain", "-p", "plain.txt", "--no-progress"],
        &[("PROWLCAT_RESULT_MEM_MB", "0"), ("PROWLCAT_TMP_DIR", nowhere.to_str().unwrap())],
    );
    assert_eq!(o.code, 2, "stdout: {}", o.stdout);
    assert!(o.stderr.contains("could not be parked on disk") && o.stderr.contains("PROWLCAT_TMP_DIR"), "{}", o.stderr);
    assert!(!o.stdout.contains("└─"), "no partial results");
}

/// A 600 KB line (a one-line SQL INSERT) with a known email in the middle, between two others.
fn long_line_corpus() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let filler = "('x@filler.test','pw'),".repeat(13_000);
    let line = format!("INSERT INTO t VALUES {filler}('needle@example.com','pw'),{filler}('other@example.com','pw');\n");
    fs::write(dir.path().join("dump.sql"), format!("short needle@example.com line\n{line}")).unwrap();
    dir
}

#[test]
fn long_lines_are_whole_by_default_and_a_snippet_shortens_them() {
    let c = long_line_corpus();
    let d = c.path();
    let (code, rows) = search(d, &["needle@example.com", "-p", "dump.sql"]);
    assert_eq!((code, rows.len()), (0, 2));
    let long = rows.iter().find(|r| r["source"]["line_number"] == 1).unwrap();
    assert!(long["line_text"].as_str().unwrap().len() > 500_000, "whole line by default");

    for (args, env) in [(vec!["--snippet", "20"], vec![]), (vec![], vec![("PROWLCAT_SNIPPET", "20")])] {
        let mut a = vec!["needle@example.com", "-p", "dump.sql"];
        a.extend(args);
        let (code, rows) = search_env(d, &a, &env);
        assert_eq!((code, rows.len()), (0, 2));
        let r = rows.iter().find(|r| r["source"]["line_number"] == 1).unwrap();
        let text = r["line_text"].as_str().unwrap();
        assert!(text.len() < 120, "{text}");
        assert!(text.starts_with('…') && text.ends_with('…'), "{text}");
        let span = r["matched_span"].as_array().unwrap();
        let (a, b) = (span[0].as_u64().unwrap() as usize, span[1].as_u64().unwrap() as usize);
        assert_eq!(&text[a..b], "needle@example.com");
        let short = rows.iter().find(|r| r["source"]["line_number"] == 0).unwrap();
        assert_eq!(short["line_text"], "short needle@example.com line", "short lines are untouched");
    }
}

#[test]
fn a_line_cap_bounds_what_is_reported_and_finds_the_same_things() {
    let c = long_line_corpus();
    let d = c.path();
    let whole = lines_of_matches(&search(d, &["example.com", "-t", "domain", "-p", "dump.sql"]).1);
    for cap in ["4096", "8K", "64K", "1M"] {
        let (code, rows) = search(d, &["example.com", "-t", "domain", "-p", "dump.sql", "--max-line-bytes", cap]);
        assert_eq!(code, 0);
        let limit = prowlcat_size(cap);
        assert!(rows.iter().all(|r| r["line_text"].as_str().unwrap().len() <= limit), "cap {cap}");
        assert_eq!(lines_of_matches(&rows), whole, "cap {cap}");
    }
    let (code, rows) = search(d, &["needle@example.com", "-p", "dump.sql", "--max-line-bytes", "8K"]);
    assert_eq!((code, rows.len()), (0, 2));
}

/// The matches' lines and values, without how often each was reported.
fn lines_of_matches(rows: &[Value]) -> BTreeSet<String> {
    rows.iter().map(|r| format!("{}|{}", r["source"]["line_number"], r["extracted"])).collect()
}

fn prowlcat_size(s: &str) -> usize {
    let n: usize = s.trim_end_matches(|c: char| c.is_alphabetic()).parse().unwrap();
    match s.chars().last().unwrap() {
        'K' => n << 10,
        'M' => n << 20,
        _ => n,
    }
}

#[test]
fn a_bad_size_is_an_error() {
    let c = long_line_corpus();
    let o = run(c.path(), &["search", "x@y.test", "-p", "dump.sql", "--no-progress", "--max-line-bytes", "lots"]);
    assert_eq!(o.code, 2);
    assert!(o.stderr.contains("not a size"), "{}", o.stderr);
}

// ---- the shape of the command line itself ----

fn json_lines(text: &str) -> Vec<Value> {
    text.lines().map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("not JSON ({e}): {l}"))).collect()
}

#[test]
fn recursion_is_the_default_and_can_be_turned_off() {
    let c = corpus();
    let d = c.path();
    fs::create_dir_all(d.join("sub/deeper")).unwrap();
    fs::write(d.join("sub/deeper/nested.txt"), "nested@example.com:p\n").unwrap();
    for flags in [&[][..], &["-r"][..]] {
        let mut a = vec!["nested@example.com", "-p", "."];
        a.extend_from_slice(flags);
        assert_eq!(search(d, &a).0, 0, "{flags:?}");
    }
    assert_eq!(search(d, &["nested@example.com", "-p", ".", "--no-recursive"]).0, 1);
    assert_eq!(run(d, &["search", "x", "-p", ".", "-r", "--no-recursive", "--no-progress"]).code, 2, "contradiction");
    let files = |args: &[&str]| -> u32 {
        let o = run(d, &[&["stats", "-p", "."][..], args].concat());
        let line = o.stdout.lines().find(|l| l.trim_start().starts_with("files:")).unwrap_or_else(|| panic!("{}", o.stdout));
        line.split_whitespace().last().unwrap().parse().unwrap()
    };
    assert_eq!(files(&[]), files(&["--no-recursive"]) + 1, "exactly the nested file is skipped");
}

#[test]
fn types_and_formats_are_checked_by_the_parser() {
    let c = corpus();
    let d = c.path();
    assert_eq!(search(d, &["alice@example.com", "-t", "EMAIL", "-p", "plain.txt"]).0, 0, "case does not matter");
    for bad in [["-t", "bogus"], ["--format", "bogus"]] {
        let o = run(d, &["search", "x", "-p", "plain.txt", "--no-progress", bad[0], bad[1]]);
        assert_eq!(o.code, 2);
        assert!(o.stderr.contains("possible values"), "{}", o.stderr);
    }
}

#[test]
fn format_without_a_file_writes_to_standard_output() {
    let c = corpus();
    let d = c.path();
    let base = ["search", "example.com", "-t", "domain", "-p", "plain.txt"];
    // No --no-progress: the banner must not be in front of the data.
    for (format, parse) in [("ndjson", 0), ("jsonl", 0), ("json", 1), ("csv", 2)] {
        let mut a = base.to_vec();
        a.extend_from_slice(&["--format", format]);
        let o = run(d, &a);
        assert_eq!(o.code, 0, "{format}: {}", o.stderr);
        match parse {
            0 => assert!(json_lines(&o.stdout).len() >= 4, "{format}"),
            1 => assert!(serde_json::from_str::<Value>(&o.stdout).unwrap()["results"].as_array().unwrap().len() >= 4),
            _ => assert!(o.stdout.starts_with("query,kind,match_kind,path,line,byte_offset,context"), "{}", o.stdout),
        }
    }
    // `-o -` is standard output too: NDJSON unless a format is given.
    let mut a = base.to_vec();
    a.extend_from_slice(&["-o", "-"]);
    assert!(json_lines(&run(d, &a).stdout).len() >= 4);
    a.extend_from_slice(&["--format", "csv"]);
    assert!(run(d, &a).stdout.starts_with("query,"));
    // The table is still the default, and `--format table` is the same thing.
    let table = run(d, &[&base[..], &["--no-progress"]].concat());
    let named = run(d, &[&base[..], &["--no-progress", "--format", "table"]].concat());
    assert_eq!(table.stdout, named.stdout);
    assert!(table.stdout.contains("└─"));
    // No matches: exit 1, and nothing but an empty document on stdout.
    let none = run(d, &["search", "nobody@example.com", "-p", "plain.txt", "--format", "ndjson"]);
    assert_eq!((none.code, none.stdout.as_str()), (1, ""));
}

// ---- bzip2, xz, 7z and the tar variants ----

fn bz2(data: &[u8]) -> Vec<u8> {
    let mut e = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::fast());
    e.write_all(data).unwrap();
    e.finish().unwrap()
}

fn xz(data: &[u8]) -> Vec<u8> {
    let mut e = liblzma::write::XzEncoder::new(Vec::new(), 1);
    e.write_all(data).unwrap();
    e.finish().unwrap()
}

fn seven(members: &[(&str, &str)], password: Option<&str>) -> Vec<u8> {
    use sevenz_rust2::{encoder_options, ArchiveEntry, ArchiveWriter};
    let mut w = ArchiveWriter::new(std::io::Cursor::new(Vec::new())).unwrap();
    if let Some(p) = password {
        w.set_content_methods(vec![
            encoder_options::AesEncoderOptions::new(p.into()).into(),
            encoder_options::Lzma2Options::from_level(1).into(),
        ]);
    }
    for (name, body) in members {
        w.push_archive_entry(ArchiveEntry::new_file(name), Some(body.as_bytes())).unwrap();
    }
    w.finish().unwrap().into_inner()
}

/// One file per container kind that is not in `corpus()`, each holding an email of its own.
fn more_containers() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    fs::write(p.join("one.txt.bz2"), bz2(b"bz@example.com:p\n")).unwrap();
    // pbzip2 and `cat a.bz2 b.bz2` make files of several streams: every one must be read.
    fs::write(p.join("multi.txt.bz2"), [bz2(b"bzfirst@example.com:p\n"), bz2(b"bzsecond@example.com:p\n")].concat()).unwrap();
    fs::write(p.join("one.txt.xz"), xz(b"xz@example.com:p\n")).unwrap();
    fs::write(p.join("multi.txt.xz"), [xz(b"xzfirst@example.com:p\n"), xz(b"xzsecond@example.com:p\n")].concat()).unwrap();
    let tar = tar_of(&[("inner/a.txt", "tar@example.com:p\n"), ("inner/b.txt", "other line\n")]);
    fs::write(p.join("bundle.tar.bz2"), bz2(&tar)).unwrap();
    fs::write(p.join("bundle.tar.xz"), xz(&tar)).unwrap();
    fs::write(p.join("bundle.tar.zst"), zstd::encode_all(&tar[..], 1).unwrap()).unwrap();
    fs::write(p.join("alias.tbz2"), bz2(&tar)).unwrap();
    fs::write(p.join("alias.txz"), xz(&tar)).unwrap();
    fs::write(p.join("open.7z"), seven(&[("s/one.txt", "sz@example.com:p\n"), ("s/two.txt", "sztwo@example.com:p\n")], None)).unwrap();
    fs::write(p.join("locked.7z"), seven(&[("secret.txt", "szlocked@example.com:p\n")], Some("s3cret"))).unwrap();
    // Recognised by what is inside, not by the name.
    fs::write(p.join("mystery.bin"), bz2(b"mysterybz@example.com:p\n")).unwrap();
    fs::write(p.join("blob.dat"), xz(b"blobxz@example.com:p\n")).unwrap();
    fs::write(p.join("passwords.txt"), "wrong\ns3cret\n").unwrap();
    dir
}

#[test]
fn bzip2_xz_7z_and_the_tar_variants_are_searched() {
    let c = more_containers();
    let d = c.path();
    let cases: &[(&str, &str, Option<&str>)] = &[
        ("bz@example.com", "one.txt.bz2", None),
        ("bzfirst@example.com", "multi.txt.bz2", None),
        ("bzsecond@example.com", "multi.txt.bz2", None),
        ("xz@example.com", "one.txt.xz", None),
        ("xzfirst@example.com", "multi.txt.xz", None),
        ("xzsecond@example.com", "multi.txt.xz", None),
        ("tar@example.com", "bundle.tar.bz2", Some("inner/a.txt")),
        ("tar@example.com", "bundle.tar.xz", Some("inner/a.txt")),
        ("tar@example.com", "bundle.tar.zst", Some("inner/a.txt")),
        ("tar@example.com", "alias.tbz2", Some("inner/a.txt")),
        ("tar@example.com", "alias.txz", Some("inner/a.txt")),
        ("sz@example.com", "open.7z", Some("s/one.txt")),
        ("sztwo@example.com", "open.7z", Some("s/two.txt")),
        ("mysterybz@example.com", "mystery.bin", None),
        ("blobxz@example.com", "blob.dat", None),
    ];
    for (target, file, member) in cases {
        let (code, rows) = search(d, &[target, "-p", file]);
        assert_eq!((code, rows.len()), (0, 1), "{target} in {file}");
        assert!(path_of(&rows[0]).ends_with(file), "{:?}", rows[0]);
        assert_eq!(rows[0]["source"]["archive_entry"].as_str(), *member, "{target} in {file}");
    }
    // A tar inside a stream is read as a tar: its text is not searched as one blob.
    let (_, rows) = search(d, &["other line", "-t", "text", "-p", "bundle.tar.bz2"]);
    assert_eq!(rows[0]["source"]["archive_entry"], "inner/b.txt");
}

#[test]
fn an_encrypted_7z_needs_a_candidate_password() {
    let c = more_containers();
    let d = c.path();
    let (code, rows) = search(d, &["szlocked@example.com", "-p", "locked.7z"]);
    assert_eq!((code, rows.len()), (1, 0), "no password");
    let (code, rows) = search(d, &["szlocked@example.com", "-p", "locked.7z", "--password", "wrong"]);
    assert_eq!((code, rows.len()), (1, 0), "wrong password");
    // An archive that could not be read is reported, not silently counted as "nothing found".
    let o = run(d, &["search", "szlocked@example.com", "-p", "locked.7z", "--no-progress"]);
    assert!(o.stdout.contains("warnings: 1 files failed"), "{}", o.stdout);
    let (code, rows) = search(d, &["szlocked@example.com", "-p", "locked.7z", "--password-file", "passwords.txt"]);
    assert_eq!((code, rows.len()), (0, 1), "password file");
    assert_eq!(rows[0]["source"]["archive_entry"], "secret.txt");
    let (code, rows) = search(d, &["sz@example.com", "-p", "open.7z", "--password", "unused"]);
    assert_eq!((code, rows.len()), (0, 1), "a password that is not needed is harmless");
}

#[test]
fn an_index_covers_the_new_containers() {
    let c = more_containers();
    let d = c.path();
    let src = d.join("indexed");
    fs::create_dir(&src).unwrap();
    for f in ["one.txt.bz2", "one.txt.xz", "bundle.tar.xz", "open.7z"] {
        fs::copy(d.join(f), src.join(f)).unwrap();
    }
    let o = run(d, &["index", "-p", "indexed", "-o", "idx"]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert!(o.stderr.contains("(0 files failed)"), "{}", o.stderr);
    for (target, file) in [("bz@example.com", "one.txt.bz2"), ("xz@example.com", "one.txt.xz"), ("tar@example.com", "bundle.tar.xz"), ("sz@example.com", "open.7z")] {
        let (code, rows) = search(d, &[target, "--index", "idx"]);
        assert_eq!((code, rows.len()), (0, 1), "{target}");
        assert!(path_of(&rows[0]).ends_with(file), "{:?}", rows[0]);
    }
}

// ---- RAR, through an external program ----

fn crc32(data: &[u8]) -> u32 {
    let mut c = flate2::Crc::new();
    c.update(data);
    c.sum()
}

/// A RAR 4 archive whose members are stored, not compressed: the format is simple enough to
/// write here, and `unrar t` accepts what this produces.
fn rar4_stored(members: &[(&str, &str)]) -> Vec<u8> {
    fn block(head_type: u8, flags: u16, add_size: Option<u32>, body: &[u8]) -> Vec<u8> {
        let size = 7 + add_size.map_or(0, |_| 4) + body.len();
        let mut b = vec![head_type];
        b.extend_from_slice(&flags.to_le_bytes());
        b.extend_from_slice(&(size as u16).to_le_bytes());
        if let Some(n) = add_size {
            b.extend_from_slice(&n.to_le_bytes());
        }
        b.extend_from_slice(body);
        let mut out = ((crc32(&b) & 0xFFFF) as u16).to_le_bytes().to_vec();
        out.extend_from_slice(&b);
        out
    }
    let mut out = b"Rar!\x1a\x07\x00".to_vec();
    out.extend(block(0x73, 0, None, &[0; 6]));
    for (name, data) in members {
        let mut body = Vec::new();
        body.extend_from_slice(&(data.len() as u32).to_le_bytes()); // unpacked size
        body.push(3); // host OS: Unix
        body.extend_from_slice(&crc32(data.as_bytes()).to_le_bytes());
        body.extend_from_slice(&0x5A21_0000u32.to_le_bytes()); // time
        body.extend_from_slice(&[29, 0x30]); // version, method: store
        body.extend_from_slice(&(name.len() as u16).to_le_bytes());
        body.extend_from_slice(&(0o100644u32 << 16).to_le_bytes());
        body.extend_from_slice(name.as_bytes());
        out.extend(block(0x74, 0x8000, Some(data.len() as u32), &body));
        out.extend_from_slice(data.as_bytes());
    }
    out.extend_from_slice(&[0xC4, 0x3D, 0x7B, 0x00, 0x40, 0x07, 0x00]); // end of archive
    out
}

fn rar_corpus() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let rar = rar4_stored(&[
        ("a/one.txt", "rarone@example.com:p\n"),
        // No newline at the end: the next member's first line must not be glued to it.
        ("two.txt", "rartwo@example.com:p\nsecond line\nrarlast@example.com:p"),
        ("three.txt", "rarthree@example.com:p\n"),
    ]);
    fs::write(dir.path().join("dump.rar"), &rar).unwrap();
    fs::write(dir.path().join("renamed.bin"), &rar).unwrap();
    dir
}

/// True when this machine has a program that reads RAR (`unrar` or 7-Zip); the tests that need
/// one say so and pass otherwise.
fn have_rar_program(d: &Path) -> bool {
    let o = run(d, &["search", "rarone@example.com", "-p", "dump.rar", "--no-progress"]);
    let missing = o.stdout.contains("no program to read RAR") || o.stderr.contains("no program to read RAR");
    if missing {
        eprintln!("skipped: no RAR program (7zz or 7z, or PROWLCAT_RAR_TOOL) installed");
    }
    !missing
}

#[test]
fn rar_archives_are_read_member_by_member() {
    let c = rar_corpus();
    let d = c.path();
    if !have_rar_program(d) {
        return;
    }
    let cases = [
        ("rarone@example.com", "dump.rar", "a/one.txt"),
        ("rartwo@example.com", "dump.rar", "two.txt"),
        ("rarlast@example.com", "dump.rar", "two.txt"),
        ("rarthree@example.com", "dump.rar", "three.txt"),
        ("rarthree@example.com", "renamed.bin", "three.txt"), // recognised from its first bytes
    ];
    for (target, file, member) in cases {
        let (code, rows) = search(d, &[target, "-p", file]);
        assert_eq!((code, rows.len()), (0, 1), "{target} in {file}");
        assert_eq!(rows[0]["source"]["archive_entry"], member, "{target} in {file}");
        assert!(rows[0]["line_text"].as_str().unwrap().starts_with(&target[..7]), "lines are not glued together: {:?}", rows[0]["line_text"]);
    }
    let (_, rows) = search(d, &["second line", "-t", "text", "-p", "dump.rar"]);
    assert_eq!((rows.len(), rows[0]["source"]["line_number"].as_u64()), (1, Some(1)), "line numbers restart in each member");
}

#[test]
fn a_rar_that_cannot_be_read_is_reported_not_skipped() {
    let c = rar_corpus();
    let d = c.path();
    let o = run_env(
        d,
        &["search", "rarone@example.com", "-p", "dump.rar", "--no-progress"],
        &[("PROWLCAT_RAR_TOOL", "/definitely/not/installed/unrar")],
    );
    assert_eq!(o.code, 1);
    assert!(o.stdout.contains("warnings: 1 files failed"), "{}", o.stdout);
}

#[test]
fn stats_check_reports_what_a_search_could_not_read() {
    let c = corpus();
    let d = c.path();
    let more = more_containers();
    fs::copy(more.path().join("locked.7z"), d.join("locked.7z")).unwrap();
    fs::copy(more.path().join("one.txt.xz"), d.join("fine.txt.xz")).unwrap();
    fs::write(d.join("broken.txt.gz"), b"this is not gzip data at all, just text pretending").unwrap();
    fs::write(d.join("broken.7z"), b"7z but not really, a few bytes only").unwrap();

    let check = |extra: &[&str]| run(d, &[&["stats", "-p", ".", "--check"][..], extra].concat());
    let o = check(&[]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    for want in ["need a password:       2", "cannot be read:        2"] {
        assert!(o.stdout.contains(want), "missing {want:?} in\n{}", o.stdout);
    }
    // Both encrypted archives are named, and so are the two that are damaged.
    for name in ["locked.zip", "locked.7z", "broken.txt.gz", "broken.7z"] {
        assert!(o.stdout.contains(name), "{name} not named in\n{}", o.stdout);
    }
    assert!(!o.stdout.contains("fine.txt.xz"), "a readable file is not listed");

    let o = check(&["--password-file", "passwords.txt"]);
    assert!(o.stdout.contains("need a password:       0"), "{}", o.stdout);
    assert!(o.stdout.contains("cannot be read:        2"), "{}", o.stdout);

    // Without --check stats is what it was.
    assert!(!run(d, &["stats", "-p", "."]).stdout.contains("Readability"));
}

#[test]
fn stats_check_says_when_there_is_no_program_for_rar() {
    let c = rar_corpus();
    let d = c.path();
    let nothing = tempfile::tempdir().unwrap();
    let o = run_env(d, &["stats", "-p", "dump.rar", "--check"], &[("PATH", nothing.path().to_str().unwrap()), ("PROWLCAT_RAR_TOOL", "")]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert!(o.stdout.contains("need a RAR program:    1"), "{}", o.stdout);
    assert!(o.stdout.contains("dump.rar"), "{}", o.stdout);
    // And with a program, the same archive is fine.
    if have_rar_program(d) {
        let o = run(d, &["stats", "-p", "dump.rar", "--check"]);
        assert!(o.stdout.contains("can be read:           1 of 1"), "{}", o.stdout);
    }
}

#[test]
fn stats_counts_rar() {
    let c = rar_corpus();
    let o = run(c.path(), &["stats", "-p", "."]);
    assert!(o.stdout.contains("rar:        2"), "{}", o.stdout);
}

#[test]
fn stats_counts_the_new_kinds() {
    let c = more_containers();
    let o = run(c.path(), &["stats", "-p", "."]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    for want in ["bzip2:      5", "xz:         5", "7z:         2", "zstd:       1", "tar/tar.gz: 0"] {
        assert!(o.stdout.contains(want), "missing {want:?} in\n{}", o.stdout);
    }
}

#[test]
fn max_matches_caps_each_target() {
    let c = corpus();
    let (code, rows) = search(c.path(), &["example.com", "-t", "domain", "-p", "plain.txt", "-m", "2"]);
    assert_eq!(code, 0);
    assert_eq!(rows.len(), 2);
}

#[test]
fn an_index_gives_the_same_matches_as_a_scan() {
    let c = corpus();
    let d = c.path();
    // Only what can be indexed without a password.
    let idx_src = d.join("indexed");
    fs::create_dir(&idx_src).unwrap();
    for f in ["plain.txt", "gzipped.txt.gz", "bundle.tar.gz", "open.zip"] {
        fs::copy(d.join(f), idx_src.join(f)).unwrap();
    }
    let o = run(d, &["index", "-p", "indexed", "-r", "-o", "idx"]);
    assert_eq!(o.code, 0, "{}", o.stderr);

    for (target, kind) in [
        ("alice@example.com", "email"),
        ("example.com", "domain"),
        ("203.0.113.42", "ip"),
        ("10.0.0.0/8", "cidr"),
        ("gzip@example.com", "email"),
    ] {
        let scan = lines_of(&search(d, &[target, "-t", kind, "-p", "indexed", "-r"]).1);
        let (code, rows) = search(d, &[target, "-t", kind, "--index", "idx"]);
        assert_eq!(lines_of(&rows), scan, "{target}");
        assert_eq!(code, if scan.is_empty() { 1 } else { 0 }, "{target}");
    }
    assert_eq!(search(d, &["Winter2024", "-t", "text", "--index", "idx"]).0, 2, "text needs a scan");
}

#[test]
fn stats_counts_each_kind() {
    let c = corpus();
    let o = run(c.path(), &["stats", "-p", ".", "-r"]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    for want in ["plaintext:  2", "gzip:       1", "zstd:       1", "tar/tar.gz: 2", "zip:        2"] {
        assert!(o.stdout.contains(want), "missing {want:?} in\n{}", o.stdout);
    }
}

/// FNV-1a: a hash that is the same on every platform and Rust version.
fn fnv(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ b as u64).wrapping_mul(0x0100_0000_01b3))
}

/// A fixed 60,000-line corpus and a fixed set of searches. If the digest changes, search
/// results changed. That is only ever intended when the change log says so; then update it.
#[test]
fn golden_digest_of_a_fixed_corpus() {
    let dir = tempfile::tempdir().unwrap();
    let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut next = move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    let domains = ["example.com", "mail.example.com", "example.org", "acme.example", "notexample.com", "example.community"];
    let mut text = String::new();
    for i in 0..60_000u32 {
        let d = domains[(next() % domains.len() as u64) as usize];
        match next() % 5 {
            0 => text.push_str(&format!("https://{d}/login:user{i}:pw{}\n", next() % 1000)),
            1 => text.push_str(&format!("{}.{}.{}.{} host{i}\n", next() % 256, next() % 256, next() % 256, next() % 256)),
            _ => text.push_str(&format!("user{}@{d}:pw{}\n", i % 9000, next() % 1000)),
        }
    }
    fs::write(dir.path().join("big.txt"), text).unwrap();

    let mut acc = String::new();
    for (target, kind) in [("example.com", "domain"), ("acme.example", "domain"), ("user42@example.com", "email"), ("10.0.0.0/8", "cidr"), ("pw7", "text")] {
        let (_, rows) = search(dir.path(), &[target, "-t", kind, "-p", "big.txt"]);
        acc.push_str(&format!("#{target} {}\n", rows.len()));
        for r in &rows {
            acc.push_str(&format!("{} {}\n", r["source"]["line_number"], r["line_text"].as_str().unwrap()));
        }
    }
    assert_eq!(format!("{:016x}", fnv(&acc)), GOLDEN, "search results changed");
}

const GOLDEN: &str = "e1079df1252b6e0c";
