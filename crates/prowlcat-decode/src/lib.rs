//! Streaming line readers for plaintext and compressed sources.

mod plain;

use std::path::Path;

use anyhow::Result;
use flate2::read::GzDecoder;
use prowlcat_source::CompressionKind;

pub use plain::PlainReader;

pub enum LineReader {
    Plain(PlainReader),
    Gzip(GzipLineReader),
}

impl LineReader {
    pub fn open(path: &Path, compression: CompressionKind, mmap_threshold: u64) -> Result<Self> {
        match compression {
            CompressionKind::None => Ok(Self::Plain(PlainReader::open(path, mmap_threshold)?)),
            CompressionKind::Gzip => Ok(Self::Gzip(GzipLineReader::open(path)?)),
        }
    }

    pub fn for_each_line(&mut self, f: impl FnMut(u64, u64, &[u8])) -> Result<()> {
        match self {
            Self::Plain(reader) => reader.for_each_line(f),
            Self::Gzip(reader) => reader.for_each_line(f),
        }
    }
}

struct GzipLineReader {
    inner: PlainReader,
}

impl GzipLineReader {
    fn open(path: &Path) -> Result<Self> {
        let file = std::fs::File::open(path)?;
        let decoder = GzDecoder::new(file);
        Ok(Self {
            inner: PlainReader::from_reader(decoder, path.to_path_buf(), 0)?,
        })
    }

    fn for_each_line(&mut self, f: impl FnMut(u64, u64, &[u8])) -> Result<()> {
        self.inner.for_each_line(f)
    }
}
