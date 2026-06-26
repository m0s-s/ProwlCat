use std::io::Write;
use std::path::Path;

use anyhow::Result;
use prowlcat_types::{ScanResult, ScanSummary};
use serde_json::json;

pub fn write_json(path: &Path, results: &[ScanResult], summary: &ScanSummary) -> Result<()> {
    let payload = json!({
        "summary": summary,
        "results": results,
    });
    let mut file = std::fs::File::create(path)?;
    serde_json::to_writer_pretty(&mut file, &payload)?;
    writeln!(file)?;
    Ok(())
}

pub fn write_ndjson(path: &Path, results: &[ScanResult]) -> Result<()> {
    let mut file = std::fs::File::create(path)?;
    for result in results {
        serde_json::to_writer(&mut file, result)?;
        writeln!(file)?;
    }
    Ok(())
}
