//! Night Eyes: amber on black. One accent colour, a graphite panther, and a fallback for every
//! terminal that cannot show it (256 colours, 16 colours, `NO_COLOR`, non-UTF-8 locales).

use ratatui::style::{Color, Modifier, Style};

/// How many colours the terminal can show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorLevel {
    True,
    Ansi256,
    Ansi16,
    /// No colour at all (`NO_COLOR`, `TERM=dumb`); hierarchy comes from bold, dim and reverse.
    None,
}

impl ColorLevel {
    pub fn detect() -> Self {
        Self::from_env(|k| std::env::var(k).ok())
    }

    /// `PROWLCAT_COLOR` (`truecolor`, `256`, `16`, `none`) wins, then `NO_COLOR`, then the usual
    /// `COLORTERM` / `TERM` hints. Windows terminals are assumed to be truecolor-capable.
    pub fn from_env(get: impl Fn(&str) -> Option<String>) -> Self {
        if let Some(v) = get("PROWLCAT_COLOR") {
            match v.to_ascii_lowercase().as_str() {
                "truecolor" | "24bit" | "true" => return Self::True,
                "256" => return Self::Ansi256,
                "16" | "ansi" => return Self::Ansi16,
                "none" | "off" | "never" | "0" => return Self::None,
                _ => {}
            }
        }
        if get("NO_COLOR").is_some_and(|v| !v.is_empty()) {
            return Self::None;
        }
        let term = get("TERM").unwrap_or_default();
        if term == "dumb" {
            return Self::None;
        }
        let colorterm = get("COLORTERM").unwrap_or_default().to_ascii_lowercase();
        if colorterm.contains("truecolor") || colorterm.contains("24bit") {
            return Self::True;
        }
        if get("WT_SESSION").is_some() || cfg!(windows) {
            return Self::True;
        }
        if term.contains("256color") {
            Self::Ansi256
        } else {
            Self::Ansi16
        }
    }
}

/// Whether box-drawing, block and symbol glyphs can be used.
pub fn detect_unicode() -> bool {
    detect_unicode_from(|k| std::env::var(k).ok())
}

pub fn detect_unicode_from(get: impl Fn(&str) -> Option<String>) -> bool {
    if get("PROWLCAT_ASCII").is_some_and(|v| !v.is_empty() && v != "0") {
        return false;
    }
    // The first locale variable that is set decides; none set means "assume UTF-8".
    for key in ["LC_ALL", "LC_CTYPE", "LANG"] {
        if let Some(v) = get(key).filter(|v| !v.is_empty()) {
            let v = v.to_ascii_uppercase();
            return v.contains("UTF-8") || v.contains("UTF8");
        }
    }
    true
}

/// Symbols used across the UI, with a plain-ASCII twin for each.
#[derive(Debug, Clone, Copy)]
pub struct Glyphs {
    pub prompt: &'static str,
    pub pointer: &'static str,
    pub ok: &'static str,
    pub miss: &'static str,
    pub dot: &'static str,
    pub mode: &'static str,
    pub sep: &'static str,
    pub arrow: &'static str,
    pub ellipsis: &'static str,
    pub bar_empty: &'static str,
    pub bar_full: &'static str,
    pub partial: [&'static str; 8],
    pub spark: &'static str,
}

impl Glyphs {
    pub const UNICODE: Glyphs = Glyphs {
        prompt: "❯",
        pointer: "▸",
        ok: "✓",
        miss: "✗",
        dot: "●",
        mode: "▌",
        sep: "·",
        arrow: "→",
        ellipsis: "…",
        bar_empty: "░",
        bar_full: "█",
        partial: [" ", "▏", "▎", "▍", "▌", "▋", "▊", "▉"],
        spark: "▇",
    };
    pub const ASCII: Glyphs = Glyphs {
        prompt: ">",
        pointer: ">",
        ok: "+",
        miss: "x",
        dot: "*",
        mode: "|",
        sep: "-",
        arrow: "->",
        ellipsis: "...",
        bar_empty: "-",
        bar_full: "#",
        partial: [" ", "=", "=", "=", "=", "=", "=", "="],
        spark: "#",
    };
}

#[derive(Debug, Clone, Copy)]
struct Rgb(u8, u8, u8);

// Night Eyes palette (the design canvas, variant A).
const NIGHT: Rgb = Rgb(0x0D, 0x0E, 0x12);
const BAR: Rgb = Rgb(0x14, 0x16, 0x1C);
const LINE: Rgb = Rgb(0x26, 0x29, 0x32);
const LINE_STRONG: Rgb = Rgb(0x34, 0x38, 0x45);
const RAISED: Rgb = Rgb(0x1F, 0x23, 0x30);
const KBD: Rgb = Rgb(0x1A, 0x1D, 0x25);
const TEXT: Rgb = Rgb(0xD7, 0xDA, 0xE0);
const DIM: Rgb = Rgb(0x8A, 0x8F, 0x9F);
const AMBER: Rgb = Rgb(0xF7, 0xB7, 0x31);
const CYAN: Rgb = Rgb(0x62, 0xD2, 0xE8);
const GREEN: Rgb = Rgb(0x7B, 0xD8, 0x8F);
const RED: Rgb = Rgb(0xFF, 0x6B, 0x81);

#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub level: ColorLevel,
    pub unicode: bool,
    pub glyphs: Glyphs,
    pub bg: Color,
    pub bar: Color,
    pub line: Color,
    pub line_strong: Color,
    pub raised: Color,
    pub kbd: Color,
    pub text: Color,
    pub dim: Color,
    pub amber: Color,
    pub cyan: Color,
    pub green: Color,
    pub red: Color,
}

impl Theme {
    pub fn detect() -> Self {
        Self::new(ColorLevel::detect(), detect_unicode())
    }

    pub fn new(level: ColorLevel, unicode: bool) -> Self {
        let c = |rgb: Rgb| to_color(level, rgb);
        Self {
            level,
            unicode,
            glyphs: if unicode { Glyphs::UNICODE } else { Glyphs::ASCII },
            bg: c(NIGHT),
            bar: c(BAR),
            line: c(LINE),
            line_strong: c(LINE_STRONG),
            raised: c(RAISED),
            kbd: c(KBD),
            text: c(TEXT),
            dim: c(DIM),
            amber: c(AMBER),
            cyan: c(CYAN),
            green: c(GREEN),
            red: c(RED),
        }
    }

    pub fn mono(&self) -> bool {
        self.level == ColorLevel::None
    }

    /// Map an arbitrary colour (the mascot palette) to what this terminal can show.
    pub fn rgb(&self, r: u8, g: u8, b: u8) -> Color {
        to_color(self.level, Rgb(r, g, b))
    }

    pub fn base(&self) -> Style {
        Style::new().fg(self.text).bg(self.bg)
    }

    pub fn dim(&self) -> Style {
        if self.mono() {
            Style::new().add_modifier(Modifier::DIM)
        } else {
            Style::new().fg(self.dim)
        }
    }

    pub fn amber(&self) -> Style {
        if self.mono() {
            Style::new().add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(self.amber)
        }
    }

    pub fn amber_bold(&self) -> Style {
        self.amber().add_modifier(Modifier::BOLD)
    }

    pub fn cyan(&self) -> Style {
        Style::new().fg(self.cyan)
    }

    pub fn green(&self) -> Style {
        Style::new().fg(self.green)
    }

    pub fn red(&self) -> Style {
        Style::new().fg(self.red)
    }

    pub fn bold(&self) -> Style {
        Style::new().add_modifier(Modifier::BOLD)
    }

    /// The selected row: a raised fill with the text in amber (reverse video without colour).
    pub fn selected(&self) -> Style {
        if self.mono() {
            Style::new().add_modifier(Modifier::REVERSED)
        } else {
            Style::new().fg(self.amber).bg(self.raised)
        }
    }

    pub fn status_bar(&self) -> Style {
        if self.mono() {
            Style::new().add_modifier(Modifier::REVERSED)
        } else {
            Style::new().fg(self.text).bg(self.bar)
        }
    }

    /// Key caps such as `esc` and `↵`.
    pub fn keycap(&self) -> Style {
        if self.mono() {
            Style::new().add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(self.text).bg(self.kbd)
        }
    }

    pub fn border(&self, active: bool) -> Style {
        if self.mono() {
            Style::new()
        } else if active {
            Style::new().fg(self.amber)
        } else {
            Style::new().fg(self.line_strong)
        }
    }

    pub fn border_soft(&self) -> Style {
        if self.mono() {
            Style::new().add_modifier(Modifier::DIM)
        } else {
            Style::new().fg(self.line)
        }
    }
}

fn to_color(level: ColorLevel, rgb: Rgb) -> Color {
    match level {
        ColorLevel::True => Color::Rgb(rgb.0, rgb.1, rgb.2),
        ColorLevel::Ansi256 => Color::Indexed(nearest_256(rgb)),
        ColorLevel::Ansi16 => nearest_16(rgb),
        ColorLevel::None => Color::Reset,
    }
}

fn dist(a: Rgb, b: Rgb) -> u32 {
    let d = |x: u8, y: u8| (x as i32 - y as i32).pow(2) as u32;
    d(a.0, b.0) + d(a.1, b.1) + d(a.2, b.2)
}

/// Nearest entry of the xterm 256-colour palette (6x6x6 cube or the grey ramp).
fn nearest_256(c: Rgb) -> u8 {
    const STEPS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    let near = |v: u8| {
        STEPS
            .iter()
            .enumerate()
            .min_by_key(|(_, s)| (**s as i32 - v as i32).abs())
            .map(|(i, _)| i)
            .unwrap_or(0)
    };
    let (ri, gi, bi) = (near(c.0), near(c.1), near(c.2));
    let cube = Rgb(STEPS[ri], STEPS[gi], STEPS[bi]);
    let cube_idx = 16 + 36 * ri as u8 + 6 * gi as u8 + bi as u8;

    let avg = ((c.0 as u32 + c.1 as u32 + c.2 as u32) / 3) as i32;
    let step = ((avg - 8).max(0) / 10).min(23) as u8;
    let g = (8 + 10 * step as i32) as u8;
    let grey = Rgb(g, g, g);
    let grey_idx = 232 + step;

    if dist(c, grey) < dist(c, cube) {
        grey_idx
    } else {
        cube_idx
    }
}

fn nearest_16(c: Rgb) -> Color {
    const TABLE: [(Rgb, Color); 16] = [
        (Rgb(0, 0, 0), Color::Black),
        (Rgb(205, 0, 0), Color::Red),
        (Rgb(0, 205, 0), Color::Green),
        (Rgb(205, 205, 0), Color::Yellow),
        (Rgb(0, 0, 238), Color::Blue),
        (Rgb(205, 0, 205), Color::Magenta),
        (Rgb(0, 205, 205), Color::Cyan),
        (Rgb(229, 229, 229), Color::Gray),
        (Rgb(127, 127, 127), Color::DarkGray),
        (Rgb(255, 0, 0), Color::LightRed),
        (Rgb(0, 255, 0), Color::LightGreen),
        (Rgb(255, 255, 0), Color::LightYellow),
        (Rgb(92, 92, 255), Color::LightBlue),
        (Rgb(255, 0, 255), Color::LightMagenta),
        (Rgb(0, 255, 255), Color::LightCyan),
        (Rgb(255, 255, 255), Color::White),
    ];
    TABLE
        .iter()
        .min_by_key(|(rgb, _)| dist(c, *rgb))
        .map(|(_, color)| *color)
        .unwrap_or(Color::Reset)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |k| pairs.iter().find(|(key, _)| *key == k).map(|(_, v)| v.to_string())
    }

    #[test]
    fn no_color_wins_over_colorterm() {
        let level = ColorLevel::from_env(env(&[("NO_COLOR", "1"), ("COLORTERM", "truecolor")]));
        assert_eq!(level, ColorLevel::None);
    }

    #[test]
    fn empty_no_color_is_ignored() {
        let level = ColorLevel::from_env(env(&[("NO_COLOR", ""), ("COLORTERM", "truecolor")]));
        assert_eq!(level, ColorLevel::True);
    }

    #[test]
    fn explicit_override_beats_everything() {
        let level = ColorLevel::from_env(env(&[("PROWLCAT_COLOR", "256"), ("NO_COLOR", "1")]));
        assert_eq!(level, ColorLevel::Ansi256);
    }

    #[test]
    fn dumb_terminal_has_no_colour() {
        assert_eq!(ColorLevel::from_env(env(&[("TERM", "dumb")])), ColorLevel::None);
    }

    #[test]
    fn locale_decides_unicode() {
        assert!(detect_unicode_from(env(&[("LANG", "en_US.UTF-8")])));
        assert!(!detect_unicode_from(env(&[("LANG", "C")])));
        assert!(!detect_unicode_from(env(&[("LC_ALL", "POSIX"), ("LANG", "en_US.UTF-8")])));
        assert!(detect_unicode_from(env(&[])));
        assert!(!detect_unicode_from(env(&[("PROWLCAT_ASCII", "1")])));
    }

    #[test]
    fn amber_maps_sensibly_at_every_depth() {
        let amber = Rgb(0xF7, 0xB7, 0x31);
        assert!(matches!(to_color(ColorLevel::True, amber), Color::Rgb(0xF7, 0xB7, 0x31)));
        // 215 is the xterm light orange (255,175,95): the closest cube entry.
        assert!(matches!(to_color(ColorLevel::Ansi256, amber), Color::Indexed(215)));
        assert!(matches!(to_color(ColorLevel::Ansi16, amber), Color::Yellow | Color::LightYellow));
        assert!(matches!(to_color(ColorLevel::None, amber), Color::Reset));
    }

    #[test]
    fn night_background_is_black_on_16_colours() {
        assert!(matches!(to_color(ColorLevel::Ansi16, NIGHT), Color::Black));
        // On the 256 palette it lands on the darkest grey, not on a coloured cube cell.
        assert!(matches!(to_color(ColorLevel::Ansi256, NIGHT), Color::Indexed(232) | Color::Indexed(233) | Color::Indexed(16)));
    }

    #[test]
    fn mono_theme_uses_modifiers() {
        let t = Theme::new(ColorLevel::None, true);
        assert!(t.mono());
        assert!(t.selected().add_modifier.contains(Modifier::REVERSED));
        assert!(t.dim().add_modifier.contains(Modifier::DIM));
        assert_eq!(t.base().fg, Some(Color::Reset));
    }
}
