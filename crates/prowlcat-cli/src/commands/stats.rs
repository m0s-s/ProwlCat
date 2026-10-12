use std::path::PathBuf;

use anyhow::Result;
use prowlcat_source::SourceEnumerator;

pub fn run(paths: Vec<PathBuf>, recursive: bool) -> Result<std::process::ExitCode> {
    let enumerator = SourceEnumerator::new(&paths, recursive)?;
    let entries = enumerator.collect()?;

    let total_bytes: u64 = entries.iter().map(|e| e.size_bytes).sum();
    use prowlcat_source::CompressionKind as K;
    let count = |f: fn(K) -> bool| entries.iter().filter(|e| f(e.compression)).count();
    let plain = count(|k| k == K::None);
    let gz = count(|k| k == K::Gzip);
    let zst = count(|k| k == K::Zstd);
    let tars = count(|k| matches!(k, K::Tar | K::TarGz));
    let zips = count(|k| k == K::Zip);

    println!("ProwlCat stats");
    println!("  files:      {}", entries.len());
    println!("  plaintext:  {}", plain);
    println!("  gzip:       {}", gz);
    println!("  zstd:       {}", zst);
    println!("  tar/tar.gz: {}", tars);
    println!("  zip:        {}", zips);
    println!(
        "  total size: {:.2} GB",
        total_bytes as f64 / (1024.0 * 1024.0 * 1024.0)
    );

    Ok(std::process::ExitCode::SUCCESS)
}
