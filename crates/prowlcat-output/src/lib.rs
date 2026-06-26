//! Terminal, JSON, CSV, and NDJSON output formatters.

mod csv;
mod json;
mod terminal;

use std::path::Path;

use anyhow::Result;
use prowlcat_types::{ScanResult, ScanSummary};

pub use terminal::print_results;

pub enum OutputFormat {
    Table,
    Json,
    Ndjson,
    Csv,
}

pub fn write_output(
    path: &Path,
    format: OutputFormat,
    results: &[ScanResult],
    summary: &ScanSummary,
) -> Result<()> {
    match format {
        OutputFormat::Json => json::write_json(path, results, summary),
        OutputFormat::Ndjson => json::write_ndjson(path, results),
        OutputFormat::Csv => csv::write_csv(path, results),
        OutputFormat::Table => {
            print_results(results, summary);
            Ok(())
        }
    }
}

pub fn infer_format(path: &Path) -> OutputFormat {
    match path.extension().and_then(|e| e.to_str()) {
        Some("json") => OutputFormat::Json,
        Some("ndjson") | Some("jsonl") => OutputFormat::Ndjson,
        Some("csv") => OutputFormat::Csv,
        _ => OutputFormat::Json,
    }
}
