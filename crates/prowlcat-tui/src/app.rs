//! Application state and behaviour. Nothing here draws; `ui` reads this and `lib` feeds it
//! keys and ticks, which keeps the whole interface testable without a terminal.

use std::cell::Cell;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::mpsc::{channel, sync_channel, Receiver, Sender};
use std::time::{Duration, Instant};

use prowlcat_core::{ProgressHandle, QueryKind, ScanResult, ScanSummary, SortedResults, TargetParser, TypeHint};
use prowlcat_index::IndexMeta;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::command::{self, Command};
use crate::history::Store;
use crate::input::Input;
use crate::mascot::{Mood, Pose, FADE_STEPS};
use crate::options::{Start, TuiOptions};
use crate::theme::Theme;
use crate::worker::{self, CorpusStats, Msg, MsgKind, SearchSpec};

/// One animation tick. Everything that moves is a function of the tick count.
pub const TICK: Duration = Duration::from_millis(100);
/// How long the intro plays, in ticks.
pub const SPLASH_TICKS: u64 = 20;
/// Matches kept for the live view while a scan runs.
const LIVE_KEEP: usize = 400;
/// Matches the scan may have queued for the live view; beyond this it drops the excess.
const LIVE_QUEUE: usize = 16_384;
/// After a successful search, how long Fang gets to pounce before the results open.
const AUTO_ADVANCE: u64 = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Home,
    Scanning,
    Results,
    Corpus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeKind {
    Info,
    Ok,
    Error,
}

#[derive(Debug, Clone)]
pub struct Notice {
    pub text: String,
    pub kind: NoticeKind,
    pub until: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptKind {
    Export,
    IndexBuild,
    IndexUse,
}

#[derive(Debug, Clone)]
pub enum Overlay {
    None,
    Help,
    Prompt(PromptKind, Input),
}

/// Settings that apply to every search in this session.
#[derive(Debug, Clone)]
pub struct Session {
    pub paths: Vec<PathBuf>,
    pub index: Option<PathBuf>,
    pub hint: TypeHint,
    pub recursive: bool,
    pub threads: Option<usize>,
    pub max_matches: u64,
    pub passwords: Vec<String>,
}

#[derive(Debug)]
pub enum Phase {
    Running,
    /// The scan ended; `found` decides Fang's reaction.
    Finished { found: bool, at: u64 },
    Failed(String),
}

/// A search in flight (or just finished).
pub struct ScanRun {
    pub job: u64,
    pub targets: Vec<String>,
    /// "domain", "3 targets" ...
    pub kind_label: String,
    pub progress: ProgressHandle,
    pub live_rx: Receiver<ScanResult>,
    pub live: VecDeque<ScanResult>,
    pub hits_seen: u64,
    pub last_hit_tick: Option<u64>,
    pub started: Instant,
    pub via_index: bool,
    pub phase: Phase,
    pub summary: Option<ScanSummary>,
}

impl ScanRun {
    pub fn running(&self) -> bool {
        matches!(self.phase, Phase::Running)
    }

    pub fn ratio(&self) -> Option<f64> {
        let total = self.progress.total.load(Ordering::Relaxed);
        (total > 0).then(|| (self.progress.done.load(Ordering::Relaxed) as f64 / total as f64).clamp(0.0, 1.0))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    Files,
    Lines,
}

/// Matches of one file or archive member.
pub struct Group {
    pub label: String,
    pub hits: Vec<usize>,
}

pub struct ResultsView {
    /// The matches shown. All of them unless `full` is set.
    pub all: Vec<ScanResult>,
    /// Every match, when there were more than the screen keeps; export reads from here.
    pub full: Option<Box<SortedResults>>,
    pub summary: ScanSummary,
    pub target_label: String,
    /// Every group, most hits first.
    groups: Vec<Group>,
    /// The groups after the filter, with the hit indices that survive it.
    pub view: Vec<Group>,
    pub file_sel: usize,
    pub line_sel: usize,
    pub file_scroll: Cell<usize>,
    pub line_scroll: Cell<usize>,
    pub focus: Pane,
    pub filter: String,
    pub filtering: Option<Input>,
    pub by_kind: Vec<(QueryKind, usize)>,
}

impl ResultsView {
    pub fn new(all: Vec<ScanResult>, summary: ScanSummary, target_label: String) -> Self {
        type FileKey = (PathBuf, Option<String>);
        let mut keyed: Vec<(FileKey, Vec<usize>)> = Vec::new();
        for (i, r) in all.iter().enumerate() {
            let key = (r.source.path.clone(), r.source.archive_entry.clone());
            match keyed.iter_mut().find(|(k, _)| *k == key) {
                Some((_, hits)) => hits.push(i),
                None => keyed.push((key, vec![i])),
            }
        }
        let mut groups: Vec<Group> = keyed
            .into_iter()
            .map(|((path, entry), mut hits)| {
                hits.sort_by_key(|&i| all[i].source.line_number);
                Group { label: file_label(&path, entry.as_deref()), hits }
            })
            .collect();
        groups.sort_by(|a, b| b.hits.len().cmp(&a.hits.len()).then_with(|| a.label.cmp(&b.label)));

        let mut by_kind: Vec<(QueryKind, usize)> = Vec::new();
        for r in &all {
            match by_kind.iter_mut().find(|(k, _)| *k == r.kind) {
                Some((_, n)) => *n += 1,
                None => by_kind.push((r.kind, 1)),
            }
        }
        by_kind.sort_by_key(|(_, n)| std::cmp::Reverse(*n));

        let mut v = Self {
            all,
            full: None,
            summary,
            target_label,
            groups,
            view: Vec::new(),
            file_sel: 0,
            line_sel: 0,
            file_scroll: Cell::new(0),
            line_scroll: Cell::new(0),
            focus: Pane::Files,
            filter: String::new(),
            filtering: None,
            by_kind,
        };
        v.rebuild();
        v
    }

    /// Attach the complete result set of a search that found more than the screen holds.
    pub fn with_full(mut self, full: Option<Box<SortedResults>>) -> Self {
        self.full = full;
        self
    }

    /// Matches found but not shown (they are still exported).
    pub fn omitted(&self) -> usize {
        self.full.as_ref().map_or(0, |f| f.len().saturating_sub(self.all.len()))
    }

    /// Recompute `view` from the filter, keeping the cursor in range.
    pub fn rebuild(&mut self) {
        let needle = self.filter.to_lowercase();
        self.view = self
            .groups
            .iter()
            .filter_map(|g| {
                if needle.is_empty() {
                    return Some(Group { label: g.label.clone(), hits: g.hits.clone() });
                }
                let label_hit = g.label.to_lowercase().contains(&needle);
                let hits: Vec<usize> = g
                    .hits
                    .iter()
                    .copied()
                    .filter(|&i| label_hit || self.all[i].line_text.to_lowercase().contains(&needle))
                    .collect();
                (!hits.is_empty()).then(|| Group { label: g.label.clone(), hits })
            })
            .collect();
        self.file_sel = self.file_sel.min(self.view.len().saturating_sub(1));
        self.clamp_line();
    }

    fn clamp_line(&mut self) {
        let n = self.view.get(self.file_sel).map_or(0, |g| g.hits.len());
        self.line_sel = self.line_sel.min(n.saturating_sub(1));
    }

    /// Files with at least one match, ignoring the filter.
    pub fn groups_len(&self) -> usize {
        self.groups.len()
    }

    pub fn visible_hits(&self) -> usize {
        self.view.iter().map(|g| g.hits.len()).sum()
    }

    pub fn selected_group(&self) -> Option<&Group> {
        self.view.get(self.file_sel)
    }

    pub fn selected_hit(&self) -> Option<&ScanResult> {
        let g = self.selected_group()?;
        g.hits.get(self.line_sel).map(|&i| &self.all[i])
    }

    pub fn move_sel(&mut self, delta: isize) {
        match self.focus {
            Pane::Files => {
                let n = self.view.len();
                self.file_sel = step(self.file_sel, delta, n);
                self.line_sel = 0;
                self.line_scroll.set(0);
            }
            Pane::Lines => {
                let n = self.selected_group().map_or(0, |g| g.hits.len());
                self.line_sel = step(self.line_sel, delta, n);
            }
        }
    }

    pub fn jump(&mut self, to_end: bool) {
        let delta = if to_end { isize::MAX / 2 } else { isize::MIN / 2 };
        self.move_sel(delta);
    }
}

fn step(cur: usize, delta: isize, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    (cur as isize).saturating_add(delta).clamp(0, len as isize - 1) as usize
}

/// `parent/file.txt` (and ` → member` inside an archive), short enough for a narrow list.
pub fn file_label(path: &Path, entry: Option<&str>) -> String {
    let comps: Vec<_> = path.components().filter_map(|c| c.as_os_str().to_str()).collect();
    let tail = match comps.len() {
        0 => path.display().to_string(),
        1 => comps[0].to_string(),
        n => format!("{}/{}", comps[n - 2], comps[n - 1]),
    };
    match entry {
        Some(e) => format!("{tail} → {e}"),
        None => tail,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CorpusAction {
    Refresh,
    BuildIndex,
    UseIndex,
    StopUsingIndex,
}

impl CorpusAction {
    pub fn title(self) -> &'static str {
        match self {
            CorpusAction::Refresh => "Refresh statistics",
            CorpusAction::BuildIndex => "Build index",
            CorpusAction::UseIndex => "Use an existing index",
            CorpusAction::StopUsingIndex => "Stop using the index",
        }
    }

    pub fn detail(self) -> &'static str {
        match self {
            CorpusAction::Refresh => "walk the corpus and count files by kind",
            CorpusAction::BuildIndex => "one pass over the corpus, then searches take milliseconds",
            CorpusAction::UseIndex => "answer searches from a prebuilt index directory",
            CorpusAction::StopUsingIndex => "go back to scanning the corpus files",
        }
    }
}

pub struct BuildRun {
    pub job: u64,
    pub out: PathBuf,
    pub started: Instant,
}

#[derive(Default)]
pub struct CorpusView {
    pub stats: Option<CorpusStats>,
    pub stats_job: Option<u64>,
    pub index_meta: Option<IndexMeta>,
    pub build: Option<BuildRun>,
    pub last_built: Option<(PathBuf, IndexMeta, u64)>,
    pub sel: usize,
}

pub struct App {
    pub theme: Theme,
    pub screen: Screen,
    pub tick: u64,
    pub animate: bool,
    pub mood: Mood,
    pub mood_since: u64,
    /// The intro runs until this tick; `None` when it is over or was skipped.
    pub splash_until: Option<u64>,
    pub input: Input,
    pub palette_sel: usize,
    hist_pos: Option<usize>,
    hist_draft: String,
    pub overlay: Overlay,
    pub notice: Option<Notice>,
    pub session: Session,
    pub store: Store,
    pub scan: Option<ScanRun>,
    pub results: Option<ResultsView>,
    pub corpus: CorpusView,
    pub quit: bool,
    job_counter: u64,
    msg_tx: Sender<Msg>,
    msg_rx: Receiver<Msg>,
}

impl App {
    pub fn new(opts: TuiOptions) -> Self {
        let store = Store::open(opts.persist);
        Self::with_parts(opts, Theme::detect(), store)
    }

    pub fn with_parts(opts: TuiOptions, theme: Theme, store: Store) -> Self {
        let explicit = !opts.paths.is_empty() || opts.index.is_some();
        let (paths, index) = if explicit {
            (opts.paths.clone(), opts.index.clone())
        } else {
            let saved = &store.saved;
            (saved.corpus.clone(), saved.index.clone().filter(|p| p.exists()))
        };
        let (msg_tx, msg_rx) = channel();
        let splash = opts.splash && opts.animate && opts.start == Start::Home;
        let mut app = Self {
            theme,
            screen: Screen::Home,
            tick: 0,
            animate: opts.animate,
            mood: Mood::Idle,
            mood_since: 0,
            splash_until: splash.then_some(SPLASH_TICKS),
            input: Input::default(),
            palette_sel: 0,
            hist_pos: None,
            hist_draft: String::new(),
            overlay: Overlay::None,
            notice: None,
            session: Session {
                paths,
                index,
                hint: opts.hint,
                recursive: opts.recursive,
                threads: opts.threads,
                max_matches: opts.max_matches,
                passwords: opts.passwords.clone(),
            },
            store,
            scan: None,
            results: None,
            corpus: CorpusView::default(),
            quit: false,
            job_counter: 0,
            msg_tx,
            msg_rx,
        };
        app.begin(opts);
        app
    }

    fn begin(&mut self, opts: TuiOptions) {
        if let Some(dir) = self.session.index.clone() {
            self.corpus.index_meta = worker::index_meta(&dir).ok();
        }
        match opts.start {
            Start::Home => self.refresh_stats_quietly(),
            Start::Search => self.start_search(opts.targets, Vec::new()),
            Start::Stats => {
                self.screen = Screen::Corpus;
                self.refresh_stats();
            }
            Start::IndexBuild(out) => {
                self.screen = Screen::Corpus;
                self.refresh_stats_quietly();
                self.build_index(out);
            }
        }
    }

    // ---- small helpers -------------------------------------------------------------------

    pub fn pose(&self) -> Pose {
        let flash = self
            .scan
            .as_ref()
            .and_then(|s| s.last_hit_tick)
            .is_some_and(|t| self.tick.saturating_sub(t) <= 1);
        let mut pose = Pose::new(self.mood, self.tick, self.mood_since).flashing(flash).frozen(!self.animate);
        if let Some(until) = self.splash_until {
            let age = SPLASH_TICKS.saturating_sub(until.saturating_sub(self.tick));
            pose = pose.waking(((age / 2) as u8).min(FADE_STEPS));
        }
        pose
    }

    pub fn set_mood(&mut self, mood: Mood) {
        if self.mood != mood {
            self.mood = mood;
            self.mood_since = self.tick;
        }
    }

    pub fn notify(&mut self, kind: NoticeKind, text: impl Into<String>) {
        let ticks = if kind == NoticeKind::Error { 60 } else { 35 };
        self.notice = Some(Notice { text: text.into(), kind, until: self.tick + ticks });
    }

    fn next_job(&mut self) -> u64 {
        self.job_counter += 1;
        self.job_counter
    }

    pub fn index_in_use(&self) -> bool {
        self.session.index.is_some()
    }

    /// The kind the first word of the prompt would be searched as, for the hint beside it.
    pub fn inferred_kind(&self) -> Option<&'static str> {
        let text = self.input.text().trim();
        let first = text.split_whitespace().find(|w| !w.starts_with('@') && !w.starts_with('/'))?;
        let q = TargetParser::new(self.session.hint).parse(first).ok()?;
        Some(kind_label(q.kind))
    }

    /// Whether the command palette is open (the prompt holds `/name` with no space yet).
    pub fn palette_open(&self) -> bool {
        let t = self.input.text();
        matches!(self.overlay, Overlay::None) && self.screen == Screen::Home && t.starts_with('/') && !t.contains(char::is_whitespace)
    }

    pub fn palette_items(&self) -> Vec<(&'static str, &'static str)> {
        command::suggestions(self.input.text().trim_start_matches('/'))
    }

    // ---- time ------------------------------------------------------------------------------

    pub fn on_tick(&mut self) {
        self.tick += 1;
        if self.splash_until.is_some_and(|u| self.tick >= u) {
            self.splash_until = None;
        }
        if self.notice.as_ref().is_some_and(|n| self.tick >= n.until) {
            self.notice = None;
        }
        self.drain_live();
        self.advance_after_search();
    }

    /// Move matches that arrived from the scan into the live view.
    fn drain_live(&mut self) {
        let tick = self.tick;
        let Some(scan) = &mut self.scan else { return };
        let mut got = 0u64;
        while let Ok(r) = scan.live_rx.try_recv() {
            scan.live.push_back(r);
            if scan.live.len() > LIVE_KEEP {
                scan.live.pop_front();
            }
            got += 1;
            if got >= 20_000 {
                break;
            }
        }
        if got > 0 {
            scan.hits_seen += got;
            scan.last_hit_tick = Some(tick);
        }
        // The live queue can drop hits when the scan outpaces the screen; the scan's own counter
        // never does.
        scan.hits_seen = scan.hits_seen.max(scan.progress.matches.load(Ordering::Relaxed));
    }

    fn advance_after_search(&mut self) {
        if self.screen != Screen::Scanning {
            return;
        }
        let Some(ScanRun { phase: Phase::Finished { found: true, at }, .. }) = &self.scan else { return };
        if self.tick.saturating_sub(*at) >= AUTO_ADVANCE && self.results.is_some() {
            self.screen = Screen::Results;
            self.set_mood(Mood::Idle);
        }
    }

    /// Collect answers from background jobs.
    pub fn pump(&mut self) {
        while let Ok(msg) = self.msg_rx.try_recv() {
            self.on_msg(msg);
        }
    }

    fn on_msg(&mut self, msg: Msg) {
        let Msg { job, kind } = msg;
        if self.scan.as_ref().is_some_and(|s| s.job == job) {
            self.on_scan_msg(kind);
        } else if self.corpus.stats_job == Some(job) {
            self.corpus.stats_job = None;
            match kind {
                MsgKind::Stats(s) => self.corpus.stats = Some(s),
                MsgKind::Failed(e) => self.notify(NoticeKind::Error, format!("statistics failed: {e}")),
                _ => {}
            }
        } else if self.corpus.build.as_ref().is_some_and(|b| b.job == job) {
            self.corpus.build = None;
            self.set_mood(Mood::Idle);
            match kind {
                MsgKind::IndexBuilt { meta, dir, elapsed_ms } => {
                    self.notify(
                        NoticeKind::Ok,
                        format!("index built: {} lines in {:.1}s. Use it with /index use {}", meta.docs, elapsed_ms as f64 / 1000.0, dir.display()),
                    );
                    self.corpus.last_built = Some((dir, meta, elapsed_ms));
                    self.set_mood(Mood::Found);
                }
                MsgKind::Failed(e) => {
                    self.notify(NoticeKind::Error, format!("index build failed: {e}"));
                    self.set_mood(Mood::Miss);
                }
                _ => {}
            }
        }
        // Anything else answers a job that was cancelled or replaced: drop it.
    }

    fn on_scan_msg(&mut self, kind: MsgKind) {
        self.drain_live();
        let tick = self.tick;
        match kind {
            MsgKind::SearchDone { results, summary, full } => {
                let Some(scan) = &mut self.scan else { return };
                let found = summary.matches > 0;
                let label = scan.targets.join(" ");
                scan.phase = Phase::Finished { found, at: tick };
                scan.summary = Some(summary.clone());
                self.store.push(&label, summary.matches as u64, summary.elapsed_ms);
                self.store.save();
                if found {
                    self.results = Some(ResultsView::new(results, summary, label).with_full(full));
                    self.set_mood(Mood::Found);
                } else {
                    self.results = None;
                    self.set_mood(Mood::Miss);
                }
            }
            MsgKind::Failed(e) => {
                if let Some(scan) = &mut self.scan {
                    scan.phase = Phase::Failed(e);
                }
                self.set_mood(Mood::Miss);
            }
            _ => {}
        }
    }

    // ---- input -----------------------------------------------------------------------------

    pub fn on_paste(&mut self, text: &str) {
        if self.splash_until.take().is_some() {
            return;
        }
        match &mut self.overlay {
            Overlay::Prompt(_, input) => input.insert_str(text),
            Overlay::Help => {}
            Overlay::None => match self.screen {
                Screen::Home => {
                    self.input.insert_str(text);
                    self.palette_sel = 0;
                }
                Screen::Results => {
                    if let Some(f) = self.results.as_mut().and_then(|r| r.filtering.as_mut()) {
                        f.insert_str(text);
                    }
                }
                _ => {}
            },
        }
    }

    pub fn on_key(&mut self, key: KeyEvent) {
        let ctrl = is_ctrl(&key);
        if ctrl && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('q')) {
            self.quit_app();
            return;
        }
        // Any key ends the intro; the key itself is not acted on.
        if self.splash_until.take().is_some() {
            return;
        }
        match self.overlay {
            Overlay::Help => {
                self.overlay = Overlay::None;
                return;
            }
            Overlay::Prompt(..) => {
                self.on_prompt_key(key);
                return;
            }
            Overlay::None => {}
        }
        match self.screen {
            Screen::Home => self.on_home_key(key),
            Screen::Scanning => self.on_scan_key(key),
            Screen::Results => self.on_results_key(key),
            Screen::Corpus => self.on_corpus_key(key),
        }
    }

    fn quit_app(&mut self) {
        self.cancel_scan();
        self.persist_session();
        self.quit = true;
    }

    fn persist_session(&mut self) {
        self.store.saved.corpus = self.session.paths.clone();
        self.store.saved.index = self.session.index.clone();
        self.store.save();
    }

    fn on_home_key(&mut self, key: KeyEvent) {
        let ctrl = is_ctrl(&key);
        let palette = self.palette_open();
        match key.code {
            KeyCode::Enter => self.submit(),
            KeyCode::Esc => {
                self.input.clear();
                self.hist_pos = None;
            }
            KeyCode::Tab => self.complete(),
            KeyCode::Up if palette => {
                let n = self.palette_items().len().max(1);
                self.palette_sel = (self.palette_sel + n - 1) % n;
            }
            KeyCode::Down if palette => {
                let n = self.palette_items().len().max(1);
                self.palette_sel = (self.palette_sel + 1) % n;
            }
            KeyCode::Up => self.history_step(1),
            KeyCode::Down => self.history_step(-1),
            KeyCode::Left => self.input.left(),
            KeyCode::Right => self.input.right(),
            KeyCode::Home => self.input.home(),
            KeyCode::End => self.input.end(),
            KeyCode::Backspace => {
                self.input.backspace();
                self.palette_sel = 0;
            }
            KeyCode::Delete => self.input.delete(),
            KeyCode::Char('a') if ctrl => self.input.home(),
            KeyCode::Char('e') if ctrl => self.input.end(),
            KeyCode::Char('u') if ctrl => self.input.kill_to_start(),
            KeyCode::Char('w') if ctrl => self.input.kill_word(),
            KeyCode::Char('l') if ctrl => self.notice = None,
            KeyCode::Char('?') if self.input.is_empty() => self.overlay = Overlay::Help,
            KeyCode::Char(c) if !ctrl => {
                self.input.insert(c);
                self.palette_sel = 0;
                self.hist_pos = None;
            }
            _ => {}
        }
    }

    /// Walk through recent searches with the arrow keys, like shell history.
    fn history_step(&mut self, dir: isize) {
        let recent = &self.store.saved.recent;
        if recent.is_empty() {
            return;
        }
        let next = match (self.hist_pos, dir) {
            (None, 1) => {
                self.hist_draft = self.input.text().to_string();
                Some(0)
            }
            (None, _) => return,
            (Some(i), 1) => Some((i + 1).min(recent.len() - 1)),
            (Some(0), _) => None,
            (Some(i), _) => Some(i - 1),
        };
        self.hist_pos = next;
        match next {
            Some(i) => self.input.set(recent[i].target.clone()),
            None => self.input.set(std::mem::take(&mut self.hist_draft)),
        }
    }

    /// Tab: finish a `/command`, or a path after `@`.
    fn complete(&mut self) {
        if self.palette_open() {
            let items = self.palette_items();
            if let Some((name, _)) = items.get(self.palette_sel.min(items.len().saturating_sub(1))) {
                self.input.set(format!("/{name} "));
                self.palette_sel = 0;
            }
            return;
        }
        let (_, word) = self.input.word_before_cursor();
        if let Some(prefix) = word.strip_prefix('@') {
            let prefix = prefix.to_string();
            if let Some(done) = complete_path(&prefix) {
                self.input.replace_word_before_cursor(&format!("@{done}"));
            }
        }
    }

    fn submit(&mut self) {
        let line = self.input.take();
        self.notice = None;
        self.hist_pos = None;
        self.palette_sel = 0;
        // A bare, unambiguous `/command` with the palette open runs the highlighted entry.
        let line = if line.starts_with('/') && !line.contains(char::is_whitespace) {
            let items = command::suggestions(line.trim_start_matches('/'));
            match items.get(self.palette_sel.min(items.len().saturating_sub(1))) {
                Some((name, _)) if !items.iter().any(|(n, _)| *n == line.trim_start_matches('/')) => format!("/{name}"),
                _ => line,
            }
        } else {
            line
        };
        match command::parse(&line, self.session.hint) {
            Ok(cmd) => {
                self.run_command(cmd);
                // Something went wrong and nothing opened: keep the line so it can be fixed.
                let failed = self.notice.as_ref().is_some_and(|n| n.kind == NoticeKind::Error);
                if failed && self.screen == Screen::Home && self.input.is_empty() {
                    self.input.set(line);
                }
            }
            Err(e) => {
                self.input.set(line);
                self.notify(NoticeKind::Error, e);
            }
        }
    }

    pub fn run_command(&mut self, cmd: Command) {
        match cmd {
            Command::Search { targets, paths } => self.start_search(targets, paths),
            Command::Corpus { refresh } => {
                self.screen = Screen::Corpus;
                if refresh {
                    self.refresh_stats();
                }
            }
            Command::IndexBuild(out) => {
                self.screen = Screen::Corpus;
                self.build_index(out);
            }
            Command::IndexUse(dir) => self.use_index(dir),
            Command::IndexOff => {
                self.session.index = None;
                self.corpus.index_meta = None;
                self.persist_session();
                self.notify(NoticeKind::Info, "scanning corpus files again");
            }
            Command::SetPaths(paths) => {
                self.set_paths(paths);
                self.notify(NoticeKind::Ok, format!("corpus: {}", display_paths(&self.session.paths)));
            }
            Command::SetType(h) => {
                self.session.hint = h;
                self.notify(NoticeKind::Ok, format!("target type: {}", command::hint_name(h)));
            }
            Command::Export(path) => match (path, self.results.is_some()) {
                (_, false) => self.notify(NoticeKind::Error, "nothing to export yet; run a search first"),
                (Some(p), true) => self.export(&p),
                (None, true) => self.open_prompt(PromptKind::Export),
            },
            Command::Passwords(file) => match std::fs::read_to_string(&file) {
                Ok(content) => {
                    let list: Vec<String> = content
                        .lines()
                        .map(|l| l.trim_end_matches('\r'))
                        .filter(|l| !l.is_empty())
                        .map(str::to_string)
                        .collect();
                    self.notify(NoticeKind::Ok, format!("{} candidate passwords loaded", list.len()));
                    self.session.passwords = list;
                }
                Err(e) => self.notify(NoticeKind::Error, format!("cannot read {}: {e}", file.display())),
            },
            Command::Threads(n) => {
                self.session.threads = Some(n);
                self.notify(NoticeKind::Ok, format!("{n} worker threads"));
            }
            Command::MaxMatches(n) => {
                self.session.max_matches = n;
                let text = if n == 0 { "no limit on matches per target".to_string() } else { format!("at most {n} matches per target") };
                self.notify(NoticeKind::Ok, text);
            }
            Command::ClearHistory => {
                self.store.clear_recent();
                self.store.save();
                self.notify(NoticeKind::Ok, "recent searches cleared");
            }
            Command::Help => self.overlay = Overlay::Help,
            Command::Quit => self.quit_app(),
        }
    }

    pub fn set_paths(&mut self, paths: Vec<PathBuf>) {
        self.session.paths = paths;
        self.session.index = None;
        self.corpus.index_meta = None;
        self.corpus.stats = None;
        self.persist_session();
        self.refresh_stats_quietly();
    }

    // ---- searching -------------------------------------------------------------------------

    pub fn start_search(&mut self, raw_targets: Vec<String>, extra_paths: Vec<PathBuf>) {
        if !extra_paths.is_empty() {
            self.set_paths(extra_paths);
        }
        if self.session.paths.is_empty() && self.session.index.is_none() {
            self.notify(NoticeKind::Error, "no corpus yet: add @<dir> to your search or use /path <dir>");
            return;
        }
        let queries = match TargetParser::new(self.session.hint).parse_many(&raw_targets) {
            Ok(q) => q,
            Err(e) => {
                self.notify(NoticeKind::Error, e.to_string());
                return;
            }
        };
        if self.session.index.is_some() {
            if let Some(q) = queries.iter().find(|q| q.kind == QueryKind::Text) {
                self.notify(
                    NoticeKind::Error,
                    format!("{:?} is a text target; the index only covers emails, domains and IPs (/index off to scan files)", q.raw),
                );
                return;
            }
        }

        let kind_label = match queries.as_slice() {
            [one] => kind_label(one.kind).to_string(),
            many => format!("{} targets", many.len()),
        };
        let (live_tx, live_rx) = sync_channel(LIVE_QUEUE);
        let progress = ProgressHandle { live: Some(live_tx), ..ProgressHandle::default() };
        let job = self.next_job();
        let spec = SearchSpec {
            queries,
            paths: self.session.paths.clone(),
            index: self.session.index.clone(),
            recursive: self.session.recursive,
            threads: self.session.threads,
            max_matches: self.session.max_matches,
            passwords: self.session.passwords.clone(),
            progress: progress.clone(),
        };
        let via_index = spec.index.is_some();
        worker::spawn_search(job, spec, self.msg_tx.clone());

        self.persist_session();
        self.scan = Some(ScanRun {
            job,
            targets: raw_targets,
            kind_label,
            progress,
            live_rx,
            live: VecDeque::new(),
            hits_seen: 0,
            last_hit_tick: None,
            started: Instant::now(),
            via_index,
            phase: Phase::Running,
            summary: None,
        });
        self.results = None;
        self.screen = Screen::Scanning;
        self.set_mood(Mood::Prowl);
    }

    /// Stop the running scan and forget its answer. Files not started are skipped; a file already
    /// being read stops matching within a few hundred lines.
    pub fn cancel_scan(&mut self) {
        if let Some(scan) = &self.scan {
            if scan.running() {
                scan.progress.cancel.store(true, Ordering::Relaxed);
            }
        }
    }

    fn on_scan_key(&mut self, key: KeyEvent) {
        let Some(scan) = &self.scan else {
            self.screen = Screen::Home;
            return;
        };
        match &scan.phase {
            Phase::Running => {
                if matches!(key.code, KeyCode::Esc | KeyCode::Char('q')) {
                    self.cancel_scan();
                    self.scan = None;
                    self.screen = Screen::Home;
                    self.set_mood(Mood::Idle);
                    self.notify(NoticeKind::Info, "search cancelled");
                }
            }
            Phase::Finished { found, .. } => {
                if *found && self.results.is_some() && matches!(key.code, KeyCode::Enter | KeyCode::Char(' ') | KeyCode::Tab) {
                    self.screen = Screen::Results;
                    self.set_mood(Mood::Idle);
                } else if matches!(key.code, KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter | KeyCode::Char('n')) {
                    self.screen = Screen::Home;
                    self.set_mood(Mood::Idle);
                }
            }
            Phase::Failed(_) => {
                if matches!(key.code, KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter) {
                    self.screen = Screen::Home;
                    self.set_mood(Mood::Idle);
                }
            }
        }
    }

    // ---- results ---------------------------------------------------------------------------

    fn on_results_key(&mut self, key: KeyEvent) {
        let Some(view) = &mut self.results else {
            self.screen = Screen::Home;
            return;
        };

        if let Some(f) = &mut view.filtering {
            match key.code {
                KeyCode::Enter => {
                    view.filter = f.text().to_string();
                    view.filtering = None;
                    view.rebuild();
                }
                KeyCode::Esc => view.filtering = None,
                KeyCode::Left => f.left(),
                KeyCode::Right => f.right(),
                KeyCode::Backspace => f.backspace(),
                KeyCode::Char(c) if !is_ctrl(&key) => f.insert(c),
                _ => {}
            }
            // Filter as you type.
            if let Some(f) = &view.filtering {
                view.filter = f.text().to_string();
                view.rebuild();
            }
            return;
        }

        match key.code {
            KeyCode::Up | KeyCode::Char('k') => view.move_sel(-1),
            KeyCode::Down | KeyCode::Char('j') => view.move_sel(1),
            KeyCode::PageUp => view.move_sel(-10),
            KeyCode::PageDown => view.move_sel(10),
            KeyCode::Char('g') | KeyCode::Home => view.jump(false),
            KeyCode::Char('G') | KeyCode::End => view.jump(true),
            KeyCode::Tab | KeyCode::Right | KeyCode::Left | KeyCode::Char('h') | KeyCode::Char('l') => {
                view.focus = match view.focus {
                    Pane::Files => Pane::Lines,
                    Pane::Lines => Pane::Files,
                };
            }
            KeyCode::Char('/') => view.filtering = Some(Input::new(view.filter.clone())),
            KeyCode::Char('e') => self.open_prompt(PromptKind::Export),
            KeyCode::Char('y') => self.copy_selected(),
            KeyCode::Char('n') => self.back_home(),
            KeyCode::Esc | KeyCode::Char('q') => {
                if view.filter.is_empty() {
                    self.back_home();
                } else {
                    view.filter.clear();
                    view.rebuild();
                }
            }
            KeyCode::Char('?') => self.overlay = Overlay::Help,
            _ => {}
        }
    }

    fn back_home(&mut self) {
        self.screen = Screen::Home;
        self.set_mood(Mood::Idle);
    }

    fn copy_selected(&mut self) {
        let Some(text) = self.results.as_ref().and_then(|r| r.selected_hit()).map(|h| h.line_text.clone()) else {
            return;
        };
        crate::clipboard::copy(&text);
        self.notify(NoticeKind::Ok, "line sent to the clipboard (needs a terminal with OSC 52 support)");
    }

    pub fn export(&mut self, path: &Path) {
        let Some(view) = &self.results else {
            self.notify(NoticeKind::Error, "nothing to export yet");
            return;
        };
        let format = prowlcat_output::infer_format(path);
        // A search with more matches than the screen holds exports from the complete set.
        let written = match &view.full {
            Some(full) => prowlcat_output::write_output_iter(path, format, full.iter(), &view.summary).map(|()| full.len()),
            None => prowlcat_output::write_output(path, format, &view.all, &view.summary).map(|()| view.all.len()),
        };
        match written {
            Ok(n) => {
                self.notify(NoticeKind::Ok, format!("exported {n} matches to {}", path.display()));
            }
            Err(e) => self.notify(NoticeKind::Error, format!("export failed: {e:#}")),
        }
    }

    // ---- prompts ---------------------------------------------------------------------------

    pub fn open_prompt(&mut self, kind: PromptKind) {
        let initial = match kind {
            PromptKind::Export => "prowlcat-results.json".to_string(),
            PromptKind::IndexBuild => default_index_dir(&self.session.paths),
            PromptKind::IndexUse => self
                .corpus
                .last_built
                .as_ref()
                .map(|(d, _, _)| d.display().to_string())
                .or_else(|| self.session.index.as_ref().map(|d| d.display().to_string()))
                .unwrap_or_default(),
        };
        self.overlay = Overlay::Prompt(kind, Input::new(initial));
    }

    fn on_prompt_key(&mut self, key: KeyEvent) {
        let ctrl = is_ctrl(&key);
        let Overlay::Prompt(kind, input) = &mut self.overlay else { return };
        let kind = *kind;
        match key.code {
            KeyCode::Esc => self.overlay = Overlay::None,
            KeyCode::Enter => {
                let value = command::expand_home(input.text().trim());
                self.overlay = Overlay::None;
                if value.is_empty() {
                    return;
                }
                let path = PathBuf::from(value);
                match kind {
                    PromptKind::Export => self.export(&path),
                    PromptKind::IndexBuild => self.build_index(path),
                    PromptKind::IndexUse => self.use_index(path),
                }
            }
            KeyCode::Left => input.left(),
            KeyCode::Right => input.right(),
            KeyCode::Home => input.home(),
            KeyCode::End => input.end(),
            KeyCode::Backspace => input.backspace(),
            KeyCode::Delete => input.delete(),
            KeyCode::Char('u') if ctrl => input.kill_to_start(),
            KeyCode::Char('w') if ctrl => input.kill_word(),
            KeyCode::Char(c) if !ctrl => input.insert(c),
            _ => {}
        }
    }

    // ---- corpus and index ------------------------------------------------------------------

    pub fn corpus_actions(&self) -> Vec<CorpusAction> {
        let mut v = vec![CorpusAction::Refresh, CorpusAction::BuildIndex, CorpusAction::UseIndex];
        if self.session.index.is_some() {
            v.push(CorpusAction::StopUsingIndex);
        }
        v
    }

    fn on_corpus_key(&mut self, key: KeyEvent) {
        let actions = self.corpus_actions();
        let n = actions.len();
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.corpus.sel = (self.corpus.sel + n - 1) % n,
            KeyCode::Down | KeyCode::Char('j') => self.corpus.sel = (self.corpus.sel + 1) % n,
            KeyCode::Char('r') => self.refresh_stats(),
            KeyCode::Enter => match actions[self.corpus.sel.min(n - 1)] {
                CorpusAction::Refresh => self.refresh_stats(),
                CorpusAction::BuildIndex => {
                    if self.session.paths.is_empty() {
                        self.notify(NoticeKind::Error, "no corpus to index: use /path <dir> first");
                    } else {
                        self.open_prompt(PromptKind::IndexBuild);
                    }
                }
                CorpusAction::UseIndex => self.open_prompt(PromptKind::IndexUse),
                CorpusAction::StopUsingIndex => self.run_command(Command::IndexOff),
            },
            KeyCode::Esc | KeyCode::Char('q') => self.back_home(),
            KeyCode::Char('?') => self.overlay = Overlay::Help,
            _ => {}
        }
        self.corpus.sel = self.corpus.sel.min(self.corpus_actions().len() - 1);
    }

    pub fn refresh_stats(&mut self) {
        if self.session.paths.is_empty() {
            self.notify(NoticeKind::Error, "no corpus yet: use /path <dir>");
            return;
        }
        self.refresh_stats_quietly();
        self.notify(NoticeKind::Info, "counting files...");
    }

    fn refresh_stats_quietly(&mut self) {
        if self.session.paths.is_empty() {
            return;
        }
        let job = self.next_job();
        self.corpus.stats_job = Some(job);
        worker::spawn_stats(job, self.session.paths.clone(), self.session.recursive, self.msg_tx.clone());
    }

    pub fn build_index(&mut self, out: PathBuf) {
        if self.corpus.build.is_some() {
            self.notify(NoticeKind::Error, "an index is already being built");
            return;
        }
        if self.session.paths.is_empty() {
            self.notify(NoticeKind::Error, "no corpus to index: use /path <dir> first");
            return;
        }
        let job = self.next_job();
        worker::spawn_index_build(
            job,
            self.session.paths.clone(),
            self.session.recursive,
            self.session.threads,
            self.session.passwords.clone(),
            out.clone(),
            self.msg_tx.clone(),
        );
        self.corpus.build = Some(BuildRun { job, out, started: Instant::now() });
        self.set_mood(Mood::Prowl);
    }

    pub fn use_index(&mut self, dir: PathBuf) {
        match worker::index_meta(&dir) {
            Ok(meta) => {
                self.notify(NoticeKind::Ok, format!("searching through the index at {}", dir.display()));
                self.corpus.index_meta = Some(meta);
                self.session.index = Some(dir);
                self.persist_session();
            }
            Err(e) => self.notify(NoticeKind::Error, format!("{e:#}")),
        }
    }

    pub fn build_elapsed(&self) -> Option<Duration> {
        self.corpus.build.as_ref().map(|b| b.started.elapsed())
    }
}

/// Ctrl as a shortcut modifier. Windows (and some Linux layouts) report AltGr as Ctrl+Alt, so
/// that combination is text entry (`@`, `#`, `[`, `€` on an Italian keyboard), not a shortcut.
fn is_ctrl(key: &KeyEvent) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL) && !key.modifiers.contains(KeyModifiers::ALT)
}

pub fn kind_label(k: QueryKind) -> &'static str {
    match k {
        QueryKind::Email => "email",
        QueryKind::Domain => "domain",
        QueryKind::Ip => "ip",
        QueryKind::Cidr => "cidr",
        QueryKind::Text => "text",
    }
}

/// Comma-separated paths with the home directory shown as `~`.
pub fn display_paths(paths: &[PathBuf]) -> String {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from);
    paths
        .iter()
        .map(|p| match home.as_ref().and_then(|h| p.strip_prefix(h).ok()) {
            Some(rest) if rest.as_os_str().is_empty() => "~".to_string(),
            Some(rest) => format!("~/{}", rest.display().to_string().replace('\\', "/")),
            None => p.display().to_string(),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// `<corpus folder name>.idx` in the current directory.
fn default_index_dir(paths: &[PathBuf]) -> String {
    let name = paths
        .first()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .filter(|n| !n.is_empty() && !n.contains('*'))
        .unwrap_or("corpus");
    format!("{name}.idx")
}

/// Complete a partial path to the longest common prefix of its matches in the directory.
pub fn complete_path(partial: &str) -> Option<String> {
    let expanded = command::expand_home(partial);
    let (dir, name) = match expanded.rfind(['/', '\\']) {
        Some(i) => (&expanded[..=i], &expanded[i + 1..]),
        None => ("", expanded.as_str()),
    };
    let read = std::fs::read_dir(if dir.is_empty() { "." } else { dir }).ok()?;
    let mut matches: Vec<(String, bool)> = read
        .filter_map(Result::ok)
        .filter_map(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
            (n.starts_with(name) && (!n.starts_with('.') || name.starts_with('.'))).then_some((n, is_dir))
        })
        .collect();
    matches.sort();
    let first = matches.first()?.0.clone();
    let common = matches.iter().fold(first, |acc, (n, _)| {
        acc.chars().zip(n.chars()).take_while(|(a, b)| a == b).map(|(a, _)| a).collect()
    });
    let single_dir = matches.len() == 1 && matches[0].1;
    Some(format!("{dir}{common}{}", if single_dir { "/" } else { "" }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::ColorLevel;
    use prowlcat_types::{MatchKind, SourceLocation};
    use ratatui::crossterm::event::{KeyEventKind, KeyEventState};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent { code, modifiers: KeyModifiers::NONE, kind: KeyEventKind::Press, state: KeyEventState::NONE }
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent { code: KeyCode::Char(c), modifiers: KeyModifiers::CONTROL, kind: KeyEventKind::Press, state: KeyEventState::NONE }
    }

    fn app_with(opts: TuiOptions) -> App {
        App::with_parts(opts, Theme::new(ColorLevel::True, true), Store::at(None))
    }

    fn app() -> App {
        app_with(TuiOptions { splash: false, ..TuiOptions::default() })
    }

    fn type_text(app: &mut App, text: &str) {
        for c in text.chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
    }

    fn hit(path: &str, line: u64, text: &str) -> ScanResult {
        ScanResult {
            query: "acme.corp".into(),
            kind: QueryKind::Domain,
            match_kind: MatchKind::Exact,
            source: SourceLocation { path: path.into(), archive_entry: None, line_number: Some(line), byte_offset: 0 },
            line_text: text.into(),
            matched_span: (0, 4),
            extracted: Vec::new(),
        }
    }

    fn view() -> ResultsView {
        let all = vec![
            hit("/d/a.txt", 3, "admin@acme.corp:x"),
            hit("/d/b.txt", 1, "root@acme.corp:y"),
            hit("/d/b.txt", 0, "bob@acme.corp:z"),
            hit("/d/b.txt", 9, "kim@acme.corp:w"),
        ];
        ResultsView::new(all, ScanSummary { matches: 4, ..ScanSummary::default() }, "acme.corp".into())
    }

    fn wait_for(app: &mut App, mut done: impl FnMut(&App) -> bool) {
        let start = Instant::now();
        while !done(app) {
            assert!(start.elapsed() < Duration::from_secs(20), "timed out waiting for the worker");
            app.pump();
            app.on_tick();
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn results_group_by_file_most_hits_first() {
        let v = view();
        assert_eq!(v.view.len(), 2);
        assert_eq!(v.view[0].label, "d/b.txt");
        assert_eq!(v.view[0].hits.len(), 3);
        // Lines inside a group are in file order.
        let lines: Vec<_> = v.view[0].hits.iter().map(|&i| v.all[i].source.line_number.unwrap()).collect();
        assert_eq!(lines, vec![0, 1, 9]);
        assert_eq!(v.by_kind, vec![(QueryKind::Domain, 4)]);
    }

    #[test]
    fn filter_narrows_files_and_lines() {
        let mut v = view();
        v.filter = "ADMIN".into();
        v.rebuild();
        assert_eq!(v.view.len(), 1);
        assert_eq!(v.view[0].label, "d/a.txt");
        assert_eq!(v.visible_hits(), 1);

        v.filter = "b.txt".into();
        v.rebuild();
        assert_eq!(v.visible_hits(), 3, "a file name match keeps every line of that file");

        v.filter = "nothing like this".into();
        v.rebuild();
        assert!(v.view.is_empty());
        assert!(v.selected_hit().is_none(), "an empty view has no selection");
    }

    #[test]
    fn selection_stays_in_range() {
        let mut v = view();
        v.move_sel(-5);
        assert_eq!(v.file_sel, 0);
        v.move_sel(99);
        assert_eq!(v.file_sel, 1);
        v.focus = Pane::Lines;
        v.jump(true);
        assert_eq!(v.line_sel, 0, "the last file has a single line");
        v.focus = Pane::Files;
        v.jump(false);
        v.focus = Pane::Lines;
        v.jump(true);
        assert_eq!(v.line_sel, 2);
        assert_eq!(v.selected_hit().unwrap().source.line_number, Some(9));
    }

    #[test]
    fn file_labels_are_short() {
        assert_eq!(file_label(Path::new("/a/b/c/leak.txt"), None), "c/leak.txt");
        assert_eq!(file_label(Path::new("leak.txt"), None), "leak.txt");
        assert_eq!(file_label(Path::new("/x/dump.zip"), Some("in/a.txt")), "x/dump.zip → in/a.txt");
    }

    #[test]
    fn altgr_characters_are_typed_not_treated_as_shortcuts() {
        let altgr = |c| KeyEvent {
            code: KeyCode::Char(c),
            modifiers: KeyModifiers::CONTROL | KeyModifiers::ALT,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        };
        let mut a = app();
        for c in ['a', '@', 'e', '#', '[', 'u'] {
            a.on_key(altgr(c));
        }
        assert_eq!(a.input.text(), "a@e#[u", "AltGr+e is not ctrl+e, AltGr+u is not ctrl+u");
        assert!(!a.quit, "AltGr+c is not ctrl+c");
        a.on_key(altgr('c'));
        assert!(!a.quit);
        a.on_key(ctrl('u'));
        assert!(a.input.is_empty(), "plain ctrl+u still clears the line");
    }

    #[test]
    fn typing_builds_the_prompt_and_esc_clears_it() {
        let mut a = app();
        type_text(&mut a, "acme.corp");
        assert_eq!(a.input.text(), "acme.corp");
        assert_eq!(a.inferred_kind(), Some("domain"));
        a.on_key(key(KeyCode::Esc));
        assert!(a.input.is_empty());
    }

    #[test]
    fn question_mark_opens_help_only_on_an_empty_prompt() {
        let mut a = app();
        a.on_key(key(KeyCode::Char('?')));
        assert!(matches!(a.overlay, Overlay::Help));
        a.on_key(key(KeyCode::Char('x')));
        assert!(matches!(a.overlay, Overlay::None), "any key closes help");
        assert!(a.input.is_empty(), "and is consumed");
        type_text(&mut a, "why?");
        assert_eq!(a.input.text(), "why?");
    }

    #[test]
    fn ctrl_c_quits_from_anywhere() {
        for screen in [Screen::Home, Screen::Scanning, Screen::Results, Screen::Corpus] {
            let mut a = app();
            a.screen = screen;
            a.on_key(ctrl('c'));
            assert!(a.quit, "{screen:?}");
        }
    }

    #[test]
    fn searching_without_a_corpus_explains_what_to_do() {
        let mut a = app();
        type_text(&mut a, "acme.corp");
        a.on_key(key(KeyCode::Enter));
        assert_eq!(a.screen, Screen::Home);
        let n = a.notice.as_ref().expect("a notice");
        assert_eq!(n.kind, NoticeKind::Error);
        assert!(n.text.contains("/path"));
    }

    #[test]
    fn bad_targets_are_reported_and_kept_for_editing() {
        let mut a = app();
        a.session.paths = vec![PathBuf::from(".")];
        type_text(&mut a, "300.1.1.1/99");
        a.session.hint = TypeHint::Cidr;
        a.on_key(key(KeyCode::Enter));
        assert_eq!(a.screen, Screen::Home);
        assert_eq!(a.input.text(), "300.1.1.1/99", "the line stays so it can be fixed");
        assert_eq!(a.notice.as_ref().unwrap().kind, NoticeKind::Error);
    }

    #[test]
    fn slash_opens_the_palette_and_tab_completes() {
        let mut a = app();
        type_text(&mut a, "/");
        assert!(a.palette_open());
        assert_eq!(a.palette_items().len(), command::COMMANDS.len());
        type_text(&mut a, "ex");
        assert_eq!(a.palette_items()[0].0, "export");
        a.on_key(key(KeyCode::Tab));
        assert_eq!(a.input.text(), "/export ");
        assert!(!a.palette_open(), "a space closes the palette");
    }

    #[test]
    fn palette_arrows_wrap_around() {
        let mut a = app();
        type_text(&mut a, "/s");
        assert_eq!(a.palette_items().len(), 2);
        a.on_key(key(KeyCode::Down));
        assert_eq!(a.palette_sel, 1);
        a.on_key(key(KeyCode::Down));
        assert_eq!(a.palette_sel, 0);
        a.on_key(key(KeyCode::Up));
        assert_eq!(a.palette_sel, 1);
        a.on_key(key(KeyCode::Tab));
        assert_eq!(a.input.text(), "/stats ");
    }

    #[test]
    fn settings_commands_change_the_session() {
        let mut a = app();
        type_text(&mut a, "/type email");
        a.on_key(key(KeyCode::Enter));
        assert_eq!(a.session.hint, TypeHint::Email);
        type_text(&mut a, "/threads 3");
        a.on_key(key(KeyCode::Enter));
        assert_eq!(a.session.threads, Some(3));
        type_text(&mut a, "/max 5");
        a.on_key(key(KeyCode::Enter));
        assert_eq!(a.session.max_matches, 5);
        type_text(&mut a, "/path /somewhere /else");
        a.on_key(key(KeyCode::Enter));
        assert_eq!(a.session.paths, vec![PathBuf::from("/somewhere"), PathBuf::from("/else")]);
    }

    #[test]
    fn enter_on_a_partial_command_runs_the_highlighted_one() {
        let mut a = app();
        type_text(&mut a, "/he");
        a.on_key(key(KeyCode::Enter));
        assert!(matches!(a.overlay, Overlay::Help));
    }

    #[test]
    fn history_arrows_recall_recent_searches() {
        let mut store = Store::at(None);
        store.push("first.com", 1, 1);
        store.push("second.com", 1, 1);
        let mut a = App::with_parts(TuiOptions { splash: false, ..TuiOptions::default() }, Theme::new(ColorLevel::True, true), store);
        type_text(&mut a, "draft");
        a.on_key(key(KeyCode::Up));
        assert_eq!(a.input.text(), "second.com");
        a.on_key(key(KeyCode::Up));
        assert_eq!(a.input.text(), "first.com");
        a.on_key(key(KeyCode::Up));
        assert_eq!(a.input.text(), "first.com", "stops at the oldest");
        a.on_key(key(KeyCode::Down));
        a.on_key(key(KeyCode::Down));
        assert_eq!(a.input.text(), "draft", "coming back restores what you were typing");
    }

    #[test]
    fn splash_swallows_the_first_key() {
        let mut a = app_with(TuiOptions::default());
        assert!(a.splash_until.is_some());
        a.on_key(key(KeyCode::Char('x')));
        assert!(a.splash_until.is_none());
        assert!(a.input.is_empty());
        a.on_key(key(KeyCode::Char('x')));
        assert_eq!(a.input.text(), "x");
    }

    #[test]
    fn splash_ends_by_itself() {
        let mut a = app_with(TuiOptions::default());
        for _ in 0..SPLASH_TICKS {
            a.on_tick();
        }
        assert!(a.splash_until.is_none());
    }

    #[test]
    fn splash_is_skipped_without_animation_or_when_a_search_is_requested() {
        assert!(app_with(TuiOptions { animate: false, ..TuiOptions::default() }).splash_until.is_none());
    }

    #[test]
    fn notices_expire() {
        let mut a = app();
        a.notify(NoticeKind::Info, "hello");
        for _ in 0..40 {
            a.on_tick();
        }
        assert!(a.notice.is_none());
    }

    #[test]
    fn completes_paths_to_their_common_prefix() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("breaches-2021")).unwrap();
        std::fs::create_dir(dir.path().join("breaches-2022")).unwrap();
        std::fs::create_dir(dir.path().join("other")).unwrap();
        let base = format!("{}/", dir.path().display());
        assert_eq!(complete_path(&format!("{base}br")), Some(format!("{base}breaches-202")));
        assert_eq!(complete_path(&format!("{base}ot")), Some(format!("{base}other/")), "a lone directory gets a slash");
        assert_eq!(complete_path(&format!("{base}zzz")), None);
    }

    #[test]
    fn default_index_name_comes_from_the_corpus() {
        assert_eq!(default_index_dir(&[PathBuf::from("/data/breaches")]), "breaches.idx");
        assert_eq!(default_index_dir(&[]), "corpus.idx");
        assert_eq!(default_index_dir(&[PathBuf::from("/data/*.txt")]), "corpus.idx");
    }

    // ---- end to end through the real worker ----------------------------------------------------

    fn corpus() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "alice@acme.corp:pw1\nnoise\nbob@acme.corp:pw2\n").unwrap();
        std::fs::write(dir.path().join("b.txt"), "carol@acme.corp:pw3\n").unwrap();
        dir
    }

    #[test]
    fn a_search_runs_to_results_and_is_remembered() {
        let dir = corpus();
        let mut a = app_with(TuiOptions {
            splash: false,
            paths: vec![dir.path().to_path_buf()],
            threads: Some(2),
            ..TuiOptions::default()
        });
        type_text(&mut a, "acme.corp");
        a.on_key(key(KeyCode::Enter));
        assert_eq!(a.screen, Screen::Scanning);
        assert_eq!(a.mood, Mood::Prowl);

        wait_for(&mut a, |a| !a.scan.as_ref().unwrap().running());
        assert_eq!(a.mood, Mood::Found);
        let scan = a.scan.as_ref().unwrap();
        assert!(matches!(scan.phase, Phase::Finished { found: true, .. }));
        assert_eq!(scan.summary.as_ref().unwrap().matches, 3);
        assert_eq!(a.results.as_ref().unwrap().all.len(), 3);
        assert_eq!(a.store.saved.recent[0].target, "acme.corp");
        assert_eq!(a.store.saved.recent[0].hits, 3);

        // Fang gets a moment to pounce, then the results open by themselves.
        wait_for(&mut a, |a| a.screen == Screen::Results);
        assert_eq!(a.mood, Mood::Idle);
    }

    #[test]
    fn live_hits_reach_the_scan_view() {
        let dir = corpus();
        let mut a = app_with(TuiOptions { splash: false, paths: vec![dir.path().to_path_buf()], threads: Some(1), ..TuiOptions::default() });
        a.start_search(vec!["acme.corp".into()], Vec::new());
        wait_for(&mut a, |a| !a.scan.as_ref().unwrap().running());
        a.on_tick();
        let scan = a.scan.as_ref().unwrap();
        assert_eq!(scan.hits_seen, 3);
        assert_eq!(scan.live.len(), 3);
        assert!(scan.last_hit_tick.is_some());
    }

    #[test]
    fn a_search_with_no_matches_makes_fang_sulk() {
        let dir = corpus();
        let mut a = app_with(TuiOptions { splash: false, paths: vec![dir.path().to_path_buf()], threads: Some(1), ..TuiOptions::default() });
        a.start_search(vec!["nobody.invalid".into()], Vec::new());
        wait_for(&mut a, |a| !a.scan.as_ref().unwrap().running());
        assert_eq!(a.mood, Mood::Miss);
        assert!(a.results.is_none());
        for _ in 0..40 {
            a.on_tick();
        }
        assert_eq!(a.screen, Screen::Scanning, "no results screen to go to; wait for a key");
        a.on_key(key(KeyCode::Enter));
        assert_eq!(a.screen, Screen::Home);
        assert_eq!(a.store.saved.recent[0].hits, 0);
    }

    #[test]
    fn a_failing_search_is_shown_not_swallowed() {
        let mut a = app_with(TuiOptions { splash: false, paths: vec![PathBuf::from("/definitely/not/here")], ..TuiOptions::default() });
        a.start_search(vec!["a@b.com".into()], Vec::new());
        wait_for(&mut a, |a| !a.scan.as_ref().unwrap().running());
        assert!(matches!(a.scan.as_ref().unwrap().phase, Phase::Failed(_)));
        assert_eq!(a.mood, Mood::Miss);
        a.on_key(key(KeyCode::Esc));
        assert_eq!(a.screen, Screen::Home);
    }

    #[test]
    fn escape_cancels_a_running_scan() {
        let dir = corpus();
        let mut a = app_with(TuiOptions { splash: false, paths: vec![dir.path().to_path_buf()], ..TuiOptions::default() });
        a.start_search(vec!["acme.corp".into()], Vec::new());
        let progress = a.scan.as_ref().unwrap().progress.clone();
        a.on_key(key(KeyCode::Esc));
        assert_eq!(a.screen, Screen::Home);
        assert!(a.scan.is_none());
        assert!(progress.cancel.load(Ordering::Relaxed), "the worker was told to stop");
        // The late answer from the cancelled job must not resurrect anything.
        std::thread::sleep(Duration::from_millis(300));
        a.pump();
        assert!(a.results.is_none());
        assert_eq!(a.screen, Screen::Home);
    }

    #[test]
    fn at_paths_in_the_search_set_the_corpus() {
        let dir = corpus();
        let mut a = app();
        type_text(&mut a, &format!("acme.corp @{}", dir.path().display()));
        a.on_key(key(KeyCode::Enter));
        assert_eq!(a.session.paths, vec![dir.path().to_path_buf()]);
        wait_for(&mut a, |a| !a.scan.as_ref().unwrap().running());
        assert_eq!(a.scan.as_ref().unwrap().summary.as_ref().unwrap().matches, 3);
    }

    #[test]
    fn start_search_option_runs_immediately() {
        let dir = corpus();
        let a = app_with(TuiOptions {
            start: Start::Search,
            targets: vec!["acme.corp".into()],
            paths: vec![dir.path().to_path_buf()],
            ..TuiOptions::default()
        });
        assert_eq!(a.screen, Screen::Scanning);
        assert!(a.splash_until.is_none());
    }

    #[test]
    fn export_writes_the_chosen_format() {
        let dir = corpus();
        let mut a = app_with(TuiOptions { splash: false, paths: vec![dir.path().to_path_buf()], threads: Some(1), ..TuiOptions::default() });
        a.start_search(vec!["acme.corp".into()], Vec::new());
        wait_for(&mut a, |a| a.screen == Screen::Results);

        let out = tempfile::tempdir().unwrap();
        let csv = out.path().join("hits.csv");
        a.run_command(Command::Export(Some(csv.clone())));
        assert_eq!(a.notice.as_ref().unwrap().kind, NoticeKind::Ok);
        let text = std::fs::read_to_string(&csv).unwrap();
        assert_eq!(text.lines().count(), 4, "header and three matches");

        let json = out.path().join("hits.json");
        a.on_key(key(KeyCode::Char('e')));
        assert!(matches!(a.overlay, Overlay::Prompt(PromptKind::Export, _)));
        a.overlay = Overlay::Prompt(PromptKind::Export, Input::new(json.display().to_string()));
        a.on_key(key(KeyCode::Enter));
        let parsed: serde_json::Value = serde_json::from_slice(&std::fs::read(&json).unwrap()).unwrap();
        assert_eq!(parsed["results"].as_array().map(Vec::len).or_else(|| parsed.as_array().map(Vec::len)), Some(3));
    }

    #[test]
    fn exporting_before_any_search_is_refused() {
        let mut a = app();
        a.run_command(Command::Export(None));
        assert_eq!(a.notice.as_ref().unwrap().kind, NoticeKind::Error);
        assert!(matches!(a.overlay, Overlay::None));
    }

    #[test]
    fn results_keys_navigate_filter_and_leave() {
        let mut a = app();
        a.screen = Screen::Results;
        a.results = Some(view());
        a.on_key(key(KeyCode::Down));
        assert_eq!(a.results.as_ref().unwrap().file_sel, 1);
        a.on_key(key(KeyCode::Tab));
        assert_eq!(a.results.as_ref().unwrap().focus, Pane::Lines);

        a.on_key(key(KeyCode::Char('/')));
        type_text(&mut a, "admin");
        assert_eq!(a.results.as_ref().unwrap().visible_hits(), 1, "filters as you type");
        a.on_key(key(KeyCode::Enter));
        assert!(a.results.as_ref().unwrap().filtering.is_none());
        assert_eq!(a.results.as_ref().unwrap().filter, "admin");

        a.on_key(key(KeyCode::Esc));
        assert_eq!(a.results.as_ref().unwrap().filter, "", "first Esc clears the filter");
        assert_eq!(a.screen, Screen::Results);
        a.on_key(key(KeyCode::Esc));
        assert_eq!(a.screen, Screen::Home);
    }

    #[test]
    fn corpus_screen_builds_and_uses_an_index() {
        let dir = corpus();
        let out = tempfile::tempdir().unwrap().keep().join("idx");
        let mut a = app_with(TuiOptions { splash: false, paths: vec![dir.path().to_path_buf()], threads: Some(1), ..TuiOptions::default() });
        a.run_command(Command::IndexBuild(out.clone()));
        assert_eq!(a.screen, Screen::Corpus);
        assert_eq!(a.mood, Mood::Prowl);
        assert!(a.corpus.build.is_some());
        wait_for(&mut a, |a| a.corpus.build.is_none());
        assert_eq!(a.mood, Mood::Found);
        assert_eq!(a.corpus.last_built.as_ref().unwrap().1.docs, 3);

        a.run_command(Command::IndexUse(out));
        assert!(a.index_in_use());
        assert_eq!(a.corpus.index_meta.as_ref().unwrap().docs, 3);
        assert!(a.corpus_actions().contains(&CorpusAction::StopUsingIndex));

        // A text target cannot use the index.
        a.session.hint = TypeHint::Text;
        a.start_search(vec!["Winter".into()], Vec::new());
        assert_eq!(a.notice.as_ref().unwrap().kind, NoticeKind::Error);

        a.session.hint = TypeHint::Auto;
        a.start_search(vec!["acme.corp".into()], Vec::new());
        wait_for(&mut a, |a| !a.scan.as_ref().unwrap().running());
        assert!(a.scan.as_ref().unwrap().via_index);
        assert_eq!(a.scan.as_ref().unwrap().summary.as_ref().unwrap().matches, 3);
    }

    #[test]
    fn stats_are_computed_in_the_background() {
        let dir = corpus();
        let mut a = app_with(TuiOptions { splash: false, paths: vec![dir.path().to_path_buf()], ..TuiOptions::default() });
        wait_for(&mut a, |a| a.corpus.stats.is_some());
        assert_eq!(a.corpus.stats.as_ref().unwrap().files, 2);
    }

    #[test]
    fn using_a_bad_index_keeps_the_old_setting() {
        let mut a = app();
        a.use_index(PathBuf::from("/no/such/index"));
        assert!(!a.index_in_use());
        assert_eq!(a.notice.as_ref().unwrap().kind, NoticeKind::Error);
    }

    #[test]
    fn saved_corpus_is_restored_unless_overridden() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("state.json");
        let mut store = Store::at(Some(file.clone()));
        store.saved.corpus = vec![PathBuf::from("/saved")];
        store.save();

        let restored = App::with_parts(TuiOptions { splash: false, ..TuiOptions::default() }, Theme::new(ColorLevel::True, true), Store::at(Some(file.clone())));
        assert_eq!(restored.session.paths, vec![PathBuf::from("/saved")]);

        let explicit = App::with_parts(
            TuiOptions { splash: false, paths: vec![PathBuf::from("/given")], ..TuiOptions::default() },
            Theme::new(ColorLevel::True, true),
            Store::at(Some(file)),
        );
        assert_eq!(explicit.session.paths, vec![PathBuf::from("/given")]);
    }

    #[test]
    fn quitting_saves_the_session() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("state.json");
        let mut a = App::with_parts(
            TuiOptions { splash: false, paths: vec![PathBuf::from("/kept")], ..TuiOptions::default() },
            Theme::new(ColorLevel::True, true),
            Store::at(Some(file.clone())),
        );
        a.run_command(Command::Quit);
        assert!(a.quit);
        assert_eq!(Store::at(Some(file)).saved.corpus, vec![PathBuf::from("/kept")]);
    }

    #[test]
    fn display_paths_abbreviates_home() {
        std::env::set_var("HOME", "/home/me");
        assert_eq!(display_paths(&[PathBuf::from("/home/me/breaches")]), "~/breaches");
        assert_eq!(display_paths(&[PathBuf::from("/data/x"), PathBuf::from("/home/me")]), "/data/x, ~");
    }
}
