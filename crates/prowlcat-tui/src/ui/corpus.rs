//! The corpus and index screen: what is being searched, and how to make searching faster.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Padding, Paragraph};
use ratatui::Frame;

use crate::app::{display_paths, App};
use crate::history;
use crate::mascot::{self, Size};
use crate::worker::CorpusStats;

use super::{
    clip, clip_start, hint_line, human_bytes, human_secs, inset, key, rounded, section, state_dot, status_bar, take_top,
    thousands,
};

pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    let [content, status] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(area);
    let facts = vec![if app.corpus.build.is_some() { "building index".to_string() } else { "text targets bypass the index".to_string() }];
    status_bar(f, status, app, "INDEX", facts, state_dot(app));

    let body = inset(content, 2, 1);
    let [head, rest, hints] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(0), Constraint::Length(1)]).areas(body);
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(format!("{} ", t.glyphs.prompt), t.amber()),
            Span::raw("/index"),
        ])),
        head,
    );

    let rest = Rect { y: rest.y + 1, height: rest.height.saturating_sub(1), ..rest };
    let cards_h = 10.min(rest.height);
    let (cards, below) = take_top(rest, cards_h);
    let [left, right] = Layout::horizontal([Constraint::Ratio(1, 2); 2]).areas(cards);
    draw_corpus(f, app, left);
    draw_index(f, app, right);

    let below = Rect { y: below.y + 1, height: below.height.saturating_sub(1), ..below };
    draw_actions(f, app, below);

    let items: Vec<(&str, &str)> = vec![(key(t, "up/down"), "select"), (key(t, "enter"), "run"), ("r", "refresh"), ("esc", "back")];
    f.render_widget(Paragraph::new(hint_line(t, &items)), hints);
}

fn draw_corpus(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    let block = rounded(t, app.session.index.is_none()).padding(Padding::horizontal(2));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let mut lines = vec![Line::from(Span::styled("CORPUS", t.amber_bold()))];
    if app.session.paths.is_empty() {
        lines.push(Line::from(Span::styled("none yet: /path <dir>", t.dim())));
    } else {
        lines.push(Line::from(Span::styled(
            clip(&display_paths(&app.session.paths), inner.width as usize, t.glyphs.ellipsis),
            t.dim(),
        )));
    }
    match &app.corpus.stats {
        Some(s) => {
            lines.push(Line::raw(""));
            lines.push(fact(app, "files", thousands(s.files as u64)));
            lines.push(fact(app, "size", human_bytes(s.bytes)));
            lines.push(fact(app, "archives", (s.gzip + s.zstd + s.tar + s.zip + s.bzip2 + s.xz + s.sevenz + s.rar).to_string()));
            lines.extend(composition(app, s, inner.width as usize));
        }
        None if app.corpus.stats_job.is_some() => {
            lines.push(Line::raw(""));
            lines.push(Line::from(Span::styled(format!("counting files{}", t.glyphs.ellipsis), t.dim())));
        }
        None => {
            lines.push(Line::raw(""));
            lines.push(Line::from(Span::styled("press r to count files", t.dim())));
        }
    }
    f.render_widget(Paragraph::new(lines), inner);
}

fn fact(app: &App, label: &str, value: String) -> Line<'static> {
    Line::from(vec![Span::styled(format!("{label:<9} "), app.theme.dim()), Span::raw(value)])
}

/// A stacked bar of file kinds and its legend.
fn composition(app: &App, s: &CorpusStats, width: usize) -> Vec<Line<'static>> {
    let t = &app.theme;
    let parts = [
        ("txt", s.plain, t.amber),
        ("gz", s.gzip, t.cyan),
        ("zst", s.zstd, t.green),
        ("tar", s.tar, t.red),
        ("zip", s.zip, t.dim),
        ("bz2 xz 7z rar", s.bzip2 + s.xz + s.sevenz + s.rar, t.text),
        ("db json", s.structured, t.line_strong),
    ];
    let total: usize = parts.iter().map(|p| p.1).sum();
    if total == 0 || width < 10 {
        return Vec::new();
    }
    let mut bar = Vec::new();
    let mut used = 0;
    for (i, (_, n, color)) in parts.iter().enumerate() {
        if *n == 0 {
            continue;
        }
        let last = i == parts.len() - 1 || parts[i + 1..].iter().all(|p| p.1 == 0);
        let cells = if last { width.saturating_sub(used) } else { ((*n as f64 / total as f64) * width as f64).round().max(1.0) as usize };
        let cells = cells.min(width.saturating_sub(used));
        used += cells;
        let style = if t.mono() { Style::new() } else { Style::new().fg(*color) };
        bar.push(Span::styled(t.glyphs.spark.repeat(cells), style));
    }
    let legend: Vec<Span<'static>> = parts
        .iter()
        .filter(|p| p.1 > 0)
        .flat_map(|(name, n, color)| {
            let style = if t.mono() { Style::new() } else { Style::new().fg(*color) };
            let pct = (*n as f64 / total as f64 * 100.0).round();
            [Span::styled(format!("{} ", t.glyphs.dot), style), Span::styled(format!("{name} {pct:.0}%  "), t.dim())]
        })
        .collect();
    vec![Line::raw(""), Line::from(bar), Line::from(legend)]
}

fn draw_index(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    let block = rounded(t, app.session.index.is_some()).padding(Padding::horizontal(2));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let mut lines = vec![Line::from(Span::styled("INDEX", t.amber_bold()))];
    match (&app.session.index, &app.corpus.index_meta) {
        (Some(dir), meta) => {
            lines.push(Line::from(Span::styled(clip_start(&display_paths(std::slice::from_ref(dir)), inner.width as usize, t.glyphs.ellipsis), t.dim())));
            lines.push(Line::raw(""));
            lines.push(Line::from(vec![
                Span::styled("status    ", t.dim()),
                Span::styled(format!("{} in use", t.glyphs.dot), t.green()),
            ]));
            if let Some(m) = meta {
                lines.push(fact_wide(app, "lines", thousands(m.lines)));
                lines.push(fact_wide(app, "indexed", thousands(m.docs)));
                lines.push(fact_wide(app, "files", thousands(m.files as u64)));
                lines.push(fact_wide(app, "source", human_bytes(m.bytes)));
            }
            if let Some(age) = index_age(dir) {
                lines.push(fact_wide(app, "built", age));
            }
        }
        (None, _) => {
            lines.push(Line::from(Span::styled("not in use", t.dim())));
            lines.push(Line::raw(""));
            lines.push(Line::from(Span::styled("An index answers email, domain and IP", t.dim())));
            lines.push(Line::from(Span::styled("searches in milliseconds. Build one", t.dim())));
            lines.push(Line::from(Span::styled("once, then use it for every search.", t.dim())));
        }
    }
    if let (Some(elapsed), Some(build)) = (app.build_elapsed(), &app.corpus.build) {
        lines.push(Line::raw(""));
        lines.push(Line::from(vec![
            Span::styled(format!("{} ", super::claws(app.tick)), t.amber_bold()),
            Span::raw(format!("building {}", human_secs(elapsed.as_millis() as u64))),
        ]));
        lines.push(Line::from(Span::styled(
            clip_start(&format!("into {}", build.out.display()), inner.width as usize, t.glyphs.ellipsis),
            t.dim(),
        )));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

fn fact_wide(app: &App, label: &str, value: String) -> Line<'static> {
    Line::from(vec![Span::styled(format!("{label:<10}"), app.theme.dim()), Span::raw(value)])
}

fn index_age(dir: &std::path::Path) -> Option<String> {
    let modified = std::fs::metadata(dir.join("prowlcat-index.json")).ok()?.modified().ok()?;
    let secs = modified.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
    Some(history::ago(secs))
}

fn draw_actions(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    if area.height < 3 {
        return;
    }
    let (head, list) = take_top(area, 1);
    f.render_widget(Paragraph::new(section(t, "Actions")), head);

    let actions = app.corpus_actions();
    let sel = app.corpus.sel.min(actions.len() - 1);
    let mut lines = Vec::new();
    for (i, action) in actions.iter().enumerate() {
        let selected = i == sel;
        let base = if selected { t.selected() } else { Style::new() };
        let marker = if selected { format!("{} ", t.glyphs.pointer) } else { "  ".to_string() };
        let title = format!("{:<24}", action.title());
        let detail = clip(action.detail(), (list.width as usize).saturating_sub(30), t.glyphs.ellipsis);
        let mut spans = vec![
            Span::styled(marker, base),
            Span::styled(title, base.patch(if selected { t.bold() } else { Style::new() })),
            Span::styled(detail, base.patch(if selected { t.amber() } else { t.dim() })),
        ];
        let used: usize = spans.iter().map(Span::width).sum();
        spans.push(Span::styled(" ".repeat((list.width as usize).saturating_sub(used)), base));
        lines.push(Line::from(spans));
    }

    if let Some((dir, meta, ms)) = &app.corpus.last_built {
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled("last build", t.dim())));
        lines.push(Line::from(vec![
            Span::styled(format!("{} ", t.glyphs.ok), t.green()),
            Span::raw(format!("{} lines indexed from {} files", thousands(meta.docs), thousands(meta.files as u64))),
        ]));
        lines.push(Line::from(vec![
            Span::styled(format!("{} ", t.glyphs.ok), t.green()),
            Span::raw(format!("written to {} in {}", display_paths(std::slice::from_ref(dir)), human_secs(*ms))),
        ]));
    }
    f.render_widget(Paragraph::new(lines), list);

    // Fang keeps watch from the corner while an index builds.
    if app.corpus.build.is_some() && list.width >= 60 && list.height >= 12 {
        let (cols, rows) = Size::Small.cells();
        let at = Rect { x: list.right() - cols, y: list.bottom() - rows, width: cols, height: rows };
        f.render_widget(Paragraph::new(mascot::render(Size::Small, app.pose(), t, t.bg)), at);
    }
}
