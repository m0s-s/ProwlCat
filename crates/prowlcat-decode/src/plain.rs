use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::Result;
use memmap2::Mmap;

const BUF_CAPACITY: usize = 8 * 1024 * 1024;

/// Trim trailing `\n` / `\r` bytes.
fn trim_eol(buf: &[u8]) -> &[u8] {
    let end = buf
        .iter()
        .rposition(|&b| b != b'\n' && b != b'\r')
        .map(|i| i + 1)
        .unwrap_or(0);
    &buf[..end]
}

/// Stream lines from any reader, calling `f(line_no, byte_offset, line)`.
pub(crate) fn read_lines<R: Read>(reader: R, mut f: impl FnMut(u64, u64, &[u8])) -> Result<()> {
    let mut reader = BufReader::with_capacity(BUF_CAPACITY, reader);
    let mut buf = Vec::new();
    let (mut line_no, mut offset) = (0u64, 0u64);
    loop {
        buf.clear();
        let n = reader.read_until(b'\n', &mut buf)?;
        if n == 0 {
            break;
        }
        f(line_no, offset, trim_eol(&buf));
        offset += n as u64;
        line_no += 1;
    }
    Ok(())
}

/// Reader wrapper that counts raw (on-disk) bytes consumed, for progress reporting.
pub(crate) struct Counted<R> {
    inner: R,
    global: Option<Arc<AtomicU64>>,
    local: Arc<AtomicU64>,
    /// File size; reads beyond it (zip re-reads, seeks) are not reported, so progress never overshoots.
    limit: u64,
}

impl<R> Counted<R> {
    pub(crate) fn new(inner: R, global: Option<Arc<AtomicU64>>, local: Arc<AtomicU64>, limit: u64) -> Self {
        Self { inner, global, local, limit }
    }
}

impl<R: Read> Read for Counted<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        let prev = self.local.fetch_add(n as u64, Ordering::Relaxed);
        if let Some(g) = &self.global {
            g.fetch_add(self.limit.saturating_sub(prev).min(n as u64), Ordering::Relaxed);
        }
        Ok(n)
    }
}

impl<R: Seek> Seek for Counted<R> {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(pos)
    }
}

enum Source {
    Stream(Box<dyn Read>),
    Mapped(Mmap),
}

pub struct PlainReader {
    source: Option<Source>,
    #[allow(dead_code)]
    path: PathBuf,
}

impl PlainReader {
    /// Open a plaintext file; files at or above `mmap_threshold` bytes are memory-mapped
    /// (a threshold of 0 disables mmap).
    pub fn open(path: &Path, mmap_threshold: u64) -> Result<Self> {
        Self::open_counting(path, mmap_threshold, None, Arc::new(AtomicU64::new(0)))
    }

    /// Like [`open`](Self::open), additionally counting raw bytes read into `global`/`local`.
    /// Memory-mapped files are not counted here; the caller tops up from the file size.
    pub(crate) fn open_counting(
        path: &Path,
        mmap_threshold: u64,
        global: Option<Arc<AtomicU64>>,
        local: Arc<AtomicU64>,
    ) -> Result<Self> {
        let file = std::fs::File::open(path)?;
        let len = file.metadata()?.len();
        if mmap_threshold > 0 && len >= mmap_threshold && len > 0 {
            // SAFETY: read-only map; concurrent truncation by another process is the usual
            // mmap caveat and is accepted for a read-only corpus scanner.
            if let Ok(map) = unsafe { Mmap::map(&file) } {
                return Ok(Self { source: Some(Source::Mapped(map)), path: path.to_path_buf() });
            }
        }
        Ok(Self {
            source: Some(Source::Stream(Box::new(Counted::new(file, global, local, len)))),
            path: path.to_path_buf(),
        })
    }

    pub fn from_reader<R: Read + 'static>(reader: R, path: PathBuf, _start_offset: u64) -> Result<Self> {
        Ok(Self { source: Some(Source::Stream(Box::new(reader))), path })
    }

    pub fn for_each_line(&mut self, mut f: impl FnMut(u64, u64, &[u8])) -> Result<()> {
        match self.source.take() {
            Some(Source::Stream(r)) => read_lines(r, f),
            Some(Source::Mapped(map)) => {
                let data: &[u8] = &map;
                let (mut line_no, mut start) = (0u64, 0usize);
                while start < data.len() {
                    let end = memchr::memchr(b'\n', &data[start..])
                        .map(|i| start + i + 1)
                        .unwrap_or(data.len());
                    f(line_no, start as u64, trim_eol(&data[start..end]));
                    start = end;
                    line_no += 1;
                }
                Ok(())
            }
            None => Ok(()),
        }
    }
}
