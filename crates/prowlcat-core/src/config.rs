use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;

/// Shared counters a UI can poll while a scan runs. Bytes are raw on-disk bytes.
#[derive(Debug, Clone, Default)]
pub struct ProgressHandle {
    /// Raw bytes consumed so far.
    pub done: Arc<AtomicU64>,
    /// Total bytes to scan; 0 until enumeration finishes.
    pub total: Arc<AtomicU64>,
    /// Files fully scanned.
    pub files_done: Arc<AtomicU64>,
    /// Matches found so far.
    pub matches: Arc<AtomicU64>,
}

/// Runtime configuration for a scan job.
#[derive(Debug, Clone)]
pub struct ScanConfig {
    pub paths: Vec<PathBuf>,
    pub recursive: bool,
    pub threads: usize,
    pub mmap_threshold_mb: u64,
    pub max_matches_per_query: u64,
    pub context_bytes: usize,
    pub include_subdomains: bool,
    pub hide_secrets: bool,
    /// Candidate passwords for encrypted zip archives.
    pub passwords: Vec<String>,
    /// Optional live progress counters.
    pub progress: Option<ProgressHandle>,
}

impl Default for ScanConfig {
    fn default() -> Self {
        Self {
            paths: Vec::new(),
            recursive: true,
            threads: std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4),
            mmap_threshold_mb: 64,
            max_matches_per_query: 0,
            context_bytes: 0,
            include_subdomains: true,
            hide_secrets: false,
            passwords: Vec::new(),
            progress: None,
        }
    }
}
