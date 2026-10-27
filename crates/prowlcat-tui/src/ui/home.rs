//! The home screen: Fang, the corpus, a few examples, recent searches and the prompt.

use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Padding, Paragraph};
use ratatui::Frame;

use crate::app::{display_paths, App};
use crate::history;
use crate::mascot::{self, Size};

use super::{hint_line, inset, key, rounded, section, state_dot, status_bar, take_top, thousands, human_bytes, clip};

/// Rows the framed banner needs for the full mascot: 10 of mascot plus the frame.
const BANNER_FULL: u16 = 12;

pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    let [content, status] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(area);

    let mut facts = vec![format!("{} threads", super::worker_threads(app))];
    facts.push(format!("type {}", crate::command::hint_name(app.session.hint)));
    if app.session.max_matches > 0 {
        facts.push(format!("max {}", app.session.max_matches));
    }
    status_bar(f, status, app, "SEARCH", facts, state_dot(app));

    let body = inset(content, 2, 1);
    let [top, prompt_area, hint_area] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(3), Constraint::Length(1)]).areas(body);

    // Banner, then as many of the other blocks as fit.
    let banner_h = if top.height >= BANNER_FULL + 2 && top.width >= 58 { BANNER_FULL } else { 3 };
    let (banner, rest) = take_top(top, banner_h);
    draw_banner(f, app, banner, banner_h == BANNER_FULL);

    let rest = Rect { y: rest.y + 1, height: rest.height.saturating_sub(1), ..rest };
    let try_h = 5;
    let recent_h = 5;
    let mut y = rest;
    if rest.height >= try_h {
        let (a, b) = take_top(rest, try_h);
        draw_examples(f, app, a);
        y = Rect { y: b.y + 1, height: b.height.saturating_sub(1), ..b };
    }
    if y.height >= 3 {
        draw_recent(f, app, take_top(y, recent_h).0);
    }

    if app.palette_open() {
        draw_palette(f, app, body, prompt_area);
    }
    draw_prompt(f, app, prompt_area);

    let items: Vec<(&str, &str)> = vec![
        ("/", "commands"),
        ("@", "path"),
        (key(t, "tab"), "complete"),
        (key(t, "enter"), "search"),
        ("esc", "clear"),
        ("?", "help"),
    ];
    f.render_widget(Paragraph::new(hint_line(t, &items)), hint_area);
}

fn draw_banner(f: &mut Frame, app: &App, area: Rect, full: bool) {
    let t = &app.theme;
    let pose = app.pose();
    let version = env!("CARGO_PKG_VERSION");

    // Width left for the text beside the mascot (frame and padding are 6 columns).
    let text_w = if full { (area.width as usize).saturating_sub(6 + 27) } else { (area.width as usize).saturating_sub(12) };
    let corpus_line = corpus_summary(app, text_w);
    let index_line = index_summary(app);

    let mut info = vec![
        Line::from(vec![Span::styled("PROWLCAT", t.amber_bold()), Span::styled(format!("  v{version}"), t.dim())]),
        Line::from(Span::styled("local-first breach corpus search", t.dim())),
        Line::raw(""),
        corpus_line,
        index_line,
        Line::raw(""),
        Line::from(vec![Span::styled("/// ", t.amber_bold()), Span::styled("Fang is watching the corpus.", t.dim())]),
    ];

    if full {
        let block = rounded(t, true).padding(Padding::horizontal(2));
        let inner = block.inner(area);
        f.render_widget(block, area);
        let (cols, rows) = Size::Small.cells();
        let [mascot_area, text_area] =
            Layout::horizontal([Constraint::Length(cols + 3), Constraint::Min(0)]).areas(inner);
        let lines = mascot::render(Size::Small, pose, t, t.bg);
        f.render_widget(Paragraph::new(lines), Rect { height: rows.min(mascot_area.height), width: cols, ..mascot_area });

        let pad = text_area.height.saturating_sub(info.len() as u16) / 2;
        let text_area = Rect { y: text_area.y + pad, height: text_area.height - pad, ..text_area };
        f.render_widget(Paragraph::new(info), text_area);
    } else {
        // Short terminal: two eyes and the name.
        let eyes = mascot::eyes_line(pose, t);
        let mut first = eyes.spans;
        first.push(Span::raw("  "));
        first.extend(info.remove(0).spans);
        let lines = vec![Line::from(first), info.remove(0), info.remove(1)];
        f.render_widget(Paragraph::new(lines), area);
    }
}

fn corpus_summary(app: &App, width: usize) -> Line<'static> {
    let t = &app.theme;
    let sep = format!(" {} ", t.glyphs.sep);
    let mut spans = vec![Span::styled("corpus  ", t.dim())];
    if let Some(index) = &app.session.index {
        spans.push(Span::raw(format!("via index {}", display_paths(std::slice::from_ref(index)))));
        return Line::from(spans);
    }
    if app.session.paths.is_empty() {
        spans.push(Span::styled("none yet: type @<dir> after a target, or /path <dir>", t.amber()));
        return Line::from(spans);
    }
    spans.push(Span::raw(clip(&display_paths(&app.session.paths), width.saturating_sub(40).clamp(12, 60), t.glyphs.ellipsis)));
    if let Some(s) = &app.corpus.stats {
        spans.push(Span::styled(sep.clone(), t.dim()));
        spans.push(Span::raw(format!("{} files", thousands(s.files as u64))));
        spans.push(Span::styled(sep, t.dim()));
        spans.push(Span::raw(human_bytes(s.bytes)));
    } else if app.corpus.stats_job.is_some() {
        spans.push(Span::styled(format!("{sep}counting files{}", t.glyphs.ellipsis), t.dim()));
    }
    Line::from(spans)
}

fn index_summary(app: &App) -> Line<'static> {
    let t = &app.theme;
    let mut spans = vec![Span::styled("index   ", t.dim())];
    match (&app.session.index, &app.corpus.index_meta) {
        (Some(dir), meta) => {
            let name = dir.file_name().map_or_else(|| dir.display().to_string(), |n| n.to_string_lossy().into_owned());
            spans.push(Span::raw(name));
            spans.push(Span::styled(format!(" {} in use", t.glyphs.dot), t.green()));
            if let Some(m) = meta {
                spans.push(Span::styled(format!(" {} {} lines", t.glyphs.sep, thousands(m.docs)), t.dim()));
            }
        }
        (None, _) => spans.push(Span::styled("none (searches scan the files)", t.dim())),
    }
    Line::from(spans)
}

fn draw_examples(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    let rows = [
        ("user@example.com", "email", "exact match"),
        ("example.com", "domain", "emails, URLs, subdomains"),
        ("10.0.0.0/8", "cidr", "every IP inside the range"),
        ("Winter2024", "text", "case-insensitive substring (/type text)"),
    ];
    let mut lines = vec![section(t, "Try one of these, or just type a target:")];
    for (target, kind, what) in rows {
        lines.push(Line::from(vec![
            Span::styled(format!("{} ", t.glyphs.prompt), t.amber()),
            Span::raw(format!("{target:<22}")),
            Span::styled(format!("{kind:<8}{} {what}", t.glyphs.sep), t.dim()),
        ]));
    }
    f.render_widget(Paragraph::new(lines), area);
}

fn draw_recent(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    let mut lines = vec![section(t, "Recent")];
    let recent = &app.store.saved.recent;
    if recent.is_empty() {
        lines.push(Line::from(Span::styled("no searches yet", t.dim())));
    }
    for r in recent.iter().take((area.height as usize).saturating_sub(1).min(3)) {
        let (glyph, style) = if r.hits > 0 { (t.glyphs.ok, t.green()) } else { (t.glyphs.miss, t.red()) };
        let what = if r.hits > 0 { format!("{} hits", thousands(r.hits)) } else { "no matches".to_string() };
        lines.push(Line::from(vec![
            Span::styled(format!("{glyph} "), style),
            Span::raw(format!("{:<28}", clip(&r.target, 27, t.glyphs.ellipsis))),
            Span::styled(
                format!("{what} {} {:.0} s {} {}", t.glyphs.sep, r.elapsed_ms as f64 / 1000.0, t.glyphs.sep, history::ago(r.at)),
                t.dim(),
            ),
        ]));
    }
    f.render_widget(Paragraph::new(lines), area);
}

fn draw_prompt(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    let block = rounded(t, true);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let prompt = format!("{} ", t.glyphs.prompt);
    let avail = (inner.width as usize).saturating_sub(prompt.chars().count() + 1);
    let kind_hint = match (app.inferred_kind(), app.session.hint) {
        (Some(kind), prowlcat_core::TypeHint::Auto) => format!("auto {} {kind}", t.glyphs.arrow),
        (Some(kind), _) => format!("type: {kind}"),
        _ => String::new(),
    };
    let reserved = if kind_hint.is_empty() { 0 } else { kind_hint.chars().count() + 2 };
    let avail = avail.saturating_sub(reserved);

    let text = app.input.text();
    let cursor = app.input.cursor();
    let start = (cursor + 1).saturating_sub(avail.max(1));
    let shown: String = text.chars().skip(start).take(avail.max(1)).collect();
    f.render_widget(
        Paragraph::new(Line::from(vec![Span::styled(prompt.clone(), t.amber()), Span::raw(shown)])),
        inner,
    );
    if !kind_hint.is_empty() {
        f.render_widget(Paragraph::new(Span::styled(kind_hint, t.dim())).alignment(Alignment::Right), inner);
    }
    let x = inner.x + prompt.chars().count() as u16 + (cursor - start) as u16;
    f.set_cursor_position((x.min(inner.right().saturating_sub(1)), inner.y));
}

/// The `/commands` list that opens above the prompt.
fn draw_palette(f: &mut Frame, app: &App, body: Rect, prompt: Rect) {
    let t = &app.theme;
    let items = app.palette_items();
    if items.is_empty() {
        return;
    }
    let height = (items.len() as u16 + 3).min(prompt.y.saturating_sub(body.y)).max(4);
    let width = body.width.min(86);
    let area = Rect { x: body.x, y: prompt.y.saturating_sub(height), width, height };
    f.render_widget(Clear, area);
    let block = rounded(t, false);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let mut lines = vec![Line::from(Span::styled("Commands", t.dim()))];
    let visible = (inner.height as usize).saturating_sub(1);
    let sel = app.palette_sel.min(items.len() - 1);
    let first = sel.saturating_sub(visible.saturating_sub(1));
    for (i, (name, help)) in items.iter().enumerate().skip(first).take(visible) {
        let selected = i == sel;
        let marker = if selected { format!("{} ", t.glyphs.pointer) } else { "  ".to_string() };
        let base = if selected { t.selected() } else { ratatui::style::Style::new() };
        let name_style = base.patch(t.amber_bold());
        let used = marker.chars().count() + name.chars().count() + 1 + 2;
        let help = clip(help, (inner.width as usize).saturating_sub(used + 12), t.glyphs.ellipsis);
        let mut spans = vec![
            Span::styled(marker, base),
            Span::styled(format!("/{name}"), name_style),
            Span::styled(format!("  {help}"), base.patch(if selected { t.amber() } else { t.dim() })),
        ];
        let pad = (inner.width as usize).saturating_sub(spans.iter().map(Span::width).sum());
        spans.push(Span::styled(" ".repeat(pad), base));
        lines.push(Line::from(spans));
    }
    f.render_widget(Paragraph::new(lines), inner);
}
