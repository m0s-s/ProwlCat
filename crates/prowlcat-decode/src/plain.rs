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

/// Smallest line cap accepted: pieces must be much longer than [`PIECE_OVERLAP`].
pub const MIN_MAX_LINE: usize = 4096;

/// Context carried from one piece of a very long line into the next, so a match that straddles
/// a cut is still found whole. Matches longer than this that cross a cut can be missed.
pub const PIECE_OVERLAP: usize = 256;

/// Bytes a long line is preferably cut after: they separate fields and rows (`,` `;` `(` `)`
/// quotes, blanks, `|`), and never occur inside an email, host name or IP address.
fn is_cut_byte(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b',' | b';' | b'(' | b')' | b'\'' | b'"' | b'|')
}

/// Where to end a piece taken from `window`, the first `cap` bytes of what is left of a line:
/// after the last separator in the second half, else after the last ASCII byte there (always
/// a character boundary), else at the end.
fn cut_point(window: &[u8]) -> usize {
    let half = window.len() / 2;
    if let Some(i) = window[half..].iter().rposition(|&b| is_cut_byte(b)) {
        return half + i + 1;
    }
    if let Some(i) = window[half..].iter().rposition(|&b| b < 0x80) {
        return half + i + 1;
    }
    window.len()
}

/// Where the next piece starts, given the cut of the last one: just after the first separator
/// within [`PIECE_OVERLAP`] bytes before the cut, so it starts on a field boundary, else that far
/// back (on a character boundary). The bytes from there to the cut are the overlap.
fn next_start(window: &[u8], cut: usize) -> usize {
    let from = cut.saturating_sub(PIECE_OVERLAP);
    if let Some(i) = window[from..cut].iter().position(|&b| is_cut_byte(b)) {
        return from + i + 1;
    }
    let mut s = from;
    while s < cut && (window[s] & 0xC0) == 0x80 {
        s += 1;
    }
    s
}

/// Hand a line, or the pieces of a line longer than `cap` bytes, to `f(offset, skip, piece)`.
/// `skip` is how many leading bytes of a piece were already part of the previous one.
/// `line_start` is the offset of the line. Used by the memory-mapped path, where the line is
/// already in memory; the streaming path cuts at exactly the same places.
pub(crate) fn split_line(line: &[u8], line_start: u64, cap: usize, f: &mut impl FnMut(u64, usize, &[u8])) {
    let (mut at, mut skip) = (0usize, 0usize);
    loop {
        let rest = &line[at..];
        if cap == 0 || rest.len() <= cap {
            f(line_start + at as u64, skip, rest);
            return;
        }
        let window = &rest[..cap];
        let cut = cut_point(window);
        f(line_start + at as u64, skip, &rest[..cut]);
        let s = next_start(window, cut);
        at += s;
        skip = cut - s;
    }
}

#[derive(Clone, Copy)]
enum Fill {
    Eol,
    Eof,
    Long,
}

/// Append to `buf` up to and including the next `\n`, but stop once the line is certainly
/// longer than `cap`, so `buf` never holds more than `cap + 3` bytes.
fn fill_line<R: BufRead>(reader: &mut R, buf: &mut Vec<u8>, cap: usize) -> std::io::Result<Fill> {
    loop {
        let avail = reader.fill_buf()?;
        if avail.is_empty() {
            return Ok(Fill::Eof);
        }
        let limit = (cap + 3).saturating_sub(buf.len()).max(1);
        let chunk = &avail[..avail.len().min(limit)];
        if let Some(i) = memchr::memchr(b'\n', chunk) {
            buf.extend_from_slice(&chunk[..=i]);
            reader.consume(i + 1);
            return Ok(Fill::Eol);
        }
        let n = chunk.len();
        buf.extend_from_slice(chunk);
        reader.consume(n);
        if buf.len() > cap + 2 {
            return Ok(Fill::Long);
        }
    }
}

/// Stream lines from any reader, calling `f(line_no, byte_offset, skip, line)`.
///
/// With `cap == 0` every line is handed over whole, however long. With a cap, a line longer than
/// `cap` bytes is handed over in pieces of at most `cap` bytes (same `line_no`, the offset of the
/// piece), each starting [`PIECE_OVERLAP`] bytes or fewer before the end of the last, so memory
/// stays bounded. `skip` is the number of leading bytes of a piece that the previous one already
/// held: matches that lie wholly inside them were seen there.
pub(crate) fn read_lines<R: Read>(
    reader: R,
    cap: usize,
    mut f: impl FnMut(u64, u64, usize, &[u8]),
) -> Result<()> {
    let mut reader = BufReader::with_capacity(BUF_CAPACITY, reader);
    let mut buf = Vec::new();
    let (mut line_no, mut offset) = (0u64, 0u64);
    if cap == 0 {
        loop {
            buf.clear();
            let n = reader.read_until(b'\n', &mut buf)?;
            if n == 0 {
                break;
            }
            f(line_no, offset, 0, trim_eol(&buf));
            offset += n as u64;
            line_no += 1;
        }
        return Ok(());
    }

    let mut carry = 0usize; // bytes at the start of `buf` that the last piece already held
    let mut pending: Option<Fill> = None; // the end of this line is already inside `buf`
    loop {
        let fill = match pending.take() {
            Some(fill) => fill,
            None => fill_line(&mut reader, &mut buf, cap)?,
        };
        if matches!(fill, Fill::Eof) && buf.len() <= carry {
            break;
        }
        let content = trim_eol(&buf).len();
        if matches!(fill, Fill::Long) || content > cap {
            let window = &buf[..cap];
            let cut = cut_point(window);
            f(line_no, offset, carry, &buf[..cut]);
            let s = next_start(window, cut);
            buf.drain(..s);
            offset += s as u64;
            carry = cut - s;
            if !matches!(fill, Fill::Long) {
                pending = Some(fill);
            }
            continue;
        }
        if carry == 0 || content > carry {
            f(line_no, offset, carry, &buf[..content]);
        }
        offset += buf.len() as u64;
        line_no += 1;
        buf.clear();
        carry = 0;
        if matches!(fill, Fill::Eof) {
            break;
        }
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
        self.for_each_piece(0, |n, o, _, l| f(n, o, l))
    }

    /// Like [`for_each_line`](Self::for_each_line), with lines longer than `cap` bytes split
    /// into overlapping pieces (`cap == 0`: never). The callback also gets `skip`.
    pub fn for_each_piece(&mut self, cap: usize, mut f: impl FnMut(u64, u64, usize, &[u8])) -> Result<()> {
        match self.source.take() {
            Some(Source::Stream(r)) => read_lines(r, cap, f),
            Some(Source::Mapped(map)) => {
                let data: &[u8] = &map;
                let (mut line_no, mut start) = (0u64, 0usize);
                while start < data.len() {
                    let end = memchr::memchr(b'\n', &data[start..])
                        .map(|i| start + i + 1)
                        .unwrap_or(data.len());
                    split_line(trim_eol(&data[start..end]), start as u64, cap, &mut |o, skip, piece| {
                        f(line_no, o, skip, piece)
                    });
                    start = end;
                    line_no += 1;
                }
                Ok(())
            }
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    const CAP: usize = MIN_MAX_LINE;

    /// A deterministic line of `len` bytes (or a little more) from a small alphabet that has
    /// separators, ASCII and multi-byte characters.
    fn line(seed: &mut u64, len: usize) -> String {
        const PARTS: [&str; 14] = ["a", "bc", "@", ".", " ", ",", "(", ")", "é", "漢", ":", "'x'", "mail.example.com", "|"];
        let mut s = String::new();
        while s.len() < len {
            *seed ^= *seed << 13;
            *seed ^= *seed >> 7;
            *seed ^= *seed << 17;
            s.push_str(PARTS[(*seed % PARTS.len() as u64) as usize]);
        }
        s
    }

    type Piece = (u64, u64, usize, Vec<u8>);

    fn collect_stream(data: &[u8], cap: usize) -> Vec<Piece> {
        let mut out = Vec::new();
        read_lines(Cursor::new(data.to_vec()), cap, |n, o, k, l| out.push((n, o, k, l.to_vec()))).unwrap();
        out
    }

    fn collect_mapped(data: &[u8], cap: usize) -> Vec<Piece> {
        let mut out = Vec::new();
        let (mut line_no, mut start) = (0u64, 0usize);
        while start < data.len() {
            let end = memchr::memchr(b'\n', &data[start..]).map(|i| start + i + 1).unwrap_or(data.len());
            split_line(trim_eol(&data[start..end]), start as u64, cap, &mut |o, k, p| out.push((line_no, o, k, p.to_vec())));
            start = end;
            line_no += 1;
        }
        out
    }

    fn corpus(seed: u64) -> (Vec<String>, Vec<u8>) {
        let mut seed = seed;
        let mut lines = Vec::new();
        let mut data = Vec::new();
        for (i, len) in [0usize, 5, CAP - 1, CAP, CAP + 1, CAP + 2, 3 * CAP, 20_000, 7, 41_000, 100].into_iter().enumerate() {
            let l = line(&mut seed, len);
            data.extend_from_slice(l.as_bytes());
            if i + 1 < 11 {
                data.extend_from_slice(if i % 3 == 0 { b"\r\n" } else { b"\n" });
            }
            lines.push(l);
        }
        (lines, data)
    }

    #[test]
    fn without_a_cap_lines_are_whole() {
        let (lines, data) = corpus(7);
        let got = collect_stream(&data, 0);
        assert_eq!(got.len(), lines.len());
        for (g, l) in got.iter().zip(&lines) {
            assert_eq!((g.2, g.3.as_slice()), (0, l.as_bytes()));
        }
        assert_eq!(collect_mapped(&data, 0), got);
    }

    #[test]
    fn pieces_rebuild_every_line_within_the_cap() {
        for seed in [1u64, 7, 99, 12345] {
            let (lines, data) = corpus(seed);
            let got = collect_stream(&data, CAP);
            let mut line_start = 0u64;
            for (n, original) in lines.iter().enumerate() {
                let pieces: Vec<&Piece> = got.iter().filter(|p| p.0 == n as u64).collect();
                assert!(!pieces.is_empty() || original.is_empty(), "line {n} lost");
                let mut rebuilt = Vec::new();
                let mut end_of_last = None;
                for (i, (_, off, skip, bytes)) in pieces.iter().map(|p| (p.0, p.1, p.2, &p.3)).enumerate() {
                    assert!(bytes.len() <= CAP, "piece of {} bytes", bytes.len());
                    assert!(std::str::from_utf8(bytes).is_ok(), "piece is not UTF-8 (line {n}, piece {i})");
                    assert!(skip <= PIECE_OVERLAP && skip <= bytes.len());
                    if i == 0 {
                        assert_eq!((skip, off), (0, line_start));
                    } else {
                        assert_eq!(off + skip as u64, end_of_last.unwrap(), "pieces are contiguous");
                    }
                    rebuilt.extend_from_slice(&bytes[skip..]);
                    end_of_last = Some(off + bytes.len() as u64);
                }
                assert_eq!(rebuilt, original.as_bytes(), "line {n}");
                let eol = if n + 1 == lines.len() { 0 } else if n % 3 == 0 { 2 } else { 1 };
                line_start += (original.len() + eol) as u64;
            }
        }
    }

    #[test]
    fn streaming_and_memory_mapped_cut_in_the_same_places() {
        for seed in [3u64, 11, 4242] {
            let (_, data) = corpus(seed);
            assert_eq!(collect_stream(&data, CAP), collect_mapped(&data, CAP), "seed {seed}");
        }
    }

    #[test]
    fn a_long_line_without_any_separator_still_makes_progress() {
        let data = vec![b'a'; 50_000];
        let got = collect_stream(&data, CAP);
        assert!(got.len() >= 12 && got.iter().all(|p| p.3.len() <= CAP));
        let total: usize = got.iter().map(|p| p.3.len() - p.2).sum();
        assert_eq!(total, 50_000);
    }
}
