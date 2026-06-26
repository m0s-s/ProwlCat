use std::path::Path;

use anyhow::Result;
use prowlcat_types::ScanResult;

pub fn write_csv(path: &Path, results: &[ScanResult]) -> Result<()> {
    let mut wtr = csv::Writer::from_path(path)?;
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
