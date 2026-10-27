//! How the TUI is started, from the command line or from tests.

use std::path::PathBuf;

use prowlcat_core::TypeHint;

/// What to do right after the interface opens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Start {
    /// Show the home screen and wait for a target.
    Home,
    /// Run a search for `TuiOptions::targets` straight away.
    Search,
    /// Open the corpus screen and compute its statistics.
    Stats,
    /// Open the corpus screen and build an index into this directory.
    IndexBuild(PathBuf),
}

#[derive(Debug, Clone)]
pub struct TuiOptions {
    pub targets: Vec<String>,
    pub hint: TypeHint,
    pub paths: Vec<PathBuf>,
    pub index: Option<PathBuf>,
    pub recursive: bool,
    pub threads: Option<usize>,
    pub max_matches: u64,
    pub passwords: Vec<String>,
    pub start: Start,
    /// Move Fang. Off holds a still frame (`--no-animation`, `PROWLCAT_NO_ANIM=1`).
    pub animate: bool,
    /// Play the short intro when the interface opens.
    pub splash: bool,
    /// Remember the corpus and recent searches between runs.
    pub persist: bool,
}

impl Default for TuiOptions {
    fn default() -> Self {
        Self {
            targets: Vec::new(),
            hint: TypeHint::Auto,
            paths: Vec::new(),
            index: None,
            recursive: true,
            threads: None,
            max_matches: 0,
            passwords: Vec::new(),
            start: Start::Home,
            animate: !env_flag("PROWLCAT_NO_ANIM"),
            splash: true,
            persist: !env_flag("PROWLCAT_NO_HISTORY"),
        }
    }
}

fn env_flag(name: &str) -> bool {
    std::env::var(name).is_ok_and(|v| !v.is_empty() && v != "0")
}
