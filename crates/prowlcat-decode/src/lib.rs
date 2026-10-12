//! Streaming line readers for plaintext and compressed sources.

mod plain;

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::Result;
use flate2::read::GzDecoder;
use prowlcat_source::CompressionKind;
use tracing::warn;

pub use plain::PlainReader;
use plain::{read_lines, Counted};

/// Line reader over plaintext, compressed streams, and tar/zip archives.
///
/// `for_each_line` callback: `(archive_entry, line_number, byte_offset, line)`.
/// `archive_entry` is `Some(name)` for lines read from inside a tar/zip archive;
/// line numbers and offsets restart for each member.
pub struct LineReader {
    path: PathBuf,
    compression: CompressionKind,
    mmap_threshold: u64,
    passwords: Vec<String>,
    progress: Option<Arc<AtomicU64>>,
}

impl LineReader {
    pub fn open(path: &Path, compression: CompressionKind, mmap_threshold: u64) -> Result<Self> {
        // Fail early if the file cannot be opened at all.
        std::fs::File::open(path)?;
        Ok(Self {
            path: path.to_path_buf(),
            compression,
            mmap_threshold,
            passwords: Vec::new(),
            progress: None,
        })
    }

    /// Report raw (on-disk) bytes consumed into `counter`. On completion the counter is
    /// topped up to the file size, so progress reaches 100% even for mmap and zip reads.
    pub fn with_progress(mut self, counter: Arc<AtomicU64>) -> Self {
        self.progress = Some(counter);
        self
    }

    /// Candidate passwords tried, in order, on encrypted zip members.
    pub fn with_passwords(mut self, passwords: &[String]) -> Self {
        self.passwords = passwords.to_vec();
        self
    }

    pub fn for_each_line(&mut self, mut f: impl FnMut(Option<&str>, u64, u64, &[u8])) -> Result<()> {
        let local = Arc::new(AtomicU64::new(0));
        let file = || -> Result<Counted<std::fs::File>> {
            let f = std::fs::File::open(&self.path)?;
            let len = f.metadata()?.len();
            Ok(Counted::new(f, self.progress.clone(), local.clone(), len))
        };
        let result = match self.compression {
            CompressionKind::None => PlainReader::open_counting(
                &self.path,
                self.mmap_threshold,
                self.progress.clone(),
                local.clone(),
            )
            .and_then(|mut p| p.for_each_line(|n, o, l| f(None, n, o, l))),
            CompressionKind::Gzip => read_lines(GzDecoder::new(file()?), |n, o, l| f(None, n, o, l)),
            CompressionKind::Zstd => {
                let dec = zstd::stream::read::Decoder::new(file()?)?;
                read_lines(dec, |n, o, l| f(None, n, o, l))
            }
            CompressionKind::Tar => scan_tar(file()?, &self.path, &mut f),
            CompressionKind::TarGz => scan_tar(GzDecoder::new(file()?), &self.path, &mut f),
            CompressionKind::Zip => scan_zip(file()?, &self.path, &self.passwords, &mut f),
        };
        if let Some(g) = &self.progress {
            let size = std::fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0);
            g.fetch_add(size.saturating_sub(local.load(Ordering::Relaxed)), Ordering::Relaxed);
        }
        result
    }
}

/// Walk tar members, scanning each regular file. A failing member is skipped (fail-soft).
fn scan_tar<R: Read>(
    reader: R,
    path: &Path,
    f: &mut impl FnMut(Option<&str>, u64, u64, &[u8]),
) -> Result<()> {
    let mut archive = tar::Archive::new(reader);
    for entry in archive.entries()? {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                warn!(path = %path.display(), error = %e, "tar entry unreadable; stopping archive");
                break;
            }
        };
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let name = entry.path().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
        if let Err(e) = read_lines(entry, |n, o, l| f(Some(&name), n, o, l)) {
            warn!(path = %path.display(), member = %name, error = %e, "tar member failed");
        }
    }
    Ok(())
}

/// Walk zip members. Encrypted members are opened with the first working candidate password
/// (the last successful one is tried first); members that cannot be opened are skipped with a warning.
fn scan_zip<R: Read + std::io::Seek>(
    reader: R,
    path: &Path,
    passwords: &[String],
    f: &mut impl FnMut(Option<&str>, u64, u64, &[u8]),
) -> Result<()> {
    let mut archive = zip::ZipArchive::new(reader)?;
    let mut preferred: Option<usize> = None;
    for i in 0..archive.len() {
        let encrypted = match archive.by_index_raw(i) {
            Ok(m) => m.encrypted(),
            Err(e) => {
                warn!(path = %path.display(), index = i, error = %e, "zip member unreadable");
                continue;
            }
        };

        let member = if !encrypted {
            archive.by_index(i)
        } else {
            let order: Vec<usize> = preferred
                .into_iter()
                .chain((0..passwords.len()).filter(|&k| Some(k) != preferred))
                .collect();
            let mut found = None;
            for k in order {
                // Probe with a throwaway handle; borrow rules require reopening on success.
                if archive.by_index_decrypt(i, passwords[k].as_bytes()).is_ok() {
                    found = Some(k);
                    break;
                }
            }
            match found {
                Some(k) => {
                    preferred = Some(k);
                    archive.by_index_decrypt(i, passwords[k].as_bytes())
                }
                None => {
                    warn!(
                        path = %path.display(), index = i,
                        "encrypted zip member skipped: no supplied password worked (use --password / --password-file)"
                    );
                    continue;
                }
            }
        };

        let member = match member {
            Ok(m) => m,
            Err(e) => {
                warn!(path = %path.display(), index = i, error = %e, "zip member skipped");
                continue;
            }
        };
        if member.is_dir() {
            continue;
        }
        let name = member.name().to_string();
        if let Err(e) = read_lines(member, |n, o, l| f(Some(&name), n, o, l)) {
            warn!(path = %path.display(), member = %name, error = %e, "zip member failed (bad password or corrupt data)");
        }
    }
    Ok(())
}
