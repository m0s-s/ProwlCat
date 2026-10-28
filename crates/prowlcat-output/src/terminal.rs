use std::borrow::Borrow;
use std::io::{BufWriter, Write};

use anyhow::Result;
use prowlcat_types::{MatchKind, QueryKind, ScanResult, ScanSummary};

pub fn print_results(results: &[ScanResult], summary: &ScanSummary) {
    // Slices cannot fail to arrive.
    let _ = print_results_iter(results.iter().map(Ok), summary);
}

/// Print the table on standard output. A reader that goes away early (`prowlcat … | head`) is
/// not an error.
pub fn print_results_iter<I, R>(results: I, summary: &ScanSummary) -> Result<()>
where
    I: IntoIterator<Item = Result<R>>,
    R: Borrow<ScanResult>,
{
    match write_table(std::io::stdout().lock(), results, summary) {
        Err(e) if e.chain().any(|c| c.downcast_ref::<std::io::Error>().is_some_and(|io| io.kind() == std::io::ErrorKind::BrokenPipe)) => Ok(()),
        other => other,
    }
}

/// Write the table to any writer, one result at a time.
pub fn write_table<W, I, R>(out: W, results: I, summary: &ScanSummary) -> Result<()>
where
    W: Write,
    I: IntoIterator<Item = Result<R>>,
    R: Borrow<ScanResult>,
{
    let mut out = BufWriter::new(out);
    let gb = summary.bytes_scanned as f64 / (1024.0 * 1024.0 * 1024.0);
    writeln!(
        out,
        "ProwlCat | {} targets | {} files | {:.2} GB scanned | {} matches | {:.1}s",
        summary.targets,
        summary.files_scanned,
        gb,
        summary.matches,
        summary.elapsed_ms as f64 / 1000.0,
    )?;
    if summary.errors > 0 {
        writeln!(out, "  warnings: {} files failed", summary.errors)?;
    }
    writeln!(out)?;

    for r in results {
        let r = r?;
        let r = r.borrow();
        let kind_label = kind_str(r.kind);
        let match_label = match_kind_suffix(r.match_kind, &r.extracted);
        writeln!(out, "[{}] {}{}", kind_label, r.query, match_label)?;
        writeln!(out, "  └─ {}", format_location(r))?;
        writeln!(out, "     {}", truncate_line(&r.line_text, 200))?;
        writeln!(out)?;
    }
    out.flush()?;
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

/// At most `max` bytes of `s`, cut on a character boundary, with `…` where it was shortened.
fn truncate_line(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

#[cfg(test)]
mod tests {
    use super::*;
    use prowlcat_types::SourceLocation;

    #[test]
    fn long_lines_are_cut_on_a_character_boundary() {
        assert_eq!(truncate_line("short", 200), "short");
        let cyrillic = "я".repeat(150); // 300 bytes: byte 200 is inside a character
        let cut = truncate_line(&cyrillic, 200);
        assert_eq!(cut, format!("{}…", "я".repeat(100)));
        assert_eq!(truncate_line(&"漢".repeat(100), 200), format!("{}…", "漢".repeat(66)));
    }

    #[test]
    fn the_table_can_be_written_to_any_writer() {
        let r = ScanResult {
            query: "a@b.co".into(),
            kind: QueryKind::Email,
            match_kind: MatchKind::Exact,
            source: SourceLocation { path: "f.txt".into(), archive_entry: None, line_number: Some(4), byte_offset: 0 },
            line_text: format!("a@b.co:{}", "я".repeat(300)),
            matched_span: (0, 6),
            extracted: Vec::new(),
        };
        let mut out = Vec::new();
        write_table(&mut out, [Ok::<_, anyhow::Error>(r)], &ScanSummary { targets: 1, matches: 1, ..ScanSummary::default() }).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.starts_with("ProwlCat | 1 targets"), "{text}");
        assert!(text.contains("[email] a@b.co\n  └─ f.txt:5\n     a@b.co:я"), "{text}");
        assert!(text.contains("…\n"));
    }
}
