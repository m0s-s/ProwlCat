use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::mpsc::SyncSender;
use std::sync::Arc;

use prowlcat_types::ScanResult;

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
    /// Set to `true` to ask the scan to stop. Files not yet started are skipped and running
    /// files stop matching within a few hundred lines; the scan then returns what it has.
    pub cancel: Arc<AtomicBool>,
    /// When set, matches are also sent here as they are found, so a UI can show hits live. The
    /// channel is bounded: if the receiver falls behind, the excess is not sent (use `matches`
    /// for the count). The final result list is unaffected.
    pub live: Option<SyncSender<ScanResult>>,
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
    /// RAM, in MiB, that [`ScanOrchestrator::run_sorted`](crate::ScanOrchestrator::run_sorted)
    /// may use to hold matches before moving them to a temporary file. The results are the
    /// same whatever the value; `0` spills as early as possible. `PROWLCAT_RESULT_MEM_MB`
    /// sets the default.
    pub result_memory_mb: u64,
}

const DEFAULT_RESULT_MEMORY_MB: u64 = 256;

fn default_result_memory_mb() -> u64 {
    std::env::var("PROWLCAT_RESULT_MEM_MB")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(DEFAULT_RESULT_MEMORY_MB)
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
            result_memory_mb: default_result_memory_mb(),
        }
    }
}
