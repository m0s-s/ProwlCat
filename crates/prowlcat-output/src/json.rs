use std::borrow::Borrow;
use std::io::{BufWriter, Write};
use std::path::Path;

use anyhow::Result;
use prowlcat_types::{ScanResult, ScanSummary};

/// Write `{"results":[...],"summary":{...}}` as compact JSON, one result at a time so memory does
/// not depend on how many there are.
pub fn write_json<I, R>(path: &Path, results: I, summary: &ScanSummary) -> Result<()>
where
    I: IntoIterator<Item = Result<R>>,
    R: Borrow<ScanResult>,
{
    write_json_to(std::fs::File::create(path)?, results, summary)
}

pub fn write_json_to<W, I, R>(out: W, results: I, summary: &ScanSummary) -> Result<()>
where
    W: Write,
    I: IntoIterator<Item = Result<R>>,
    R: Borrow<ScanResult>,
{
    let mut out = BufWriter::new(out);
    out.write_all(b"{\"results\":[")?;
    for (n, result) in results.into_iter().enumerate() {
        if n > 0 {
            out.write_all(b",")?;
        }
        serde_json::to_writer(&mut out, result?.borrow())?;
    }
    out.write_all(b"],\"summary\":")?;
    serde_json::to_writer(&mut out, summary)?;
    out.write_all(b"}\n")?;
    out.flush()?;
    Ok(())
}
pub fn write_ndjson<I, R>(path: &Path, results: I) -> Result<()>
where
    I: IntoIterator<Item = Result<R>>,
    R: Borrow<ScanResult>,
{
    write_ndjson_to(std::fs::File::create(path)?, results)
}

pub fn write_ndjson_to<W, I, R>(out: W, results: I) -> Result<()>
where
    W: Write,
    I: IntoIterator<Item = Result<R>>,
    R: Borrow<ScanResult>,
{
    let mut out = BufWriter::new(out);
    for result in results {
        serde_json::to_writer(&mut out, result?.borrow())?;
        out.write_all(b"\n")?;
    }
    out.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use prowlcat_types::{ExtractedValue, MatchKind, QueryKind, SourceLocation};

    fn result(n: u64) -> ScanResult {
        ScanResult {
            query: format!("q{n}"),
            kind: QueryKind::Email,
            match_kind: MatchKind::Exact,
            source: SourceLocation {
                path: format!("/data/f{n}.txt").into(),
                archive_entry: n.is_multiple_of(2).then(|| "inner.txt".to_string()),
                line_number: Some(n),
                byte_offset: n * 3,
            },
            line_text: format!("a@b.com:pw\"{n}\" \\ \n tab\t é"),
            matched_span: (0, 7),
            extracted: vec![ExtractedValue::Email("a@b.com".into())],
        }
    }

    /// The same document built in one piece.
    fn reference(results: &[ScanResult], summary: &ScanSummary) -> String {
        #[derive(serde::Serialize)]
        struct Doc<'a> {
            results: &'a [ScanResult],
            summary: &'a ScanSummary,
        }
        let mut text = serde_json::to_string(&Doc { results, summary }).unwrap();
        text.push('\n');
        text
    }

    #[test]
    fn streamed_json_is_the_compact_form_of_the_whole_document() {
        let dir = tempfile::tempdir().unwrap();
        let summary = ScanSummary { targets: 2, files_scanned: 5, bytes_scanned: 99, matches: 3, elapsed_ms: 12, errors: 1 };
        for n in [0u64, 1, 2, 5] {
            let results: Vec<_> = (0..n).map(result).collect();
            let path = dir.path().join(format!("out{n}.json"));
            write_json(&path, results.iter().map(Ok), &summary).unwrap();
            assert_eq!(std::fs::read_to_string(&path).unwrap(), reference(&results, &summary), "{n} results");
        }
    }

    #[test]
    fn ndjson_has_one_result_per_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.ndjson");
        let results: Vec<_> = (0..3).map(result).collect();
        write_ndjson(&path, results.iter().map(Ok)).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.lines().count(), 3);
        assert!(text.lines().all(|l| serde_json::from_str::<ScanResult>(l).is_ok()));
    }
}
