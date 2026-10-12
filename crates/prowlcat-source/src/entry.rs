use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompressionKind {
    None,
    Gzip,
    Zstd,
    TarGz,
    Tar,
    Zip,
    // Xz, Bzip2 — later
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

    if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        return CompressionKind::TarGz;
    }
    if name.ends_with(".tar") {
        return CompressionKind::Tar;
    }
    if name.ends_with(".zip") {
        return CompressionKind::Zip;
    }
    if name.ends_with(".zst") || name.ends_with(".zstd") {
        return CompressionKind::Zstd;
    }
    if name.ends_with(".gz") || name.ends_with(".gzip") {
        return CompressionKind::Gzip;
    }

    // Magic bytes
    if let Ok(mut f) = std::fs::File::open(path) {
        use std::io::Read;
        let mut buf = [0u8; 4];
        let n = f.read(&mut buf).unwrap_or(0);
        if n >= 2 && buf[..2] == [0x1f, 0x8b] {
            return CompressionKind::Gzip;
        }
        if n == 4 && buf == [0x28, 0xb5, 0x2f, 0xfd] {
            return CompressionKind::Zstd;
        }
        if n == 4 && buf == [0x50, 0x4b, 0x03, 0x04] {
            return CompressionKind::Zip;
        }
    }

    CompressionKind::None
}
