use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

use anyhow::Result;

pub struct PlainReader {
    reader: Box<dyn BufRead>,
    path: PathBuf,
    line_no: u64,
    byte_offset: u64,
}

impl PlainReader {
    pub fn open(path: &Path, _mmap_threshold: u64) -> Result<Self> {
        let file = std::fs::File::open(path)?;
        // TODO(M3): mmap for files >= mmap_threshold
        Self::from_reader(file, path.to_path_buf(), 0)
    }

    pub fn from_reader<R: Read + 'static>(reader: R, path: PathBuf, start_offset: u64) -> Result<Self> {
        Ok(Self {
            reader: Box::new(BufReader::with_capacity(8 * 1024 * 1024, reader)),
            path,
            line_no: 0,
            byte_offset: start_offset,
        })
    }

    pub fn for_each_line(&mut self, mut f: impl FnMut(u64, u64, &[u8])) -> Result<()> {
        let mut buf = Vec::new();
        loop {
            buf.clear();
            let n = self.reader.read_until(b'\n', &mut buf)?;
            if n == 0 {
                break;
            }
            let end = buf
                .iter()
                .rposition(|&b| b != b'\n' && b != b'\r')
                .map(|i| i + 1)
                .unwrap_or(0);
            let line = &buf[..end];
            f(self.line_no, self.byte_offset, line);
            self.byte_offset += n as u64;
            self.line_no += 1;
        }
        Ok(())
    }
}
