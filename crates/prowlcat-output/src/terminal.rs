use std::borrow::Borrow;

use anyhow::Result;
use prowlcat_types::{MatchKind, QueryKind, ScanResult, ScanSummary};

pub fn print_results(results: &[ScanResult], summary: &ScanSummary) {
    // Slices cannot fail to arrive.
    let _ = print_results_iter(results.iter().map(Ok), summary);
}

pub fn print_results_iter<I, R>(results: I, summary: &ScanSummary) -> Result<()>
where
    I: IntoIterator<Item = Result<R>>,
    R: Borrow<ScanResult>,
{
    let gb = summary.bytes_scanned as f64 / (1024.0 * 1024.0 * 1024.0);
    println!(
        "ProwlCat | {} targets | {} files | {:.2} GB scanned | {} matches | {:.1}s",
        summary.targets,
        summary.files_scanned,
        gb,
        summary.matches,
        summary.elapsed_ms as f64 / 1000.0,
    );
    if summary.errors > 0 {
        println!("  warnings: {} files failed", summary.errors);
    }
    println!();

    for r in results {
        let r = r?;
        let r = r.borrow();
        let kind_label = kind_str(r.kind);
        let match_label = match_kind_suffix(r.match_kind, &r.extracted);
        println!("[{}] {}{}", kind_label, r.query, match_label);
        let loc = format_location(r);
        println!("  └─ {}", loc);
        println!("     {}", truncate_line(&r.line_text, 200));
        println!();
    }
    Ok(())
}

fn kind_str(k: QueryKind) -> &'static str {
    match k {
        QueryKind::Email => "email",
        QueryKind::Domain => "domain",
        QueryKind::Ip => "ip",
        QueryKind::Cidr => "cidr",
        QueryKind::Text => "text",
    }
}

fn match_kind_suffix(kind: MatchKind, extracted: &[prowlcat_types::ExtractedValue]) -> String {
    match kind {
        MatchKind::Exact => String::new(),
        MatchKind::CidrLiteral => " [CidrLiteral]".to_string(),
        MatchKind::IpInNetwork => extracted
            .first()
            .map(|e| format!(" [IpInNetwork: {e:?}]"))
            .unwrap_or_else(|| " [IpInNetwork]".to_string()),
    }
}

fn format_location(r: &ScanResult) -> String {
    let mut s = r.source.path.display().to_string();
    if let Some(entry) = &r.source.archive_entry {
        s.push_str(" → ");
        s.push_str(entry);
    }
    if let Some(line) = r.source.line_number {
        s.push(':');
        s.push_str(&(line + 1).to_string());
    }
    s
}

fn truncate_line(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max])
    }
}
