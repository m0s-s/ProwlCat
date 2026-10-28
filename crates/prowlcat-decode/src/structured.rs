//! Sources that are not lines of text: SQLite databases, MongoDB dumps, JSON arrays.
//!
//! Every record becomes one line, handed to the search like any other, so matching, results and
//! output work as they do for a text file; only the reading differs. A SQLite row is its values
//! separated by tabs (blobs as hex), with the table as the archive entry; a BSON document or a
//! JSON array element is its JSON text.

use std::io::{BufRead, BufReader, Read};
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::de::{SeqAccess, Visitor};
use serde::Deserializer;
use tracing::warn;

use crate::plain::split_line;

/// Numbers the lines of one source (a file, a table) and hands each to the callback, in pieces
/// when it is longer than the cap.
struct Emitter<'a, F> {
    f: &'a mut F,
    cap: usize,
    member: Option<String>,
    line_no: u64,
    offset: u64,
}

impl<'a, F: FnMut(Option<&str>, u64, u64, usize, &[u8])> Emitter<'a, F> {
    fn new(f: &'a mut F, cap: usize, member: Option<String>) -> Self {
        Self { f, cap, member, line_no: 0, offset: 0 }
    }

    fn emit(&mut self, text: &str) {
        let (member, line_no, f) = (self.member.as_deref(), self.line_no, &mut *self.f);
        split_line(text.as_bytes(), self.offset, self.cap, &mut |o, skip, piece| f(member, line_no, o, skip, piece));
        self.offset += text.len() as u64 + 1;
        self.line_no += 1;
    }
}

/// Values that would break the one-row-one-line rule are replaced by a blank.
fn one_line(s: &str, out: &mut String) {
    out.extend(s.chars().map(|c| if matches!(c, '\t' | '\n' | '\r') { ' ' } else { c }));
}

const BLOB_HEX_BYTES: usize = 256;

fn push_hex(bytes: &[u8], out: &mut String) {
    for b in bytes.iter().take(BLOB_HEX_BYTES) {
        out.push_str(&format!("{b:02x}"));
    }
    if bytes.len() > BLOB_HEX_BYTES {
        out.push('…');
    }
}

/// Every row of every table, a table at a time. A table that cannot be read is skipped with a
/// warning; a file that is not a SQLite database (or is encrypted) is an error.
pub(crate) fn scan_sqlite(
    path: &Path,
    cap: usize,
    f: &mut impl FnMut(Option<&str>, u64, u64, usize, &[u8]),
) -> Result<()> {
    use rusqlite::types::ValueRef;
    use rusqlite::{Connection, OpenFlags};

    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)
        .context("open the SQLite database")?;
    let tables: Vec<String> = {
        let mut st = conn
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
            .context("read the tables (not a SQLite database, or encrypted)")?;
        let rows = st.query_map([], |r| r.get::<_, String>(0))?;
        rows.collect::<std::result::Result<_, _>>()?
    };

    for table in tables {
        let mut emitter = Emitter::new(f, cap, Some(table.clone()));
        let read = (|| -> rusqlite::Result<()> {
            let mut st = conn.prepare(&format!("SELECT * FROM \"{}\"", table.replace('"', "\"\"")))?;
            let columns = st.column_count();
            let mut rows = st.query([])?;
            let mut line = String::new();
            while let Some(row) = rows.next()? {
                line.clear();
                for i in 0..columns {
                    if i > 0 {
                        line.push('\t');
                    }
                    match row.get_ref(i)? {
                        ValueRef::Null => {}
                        ValueRef::Integer(n) => line.push_str(&n.to_string()),
                        ValueRef::Real(x) => line.push_str(&x.to_string()),
                        ValueRef::Text(t) => one_line(&String::from_utf8_lossy(t), &mut line),
                        ValueRef::Blob(b) => push_hex(b, &mut line),
                    }
                }
                emitter.emit(&line);
            }
            Ok(())
        })();
        if let Err(e) = read {
            warn!(path = %path.display(), table = %table, error = %e, "SQLite table skipped");
        }
    }
    Ok(())
}

/// The largest document read, to refuse a file that only looks like BSON (MongoDB's own limit is 16 MiB).
const BSON_MAX_DOCUMENT: i64 = 128 << 20;

/// A MongoDB dump: documents one after the other, each written out as relaxed extended JSON.
pub(crate) fn scan_bson<R: Read>(
    reader: R,
    cap: usize,
    f: &mut impl FnMut(Option<&str>, u64, u64, usize, &[u8]),
) -> Result<()> {
    let mut r = BufReader::with_capacity(1 << 20, reader);
    let mut emitter = Emitter::new(f, cap, None);
    loop {
        let head = r.fill_buf().context("read the BSON file")?;
        if head.is_empty() {
            return Ok(());
        }
        if head.len() >= 4 {
            let len = i32::from_le_bytes([head[0], head[1], head[2], head[3]]) as i64;
            if !(5..=BSON_MAX_DOCUMENT).contains(&len) {
                bail!("not a BSON document (length {len})");
            }
        }
        let doc = bson::Document::from_reader(&mut r).context("decode a BSON document")?;
        emitter.emit(&bson::Bson::Document(doc).into_relaxed_extjson().to_string());
    }
}

struct Elements<'a>(&'a mut dyn FnMut(&str));

impl<'de> Visitor<'de> for Elements<'_> {
    type Value = ();

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("a JSON array")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> std::result::Result<(), A::Error> {
        while let Some(v) = seq.next_element::<serde_json::Value>()? {
            (self.0)(&v.to_string());
        }
        Ok(())
    }
}

/// A `.json` file holding one array: each element, compact, is a line. Read an element at a time,
/// so an array of any length needs memory for one element.
pub(crate) fn scan_json_array<R: Read>(
    reader: R,
    cap: usize,
    f: &mut impl FnMut(Option<&str>, u64, u64, usize, &[u8]),
) -> Result<()> {
    let mut emitter = Emitter::new(f, cap, None);
    let mut de = serde_json::Deserializer::from_reader(BufReader::with_capacity(1 << 20, reader));
    de.deserialize_seq(Elements(&mut |line| emitter.emit(line))).context("read the JSON array")
}

/// Open the database and read its table list: enough to tell a database from anything else.
pub(crate) fn probe_sqlite(path: &Path) -> Result<()> {
    let conn = rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.query_row("SELECT count(*) FROM sqlite_master", [], |r| r.get::<_, i64>(0))
        .context("not a SQLite database, or encrypted")?;
    Ok(())
}

/// Decode the first document.
pub(crate) fn probe_bson<R: Read>(reader: R) -> Result<()> {
    let mut r = BufReader::new(reader);
    if !r.fill_buf()?.is_empty() {
        bson::Document::from_reader(&mut r).context("decode the first BSON document")?;
    }
    Ok(())
}

/// Read the first element.
pub(crate) fn probe_json_array<R: Read>(reader: R) -> Result<()> {
    struct First;
    impl<'de> Visitor<'de> for First {
        type Value = ();
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a JSON array")
        }
        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> std::result::Result<(), A::Error> {
            // The parser insists that a sequence is read to its end, so stopping after the
            // first element is done with an error of our own and recognised below.
            match seq.next_element::<serde_json::Value>()? {
                Some(_) => Err(serde::de::Error::custom(STOP)),
                None => Ok(()),
            }
        }
    }
    const STOP: &str = "prowlcat: first element read";
    let mut de = serde_json::Deserializer::from_reader(BufReader::new(reader));
    match de.deserialize_seq(First) {
        Err(e) if e.to_string().contains(STOP) => Ok(()),
        other => other.context("read the start of the JSON array"),
    }
}
