//! ProwlCat's interactive terminal UI: the Night Eyes theme and Fang, the animated mascot.
//!
//! The command line stays the scripting interface. This crate is only started for people:
//! `prowlcat` on its own in a terminal, or `prowlcat tui` / `--ui tui` explicitly.

mod app;
mod clipboard;
mod command;
mod history;
mod input;
mod mascot;
mod mascot_data;
mod options;
mod theme;
mod ui;
mod worker;

use std::io::{self, IsTerminal, Write};
use std::time::Instant;

use anyhow::{bail, Context, Result};
use ratatui::crossterm::event::{self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyEventKind};
use ratatui::crossterm::execute;
use ratatui::DefaultTerminal;

pub use options::{Start, TuiOptions};

use app::{App, TICK};

/// Whether both ends of the terminal are a person's terminal, not a pipe or a file.
pub fn is_interactive() -> bool {
    io::stdin().is_terminal() && io::stdout().is_terminal()
}

/// Open the interface and run it until the user quits. The terminal is restored on every exit,
/// including panics.
pub fn run(opts: TuiOptions) -> Result<()> {
    if !is_interactive() {
        bail!("the interactive interface needs a terminal; use `--ui cli` (or a subcommand) for scripts and pipes");
    }
    let app = App::new(opts);
    let mut terminal = ratatui::try_init().context("could not take over the terminal")?;
    // Paste arrives as one event, so a pasted target is not typed key by key.
    let _ = execute!(io::stdout(), EnableBracketedPaste);
    let result = event_loop(&mut terminal, app);
    let _ = execute!(io::stdout(), DisableBracketedPaste);
    ratatui::restore();
    let _ = io::stdout().flush();
    result.context("terminal error")
}

fn event_loop(terminal: &mut DefaultTerminal, mut app: App) -> io::Result<()> {
    let mut next_tick = Instant::now() + TICK;
    loop {
        terminal.draw(|frame| ui::draw(frame, &app))?;
        if app.quit {
            return Ok(());
        }

        let wait = next_tick.saturating_duration_since(Instant::now());
        if event::poll(wait)? {
            match event::read()? {
                // Windows reports key releases too; only act on presses and repeats.
                Event::Key(key) if key.kind != KeyEventKind::Release => app.on_key(key),
                Event::Paste(text) => app.on_paste(&text),
                _ => {}
            }
        }
        app.pump();

        let now = Instant::now();
        if now >= next_tick {
            app.on_tick();
            next_tick += TICK;
            // After a long stall (laptop sleep, a slow terminal) skip ahead instead of racing.
            if next_tick < now {
                next_tick = now + TICK;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    //! Draw every screen into an in-memory terminal and look at what ends up on it.

    use std::path::PathBuf;

    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventState, KeyModifiers};
    use ratatui::Terminal;

    use super::*;
    use crate::app::{Screen, ScanRun};
    use crate::history::Store;
    use crate::theme::{ColorLevel, Theme};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent { code, modifiers: KeyModifiers::NONE, kind: KeyEventKind::Press, state: KeyEventState::NONE }
    }

    fn text(buf: &Buffer) -> String {
        let area = buf.area;
        (0..area.height)
            .map(|y| (0..area.width).map(|x| buf[(x, y)].symbol()).collect::<String>().trim_end().to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn render(app: &App, w: u16, h: u16) -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal.draw(|f| ui::draw(f, app)).unwrap();
        terminal.backend().buffer().clone()
    }

    fn corpus() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("crm-dump.txt"),
            "admin@acme.corp:Winter2024!\nnoise line\nj.rossi@acme.corp:hunter2\nsupport@acme.corp\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("other.txt"), "billing@acme.corp:pw\n").unwrap();
        dir
    }

    fn app_in(dir: &std::path::Path, level: ColorLevel, unicode: bool) -> App {
        let opts = TuiOptions { splash: false, paths: vec![dir.to_path_buf()], threads: Some(2), ..TuiOptions::default() };
        App::with_parts(opts, Theme::new(level, unicode), Store::at(None))
    }

    fn settle(app: &mut App, mut done: impl FnMut(&App) -> bool) {
        let start = Instant::now();
        while !done(app) {
            assert!(start.elapsed().as_secs() < 20, "timed out");
            app.pump();
            app.on_tick();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    #[test]
    fn home_shows_fang_the_corpus_and_the_prompt() {
        let dir = corpus();
        let app = app_in(dir.path(), ColorLevel::True, true);
        let screen = text(&render(&app, 100, 36));
        assert!(screen.contains("PROWLCAT"), "{screen}");
        assert!(screen.contains("local-first breach corpus search"));
        assert!(screen.contains("Fang is watching the corpus."));
        assert!(screen.contains("Try one of these"));
        assert!(screen.contains("▌ SEARCH"));
        assert!(screen.contains("❯"));
        assert!(screen.contains("▀"), "half-block pixels of the mascot");
        assert!(screen.contains("commands"));
    }

    #[test]
    fn home_without_a_corpus_says_how_to_add_one() {
        let opts = TuiOptions { splash: false, ..TuiOptions::default() };
        let app = App::with_parts(opts, Theme::new(ColorLevel::True, true), Store::at(None));
        let screen = text(&render(&app, 100, 36));
        assert!(screen.contains("none yet"), "{screen}");
        assert!(screen.contains("no corpus"));
    }

    #[test]
    fn fang_uses_the_amber_palette() {
        let dir = corpus();
        let app = app_in(dir.path(), ColorLevel::True, true);
        let buf = render(&app, 100, 36);
        let mut amber_cells = 0;
        let mut body_cells = 0;
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                let cell = &buf[(x, y)];
                if cell.symbol() == "▀" {
                    for c in [cell.fg, cell.bg] {
                        if matches!(c, ratatui::style::Color::Rgb(0xC9, 0x97, 0x2B)) {
                            amber_cells += 1;
                        }
                        if matches!(c, ratatui::style::Color::Rgb(0x2A, 0x2F, 0x3B)) {
                            body_cells += 1;
                        }
                    }
                }
            }
        }
        assert!(amber_cells >= 4, "two eyes in resting amber, found {amber_cells}");
        assert!(body_cells > 100, "graphite fur, found {body_cells}");
        assert!(matches!(buf[(0, 0)].bg, ratatui::style::Color::Rgb(0x0D, 0x0E, 0x12)), "night background");
    }

    #[test]
    fn palette_lists_commands_under_the_slash() {
        let dir = corpus();
        let mut app = app_in(dir.path(), ColorLevel::True, true);
        app.on_key(key(KeyCode::Char('/')));
        let screen = text(&render(&app, 100, 36));
        assert!(screen.contains("Commands"), "{screen}");
        assert!(screen.contains("/search"));
        assert!(screen.contains("/index"));
        assert!(screen.contains("/export"));
        app.on_key(key(KeyCode::Char('e')));
        let screen = text(&render(&app, 100, 36));
        assert!(screen.contains("/export") && !screen.contains("/search"), "narrowed to matches");
    }

    #[test]
    fn help_overlay_mentions_the_scripting_mode() {
        let dir = corpus();
        let mut app = app_in(dir.path(), ColorLevel::True, true);
        app.on_key(key(KeyCode::Char('?')));
        let screen = text(&render(&app, 100, 40));
        assert!(screen.contains("cheat sheet"), "{screen}");
        assert!(screen.contains("--ui cli"));
    }

    #[test]
    fn a_whole_search_is_drawn_from_scan_to_results() {
        let dir = corpus();
        let mut app = app_in(dir.path(), ColorLevel::True, true);
        for c in "acme.corp".chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
        app.on_key(key(KeyCode::Enter));
        let early = text(&render(&app, 110, 40));
        assert!(early.contains("▌ SCANNING") || early.contains("▌ RESULTS"), "{early}");
        assert!(early.contains("acme.corp"));

        settle(&mut app, |a| !a.scan.as_ref().is_some_and(ScanRun::running));
        let done = text(&render(&app, 110, 40));
        assert!(done.contains("Found"), "{done}");
        assert!(done.contains("HITS"));
        assert!(done.contains("live matches"));
        assert!(done.contains("admin@acme.corp:Winter2024!"), "live line with its text");
        assert!(done.contains("open results"));

        settle(&mut app, |a| a.screen == Screen::Results);
        let results = text(&render(&app, 110, 40));
        assert!(results.contains("▌ RESULTS"), "{results}");
        assert!(results.contains("FILES"));
        assert!(results.contains("crm-dump.txt"));
        assert!(results.contains("4 hits"), "{results}");
        assert!(results.contains("MATCH"));
        assert!(results.contains("BY TYPE"));
        assert!(results.contains("domain"));
    }

    #[test]
    fn matches_are_highlighted_in_amber() {
        let dir = corpus();
        let mut app = app_in(dir.path(), ColorLevel::True, true);
        app.start_search(vec!["acme.corp".into()], Vec::new());
        settle(&mut app, |a| a.screen == Screen::Results);
        let buf = render(&app, 110, 40);
        let mut highlighted = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                let c = &buf[(x, y)];
                if matches!(c.fg, ratatui::style::Color::Rgb(0xF7, 0xB7, 0x31)) && c.modifier.contains(ratatui::style::Modifier::BOLD) {
                    highlighted.push_str(c.symbol());
                }
            }
        }
        assert!(highlighted.contains("acme.corp"), "got {highlighted:?}");
    }

    #[test]
    fn no_matches_screen_explains_itself() {
        let dir = corpus();
        let mut app = app_in(dir.path(), ColorLevel::True, true);
        app.start_search(vec!["nobody.invalid".into()], Vec::new());
        settle(&mut app, |a| !a.scan.as_ref().is_some_and(ScanRun::running));
        let screen = text(&render(&app, 100, 36));
        assert!(screen.contains("No matches"), "{screen}");
        assert!(screen.contains("▌ NO MATCH"));
        assert!(screen.contains("no matches for nobody.invalid"));
    }

    #[test]
    fn a_failed_search_shows_the_error() {
        let opts = TuiOptions { splash: false, paths: vec![PathBuf::from("/definitely/not/here")], ..TuiOptions::default() };
        let mut app = App::with_parts(opts, Theme::new(ColorLevel::True, true), Store::at(None));
        app.start_search(vec!["a@b.com".into()], Vec::new());
        settle(&mut app, |a| !a.scan.as_ref().is_some_and(ScanRun::running));
        let screen = text(&render(&app, 100, 36));
        assert!(screen.contains("Search failed"), "{screen}");
        assert!(screen.contains("▌ FAILED"));
    }

    #[test]
    fn a_stuck_progress_bar_is_replaced_by_an_honest_message() {
        use std::sync::atomic::Ordering;
        let dir = corpus();
        let mut app = app_in(dir.path(), ColorLevel::True, true);
        app.start_search(vec!["acme.corp".into()], Vec::new());
        settle(&mut app, |a| !a.scan.as_ref().is_some_and(ScanRun::running));
        // Pretend a multi-gigabyte file has been read for three seconds with nothing reported yet.
        let scan = app.scan.as_mut().unwrap();
        scan.phase = app::Phase::Running;
        scan.summary = None;
        scan.started = Instant::now() - std::time::Duration::from_secs(3);
        scan.progress.total.store(4 << 30, Ordering::Relaxed);
        scan.progress.done.store(0, Ordering::Relaxed);
        let screen = text(&render(&app, 110, 40));
        assert!(screen.contains("progress shows when a file ends"), "{screen}");
        assert!(screen.contains("reading 4.00 GB"));
        assert!(!screen.contains("  0%"), "no misleading 0%");

        // Once bytes are reported the real bar comes back.
        app.scan.as_ref().unwrap().progress.done.store(1 << 30, Ordering::Relaxed);
        let screen = text(&render(&app, 110, 40));
        assert!(screen.contains(" 25%"), "{screen}");
        assert!(!screen.contains("progress shows when a file ends"));
    }

    #[test]
    fn corpus_screen_shows_stats_and_actions() {
        let dir = corpus();
        let mut app = app_in(dir.path(), ColorLevel::True, true);
        settle(&mut app, |a| a.corpus.stats.is_some());
        app.run_command(command::Command::Corpus { refresh: false });
        let screen = text(&render(&app, 110, 36));
        assert!(screen.contains("CORPUS") && screen.contains("INDEX"), "{screen}");
        assert!(screen.contains("files"));
        assert!(screen.contains("Build index"));
        assert!(screen.contains("Use an existing index"));
        assert!(screen.contains("not in use"));
        assert!(!screen.contains("Stop using"));
    }

    #[test]
    fn export_prompt_is_drawn_over_the_results() {
        let dir = corpus();
        let mut app = app_in(dir.path(), ColorLevel::True, true);
        app.start_search(vec!["acme.corp".into()], Vec::new());
        settle(&mut app, |a| a.screen == Screen::Results);
        app.on_key(key(KeyCode::Char('e')));
        let screen = text(&render(&app, 110, 36));
        assert!(screen.contains("Export results to"), "{screen}");
        assert!(screen.contains("prowlcat-results.json"));
    }

    #[test]
    fn splash_draws_the_big_mascot_then_gives_way() {
        let dir = corpus();
        let opts = TuiOptions { paths: vec![dir.path().to_path_buf()], ..TuiOptions::default() };
        let mut app = App::with_parts(opts, Theme::new(ColorLevel::True, true), Store::at(None));
        let early = text(&render(&app, 100, 36));
        assert!(early.contains("▀") && !early.contains("Try one of these"));
        for _ in 0..16 {
            app.on_tick();
        }
        let later = text(&render(&app, 100, 36));
        assert!(later.contains("P R O W L C A T"), "{later}");
        assert!(later.contains("local-first breach corpus search"));
        for _ in 0..10 {
            app.on_tick();
        }
        assert!(text(&render(&app, 100, 36)).contains("Try one of these"));
    }

    #[test]
    fn splash_is_skipped_on_small_terminals() {
        let dir = corpus();
        let opts = TuiOptions { paths: vec![dir.path().to_path_buf()], ..TuiOptions::default() };
        let app = App::with_parts(opts, Theme::new(ColorLevel::True, true), Store::at(None));
        assert!(text(&render(&app, 80, 24)).contains("▌ SEARCH"), "falls straight through to the home screen");
    }

    #[test]
    fn tiny_terminals_get_a_polite_message() {
        let dir = corpus();
        let app = app_in(dir.path(), ColorLevel::True, true);
        let screen = text(&render(&app, 30, 8));
        assert!(screen.contains("needs more room"), "{screen}");
        assert!(screen.contains("have 30 x 8"));
    }

    #[test]
    fn short_terminals_swap_the_mascot_for_eyes() {
        let dir = corpus();
        let app = app_in(dir.path(), ColorLevel::True, true);
        let screen = text(&render(&app, 90, 18));
        assert!(screen.contains("PROWLCAT"), "{screen}");
        assert!(screen.contains("◢ ◣") || screen.contains("▬ ▬"));
        assert!(screen.contains("▌ SEARCH"));
    }

    #[test]
    fn no_colour_mode_uses_no_colours_at_all() {
        let dir = corpus();
        let mut app = app_in(dir.path(), ColorLevel::None, true);
        app.start_search(vec!["acme.corp".into()], Vec::new());
        settle(&mut app, |a| a.screen == Screen::Results);
        for screen in [Screen::Home, Screen::Scanning, Screen::Results, Screen::Corpus] {
            app.screen = screen;
            let buf = render(&app, 100, 36);
            for y in 0..buf.area.height {
                for x in 0..buf.area.width {
                    let c = &buf[(x, y)];
                    assert!(
                        matches!(c.fg, ratatui::style::Color::Reset) && matches!(c.bg, ratatui::style::Color::Reset),
                        "{screen:?} has colour {:?}/{:?} at {x},{y}",
                        c.fg,
                        c.bg
                    );
                }
            }
        }
        app.screen = Screen::Home;
        assert!(text(&render(&app, 100, 36)).contains('◆'), "Fang's eyes survive without colour");
    }

    #[test]
    fn ascii_mode_has_no_box_drawing_or_blocks() {
        let dir = corpus();
        let mut app = app_in(dir.path(), ColorLevel::Ansi16, false);
        app.start_search(vec!["acme.corp".into()], Vec::new());
        settle(&mut app, |a| a.screen == Screen::Results);
        for screen in [Screen::Home, Screen::Scanning, Screen::Results, Screen::Corpus] {
            app.screen = screen;
            let shown = text(&render(&app, 100, 36));
            if let Some(bad) = shown.chars().find(|c| !c.is_ascii()) {
                panic!("{screen:?} drew non-ASCII {bad:?}:\n{shown}");
            }
        }
    }

    #[test]
    fn colour_levels_are_all_honoured() {
        let dir = corpus();
        for (level, check) in [
            (ColorLevel::True, (|c: ratatui::style::Color| matches!(c, ratatui::style::Color::Rgb(..))) as fn(_) -> bool),
            (ColorLevel::Ansi256, |c| matches!(c, ratatui::style::Color::Indexed(_) | ratatui::style::Color::Reset)),
            (ColorLevel::Ansi16, |c| !matches!(c, ratatui::style::Color::Rgb(..) | ratatui::style::Color::Indexed(_))),
        ] {
            let app = app_in(dir.path(), level, true);
            let buf = render(&app, 100, 36);
            for y in 0..buf.area.height {
                for x in 0..buf.area.width {
                    let c = &buf[(x, y)];
                    assert!(check(c.fg) && check(c.bg), "{level:?} leaked {:?}/{:?}", c.fg, c.bg);
                }
            }
        }
    }

    #[test]
    fn every_screen_survives_every_terminal_size() {
        let dir = corpus();
        let mut app = app_in(dir.path(), ColorLevel::True, true);
        app.start_search(vec!["acme.corp".into()], Vec::new());
        settle(&mut app, |a| a.screen == Screen::Results);
        app.on_key(key(KeyCode::Tab));
        for screen in [Screen::Home, Screen::Scanning, Screen::Results, Screen::Corpus] {
            app.screen = screen;
            for overlay in 0..3 {
                app.overlay = match overlay {
                    1 => app::Overlay::Help,
                    2 => app::Overlay::Prompt(app::PromptKind::Export, input::Input::new("some/very/long/path/".repeat(8))),
                    _ => app::Overlay::None,
                };
                for h in (1..=45).step_by(2) {
                    for w in (1..=130).step_by(3) {
                        let _ = render(&app, w, h);
                    }
                }
            }
        }
    }

    #[test]
    fn long_input_and_odd_text_do_not_break_the_prompt() {
        let dir = corpus();
        let mut app = app_in(dir.path(), ColorLevel::True, true);
        app.overlay = app::Overlay::None;
        for c in "ünïcödé-täŕgêt-".repeat(20).chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
        let screen = text(&render(&app, 80, 30));
        assert!(screen.contains('❯'));
        app.on_key(key(KeyCode::Home));
        let _ = render(&app, 80, 30);
    }

    #[test]
    fn a_hostile_line_cannot_inject_escape_sequences() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("evil.txt"), "bob@acme.corp:\x1b]52;c;ZXZpbA==\x07\x1b[2Jpwned\n").unwrap();
        let mut app = app_in(dir.path(), ColorLevel::True, true);
        app.start_search(vec!["acme.corp".into()], Vec::new());
        settle(&mut app, |a| a.screen == Screen::Results);
        for screen in [Screen::Scanning, Screen::Results] {
            app.screen = screen;
            let buf = render(&app, 110, 36);
            for y in 0..buf.area.height {
                for x in 0..buf.area.width {
                    assert!(!buf[(x, y)].symbol().chars().any(char::is_control), "control character reached the screen");
                }
            }
        }
    }

    #[test]
    fn mascot_actually_moves_between_ticks() {
        let dir = corpus();
        let mut app = app_in(dir.path(), ColorLevel::True, true);
        app.set_mood(mascot::Mood::Prowl);
        let a = render(&app, 100, 36);
        for _ in 0..3 {
            app.on_tick();
        }
        let b = render(&app, 100, 36);
        assert_ne!(a, b, "the mascot is animated (its eyes change colour)");
        // ...and a frozen one is not.
        let opts = TuiOptions { splash: false, animate: false, paths: vec![dir.path().to_path_buf()], ..TuiOptions::default() };
        let mut still = App::with_parts(opts, Theme::new(ColorLevel::True, true), Store::at(None));
        still.set_mood(mascot::Mood::Prowl);
        let a = render(&still, 100, 36);
        for _ in 0..7 {
            still.on_tick();
        }
        let b = render(&still, 100, 36);
        let diff = (0..a.area.height).any(|y| (0..a.area.width).any(|x| a[(x, y)] != b[(x, y)]));
        assert!(!diff, "--no-animation holds a still frame");
    }
}
