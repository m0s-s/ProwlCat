use std::path::PathBuf;

use anyhow::Result;
use prowlcat_source::SourceEnumerator;

pub fn run(paths: Vec<PathBuf>, recursive: bool, check: bool, passwords: &[String]) -> Result<std::process::ExitCode> {
    let enumerator = SourceEnumerator::new(&paths, recursive)?;
    let entries = enumerator.collect()?;

    let total_bytes: u64 = entries.iter().map(|e| e.size_bytes).sum();
    use prowlcat_source::CompressionKind as K;
    let count = |f: fn(K) -> bool| entries.iter().filter(|e| f(e.compression)).count();
    let plain = count(|k| k == K::None);
    let gz = count(|k| k == K::Gzip);
    let zst = count(|k| matches!(k, K::Zstd | K::TarZst));
    let tars = count(|k| matches!(k, K::Tar | K::TarGz));
    let zips = count(|k| k == K::Zip);
    let bz2 = count(|k| matches!(k, K::Bzip2 | K::TarBz2));
    let xz = count(|k| matches!(k, K::Xz | K::TarXz));
    let sevenz = count(|k| k == K::SevenZ);
    let rar = count(|k| k == K::Rar);

    println!("ProwlCat stats");
    println!("  files:      {}", entries.len());
    println!("  plaintext:  {}", plain);
    println!("  gzip:       {}", gz);
    println!("  zstd:       {}", zst);
    println!("  tar/tar.gz: {}", tars);
    println!("  zip:        {}", zips);
    println!("  bzip2:      {}", bz2);
    println!("  xz:         {}", xz);
    println!("  7z:         {}", sevenz);
    println!("  rar:        {}", rar);
    println!(
        "  total size: {:.2} GB",
        total_bytes as f64 / (1024.0 * 1024.0 * 1024.0)
    );

    if check {
        readability(&entries, passwords);
    }

    Ok(std::process::ExitCode::SUCCESS)
}

/// Open every file, without searching it, and say which ones a search would not be able to read.
fn readability(entries: &[prowlcat_source::SourceEntry], passwords: &[String]) {
    use prowlcat_decode::{probe, Probe};

    let (mut readable, mut password, mut no_tool) = (0usize, Vec::new(), Vec::new());
    let mut broken: Vec<(String, String)> = Vec::new();
    for e in entries {
        let name = e.path.display().to_string();
        match probe(&e.path, e.compression, passwords) {
            Probe::Readable => readable += 1,
            Probe::NeedsPassword => password.push(name),
            Probe::NoTool => no_tool.push(name),
            Probe::Unreadable(why) => broken.push((name, why)),
        }
    }

    println!();
    println!("Readability (every file is opened and its start read; nothing is searched)");
    println!("  can be read:           {} of {}", readable, entries.len());
    println!("  need a password:       {}", password.len());
    println!("  need a RAR program:    {}", no_tool.len());
    println!("  cannot be read:        {}", broken.len());

    const SHOWN: usize = 20;
    let list = |title: &str, hint: &str, items: &[String]| {
        if items.is_empty() {
            return;
        }
        println!();
        println!("{title} ({hint}):");
        for name in items.iter().take(SHOWN) {
            println!("  {name}");
        }
        if items.len() > SHOWN {
            println!("  … and {} more", items.len() - SHOWN);
        }
    };
    list("Need a password", "--password or --password-file", &password);
    list("Need a RAR program", "install 7-Zip", &no_tool);
    if !broken.is_empty() {
        println!();
        println!("Cannot be read (damaged, or a format that is not what its name says):");
        for (name, why) in broken.iter().take(SHOWN) {
            let why = why.lines().next().unwrap_or("");
            println!("  {name}: {why}");
        }
        if broken.len() > SHOWN {
            println!("  … and {} more", broken.len() - SHOWN);
        }
    }
    println!();
    println!("Damage further inside a file than its start is not found by this check.");
}
