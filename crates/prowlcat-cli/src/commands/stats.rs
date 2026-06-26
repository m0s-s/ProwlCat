use std::path::PathBuf;

use anyhow::Result;
use prowlcat_source::SourceEnumerator;

pub fn run(paths: Vec<PathBuf>, recursive: bool) -> Result<std::process::ExitCode> {
    let enumerator = SourceEnumerator::new(&paths, recursive)?;
    let entries = enumerator.collect()?;

    let total_bytes: u64 = entries.iter().map(|e| e.size_bytes).sum();
    let gz = entries
        .iter()
        .filter(|e| matches!(e.compression, prowlcat_source::CompressionKind::Gzip))
        .count();
    let plain = entries.len() - gz;

    println!("ProwlCat stats");
    println!("  files:      {}", entries.len());
    println!("  plaintext:  {}", plain);
    println!("  gzip:       {}", gz);
    println!(
        "  total size: {:.2} GB",
        total_bytes as f64 / (1024.0 * 1024.0 * 1024.0)
    );

    Ok(std::process::ExitCode::SUCCESS)
}
