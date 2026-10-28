//! Dropping duplicate records without holding them all in memory.
//!
//! Records are collected up to a memory budget, sorted and parked in temporary files (the same
//! place and the same way as the results of a search), then merged back in order, so equal
//! records meet and all but the first are dropped. Whatever the number of records, memory stays
//! near the budget. Two records are the same when their accounts are equal ignoring case and
//! their secrets are equal exactly; the first one found is the one kept.

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Seek, SeekFrom, Write};
use std::path::Path;

use anyhow::{Context, Result};
use bincode::Options;
use prowlcat_types::Record;
use serde::{Deserialize, Serialize};

use crate::sorted::spill_dir;

#[derive(Serialize, Deserialize)]
struct Entry {
    key: String,
    seq: u64,
    record: Record,
}

fn codec() -> impl bincode::Options {
    bincode::DefaultOptions::new()
}

fn weight(e: &Entry) -> usize {
    let r = &e.record;
    192 + e.key.len() * 2 + r.account.len() + r.secret.len() + r.query.len() + r.path.len()
        + r.secret_looks_like.iter().map(|s| s.len() + 24).sum::<usize>()
}

pub struct Unique {
    budget: usize,
    mem: Vec<Entry>,
    mem_bytes: usize,
    runs: Vec<File>,
    seq: u64,
}

impl Unique {
    /// Hold at most about `budget_bytes` of records in memory.
    pub fn new(budget_bytes: usize) -> Self {
        Self { budget: budget_bytes, mem: Vec::new(), mem_bytes: 0, runs: Vec::new(), seq: 0 }
    }

    pub fn push(&mut self, record: Record) -> Result<()> {
        let key = format!("{}\0{}", record.account.to_lowercase(), record.secret);
        let entry = Entry { key, seq: self.seq, record };
        self.seq += 1;
        self.mem_bytes += weight(&entry);
        self.mem.push(entry);
        if self.mem_bytes > self.budget {
            self.spill()?;
        }
        Ok(())
    }

    fn spill(&mut self) -> Result<()> {
        self.mem.sort_unstable_by(|a, b| (&a.key, a.seq).cmp(&(&b.key, b.seq)));
        let file = match spill_dir() {
            Some(dir) => tempfile::tempfile_in(Path::new(dir)),
            None => tempfile::tempfile(),
        }
        .context("create a temporary file for the records")?;
        {
            let mut out = BufWriter::with_capacity(1 << 20, &file);
            for e in &self.mem {
                codec().serialize_into(&mut out, e).context("write a record to disk")?;
            }
            out.flush()?;
        }
        self.runs.push(file);
        self.mem = Vec::new();
        self.mem_bytes = 0;
        Ok(())
    }

    /// The records, each once, in order of account and secret.
    pub fn finish(mut self) -> Result<UniqueIter> {
        self.mem.sort_unstable_by(|a, b| (&a.key, a.seq).cmp(&(&b.key, b.seq)));
        let mut sources: Vec<Source> = vec![Source::Mem(std::mem::take(&mut self.mem).into_iter())];
        for mut f in self.runs {
            f.seek(SeekFrom::Start(0))?;
            sources.push(Source::Run(BufReader::with_capacity(64 * 1024, f)));
        }
        let mut it = UniqueIter { heads: (0..sources.len()).map(|_| None).collect(), sources, heap: BinaryHeap::new(), last_key: None };
        for i in 0..it.sources.len() {
            it.refill(i)?;
        }
        Ok(it)
    }
}

enum Source {
    Mem(std::vec::IntoIter<Entry>),
    Run(BufReader<File>),
}

pub struct UniqueIter {
    sources: Vec<Source>,
    heads: Vec<Option<Entry>>,
    heap: BinaryHeap<Reverse<(String, u64, usize)>>,
    last_key: Option<String>,
}

impl UniqueIter {
    fn refill(&mut self, i: usize) -> Result<()> {
        let next = match &mut self.sources[i] {
            Source::Mem(it) => it.next(),
            Source::Run(r) => {
                if r.fill_buf().context("read records back from disk")?.is_empty() {
                    None
                } else {
                    Some(codec().deserialize_from(&mut *r).context("decode a spilled record")?)
                }
            }
        };
        if let Some(e) = &next {
            self.heap.push(Reverse((e.key.clone(), e.seq, i)));
        }
        self.heads[i] = next;
        Ok(())
    }

    fn next_record(&mut self) -> Result<Option<Record>> {
        while let Some(Reverse((key, _, i))) = self.heap.pop() {
            let entry = self.heads[i].take().expect("a head for every heap entry");
            self.refill(i)?;
            if self.last_key.as_deref() == Some(key.as_str()) {
                continue;
            }
            self.last_key = Some(key);
            return Ok(Some(entry.record));
        }
        Ok(None)
    }
}

impl Iterator for UniqueIter {
    type Item = Result<Record>;

    fn next(&mut self) -> Option<Self::Item> {
        self.next_record().transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(account: &str, secret: &str, line: u64) -> Record {
        Record {
            account: account.into(),
            secret: secret.into(),
            secret_looks_like: Vec::new(),
            query: "q".into(),
            path: "f".into(),
            archive_entry: None,
            line_number: Some(line),
        }
    }

    fn run(budget: usize, input: &[(&str, &str)]) -> Vec<(String, String, u64)> {
        let mut u = Unique::new(budget);
        for (n, (a, s)) in input.iter().enumerate() {
            u.push(rec(a, s, n as u64)).unwrap();
        }
        u.finish().unwrap().map(|r| r.unwrap()).map(|r| (r.account, r.secret, r.line_number.unwrap())).collect()
    }

    #[test]
    fn keeps_the_first_of_each_and_sorts() {
        let input = [("b@x.io", "p2"), ("a@x.io", "p1"), ("B@X.io", "p2"), ("a@x.io", "other"), ("a@x.io", "p1")];
        let want = vec![("a@x.io".into(), "other".into(), 3), ("a@x.io".into(), "p1".into(), 1), ("b@x.io".into(), "p2".into(), 0)];
        for budget in [usize::MAX, 0, 600] {
            assert_eq!(run(budget, &input), want, "budget {budget}");
        }
    }

    #[test]
    fn a_secret_that_differs_only_in_case_is_a_different_record() {
        assert_eq!(run(0, &[("a@x.io", "Pw"), ("a@x.io", "pw")]).len(), 2);
    }

    #[test]
    fn many_records_through_many_runs() {
        let owned: Vec<(String, String)> = (0..5000).map(|i| (format!("u{}@x.io", i % 700), format!("pw{}", i % 3))).collect();
        let input: Vec<(&str, &str)> = owned.iter().map(|(a, s)| (a.as_str(), s.as_str())).collect();
        let spilled = run(2_000, &input);
        let in_memory = run(usize::MAX, &input);
        assert_eq!(spilled, in_memory);
        assert_eq!(spilled.len(), 2100);
        assert!(spilled.windows(2).all(|w| (w[0].0.to_lowercase(), &w[0].1) <= (w[1].0.to_lowercase(), &w[1].1)));
    }

    #[test]
    fn nothing_in_nothing_out() {
        assert!(run(0, &[]).is_empty());
    }
}
