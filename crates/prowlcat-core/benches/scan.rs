//! Throughput benchmarks over a synthetic corpus (no real breach data).
//!
//! Run with `cargo bench -p prowlcat-core`. Set `PROWLCAT_BENCH_MB` to change the corpus size
//! (default 32 MiB). Throughput is reported against the uncompressed corpus size.

use std::io::Write;
use std::path::PathBuf;

use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use prowlcat_core::{ScanConfig, ScanOrchestrator, TargetParser, TypeHint};

/// Deterministic pseudo-random lines shaped like credential dumps; ~1 in 5000 lines hits a target.
fn build_corpus(target_bytes: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(target_bytes + 256);
    let mut x: u64 = 0x9E3779B97F4A7C15;
    let mut i = 0u64;
    while out.len() < target_bytes {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        let line = if i % 5000 == 0 {
            format!("target.user{}@bench-target.test:Passw0rd{}\n", i, x % 10_000)
        } else if i % 7919 == 0 {
            format!("https://login.bench-target.test/u{}:{}.{}.{}.{}\n", i, 10, x % 256, (x >> 8) % 256, (x >> 16) % 256)
        } else {
            format!(
                "user{:x}@mail{}.example:pw{:x}{:x} 192.168.{}.{}\n",
                x % 1_000_000,
                x % 97,
                x,
                i,
                (x >> 24) % 256,
                (x >> 32) % 256
            )
        };
        out.extend_from_slice(line.as_bytes());
        i += 1;
    }
    out
}

fn write_corpus() -> (PathBuf, u64) {
    let mb: usize = std::env::var("PROWLCAT_BENCH_MB").ok().and_then(|v| v.parse().ok()).unwrap_or(32);
    let dir = std::env::temp_dir().join(format!("prowlcat-bench-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let data = build_corpus(mb * 1024 * 1024);

    std::fs::write(dir.join("corpus.txt"), &data).unwrap();
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    gz.write_all(&data).unwrap();
    std::fs::write(dir.join("corpus.txt.gz"), gz.finish().unwrap()).unwrap();
    std::fs::write(dir.join("corpus.txt.zst"), zstd::encode_all(&data[..], 3).unwrap()).unwrap();
    (dir, data.len() as u64)
}

fn run(path: PathBuf, hint: TypeHint, targets: &[&str], threads: usize, mmap_mb: u64) -> usize {
    let queries = TargetParser::new(hint).parse_many(targets.iter().copied()).unwrap();
    let config = ScanConfig {
        paths: vec![path],
        recursive: false,
        threads,
        mmap_threshold_mb: mmap_mb,
        ..ScanConfig::default()
    };
    ScanOrchestrator::new(config, queries).run().unwrap().1.matches
}

fn bench_scan(c: &mut Criterion) {
    let (dir, size) = write_corpus();
    let mut g = c.benchmark_group("scan");
    g.throughput(Throughput::Bytes(size));
    g.sample_size(10);

    let plain = dir.join("corpus.txt");
    // Formats (single target, single thread isolates the decoder cost)
    g.bench_function("plain_stream/domain", |b| {
        b.iter(|| run(plain.clone(), TypeHint::Domain, &["bench-target.test"], 1, 0))
    });
    g.bench_function("plain_mmap/domain", |b| {
        b.iter(|| run(plain.clone(), TypeHint::Domain, &["bench-target.test"], 1, 1))
    });
    g.bench_function("gzip/domain", |b| {
        b.iter(|| run(dir.join("corpus.txt.gz"), TypeHint::Domain, &["bench-target.test"], 1, 0))
    });
    g.bench_function("zstd/domain", |b| {
        b.iter(|| run(dir.join("corpus.txt.zst"), TypeHint::Domain, &["bench-target.test"], 1, 0))
    });

    // Matchers on the memory-mapped file
    g.bench_function("email", |b| {
        b.iter(|| run(plain.clone(), TypeHint::Email, &["target.user0@bench-target.test"], 1, 1))
    });
    g.bench_function("cidr", |b| {
        b.iter(|| run(plain.clone(), TypeHint::Cidr, &["10.0.0.0/8"], 1, 1))
    });
    g.bench_function("text_loose", |b| {
        b.iter(|| run(plain.clone(), TypeHint::Text, &["Passw0rd42"], 1, 1))
    });
    let many: Vec<String> = (0..1000).map(|i| format!("user{i}.nomatch@absent.test")).collect();
    let many_refs: Vec<&str> = many.iter().map(String::as_str).collect();
    g.bench_function("email_x1000_targets", |b| {
        b.iter(|| run(plain.clone(), TypeHint::Email, &many_refs, 1, 1))
    });
    g.finish();

    // Parallel scaling needs several files: split the corpus into shards.
    let shards = dir.join("shards");
    std::fs::create_dir_all(&shards).unwrap();
    let data = std::fs::read(&plain).unwrap();
    let cut = data.len() / 8;
    for i in 0..8 {
        let start = if i == 0 { 0 } else { data[..i * cut].iter().rposition(|&b| b == b'\n').unwrap() + 1 };
        let end = if i == 7 { data.len() } else { data[..(i + 1) * cut].iter().rposition(|&b| b == b'\n').unwrap() + 1 };
        std::fs::write(shards.join(format!("s{i}.txt")), &data[start..end]).unwrap();
    }
    let mut g = c.benchmark_group("parallel");
    g.throughput(Throughput::Bytes(size));
    g.sample_size(10);
    for threads in [1usize, 2, 4, 8] {
        g.bench_function(format!("domain_8_files/{threads}_threads"), |b| {
            b.iter(|| run(shards.clone(), TypeHint::Domain, &["bench-target.test"], threads, 1))
        });
    }
    g.finish();
    let _ = std::fs::remove_dir_all(&dir);
}

criterion_group!(benches, bench_scan);
criterion_main!(benches);
