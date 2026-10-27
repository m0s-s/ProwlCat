//! A search in progress: Fang prowls, the bar fills, hits stream in underneath.

use std::sync::atomic::Ordering;

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Padding, Paragraph, Wrap};
use ratatui::Frame;

use crate::app::{display_paths, file_label, App, Phase, ScanRun};
use crate::mascot::{self, Size};

use super::{
    bar, claws, highlighted, hint_line, human_bytes, human_secs, inset, key, rounded, section, state_dot,
    status_bar, take_top, thousands,
};

const HEADER_FULL: u16 = 12;
const HEADER_SMALL: u16 = 5;
const CARDS: u16 = 4;

pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    let Some(scan) = &app.scan else { return };
    let [content, status] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(area);

    let mode = match &scan.phase {
        Phase::Running => "SCANNING",
        Phase::Finished { found: true, .. } => "RESULTS",
        Phase::Finished { .. } => "NO MATCH",
        Phase::Failed(_) => "FAILED",
    };
    let facts = vec![format!("target {}", scan.targets.join(" ")), format!("type {}", scan.kind_label)];
    let workers = super::worker_threads(app);
    let right = if scan.via_index {
        state_dot(app)
    } else {
        vec![Span::styled(format!("{} {workers} workers", t.glyphs.dot), t.amber())]
    };
    status_bar(f, status, app, mode, facts, right);

    let body = inset(content, 2, 1);
    let [echo, rest, hints] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(0), Constraint::Length(1)]).areas(body);

    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(format!("{} ", t.glyphs.prompt), t.amber()),
            Span::raw(scan.targets.join(" ")),
            Span::styled(format!("  ({})", scan.kind_label), t.dim()),
        ])),
        echo,
    );

    let header_h = if rest.height >= HEADER_FULL + CARDS + 4 && rest.width >= 64 {
        HEADER_FULL
    } else {
        HEADER_SMALL
    };
    let (header, rest) = take_top(Rect { y: rest.y + 1, height: rest.height.saturating_sub(1), ..rest }, header_h);
    draw_header(f, app, scan, header, header_h == HEADER_FULL);

    let mut rest = Rect { y: rest.y + 1, height: rest.height.saturating_sub(1), ..rest };
    if rest.height >= CARDS + 4 {
        let (cards, below) = take_top(rest, CARDS);
        draw_cards(f, app, scan, cards);
        rest = Rect { y: below.y + 1, height: below.height.saturating_sub(1), ..below };
    }
    draw_live(f, app, scan, rest);

    let items: Vec<(&str, &str)> = match &scan.phase {
        Phase::Running => vec![("esc", "stop")],
        Phase::Finished { found: true, .. } => vec![(key(t, "enter"), "open results"), ("esc", "new search")],
        _ => vec![(key(t, "enter"), "back"), ("esc", "back")],
    };
    f.render_widget(Paragraph::new(hint_line(t, &items)), hints);
}

fn draw_header(f: &mut Frame, app: &App, scan: &ScanRun, area: Rect, full: bool) {
    let t = &app.theme;
    let block = rounded(t, true).padding(Padding::horizontal(2));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let pose = app.pose();
    let text_area = if full {
        let (cols, rows) = Size::Small.cells();
        let [m, text] = Layout::horizontal([Constraint::Length(cols + 3), Constraint::Min(0)]).areas(inner);
        let lines = mascot::render(Size::Small, pose, t, t.bg);
        f.render_widget(Paragraph::new(lines), Rect { height: rows.min(m.height), width: cols, ..m });
        text
    } else {
        let [m, text] = Layout::horizontal([Constraint::Length(8), Constraint::Min(0)]).areas(inner);
        f.render_widget(Paragraph::new(mascot::eyes_line(pose, t)), m);
        text
    };

    let width = text_area.width as usize;
    let elapsed_ms = match (&scan.phase, &scan.summary) {
        (Phase::Running, _) => scan.started.elapsed().as_millis() as u64,
        (_, Some(s)) => s.elapsed_ms,
        _ => scan.started.elapsed().as_millis() as u64,
    };

    let (title, sub) = match &scan.phase {
        Phase::Running => ("Prowling", "fang is on it"),
        Phase::Finished { found: true, .. } => ("Found", "fang got it"),
        Phase::Finished { .. } => ("No matches", "nothing here"),
        Phase::Failed(_) => ("Search failed", "see below"),
    };
    let title_style = match &scan.phase {
        Phase::Failed(_) => t.red().add_modifier(ratatui::style::Modifier::BOLD),
        _ => t.amber_bold(),
    };
    let done = scan.progress.done.load(Ordering::Relaxed);
    let total = scan.progress.total.load(Ordering::Relaxed);

    let mut lines = vec![Line::from(vec![
        Span::styled(title, title_style),
        Span::styled(format!("  /// {sub}"), t.dim()),
    ])];
    lines.push(Line::from(Span::styled(
        match (&scan.phase, scan.via_index) {
            (Phase::Running, true) => "answering from the index".to_string(),
            (Phase::Running, false) if total == 0 => "listing files...".to_string(),
            (Phase::Running, false) => format!(
                "{} files scanned {} {}",
                thousands(scan.progress.files_done.load(Ordering::Relaxed)),
                t.glyphs.sep,
                super::clip_start(&display_paths(&app.session.paths), width.saturating_sub(24).max(10), t.glyphs.ellipsis)
            ),
            (Phase::Finished { .. }, _) => scan
                .summary
                .as_ref()
                .map(|s| format!("{} files {} {} scanned", thousands(s.files_scanned as u64), t.glyphs.sep, human_bytes(s.bytes_scanned)))
                .unwrap_or_default(),
            (Phase::Failed(_), _) => String::new(),
        },
        t.dim(),
    )));
    if full {
        lines.push(Line::raw(""));
    }

    let bar_w = width.saturating_sub(7).clamp(8, 48);
    // Big plain files are memory-mapped and the core reports their bytes when each file ends, so
    // a single huge file shows no progress until it is done. Say so rather than show a stuck 0%.
    let unmeasured = total > 0 && done == 0 && elapsed_ms > 1500;
    match &scan.phase {
        Phase::Running if scan.via_index || total == 0 || unmeasured => {
            // No byte counts to show: a claw marks that something is happening.
            let pos = (app.tick as usize / 2) % bar_w.saturating_sub(3).max(1);
            let before = t.glyphs.bar_empty.repeat(pos);
            let after = t.glyphs.bar_empty.repeat(bar_w.saturating_sub(pos + 3));
            lines.push(Line::from(vec![
                Span::styled(before, t.dim()),
                Span::styled("///", t.amber_bold()),
                Span::styled(after, t.dim()),
            ]));
        }
        Phase::Running | Phase::Finished { .. } => {
            let ratio = if matches!(scan.phase, Phase::Finished { .. }) { 1.0 } else { scan.ratio().unwrap_or(0.0) };
            let (on, off) = bar(t, ratio, bar_w);
            lines.push(Line::from(vec![
                Span::styled(on, t.amber()),
                Span::styled(off, t.dim()),
                Span::styled(format!("  {:>3.0}%", ratio * 100.0), t.bold()),
            ]));
        }
        Phase::Failed(_) => {}
    }

    if let Phase::Running = scan.phase {
        if unmeasured {
            lines.push(Line::from(Span::styled(
                format!("{} {} reading {} (progress shows when a file ends)", claws(app.tick), human_secs(elapsed_ms), human_bytes(total)),
                t.dim(),
            )));
        } else if total > 0 {
            let secs = (elapsed_ms as f64 / 1000.0).max(0.001);
            let rate = done as f64 / secs;
            let eta = if rate > 1.0 { ((total.saturating_sub(done)) as f64 / rate * 1000.0) as u64 } else { 0 };
            lines.push(Line::from(Span::styled(
                format!(
                    "{} / {} {} {}/s {} {}{}",
                    human_bytes(done),
                    human_bytes(total),
                    t.glyphs.sep,
                    human_bytes(rate as u64),
                    t.glyphs.sep,
                    human_secs(elapsed_ms),
                    if eta > 0 { format!(" {} eta {}", t.glyphs.sep, human_secs(eta)) } else { String::new() }
                ),
                t.dim(),
            )));
        } else {
            lines.push(Line::from(Span::styled(format!("{} {}", claws(app.tick), human_secs(elapsed_ms)), t.dim())));
        }
    } else if let Phase::Finished { .. } = scan.phase {
        lines.push(Line::from(Span::styled(format!("in {}", human_secs(elapsed_ms)), t.dim())));
    }

    let pad = text_area.height.saturating_sub(lines.len() as u16) / 2;
    let text_area = Rect { y: text_area.y + pad, height: text_area.height - pad, ..text_area };
    f.render_widget(Paragraph::new(lines), text_area);
}

fn draw_cards(f: &mut Frame, app: &App, scan: &ScanRun, area: Rect) {
    let t = &app.theme;
    let [a, b, c] = Layout::horizontal([Constraint::Ratio(1, 3); 3]).areas(area);
    let files = match &scan.summary {
        Some(s) => s.files_scanned as u64,
        None => scan.progress.files_done.load(Ordering::Relaxed),
    };
    let hits = match &scan.summary {
        Some(s) => s.matches as u64,
        None => scan.progress.matches.load(Ordering::Relaxed).max(scan.hits_seen),
    };
    let state = match &scan.phase {
        Phase::Running => "SCAN",
        Phase::Finished { .. } => "DONE",
        Phase::Failed(_) => "FAIL",
    };
    let flash = scan.last_hit_tick.is_some_and(|tk| app.tick.saturating_sub(tk) <= 1);
    let hit_style = if flash { t.amber_bold().add_modifier(ratatui::style::Modifier::REVERSED) } else { t.amber_bold() };
    card(f, app, a, "HITS", Span::styled(thousands(hits), hit_style));
    card(f, app, b, "FILES SCANNED", Span::styled(thousands(files), t.bold()));
    card(f, app, c, "STATE", Span::styled(state, t.bold()));
}

fn card(f: &mut Frame, app: &App, area: Rect, title: &str, value: Span<'static>) {
    let t = &app.theme;
    let block = rounded(t, false).padding(Padding::horizontal(1));
    let inner = block.inner(area);
    f.render_widget(block, area);
    f.render_widget(
        Paragraph::new(vec![Line::from(Span::styled(title.to_string(), t.dim())), Line::from(value)]),
        inner,
    );
}

fn draw_live(f: &mut Frame, app: &App, scan: &ScanRun, area: Rect) {
    let t = &app.theme;
    if area.height < 2 {
        return;
    }
    let (head, list) = take_top(area, 1);
    f.render_widget(Paragraph::new(section(t, "live matches")), head);

    if let Phase::Failed(msg) = &scan.phase {
        f.render_widget(Paragraph::new(Span::styled(msg.clone(), t.red())).wrap(Wrap { trim: true }), list);
        return;
    }
    if scan.live.is_empty() {
        let text = match &scan.phase {
            Phase::Running => format!("{} waiting for the first hit", claws(app.tick)),
            _ => format!("no matches for {}", scan.targets.join(" ")),
        };
        f.render_widget(Paragraph::new(Span::styled(text, t.dim())), list);
        return;
    }

    let rows = list.height as usize;
    let skip = scan.live.len().saturating_sub(rows);
    let mut lines = Vec::with_capacity(rows);
    for r in scan.live.iter().skip(skip) {
        let loc = format!(
            "{}{}",
            file_label(&r.source.path, r.source.archive_entry.as_deref()),
            r.source.line_number.map(|n| format!(":{}", n + 1)).unwrap_or_default()
        );
        let loc = super::clip_start(&loc, 36, t.glyphs.ellipsis);
        let width = (list.width as usize).saturating_sub(loc.chars().count() + 2).max(10);
        let mut spans = vec![Span::styled(loc, t.dim()), Span::raw("  ")];
        spans.extend(highlighted(t, &r.line_text, r.matched_span, width, Style::new()));
        lines.push(Line::from(spans));
    }
    f.render_widget(Paragraph::new(lines), list);
}
