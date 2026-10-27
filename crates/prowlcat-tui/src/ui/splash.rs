//! The intro: Fang's eyes open in the dark, then the name is spelled out. Any key skips it.

use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::app::App;
use crate::mascot::{self, Size};

const NAME: &str = "PROWLCAT";
/// First tick at which letters start to appear.
const NAME_FROM: u64 = 6;
const TAGLINE_FROM: u64 = 15;

/// The big mascot plus two lines of text need this much room.
pub fn fits(area: Rect) -> bool {
    let (cols, rows) = Size::Big.cells();
    area.width >= cols + 4 && area.height >= rows + 6
}

pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    let (cols, rows) = Size::Big.cells();
    let total = rows + 4;
    let top = area.y + area.height.saturating_sub(total) / 2;
    let left = area.x + area.width.saturating_sub(cols) / 2;

    let lines = mascot::render(Size::Big, app.pose(), t, t.bg);
    f.render_widget(Paragraph::new(lines), Rect { x: left, y: top, width: cols, height: rows });

    let letters = (app.tick.saturating_sub(NAME_FROM) as usize).min(NAME.len());
    let name: String = NAME.chars().take(letters).collect::<Vec<_>>().iter().map(|c| format!("{c} ")).collect();
    let mut text = vec![Line::raw(""), Line::from(Span::styled(name.trim_end().to_string(), t.amber_bold())).centered()];
    if app.tick >= TAGLINE_FROM {
        text.push(
            Line::from(vec![Span::styled("/// ", t.amber_bold()), Span::styled("local-first breach corpus search", t.dim())]).centered(),
        );
    }
    f.render_widget(
        Paragraph::new(text),
        Rect { x: area.x, y: top + rows, width: area.width, height: 4.min(area.bottom().saturating_sub(top + rows)) },
    );
}
