//! Streaming line readers for plaintext and compressed sources.

mod plain;
mod rar;

use std::io::{Read, Seek};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::Result;
use flate2::read::GzDecoder;
use prowlcat_source::CompressionKind;
use tracing::warn;

pub use plain::PlainReader;
use plain::{read_lines, Counted};
pub use rar::rar_tool_available;
pub use plain::{MIN_MAX_LINE, PIECE_OVERLAP};

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
    max_line: usize,
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
            max_line: 0,
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

    /// Split lines longer than `bytes` into overlapping pieces when read with
    /// [`for_each_piece`](Self::for_each_piece), so memory stays bounded whatever the line
    /// length. `0` (the default) keeps every line whole. Values below [`MIN_MAX_LINE`] are raised
    /// to it.
    pub fn with_max_line_bytes(mut self, bytes: usize) -> Self {
        self.max_line = if bytes == 0 { 0 } else { bytes.max(MIN_MAX_LINE) };
        self
    }

    /// Every line whole, however long (the line cap does not apply).
    pub fn for_each_line(&mut self, mut f: impl FnMut(Option<&str>, u64, u64, &[u8])) -> Result<()> {
        self.run(0, &mut |m, n, o, _, l| f(m, n, o, l))
    }

    /// Like [`for_each_line`](Self::for_each_line), but lines longer than the cap set with
    /// [`with_max_line_bytes`](Self::with_max_line_bytes) arrive in pieces. Callback:
    /// `(archive_entry, line_number, byte_offset_of_piece, skip, piece)`; `skip` is how many
    /// leading bytes of the piece the previous piece of the same line already held, so a match
    /// wholly inside them has been seen already. Without a cap `skip` is always 0.
    pub fn for_each_piece(&mut self, mut f: impl FnMut(Option<&str>, u64, u64, usize, &[u8])) -> Result<()> {
        self.run(self.max_line, &mut f)
    }

    fn run(&mut self, cap: usize, f: &mut impl FnMut(Option<&str>, u64, u64, usize, &[u8])) -> Result<()> {
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
            .and_then(|mut p| p.for_each_piece(cap, |n, o, k, l| f(None, n, o, k, l))),
            CompressionKind::Gzip => read_lines(GzDecoder::new(file()?), cap, |n, o, k, l| f(None, n, o, k, l)),
            CompressionKind::Zstd => {
                let dec = zstd::stream::read::Decoder::new(file()?)?;
                read_lines(dec, cap, |n, o, k, l| f(None, n, o, k, l))
            }
            CompressionKind::Bzip2 => {
                read_lines(bzip2::read::MultiBzDecoder::new(file()?), cap, |n, o, k, l| f(None, n, o, k, l))
            }
            CompressionKind::Xz => {
                read_lines(liblzma::read::XzDecoder::new_multi_decoder(file()?), cap, |n, o, k, l| f(None, n, o, k, l))
            }
            CompressionKind::Tar => scan_tar(file()?, &self.path, cap, f),
            CompressionKind::TarGz => scan_tar(GzDecoder::new(file()?), &self.path, cap, f),
            CompressionKind::TarBz2 => scan_tar(bzip2::read::MultiBzDecoder::new(file()?), &self.path, cap, f),
            CompressionKind::TarXz => scan_tar(liblzma::read::XzDecoder::new_multi_decoder(file()?), &self.path, cap, f),
            CompressionKind::TarZst => scan_tar(zstd::stream::read::Decoder::new(file()?)?, &self.path, cap, f),
            CompressionKind::SevenZ => scan_7z(&file, &self.path, &self.passwords, cap, f),
            CompressionKind::Rar => rar::scan_rar(&self.path, &self.passwords, cap, f),
            CompressionKind::Zip => scan_zip(file()?, &self.path, &self.passwords, cap, f),
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
    cap: usize,
    f: &mut impl FnMut(Option<&str>, u64, u64, usize, &[u8]),
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
        if let Err(e) = read_lines(entry, cap, |n, o, k, l| f(Some(&name), n, o, k, l)) {
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
    cap: usize,
    f: &mut impl FnMut(Option<&str>, u64, u64, usize, &[u8]),
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
        if let Err(e) = read_lines(member, cap, |n, o, k, l| f(Some(&name), n, o, k, l)) {
            warn!(path = %path.display(), member = %name, error = %e, "zip member failed (bad password or corrupt data)");
        }
    }
    Ok(())
}

/// Walk a 7z archive. The archive is tried with no password first and then with each candidate
/// password in turn: a wrong or missing one makes opening fail (encrypted headers) or the first
/// member fail to decode, before any line has been handed over, and the next one is tried.
fn scan_7z<R: Read + Seek>(
    open: &dyn Fn() -> Result<R>,
    path: &Path,
    passwords: &[String],
    cap: usize,
    f: &mut impl FnMut(Option<&str>, u64, u64, usize, &[u8]),
) -> Result<()> {
    use sevenz_rust2::{ArchiveReader, Error as SevenZError, Password};

    let mut candidates = vec![Password::empty()];
    candidates.extend(passwords.iter().map(|p| Password::new(p)));
    let tries = candidates.len();
    let mut last: Option<SevenZError> = None;
    for (n, password) in candidates.into_iter().enumerate() {
        let mut delivered = false;
        let outcome = ArchiveReader::new(open()?, password).and_then(|mut archive| {
            archive.for_each_entries(|entry, reader| {
                if entry.is_directory() || entry.is_anti_item() {
                    return Ok(true);
                }
                let name = entry.name().to_string();
                let read = read_lines(reader, cap, |ln, off, skip, line| {
                    delivered = true;
                    f(Some(&name), ln, off, skip, line)
                });
                match read {
                    Ok(()) => Ok(true),
                    Err(e) => Err(match e.downcast::<std::io::Error>() {
                        Ok(io) => SevenZError::from(io),
                        Err(other) => SevenZError::Other(other.to_string().into()),
                    }),
                }
            })
        });
        match outcome {
            Ok(()) => return Ok(()),
            // Nothing delivered yet and another password to try: this one was wrong.
            Err(e) if !delivered && n + 1 < tries => last = Some(e),
            Err(e) => {
                warn!(path = %path.display(), error = %e, "7z archive failed (bad password or corrupt data)");
                return Err(anyhow::anyhow!("7z archive {}: {e}", path.display()));
            }
        }
    }
    Err(anyhow::anyhow!(
        "7z archive {}: {}",
        path.display(),
        last.map_or_else(|| "unreadable".to_string(), |e| e.to_string())
    ))
}

/// What opening a file, without searching it, showed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Probe {
    /// It opens, and its start decodes.
    Readable,
    /// An archive with encrypted members that none of the supplied passwords opens.
    NeedsPassword,
    /// A RAR archive and no program to read it (7-Zip) is installed.
    NoTool,
    /// It could not be opened or its start does not decode.
    Unreadable(String),
}

/// Open `path` as `compression` says it is and read the start of it, to tell whether a search
/// would be able to read it. Damage further inside a file is not found this way.
pub fn probe(path: &Path, compression: CompressionKind, passwords: &[String]) -> Probe {
    match probe_inner(path, compression, passwords) {
        Ok(p) => p,
        Err(e) => Probe::Unreadable(format!("{e:#}")),
    }
}

fn read_start(r: impl Read) -> Result<()> {
    let mut start = Vec::new();
    r.take(4096).read_to_end(&mut start)?;
    Ok(())
}

fn first_tar_entry(r: impl Read) -> Result<()> {
    let mut archive = tar::Archive::new(r);
    if let Some(entry) = archive.entries()?.next() {
        entry?;
    }
    Ok(())
}

fn probe_inner(path: &Path, compression: CompressionKind, passwords: &[String]) -> Result<Probe> {
    let file = || std::fs::File::open(path);
    match compression {
        CompressionKind::None => {
            file()?;
        }
        CompressionKind::Gzip => read_start(GzDecoder::new(file()?))?,
        CompressionKind::Zstd => read_start(zstd::stream::read::Decoder::new(file()?)?)?,
        CompressionKind::Bzip2 => read_start(bzip2::read::MultiBzDecoder::new(file()?))?,
        CompressionKind::Xz => read_start(liblzma::read::XzDecoder::new_multi_decoder(file()?))?,
        CompressionKind::Tar => first_tar_entry(file()?)?,
        CompressionKind::TarGz => first_tar_entry(GzDecoder::new(file()?))?,
        CompressionKind::TarBz2 => first_tar_entry(bzip2::read::MultiBzDecoder::new(file()?))?,
        CompressionKind::TarXz => first_tar_entry(liblzma::read::XzDecoder::new_multi_decoder(file()?))?,
        CompressionKind::TarZst => first_tar_entry(zstd::stream::read::Decoder::new(file()?)?)?,
        CompressionKind::Zip => {
            let mut archive = zip::ZipArchive::new(file()?)?;
            for i in 0..archive.len() {
                if !archive.by_index_raw(i)?.encrypted() {
                    continue;
                }
                let opens = passwords.iter().any(|p| archive.by_index_decrypt(i, p.as_bytes()).is_ok());
                return Ok(if opens { Probe::Readable } else { Probe::NeedsPassword });
            }
        }
        CompressionKind::SevenZ => return Ok(probe_7z(path, passwords)),
        CompressionKind::Rar => return Ok(rar::probe_rar(path, passwords)),
    }
    Ok(Probe::Readable)
}

fn probe_7z(path: &Path, passwords: &[String]) -> Probe {
    use sevenz_rust2::{ArchiveReader, Error as SevenZError, Password};

    let candidates = std::iter::once(Password::empty()).chain(passwords.iter().map(|p| Password::new(p)));
    let mut needs_password = false;
    let mut other = None;
    for password in candidates {
        let Ok(file) = std::fs::File::open(path) else { continue };
        let outcome = ArchiveReader::new(file, password).and_then(|mut archive| {
            // Decode the start of the first member that has data.
            archive.for_each_entries(|entry, reader| {
                if entry.is_directory() || entry.size() == 0 {
                    return Ok(true);
                }
                let mut start = Vec::new();
                reader.take(4096).read_to_end(&mut start).map_err(SevenZError::from)?;
                Ok(false)
            })
        });
        match outcome {
            Ok(()) => return Probe::Readable,
            Err(SevenZError::PasswordRequired | SevenZError::MaybeBadPassword(_)) => needs_password = true,
            Err(e) => other = Some(e.to_string()),
        }
    }
    match (needs_password, other) {
        (true, _) => Probe::NeedsPassword,
        (false, Some(e)) => Probe::Unreadable(e),
        (false, None) => Probe::Unreadable("unreadable".into()),
    }
}
