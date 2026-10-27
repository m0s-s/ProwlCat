//! Modal boxes drawn over a screen: the help sheet and the one-line prompts.

use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Padding, Paragraph};
use ratatui::Frame;

use crate::app::{App, PromptKind};
use crate::input::Input;

use super::{centered, rounded};

pub fn help(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    let key = |k: &str| Span::styled(format!("{k:<16}"), t.amber());
    let row = |k: &str, what: &str| Line::from(vec![key(k), Span::raw(what.to_string())]);
    let head = |s: &str| Line::from(Span::styled(s.to_string(), t.amber_bold()));

    let lines = vec![
        head("Searching"),
        row("<target>", "email, domain, IP, CIDR; several separated by spaces"),
        row("<target> @dir", "search that folder, and remember it"),
        row("/type text", "plain substring search (the whole line is one target)"),
        row("up / down", "recent searches (or the command list after /)"),
        row("tab", "complete a /command or an @path"),
        Line::raw(""),
        head("Commands"),
        row("/path <dir>", "choose the corpus"),
        row("/index", "build or use a Tantivy index"),
        row("/stats", "count the corpus files by kind"),
        row("/export [file]", "write results as .json .csv or .ndjson"),
        row("/passwords <f>", "candidate passwords for encrypted zips"),
        row("/threads /max", "workers and matches per target"),
        Line::raw(""),
        head("Results"),
        row("up/down tab", "move, and switch between files and lines"),
        row("/  e  y", "filter, export, copy the line"),
        row("esc", "clear the filter, then go back"),
        Line::raw(""),
        Line::from(Span::styled("ctrl+c quits from anywhere. Scripts: prowlcat --ui cli ...", t.dim())),
    ];

    let height = lines.len() as u16 + 4;
    let boxed = centered(area, 80, height);
    f.render_widget(Clear, boxed);
    let block = rounded(t, true).padding(Padding::new(2, 2, 1, 1)).title(Line::from(vec![
        Span::styled(" Fang's cheat sheet ", t.amber_bold()),
    ]));
    let inner = block.inner(boxed);
    f.render_widget(block, boxed);
    f.render_widget(Paragraph::new(lines).style(t.base()), inner);
}

pub fn prompt(f: &mut Frame, app: &App, area: Rect, kind: PromptKind, input: &Input) {
    let t = &app.theme;
    let (title, help) = match kind {
        PromptKind::Export => (" Export results to ", "extension decides the format: .json .csv .ndjson"),
        PromptKind::IndexBuild => (" Build an index in ", "a new or empty directory"),
        PromptKind::IndexUse => (" Use the index in ", "a directory made by /index build"),
    };
    let boxed = centered(area, 72, 6);
    f.render_widget(Clear, boxed);
    let block = rounded(t, true).padding(Padding::horizontal(2)).title(Span::styled(title, t.amber_bold()));
    let inner = block.inner(boxed);
    f.render_widget(block, boxed);

    let prompt = format!("{} ", t.glyphs.prompt);
    let avail = (inner.width as usize).saturating_sub(prompt.chars().count() + 1).max(1);
    let cursor = input.cursor();
    let start = (cursor + 1).saturating_sub(avail);
    let shown: String = input.text().chars().skip(start).take(avail).collect();
    let lines = vec![
        Line::from(vec![Span::styled(prompt.clone(), t.amber()), Span::raw(shown)]),
        Line::raw(""),
        Line::from(Span::styled(help, t.dim())),
        Line::from(vec![
            Span::styled(format!(" {} ", super::key(t, "enter")), t.keycap()),
            Span::styled(" confirm   ", t.dim()),
            Span::styled(" esc ", t.keycap()),
            Span::styled(" cancel", t.dim()),
        ]),
    ];
    f.render_widget(Paragraph::new(lines), inner);
    let x = inner.x + prompt.chars().count() as u16 + (cursor - start) as u16;
    f.set_cursor_position((x.min(inner.right().saturating_sub(1)), inner.y));
}
