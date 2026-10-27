//! The results explorer: files on the left, matching lines on the right, details underneath.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Padding, Paragraph};
use ratatui::Frame;

use crate::app::{kind_label, App, Group, Pane, ResultsView};
use crate::theme::Theme;

use super::{
    clip, clip_start, highlighted, hint_line, human_secs, inset, key, rounded, state_dot, status_bar, take_top, thousands,
};

pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    let Some(view) = &app.results else { return };
    let [content, status] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(area);

    let facts = vec![
        format!("{} of {} shown", thousands(view.visible_hits() as u64), thousands(view.all.len() as u64)),
        "export .json .csv .ndjson".to_string(),
    ];
    status_bar(f, status, app, "RESULTS", facts, state_dot(app));

    let body = inset(content, 2, 1);
    let [head, main, hints] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(0), Constraint::Length(1)]).areas(body);

    draw_head(f, app, view, head);

    let main = Rect { y: main.y + 1, height: main.height.saturating_sub(1), ..main };
    let left_w = (main.width * 2 / 5).clamp(28, 52).min(main.width.saturating_sub(24));
    let [left, right] = Layout::horizontal([Constraint::Length(left_w), Constraint::Min(0)]).areas(main);
    draw_files(f, app, view, left);
    draw_lines(f, app, view, right);

    let items: Vec<(&str, &str)> = if view.filtering.is_some() {
        vec![(key(t, "enter"), "keep filter"), ("esc", "cancel")]
    } else {
        vec![
            (key(t, "up/down"), "move"),
            (key(t, "tab"), "pane"),
            ("/", "filter"),
            ("e", "export"),
            ("y", "copy line"),
            ("esc", "back"),
        ]
    };
    f.render_widget(Paragraph::new(hint_line(t, &items)), hints);
}

fn draw_head(f: &mut Frame, app: &App, view: &ResultsView, area: Rect) {
    let t = &app.theme;
    let s = &view.summary;
    let mut spans = vec![
        Span::styled(format!("{} ", t.glyphs.ok), t.green()),
        Span::styled(clip(&view.target_label, 40, t.glyphs.ellipsis), t.bold()),
        Span::styled(
            format!(
                "   {} hits {} {} files {} {}",
                thousands(s.matches as u64),
                t.glyphs.sep,
                thousands(view.groups_len() as u64),
                t.glyphs.sep,
                human_secs(s.elapsed_ms)
            ),
            t.dim(),
        ),
    ];
    if s.errors > 0 {
        spans.push(Span::styled(format!("   {} files failed", s.errors), t.red()));
    }
    if view.omitted() > 0 {
        spans.push(Span::styled(
            format!("   first {} shown, export saves all", thousands(view.all.len() as u64)),
            t.amber(),
        ));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);

    let filter_text = match &view.filtering {
        Some(input) => Some((input.text().to_string(), true)),
        None if !view.filter.is_empty() => Some((view.filter.clone(), false)),
        None => None,
    };
    if let Some((text, editing)) = filter_text {
        let label = format!("filter / {text}{}", if editing { "_" } else { "" });
        let w = label.chars().count() as u16;
        let at = Rect { x: area.right().saturating_sub(w + 1), width: w + 1, ..area };
        f.render_widget(Paragraph::new(Span::styled(label, t.amber())), at);
    }
}

fn draw_files(f: &mut Frame, app: &App, view: &ResultsView, area: Rect) {
    let t = &app.theme;
    let focused = view.focus == Pane::Files;
    let block = super::divider(t).padding(Padding::right(1));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let (title, rest) = take_top(inner, 1);
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("FILES", t.dim()),
            Span::styled(format!(" {} sorted by hits", t.glyphs.sep), t.dim()),
        ])),
        title,
    );

    // Leave room for the per-type bars under the list when the terminal is tall enough.
    let kinds = view.by_kind.len().min(4) as u16;
    let bars_h = if rest.height > kinds + 8 { kinds + 1 } else { 0 };
    let list = Rect { height: rest.height - bars_h, ..rest };
    let width = list.width as usize;

    let rows = list.height as usize;
    let off = scroll(&view.file_scroll, view.file_sel, rows);
    let lines: Vec<Line> = view
        .view
        .iter()
        .enumerate()
        .skip(off)
        .take(rows)
        .map(|(i, g)| file_row(t, g, i == view.file_sel, focused, width))
        .collect();
    if lines.is_empty() {
        f.render_widget(Paragraph::new(Span::styled("no line matches the filter", t.dim())), list);
    } else {
        f.render_widget(Paragraph::new(lines), list);
    }

    if bars_h > 0 {
        let bars_area = Rect { y: list.bottom(), height: bars_h, ..rest };
        draw_kinds(f, app, view, bars_area);
    }
}

fn file_row<'a>(t: &Theme, g: &Group, selected: bool, focused: bool, width: usize) -> Line<'a> {
    let count = thousands(g.hits.len() as u64);
    let label_w = width.saturating_sub(count.chars().count() + 4);
    let label = clip_start(&g.label, label_w, t.glyphs.ellipsis);
    let pad = width.saturating_sub(2 + label.chars().count() + count.chars().count());
    let marker = if selected { format!("{} ", t.glyphs.pointer) } else { "  ".to_string() };
    let base = match (selected, focused) {
        (true, true) => t.selected(),
        (true, false) => t.amber(),
        _ => Style::new(),
    };
    let count_style = if selected { base.patch(t.bold()) } else { t.bold() };
    Line::from(vec![
        Span::styled(marker, base),
        Span::styled(label, base),
        Span::styled(" ".repeat(pad), base),
        Span::styled(count, count_style),
    ])
}

fn draw_kinds(f: &mut Frame, app: &App, view: &ResultsView, area: Rect) {
    let t = &app.theme;
    let max = view.by_kind.first().map_or(1, |(_, n)| *n).max(1);
    let mut lines = vec![Line::from(Span::styled("BY TYPE", t.dim()))];
    for (kind, n) in view.by_kind.iter().take(4) {
        let w = ((*n as f64 / max as f64) * 12.0).ceil().max(1.0) as usize;
        lines.push(Line::from(vec![
            Span::styled(t.glyphs.spark.repeat(w), t.amber()),
            Span::styled(" ".repeat(13 - w.min(12)), Style::new()),
            Span::styled(format!("{:<7}", kind_label(*kind)), t.dim()),
            Span::raw(thousands(*n as u64)),
        ]));
    }
    f.render_widget(Paragraph::new(lines), area);
}

fn draw_lines(f: &mut Frame, app: &App, view: &ResultsView, area: Rect) {
    let t = &app.theme;
    let area = Rect { x: area.x + 2, width: area.width.saturating_sub(2), ..area };
    let focused = view.focus == Pane::Lines;

    let Some(group) = view.selected_group() else {
        f.render_widget(Paragraph::new(Span::styled("nothing to show", t.dim())), area);
        return;
    };
    let (title, rest) = take_top(area, 1);
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(clip_start(&group.label, area.width as usize - 14, t.glyphs.ellipsis), t.dim()),
            Span::styled(format!(" {} {} hits", t.glyphs.sep, thousands(group.hits.len() as u64)), t.dim()),
        ])),
        title,
    );

    let detail_h = if rest.height >= 14 { 6 } else { 0 };
    let list = Rect { y: rest.y + 1, height: (rest.height - detail_h).saturating_sub(1), ..rest };
    let rows = list.height as usize;
    let off = scroll(&view.line_scroll, view.line_sel, rows);

    let lines: Vec<Line> = group
        .hits
        .iter()
        .enumerate()
        .skip(off)
        .take(rows)
        .map(|(i, &hit)| {
            let r = &view.all[hit];
            let selected = i == view.line_sel;
            let base = match (selected, focused) {
                (true, true) => t.selected(),
                (true, false) => Style::new().bg(t.raised),
                _ => Style::new(),
            };
            let number = r.source.line_number.map_or_else(|| "      ?".to_string(), |n| format!("{:>7}", n + 1));
            let marker = if selected { format!("{} ", t.glyphs.pointer) } else { "  ".to_string() };
            let text_w = (list.width as usize).saturating_sub(marker.chars().count() + number.chars().count() + 2);
            let mut spans = vec![
                Span::styled(marker, base),
                Span::styled(number, base.patch(if selected { Style::new() } else { t.cyan() })),
                Span::styled("  ", base),
            ];
            spans.extend(highlighted(t, &r.line_text, r.matched_span, text_w, base));
            let used: usize = spans.iter().map(Span::width).sum();
            spans.push(Span::styled(" ".repeat((list.width as usize).saturating_sub(used)), base));
            Line::from(spans)
        })
        .collect();
    f.render_widget(Paragraph::new(lines), list);

    if detail_h > 0 {
        let detail = Rect { y: rest.bottom() - detail_h, height: detail_h, ..rest };
        draw_detail(f, app, view, detail);
    }
}

fn draw_detail(f: &mut Frame, app: &App, view: &ResultsView, area: Rect) {
    let t = &app.theme;
    let Some(hit) = view.selected_hit() else { return };
    let block = rounded(t, false).padding(Padding::horizontal(1));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let width = inner.width as usize;
    let place = format!(
        "{}{}",
        hit.source.path.display(),
        hit.source.archive_entry.as_ref().map(|e| format!(" {} {e}", t.glyphs.arrow)).unwrap_or_default()
    );
    let how = match hit.match_kind {
        prowlcat_core::MatchKind::Exact => "exact".to_string(),
        prowlcat_core::MatchKind::CidrLiteral => "literal CIDR".to_string(),
        prowlcat_core::MatchKind::IpInNetwork => "IP inside the network".to_string(),
    };
    let found: Vec<String> = hit
        .extracted
        .iter()
        .map(|v| match v {
            prowlcat_core::ExtractedValue::Email(s)
            | prowlcat_core::ExtractedValue::Domain(s)
            | prowlcat_core::ExtractedValue::Ip(s)
            | prowlcat_core::ExtractedValue::Cidr(s)
            | prowlcat_core::ExtractedValue::Text(s) => s.clone(),
        })
        .collect();
    let mut lines = vec![
        Line::from(Span::styled(
            format!("MATCH {} line {}", t.glyphs.sep, hit.source.line_number.map_or("?".to_string(), |n| thousands(n + 1))),
            t.dim(),
        )),
        Line::from(vec![
            Span::styled("type   ", t.dim()),
            Span::raw(format!("{} {} {}", kind_label(hit.kind), t.glyphs.sep, clip(&hit.query, width.saturating_sub(24), t.glyphs.ellipsis))),
            Span::styled(format!("  ({how})"), t.dim()),
        ]),
        Line::from(vec![Span::styled("file   ", t.dim()), Span::raw(clip_start(&place, width.saturating_sub(7), t.glyphs.ellipsis))]),
        Line::from(vec![
            Span::styled("offset ", t.dim()),
            Span::raw(format!("0x{:X} {} {} bytes", hit.source.byte_offset, t.glyphs.sep, hit.line_text.len())),
        ]),
    ];
    if !found.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("found  ", t.dim()),
            Span::raw(clip(&found.join(", "), width.saturating_sub(7), t.glyphs.ellipsis)),
        ]));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

/// Scroll offset that keeps `sel` visible in a window of `rows`, remembering the last offset so
/// the list does not jump when you move back up.
fn scroll(offset: &std::cell::Cell<usize>, sel: usize, rows: usize) -> usize {
    if rows == 0 {
        return 0;
    }
    let mut off = offset.get();
    if sel < off {
        off = sel;
    } else if sel >= off + rows {
        off = sel + 1 - rows;
    }
    offset.set(off);
    off
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn scroll_follows_the_selection_without_jumping() {
        let off = Cell::new(0);
        assert_eq!(scroll(&off, 0, 5), 0);
        assert_eq!(scroll(&off, 4, 5), 0);
        assert_eq!(scroll(&off, 5, 5), 1, "scrolls by one when the cursor leaves the window");
        assert_eq!(scroll(&off, 9, 5), 5);
        assert_eq!(scroll(&off, 8, 5), 5, "moving up inside the window does not scroll");
        assert_eq!(scroll(&off, 4, 5), 4, "moving above the window scrolls up");
        assert_eq!(scroll(&off, 0, 0), 0);
    }
}
