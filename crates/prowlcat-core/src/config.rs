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
    /// Longest stretch of one line held in memory, in bytes. A longer line (a SQL `INSERT` with
    /// thousands of rows, a minified dump on one line) is searched in overlapping pieces of at
    /// most this size, each cut at a separator, and a match is reported with the piece it lies
    /// in. `0` keeps every line whole, however long (the default). Values under 4 KiB are raised
    /// to it. `PROWLCAT_MAX_LINE_BYTES` sets the default.
    pub max_line_bytes: usize,
    /// Report, instead of the whole line, this many bytes of context on each side of the match
    /// (with `…` where the line goes on), so one match in a huge line does not drag the line along.
    /// Lines that fit in the window are unchanged. `0` reports whole lines (the default).
    /// `PROWLCAT_SNIPPET` sets the default.
    pub snippet_bytes: usize,
}

/// Parse a size such as `4096`, `64K`, `8M` or `1G` (powers of 1024; the `B` and `iB` forms work too).
pub fn parse_size(s: &str) -> Option<usize> {
    let s = s.trim();
    let lower = s.to_ascii_lowercase();
    let t = lower.strip_suffix("ib").or_else(|| lower.strip_suffix('b')).unwrap_or(&lower);
    let (digits, mult) = match t.chars().last()? {
        'k' => (&t[..t.len() - 1], 1usize << 10),
        'm' => (&t[..t.len() - 1], 1 << 20),
        'g' => (&t[..t.len() - 1], 1 << 30),
        _ => (t, 1),
    };
    digits.trim().parse::<usize>().ok()?.checked_mul(mult)
}

fn env_size(name: &str) -> usize {
    std::env::var(name).ok().and_then(|v| parse_size(&v)).unwrap_or(0)
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
            max_line_bytes: env_size("PROWLCAT_MAX_LINE_BYTES"),
            snippet_bytes: env_size("PROWLCAT_SNIPPET"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::parse_size;

    #[test]
    fn sizes_parse_with_suffixes() {
        assert_eq!(parse_size("4096"), Some(4096));
        assert_eq!(parse_size("64K"), Some(64 * 1024));
        assert_eq!(parse_size("64k"), Some(64 * 1024));
        assert_eq!(parse_size("8MiB"), Some(8 << 20));
        assert_eq!(parse_size(" 1 G "), Some(1 << 30));
        assert_eq!(parse_size("0"), Some(0));
        for bad in ["", "K", "x", "-5", "1.5M", "12T"] {
            assert_eq!(parse_size(bad), None, "{bad:?}");
        }
    }
}
