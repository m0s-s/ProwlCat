use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompressionKind {
    None,
    Gzip,
    // Zstd, Xz, Bzip2 — M2+
}

#[derive(Debug, Clone)]
pub struct SourceEntry {
    pub path: PathBuf,
    pub compression: CompressionKind,
    pub size_bytes: u64,
    /// Set when the entry is inside a tar/zip archive (future)
    pub archive_entry: Option<String>,
}

impl SourceEntry {
    pub fn from_path(path: PathBuf) -> std::io::Result<Self> {
        let meta = std::fs::metadata(&path)?;
        let compression = detect_compression(&path);
        Ok(Self {
            path,
            compression,
            size_bytes: meta.len(),
            archive_entry: None,
        })
    }
}

pub fn detect_compression(path: &Path) -> CompressionKind {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    if name.ends_with(".gz") || name.ends_with(".gzip") {
        return CompressionKind::Gzip;
    }

    // Magic bytes for gzip
    if let Ok(mut f) = std::fs::File::open(path) {
        use std::io::Read;
        let mut buf = [0u8; 2];
        if f.read_exact(&mut buf).is_ok() && buf == [0x1f, 0x8b] {
            return CompressionKind::Gzip;
        }
    }

    CompressionKind::None
}
