//! Terminal, JSON, CSV, and NDJSON output formatters.
//!
//! Every writer takes its results as an iterator and writes them one by one, so the memory used
//! does not depend on how many results there are. The slice-based functions are conveniences
//! over the same code.

mod csv;
mod json;
mod terminal;

use std::borrow::Borrow;
use std::path::Path;

use anyhow::Result;
use prowlcat_types::{ScanResult, ScanSummary};

pub use terminal::{print_results, print_results_iter};

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
    write_output_iter(path, format, results.iter().map(Ok), summary)
}

/// Like [`write_output`], for results that arrive one at a time (and may fail to arrive).
pub fn write_output_iter<I, R>(path: &Path, format: OutputFormat, results: I, summary: &ScanSummary) -> Result<()>
where
    I: IntoIterator<Item = Result<R>>,
    R: Borrow<ScanResult>,
{
    match format {
        OutputFormat::Json => json::write_json(path, results, summary),
        OutputFormat::Ndjson => json::write_ndjson(path, results),
        OutputFormat::Csv => csv::write_csv(path, results),
        OutputFormat::Table => print_results_iter(results, summary),
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
