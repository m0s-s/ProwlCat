use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use glob::glob;
use walkdir::WalkDir;

use crate::entry::SourceEntry;

pub struct SourceEnumerator {
    roots: Vec<PathBuf>,
    recursive: bool,
}

impl SourceEnumerator {
    pub fn new(paths: &[PathBuf], recursive: bool) -> Result<Self> {
        if paths.is_empty() {
            anyhow::bail!("no scan paths provided");
        }
        Ok(Self {
            roots: paths.to_vec(),
            recursive,
        })
    }

    pub fn collect(&self) -> Result<Vec<SourceEntry>> {
        let mut entries = Vec::new();
        let mut seen = std::collections::HashSet::new();

        for root in &self.roots {
            let root_str = root.to_string_lossy();
            if root_str.contains('*') || root_str.contains('?') {
                for path in glob(&root_str).context("invalid glob pattern")? {
                    let path = path?;
                    push_file(&mut entries, &mut seen, path)?;
                }
                continue;
            }

            if root.is_file() {
                push_file(&mut entries, &mut seen, root.clone())?;
            } else if root.is_dir() {
                if self.recursive {
                    for entry in WalkDir::new(root)
                        .follow_links(false)
                        .into_iter()
                        .filter_map(|e| e.ok())
                    {
                        if entry.file_type().is_file() {
                            push_file(&mut entries, &mut seen, entry.path().to_path_buf())?;
                        }
                    }
                } else if let Some(entries_read) = read_flat_dir(root)? {
                    for path in entries_read {
                        push_file(&mut entries, &mut seen, path)?;
                    }
                }
            } else {
                anyhow::bail!("path not found: {}", root.display());
            }
        }

        entries.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(entries)
    }
}

fn push_file(
    out: &mut Vec<SourceEntry>,
    seen: &mut std::collections::HashSet<PathBuf>,
    path: PathBuf,
) -> Result<()> {
    if seen.insert(path.clone()) {
        out.push(SourceEntry::from_path(path)?);
    }
    Ok(())
}

fn read_flat_dir(dir: &Path) -> Result<Option<Vec<PathBuf>>> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            files.push(entry.path());
        }
    }
    Ok(Some(files))
}
