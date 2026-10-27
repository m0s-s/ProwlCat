//! Drawing. Every function here only reads the [`App`]; it never changes it.

mod corpus;
mod home;
mod overlay;
mod results;
mod scan;
mod splash;

use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};
use ratatui::Frame;

use crate::app::{App, NoticeKind, Overlay, Screen};
use crate::theme::Theme;

/// Below this the layouts stop making sense and a short message is shown instead.
const MIN_WIDTH: u16 = 50;
const MIN_HEIGHT: u16 = 12;

pub fn draw(f: &mut Frame, app: &App) {
    let area = f.area();
    f.render_widget(Block::new().style(app.theme.base()), area);

    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        too_small(f, app, area);
        return;
    }
    if app.splash_until.is_some() && splash::fits(area) {
        splash::draw(f, app, area);
        return;
    }

    match app.screen {
        Screen::Home => home::draw(f, app, area),
        Screen::Scanning => scan::draw(f, app, area),
        Screen::Results => results::draw(f, app, area),
        Screen::Corpus => corpus::draw(f, app, area),
    }

    match &app.overlay {
        Overlay::None => {}
        Overlay::Help => overlay::help(f, app, area),
        Overlay::Prompt(kind, input) => overlay::prompt(f, app, area, *kind, input),
    }
}

fn too_small(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    let pose = app.pose();
    let lines = vec![
        crate::mascot::eyes_line(pose, t).centered(),
        Line::from(Span::styled("ProwlCat needs more room", t.amber_bold())).centered(),
        Line::from(Span::styled(format!("need {MIN_WIDTH} x {MIN_HEIGHT}, have {} x {}", area.width, area.height), t.dim())).centered(),
    ];
    let h = lines.len() as u16;
    let at = Rect { y: area.y + area.height.saturating_sub(h) / 2, height: h.min(area.height), ..area };
    f.render_widget(Paragraph::new(lines).style(t.base()), at);
}

// ---- layout helpers --------------------------------------------------------------------------

pub(crate) fn inset(area: Rect, horizontal: u16, vertical: u16) -> Rect {
    area.inner(Margin { horizontal: horizontal.min(area.width / 2), vertical: vertical.min(area.height / 2) })
}

/// Split `area` into a strip of `height` rows at the top and the rest below it.
pub(crate) fn take_top(area: Rect, height: u16) -> (Rect, Rect) {
    let h = height.min(area.height);
    let [top, rest] = Layout::vertical([Constraint::Length(h), Constraint::Min(0)]).areas(area);
    (top, rest)
}

pub(crate) fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let w = width.min(area.width);
    let h = height.min(area.height);
    Rect { x: area.x + (area.width - w) / 2, y: area.y + (area.height - h) / 2, width: w, height: h }
}

pub(crate) fn rounded<'a>(theme: &Theme, active: bool) -> Block<'a> {
    let block = Block::new().borders(Borders::ALL).border_style(theme.border(active));
    if theme.unicode {
        block.border_type(BorderType::Rounded)
    } else {
        block.border_set(ASCII_BORDER)
    }
}

/// A single vertical rule on the right edge, used between the two panes of the results.
pub(crate) fn divider<'a>(theme: &Theme) -> Block<'a> {
    let block = Block::new().borders(Borders::RIGHT).border_style(theme.border_soft());
    if theme.unicode {
        block
    } else {
        block.border_set(ASCII_BORDER)
    }
}

const ASCII_BORDER: ratatui::symbols::border::Set<'static> = ratatui::symbols::border::Set {
    top_left: "+",
    top_right: "+",
    bottom_left: "+",
    bottom_right: "+",
    vertical_left: "|",
    vertical_right: "|",
    horizontal_top: "-",
    horizontal_bottom: "-",
};

// ---- shared pieces ---------------------------------------------------------------------------

/// `key  label   key  label`: the hint row under the prompt.
pub(crate) fn hint_line<'a>(theme: &Theme, items: &[(&str, &str)]) -> Line<'a> {
    let mut spans = Vec::new();
    for (i, (key, label)) in items.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("   "));
        }
        spans.push(Span::styled(format!(" {key} "), theme.keycap()));
        spans.push(Span::styled(format!(" {label}"), theme.dim()));
    }
    Line::from(spans)
}

/// Key names: symbols where the terminal can show them, words where it cannot.
pub(crate) fn key<'a>(theme: &Theme, name: &'a str) -> &'a str {
    if !theme.unicode {
        return name;
    }
    match name {
        "tab" => "⇥",
        "enter" => "↵",
        "up/down" => "↑↓",
        "left/right" => "←→",
        other => other,
    }
}

/// The bottom bar: mode tag, a few facts, then a notice (if any) and a status on the right.
pub(crate) fn status_bar(f: &mut Frame, area: Rect, app: &App, mode: &str, facts: Vec<String>, right: Vec<Span<'static>>) {
    let t = &app.theme;
    let bar = t.status_bar();
    let mut left = vec![Span::styled(format!(" {} {mode} ", t.glyphs.mode), bar.patch(t.amber_bold()))];
    match &app.notice {
        Some(n) => {
            let style = match n.kind {
                NoticeKind::Info => bar.patch(t.cyan()),
                NoticeKind::Ok => bar.patch(t.green()),
                NoticeKind::Error => bar.patch(t.red().add_modifier(ratatui::style::Modifier::BOLD)),
            };
            left.push(Span::styled(format!(" {}", n.text), style));
        }
        None => {
            for fact in facts {
                left.push(Span::styled(format!(" {fact}  "), bar.patch(t.dim())));
            }
        }
    }
    let right_width: usize = right.iter().map(Span::width).sum();
    let left_width: usize = left.iter().map(Span::width).sum();
    let gap = (area.width as usize).saturating_sub(left_width + right_width + 1);
    let mut spans = left;
    spans.push(Span::styled(" ".repeat(gap), bar));
    spans.extend(right.into_iter().map(|s| Span::styled(s.content, bar.patch(s.style))));
    spans.push(Span::styled(" ", bar));
    f.render_widget(Paragraph::new(Line::from(spans)).style(bar), area);
}

/// Worker threads a search will use: the setting, or one per core like the command line.
pub(crate) fn worker_threads(app: &App) -> usize {
    app.session.threads.unwrap_or_else(|| std::thread::available_parallelism().map_or(4, |n| n.get()))
}

/// `● index ready` style marker for the right of the status bar.
pub(crate) fn state_dot(app: &App) -> Vec<Span<'static>> {
    let t = &app.theme;
    if app.index_in_use() {
        vec![Span::styled(format!("{} index ready", t.glyphs.dot), t.green())]
    } else if app.session.paths.is_empty() {
        vec![Span::styled("no corpus", t.dim())]
    } else {
        vec![Span::styled("no index: scans the files", t.dim())]
    }
}

pub(crate) fn section<'a>(theme: &Theme, title: &str) -> Line<'a> {
    Line::from(vec![Span::styled("/// ", theme.amber_bold()), Span::styled(title.to_string(), theme.dim())])
}

/// A fixed-width bar with sub-cell precision: `████▌░░░░`.
pub(crate) fn bar(theme: &Theme, ratio: f64, width: usize) -> (String, String) {
    let g = &theme.glyphs;
    let ratio = ratio.clamp(0.0, 1.0);
    let eighths = (ratio * width as f64 * 8.0).round() as usize;
    let (full, part) = (eighths / 8, eighths % 8);
    let mut on = g.bar_full.repeat(full.min(width));
    let mut used = full.min(width);
    if part > 0 && used < width {
        on.push_str(g.partial[part]);
        used += 1;
    }
    (on, g.bar_empty.repeat(width - used))
}

/// The claw spinner: `/  `, `// `, `///`, ` //`, `  /`.
pub(crate) fn claws(tick: u64) -> &'static str {
    const FRAMES: [&str; 6] = ["/  ", "// ", "///", " //", "  /", "   "];
    FRAMES[(tick % FRAMES.len() as u64) as usize]
}

// ---- text helpers ----------------------------------------------------------------------------

pub(crate) fn thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

pub(crate) fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut unit = 0;
    while v >= 1024.0 && unit < UNITS.len() - 1 {
        v /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{n} B")
    } else {
        format!("{v:.2} {}", UNITS[unit])
    }
}

pub(crate) fn human_secs(ms: u64) -> String {
    let s = ms as f64 / 1000.0;
    if s < 60.0 {
        format!("{s:.1} s")
    } else {
        format!("{} min {:02} s", ms / 60_000, (ms / 1000) % 60)
    }
}

/// Replace control characters (tabs, escapes) that would disturb the layout.
pub(crate) fn sanitize(s: &str) -> String {
    s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect()
}

/// Shorten to `max` characters, ending with the ellipsis glyph.
pub(crate) fn clip(s: &str, max: usize, ellipsis: &str) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_string();
    }
    let keep = max.saturating_sub(ellipsis.chars().count());
    format!("{}{ellipsis}", s.chars().take(keep).collect::<String>())
}

/// Like [`clip`] but keeps the end, which is the interesting part of a path.
pub(crate) fn clip_start(s: &str, max: usize, ellipsis: &str) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_string();
    }
    let keep = max.saturating_sub(ellipsis.chars().count());
    format!("{ellipsis}{}", s.chars().skip(n - keep).collect::<String>())
}

/// A line of text cut to `width` columns around its match, split as `(before, match, after)`.
///
/// `span` is a byte range into `text`; if it is not valid for `text` nothing is highlighted.
pub(crate) fn match_window(text: &str, span: (usize, usize), width: usize, ellipsis: char) -> (String, String, String) {
    let text = sanitize(text);
    let chars: Vec<char> = text.chars().collect();
    let to_char = |byte: usize| text.char_indices().position(|(i, _)| i == byte).or((byte == text.len()).then_some(chars.len()));
    let (start, end) = match (to_char(span.0), to_char(span.1)) {
        (Some(s), Some(e)) if s <= e => (s, e),
        _ => (0, 0),
    };

    let width = width.max(1);
    let mut from = 0;
    if chars.len() > width {
        // Keep a little context before the match, and the whole match if it fits.
        from = start.saturating_sub(width / 4);
        if end > from + width {
            from = if end - start >= width { start } else { end - width };
        }
        from = from.min(chars.len() - width);
    }
    let to = (from + width).min(chars.len());
    let mut window: Vec<char> = chars[from..to].to_vec();
    if from > 0 {
        window[0] = ellipsis;
    }
    if to < chars.len() {
        if let Some(last) = window.last_mut() {
            *last = ellipsis;
        }
    }

    let s = start.saturating_sub(from).min(window.len());
    let e = end.saturating_sub(from).min(window.len()).max(s);
    let (before, rest) = window.split_at(s);
    let (mid, after) = rest.split_at(e - s);
    (before.iter().collect(), mid.iter().collect(), after.iter().collect())
}

/// A line with the matched part picked out in amber.
pub(crate) fn highlighted<'a>(theme: &Theme, text: &str, span: (usize, usize), width: usize, base: Style) -> Vec<Span<'a>> {
    let ell = if theme.unicode { '…' } else { '.' };
    let (before, mid, after) = match_window(text, span, width, ell);
    let hit = base.patch(theme.amber_bold());
    vec![Span::styled(before, base), Span::styled(mid, hit), Span::styled(after, base)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::ColorLevel;

    #[test]
    fn thousands_separators() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1000), "1,000");
        assert_eq!(thousands(3481), "3,481");
        assert_eq!(thousands(1_234_567), "1,234,567");
    }

    #[test]
    fn byte_sizes() {
        assert_eq!(human_bytes(12), "12 B");
        assert_eq!(human_bytes(1536), "1.50 KB");
        assert_eq!(human_bytes(5 * 1024 * 1024 * 1024), "5.00 GB");
    }

    #[test]
    fn durations() {
        assert_eq!(human_secs(12_340), "12.3 s");
        assert_eq!(human_secs(125_000), "2 min 05 s");
    }

    #[test]
    fn clipping_keeps_the_interesting_end() {
        assert_eq!(clip("short", 10, "…"), "short");
        assert_eq!(clip("0123456789", 6, "…"), "01234…");
        assert_eq!(clip_start("/very/long/path/file.txt", 12, "…"), "…th/file.txt");
        assert_eq!(clip("abc", 3, "..."), "abc");
        assert_eq!(clip("abcdef", 5, "..."), "ab...");
    }

    #[test]
    fn bar_fills_proportionally_with_partial_cells() {
        let t = Theme::new(ColorLevel::True, true);
        assert_eq!(bar(&t, 0.0, 8), (String::new(), "░".repeat(8)));
        assert_eq!(bar(&t, 1.0, 8), ("█".repeat(8), String::new()));
        let (on, off) = bar(&t, 0.5, 8);
        assert_eq!((on.as_str(), off.chars().count()), ("████", 4));
        // 0.5625 of 8 cells is 4.5 cells: four full blocks and a half block.
        let (on, off) = bar(&t, 0.5625, 8);
        assert_eq!(on, "████▌");
        assert_eq!(off.chars().count(), 3);
        for r in [0.0, 0.1, 0.33, 0.99, 1.0, 7.0, -3.0] {
            let (a, b) = bar(&t, r, 20);
            assert_eq!(a.chars().count() + b.chars().count(), 20, "ratio {r}");
        }
    }

    #[test]
    fn ascii_bar_uses_plain_characters() {
        let t = Theme::new(ColorLevel::True, false);
        let (on, off) = bar(&t, 0.5, 10);
        assert!(on.is_ascii() && off.is_ascii());
        assert_eq!(on.chars().count() + off.chars().count(), 10);
    }

    #[test]
    fn claws_cycle() {
        let frames: Vec<_> = (0..7).map(claws).collect();
        assert_eq!(frames[0], frames[6]);
        assert!(frames.iter().all(|f| f.len() == 3));
    }

    #[test]
    fn short_lines_are_split_around_the_match() {
        let (b, m, a) = match_window("admin@acme.corp:x", (6, 15), 40, '…');
        assert_eq!((b.as_str(), m.as_str(), a.as_str()), ("admin@", "acme.corp", ":x"));
    }

    #[test]
    fn long_lines_keep_the_match_visible() {
        let text = format!("{}needle{}", "a".repeat(100), "b".repeat(100));
        let (b, m, a) = match_window(&text, (100, 106), 40, '…');
        assert_eq!(m, "needle");
        assert_eq!(b.chars().count() + m.chars().count() + a.chars().count(), 40);
        assert!(b.starts_with('…') && a.ends_with('…'));
    }

    #[test]
    fn a_match_near_the_end_is_not_cut_off() {
        let text = format!("{}needle", "x".repeat(200));
        let (_, m, a) = match_window(&text, (200, 206), 30, '…');
        assert_eq!(m, "needle");
        assert_eq!(a, "");
    }

    #[test]
    fn multibyte_text_and_bad_spans_do_not_panic() {
        let text = "héllo wörld ñandú";
        let start = text.find("wörld").unwrap();
        let (_, m, _) = match_window(text, (start, start + "wörld".len()), 40, '…');
        assert_eq!(m, "wörld");
        // A span that splits a character, or points past the end, is ignored.
        let (b, m, a) = match_window(text, (2, 3), 40, '…');
        assert_eq!((b.as_str(), m.as_str()), ("", ""));
        assert!(a.starts_with('h'));
        let (_, m, _) = match_window(text, (0, 9999), 40, '…');
        assert_eq!(m, "");
    }

    #[test]
    fn control_characters_are_neutralised() {
        let (b, _, _) = match_window("a\tb\x1b[31mc", (0, 0), 40, '…');
        assert!(!b.chars().any(char::is_control));
    }

    #[test]
    fn centered_never_exceeds_the_area() {
        let r = centered(Rect::new(0, 0, 40, 10), 100, 100);
        assert_eq!((r.width, r.height), (40, 10));
        let r = centered(Rect::new(10, 5, 40, 10), 20, 4);
        assert_eq!((r.x, r.y), (20, 8));
    }
}
