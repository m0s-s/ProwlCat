//! Writers for extracted records (`--extract`): `account:secret` lines, or JSON, NDJSON and CSV.

use std::io::{BufWriter, Write};

use anyhow::Result;
use prowlcat_types::Record;

use crate::OutputFormat;

/// What stands in for the secret when it is hidden.
const HIDDEN: &str = "***";

fn shown<'a>(r: &'a Record, mask: bool) -> std::borrow::Cow<'a, Record> {
    if mask {
        std::borrow::Cow::Owned(Record { secret: HIDDEN.to_string(), ..r.clone() })
    } else {
        std::borrow::Cow::Borrowed(r)
    }
}

/// Write the records one at a time and return how many there were. `Table` is the plain
/// `account:secret` form. With `mask` the secret is written as `***`.
pub fn write_records_to<W, I>(out: W, format: OutputFormat, mask: bool, records: I) -> Result<u64>
where
    W: Write,
    I: IntoIterator<Item = Result<Record>>,
{
    let mut out = BufWriter::new(out);
    let mut n = 0u64;
    match format {
        OutputFormat::Table => {
            for r in records {
                let r = r?;
                let r = shown(&r, mask);
                writeln!(out, "{}:{}", r.account, r.secret)?;
                n += 1;
            }
        }
        OutputFormat::Ndjson => {
            for r in records {
                serde_json::to_writer(&mut out, &*shown(&r?, mask))?;
                out.write_all(b"\n")?;
                n += 1;
            }
        }
        OutputFormat::Json => {
            out.write_all(b"{\"records\":[")?;
            for r in records {
                if n > 0 {
                    out.write_all(b",")?;
                }
                serde_json::to_writer(&mut out, &*shown(&r?, mask))?;
                n += 1;
            }
            out.write_all(b"]}\n")?;
        }
        OutputFormat::Csv => {
            let mut w = csv::Writer::from_writer(&mut out);
            w.write_record(["account", "secret", "secret_looks_like", "query", "path", "archive_entry", "line"])?;
            for r in records {
                let r = r?;
                let r = shown(&r, mask);
                w.write_record([
                    r.account.as_str(),
                    r.secret.as_str(),
                    &r.secret_looks_like.join("|"),
                    r.query.as_str(),
                    r.path.as_str(),
                    r.archive_entry.as_deref().unwrap_or(""),
                    &r.line_number.map(|n| n.to_string()).unwrap_or_default(),
                ])?;
                n += 1;
            }
            w.flush()?;
        }
    }
    out.flush()?;
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(n: u64) -> Record {
        Record {
            account: format!("u{n}@x.io"),
            secret: format!("pw,{n}"),
            secret_looks_like: if n == 1 { vec!["MD5".into(), "NTLM".into()] } else { Vec::new() },
            query: "x.io".into(),
            path: "dir/f.txt".into(),
            archive_entry: (n == 2).then(|| "in/side.txt".to_string()),
            line_number: Some(n),
        }
    }

    fn render(format: OutputFormat, mask: bool, n: u64) -> String {
        let mut out = Vec::new();
        let count = write_records_to(&mut out, format, mask, (0..n).map(|i| Ok(rec(i)))).unwrap();
        assert_eq!(count, n);
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn plain_and_masked_lines() {
        assert_eq!(render(OutputFormat::Table, false, 2), "u0@x.io:pw,0\nu1@x.io:pw,1\n");
        assert_eq!(render(OutputFormat::Table, true, 2), "u0@x.io:***\nu1@x.io:***\n");
    }

    #[test]
    fn ndjson_json_and_csv() {
        let nd = render(OutputFormat::Ndjson, false, 3);
        let first: serde_json::Value = serde_json::from_str(nd.lines().nth(1).unwrap()).unwrap();
        assert_eq!(first["account"], "u1@x.io");
        assert_eq!(first["secret_looks_like"], serde_json::json!(["MD5", "NTLM"]));
        assert_eq!(nd.lines().count(), 3);

        let j: serde_json::Value = serde_json::from_str(&render(OutputFormat::Json, true, 3)).unwrap();
        assert_eq!(j["records"].as_array().unwrap().len(), 3);
        assert_eq!(j["records"][0]["secret"], "***");
        assert_eq!(render(OutputFormat::Json, false, 0), "{\"records\":[]}\n");

        let csv = render(OutputFormat::Csv, false, 3);
        assert!(csv.starts_with("account,secret,secret_looks_like,query,path,archive_entry,line\n"));
        assert!(csv.contains("u1@x.io,\"pw,1\",MD5|NTLM,x.io,dir/f.txt,,1"), "{csv}");
        assert!(csv.contains("in/side.txt"));
    }
}
