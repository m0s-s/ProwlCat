use std::io::{BufRead, IsTerminal};
use std::process::ExitCode;

use anyhow::{bail, Result};

/// Print each hash with what it can be. The exit code is 1 if any of them could not be identified.
pub fn run(hashes: Vec<String>) -> Result<ExitCode> {
    let hashes = if hashes.is_empty() {
        let stdin = std::io::stdin();
        if stdin.is_terminal() {
            bail!("give the hashes as arguments, or pipe them in one per line");
        }
        stdin.lock().lines().collect::<std::io::Result<Vec<_>>>()?
    } else {
        hashes
    };

    let mut all_known = true;
    for hash in hashes.iter().map(|h| h.trim()).filter(|h| !h.is_empty()) {
        let candidates = prowlcat_core::hashid::identify(hash);
        all_known &= !candidates.is_empty();
        let what = if candidates.is_empty() { "unknown".to_string() } else { candidates.join(", ") };
        println!("{hash}\t{what}");
    }
    Ok(if all_known { ExitCode::SUCCESS } else { ExitCode::from(1) })
}
