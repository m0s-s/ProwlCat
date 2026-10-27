//! What ProwlCat remembers between runs: the last corpus and index, and recent searches.
//!
//! The file lives in the user's state directory and holds the targets you searched for, so it can
//! be switched off (`--no-history`, `PROWLCAT_NO_HISTORY=1`) and cleared (`/history clear`).

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

const MAX_RECENT: usize = 20;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recent {
    pub target: String,
    pub hits: u64,
    pub elapsed_ms: u64,
    /// Seconds since the Unix epoch.
    pub at: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Saved {
    #[serde(default)]
    pub corpus: Vec<PathBuf>,
    #[serde(default)]
    pub index: Option<PathBuf>,
    #[serde(default)]
    pub recent: Vec<Recent>,
}

/// A [`Saved`] bound to its file. With no file nothing is read or written.
#[derive(Debug, Clone)]
pub struct Store {
    file: Option<PathBuf>,
    pub saved: Saved,
}

impl Store {
    /// Open the default location, or a disabled store when persistence is off.
    pub fn open(enabled: bool) -> Self {
        let file = enabled.then(state_file).flatten();
        Self::at(file)
    }

    pub fn at(file: Option<PathBuf>) -> Self {
        let saved = file.as_deref().and_then(read).unwrap_or_default();
        Self { file, saved }
    }

    pub fn push(&mut self, target: &str, hits: u64, elapsed_ms: u64) {
        self.saved.recent.retain(|r| r.target != target);
        self.saved.recent.insert(0, Recent { target: target.to_string(), hits, elapsed_ms, at: now() });
        self.saved.recent.truncate(MAX_RECENT);
    }

    pub fn clear_recent(&mut self) {
        self.saved.recent.clear();
    }

    /// Best effort: a read-only home directory must never break a search.
    pub fn save(&self) {
        let Some(file) = &self.file else { return };
        if let Err(e) = write(file, &self.saved) {
            tracing::debug!(error = %e, path = %file.display(), "could not save ProwlCat state");
        }
    }
}

fn read(file: &Path) -> Option<Saved> {
    serde_json::from_slice(&std::fs::read(file).ok()?).ok()
}

fn write(file: &Path, saved: &Saved) -> std::io::Result<()> {
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // Write beside the target and rename, so a crash never leaves half a file.
    let tmp = file.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(saved).map_err(std::io::Error::other)?)?;
    std::fs::rename(&tmp, file)
}

/// `PROWLCAT_STATE_DIR` if set, else the platform's per-user state directory.
pub fn state_file() -> Option<PathBuf> {
    state_dir_from(|k| std::env::var_os(k).map(PathBuf::from)).map(|d| d.join("state.json"))
}

fn state_dir_from(get: impl Fn(&str) -> Option<PathBuf>) -> Option<PathBuf> {
    if let Some(dir) = get("PROWLCAT_STATE_DIR").filter(|p| !p.as_os_str().is_empty()) {
        return Some(dir);
    }
    if cfg!(windows) {
        return get("LOCALAPPDATA").map(|d| d.join("prowlcat"));
    }
    if cfg!(target_os = "macos") {
        return get("HOME").map(|h| h.join("Library/Application Support/prowlcat"));
    }
    get("XDG_STATE_HOME")
        .filter(|p| p.is_absolute())
        .or_else(|| get("HOME").map(|h| h.join(".local/state")))
        .map(|d| d.join("prowlcat"))
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// "just now", "12 min ago", "3 h ago", "yesterday", "5 d ago".
pub fn ago(at: u64) -> String {
    ago_between(at, now())
}

pub fn ago_between(at: u64, now: u64) -> String {
    let secs = now.saturating_sub(at);
    match secs {
        0..=59 => "just now".to_string(),
        60..=3599 => format!("{} min ago", secs / 60),
        3600..=86_399 => format!("{} h ago", secs / 3600),
        86_400..=172_799 => "yesterday".to_string(),
        _ => format!("{} d ago", secs / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_store_touches_nothing() {
        let mut s = Store::at(None);
        s.push("acme.corp", 3, 10);
        s.save();
        assert_eq!(s.saved.recent.len(), 1, "still remembered for this session");
    }

    #[test]
    fn round_trips_through_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("nested/state.json");
        let mut s = Store::at(Some(file.clone()));
        s.saved.corpus = vec!["/data".into()];
        s.saved.index = Some("/data.idx".into());
        s.push("acme.corp", 3481, 12_000);
        s.save();

        let back = Store::at(Some(file));
        assert_eq!(back.saved.corpus, vec![PathBuf::from("/data")]);
        assert_eq!(back.saved.index.as_deref(), Some(Path::new("/data.idx")));
        assert_eq!(back.saved.recent[0].target, "acme.corp");
        assert_eq!(back.saved.recent[0].hits, 3481);
    }

    #[test]
    fn recent_is_deduplicated_newest_first_and_capped() {
        let mut s = Store::at(None);
        s.push("a", 1, 1);
        s.push("b", 2, 1);
        s.push("a", 3, 1);
        let targets: Vec<_> = s.saved.recent.iter().map(|r| r.target.as_str()).collect();
        assert_eq!(targets, vec!["a", "b"]);
        assert_eq!(s.saved.recent[0].hits, 3);

        for i in 0..50 {
            s.push(&format!("t{i}"), 0, 0);
        }
        assert_eq!(s.saved.recent.len(), MAX_RECENT);
        assert_eq!(s.saved.recent[0].target, "t49");
    }

    #[test]
    fn corrupt_file_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("state.json");
        std::fs::write(&file, "{ not json").unwrap();
        let s = Store::at(Some(file));
        assert_eq!(s.saved, Saved::default());
    }

    #[test]
    fn missing_fields_default() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("state.json");
        std::fs::write(&file, r#"{"corpus":["/x"]}"#).unwrap();
        let s = Store::at(Some(file));
        assert_eq!(s.saved.corpus, vec![PathBuf::from("/x")]);
        assert!(s.saved.recent.is_empty());
    }

    #[test]
    fn state_dir_prefers_the_override() {
        let get = |k: &str| match k {
            "PROWLCAT_STATE_DIR" => Some(PathBuf::from("/custom")),
            "HOME" => Some(PathBuf::from("/home/me")),
            _ => None,
        };
        assert_eq!(state_dir_from(get), Some(PathBuf::from("/custom")));
    }

    #[test]
    fn ago_reads_naturally() {
        assert_eq!(ago_between(100, 130), "just now");
        assert_eq!(ago_between(0, 18 * 60), "18 min ago");
        assert_eq!(ago_between(0, 3 * 3600), "3 h ago");
        assert_eq!(ago_between(0, 30 * 3600), "yesterday");
        assert_eq!(ago_between(0, 5 * 86_400), "5 d ago");
        assert_eq!(ago_between(500, 100), "just now", "clock skew never goes negative");
    }
}
