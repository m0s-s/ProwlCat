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
    Bzip2,
    Xz,
    TarBz2,
    TarXz,
    TarZst,
    SevenZ,
    /// Read through an external program (see `prowlcat-decode`).
    Rar,
}

impl CompressionKind {
    /// A container of many files (tar, zip, 7z) rather than one compressed stream.
    pub fn is_archive(self) -> bool {
        matches!(
            self,
            Self::Tar | Self::TarGz | Self::TarBz2 | Self::TarXz | Self::TarZst | Self::Zip | Self::SevenZ | Self::Rar
        )
    }
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

    let ends = |suffixes: &[&str]| suffixes.iter().any(|s| name.ends_with(s));
    if ends(&[".tar.gz", ".tgz"]) {
        return CompressionKind::TarGz;
    }
    if ends(&[".tar.bz2", ".tbz2", ".tbz"]) {
        return CompressionKind::TarBz2;
    }
    if ends(&[".tar.xz", ".txz"]) {
        return CompressionKind::TarXz;
    }
    if ends(&[".tar.zst", ".tar.zstd", ".tzst"]) {
        return CompressionKind::TarZst;
    }
    if ends(&[".tar"]) {
        return CompressionKind::Tar;
    }
    if ends(&[".zip"]) {
        return CompressionKind::Zip;
    }
    if ends(&[".7z"]) {
        return CompressionKind::SevenZ;
    }
    if ends(&[".rar"]) {
        return CompressionKind::Rar;
    }
    if ends(&[".zst", ".zstd"]) {
        return CompressionKind::Zstd;
    }
    if ends(&[".gz", ".gzip"]) {
        return CompressionKind::Gzip;
    }
    if ends(&[".bz2", ".bzip2"]) {
        return CompressionKind::Bzip2;
    }
    if ends(&[".xz"]) {
        return CompressionKind::Xz;
    }

    // Magic bytes
    if let Ok(mut f) = std::fs::File::open(path) {
        use std::io::Read;
        let mut buf = [0u8; 6];
        let n = f.read(&mut buf).unwrap_or(0);
        let head = &buf[..n];
        if head.starts_with(&[0x1f, 0x8b]) {
            return CompressionKind::Gzip;
        }
        if head.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]) {
            return CompressionKind::Zstd;
        }
        if head.starts_with(&[0x50, 0x4b, 0x03, 0x04]) {
            return CompressionKind::Zip;
        }
        if head.starts_with(&[0x37, 0x7a, 0xbc, 0xaf, 0x27, 0x1c]) {
            return CompressionKind::SevenZ;
        }
        if head.starts_with(b"Rar!\x1a\x07") {
            return CompressionKind::Rar;
        }
        if head.starts_with(&[0xfd, 0x37, 0x7a, 0x58, 0x5a, 0x00]) {
            return CompressionKind::Xz;
        }
        // "BZh" and a block size digit: plain text rarely starts that way.
        if head.len() >= 4 && head.starts_with(b"BZh") && (b'1'..=b'9').contains(&head[3]) {
            return CompressionKind::Bzip2;
        }
    }

    CompressionKind::None
}
