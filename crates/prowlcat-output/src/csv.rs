use std::borrow::Borrow;
use std::path::Path;

use anyhow::Result;
use prowlcat_types::ScanResult;

pub fn write_csv<I, R>(path: &Path, results: I) -> Result<()>
where
    I: IntoIterator<Item = Result<R>>,
    R: Borrow<ScanResult>,
{
    write_csv_to(std::fs::File::create(path)?, results)
}

pub fn write_csv_to<W, I, R>(out: W, results: I) -> Result<()>
where
    W: std::io::Write,
    I: IntoIterator<Item = Result<R>>,
    R: Borrow<ScanResult>,
{
    let mut wtr = csv::Writer::from_writer(out);
    wtr.write_record([
        "query",
        "kind",
        "match_kind",
        "path",
        "line",
        "byte_offset",
        "context",
    ])?;

    for r in results {
        let r = r?;
        let r = r.borrow();
        wtr.write_record([
            &r.query,
            &format!("{:?}", r.kind),
            &format!("{:?}", r.match_kind),
            &r.source.path.display().to_string(),
            &r.source.line_number.map(|n| n.to_string()).unwrap_or_default(),
            &r.source.byte_offset.to_string(),
            &r.line_text,
        ])?;
    }

    wtr.flush()?;
    Ok(())
}
