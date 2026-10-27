//! Fang, ProwlCat's mascot: a snarling panther drawn in half-block pixels, with a mood for every
//! moment of a search. All motion is derived from a global tick counter, so rendering stays a
//! pure function of `(pose, tick)` and is easy to test.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::mascot_data;
use crate::theme::Theme;

/// What Fang is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mood {
    /// Waiting at the prompt: mouth shut, eyes low, blinking now and then.
    Idle,
    /// A scan is running: snarling, eyes pulsing, creeping forward.
    Prowl,
    /// Targets were found: a short pounce, then eyes white-hot.
    Found,
    /// Nothing matched: mouth shut, eyes dim red.
    Miss,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Size {
    /// 24 x 10 cells, for banners and the scan header.
    Small,
    /// 48 x 20 cells, for the splash screen.
    Big,
}

impl Size {
    pub fn cells(self) -> (u16, u16) {
        match self {
            Size::Small => (24, 10),
            Size::Big => (48, 20),
        }
    }

    fn grid(self) -> &'static [&'static str] {
        match self {
            Size::Small => &mascot_data::SMALL,
            Size::Big => &mascot_data::BIG,
        }
    }
}

/// Everything that decides how Fang looks on one frame.
#[derive(Debug, Clone, Copy)]
pub struct Pose {
    pub mood: Mood,
    /// Global animation tick (100 ms each).
    pub tick: u64,
    /// Tick at which the current mood began; drives one-shot motion such as the pounce.
    pub since: u64,
    /// A fresh hit just arrived: eyes flash white for this frame.
    pub flash: bool,
    /// Eyes fading in, `0` (dark) to `FADE_STEPS` (fully lit). `None` is fully lit.
    pub wake: Option<u8>,
    /// Hold the first frame instead of animating (`--no-animation`).
    pub still: bool,
}

pub const FADE_STEPS: u8 = 6;

impl Pose {
    pub fn new(mood: Mood, tick: u64, since: u64) -> Self {
        Self { mood, tick, since, flash: false, wake: None, still: false }
    }

    pub fn flashing(mut self, on: bool) -> Self {
        self.flash = on;
        self
    }

    pub fn waking(mut self, step: u8) -> Self {
        self.wake = Some(step.min(FADE_STEPS));
        self
    }

    pub fn frozen(mut self, still: bool) -> Self {
        self.still = still;
        self
    }

    /// The tick everything below is computed from.
    fn t(&self) -> u64 {
        if self.still {
            0
        } else {
            self.tick
        }
    }

    /// Vertical offset in pixels (negative is up). Fang breathes when idle, creeps while
    /// prowling, and leaps when a search succeeds.
    pub fn lift(&self) -> i32 {
        if self.still {
            return 0;
        }
        let t = self.t();
        match self.mood {
            Mood::Idle => ((t / 12) % 2) as i32,
            Mood::Prowl => i32::from((t / 3) % 4 == 2),
            Mood::Found => match t.saturating_sub(self.since) {
                0 => -3,
                1 => -2,
                2 => -1,
                _ => 0,
            },
            Mood::Miss => 0,
        }
    }

    pub fn mouth_open(&self) -> bool {
        matches!(self.mood, Mood::Prowl | Mood::Found)
    }

    /// Both eyes closed for a moment (idle only: a blink every three seconds).
    pub fn blinking(&self) -> bool {
        self.mood == Mood::Idle && !self.still && matches!(self.t() % 30, 28 | 29)
    }

    /// Eye colour as RGB.
    pub fn eye_rgb(&self) -> (u8, u8, u8) {
        let t = self.t();
        let lit = if self.flash {
            (0xFF, 0xFF, 0xFF)
        } else {
            match self.mood {
                Mood::Idle => (0xC9, 0x97, 0x2B),
                Mood::Prowl => {
                    if (t / 3).is_multiple_of(2) {
                        (0xF7, 0xB7, 0x31)
                    } else {
                        (0xFF, 0xD8, 0x66)
                    }
                }
                Mood::Found => {
                    // Flicker for about a second, then hold.
                    let age = t.saturating_sub(self.since);
                    if age < 10 && age % 2 == 1 {
                        (0xFF, 0xFF, 0xFF)
                    } else {
                        (0xFF, 0xF3, 0xC4)
                    }
                }
                Mood::Miss => (0x8C, 0x33, 0x42),
            }
        };
        match self.wake {
            Some(step) if step < FADE_STEPS => {
                // Fade in from the body colour.
                let from = BODY;
                let k = step as f32 / FADE_STEPS as f32;
                let mix = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * k).round() as u8;
                (mix(from.0, lit.0), mix(from.1, lit.1), mix(from.2, lit.2))
            }
            _ => lit,
        }
    }
}

const BODY: (u8, u8, u8) = (0x2A, 0x2F, 0x3B);
const RIM: (u8, u8, u8) = (0x4A, 0x53, 0x68);
const DARK: (u8, u8, u8) = (0x10, 0x12, 0x1A);
const NOSE: (u8, u8, u8) = (0xE0, 0x53, 0x6B);
const MOUTH: (u8, u8, u8) = (0x2A, 0x0A, 0x12);
const FANG: (u8, u8, u8) = (0xF4, 0xF6, 0xFA);

/// Render Fang as lines of styled half blocks. `ground` is the colour behind the mascot.
pub fn render(size: Size, pose: Pose, theme: &Theme, ground: Color) -> Vec<Line<'static>> {
    if !theme.unicode {
        return render_ascii(pose, theme);
    }
    if theme.mono() {
        return render_mono(size, pose);
    }

    let grid = size.grid();
    let lift = pose.lift();
    let eye = pose.eye_rgb();
    let blink = pose.blinking();
    let open = pose.mouth_open();

    let sample = |x: usize, y: i32| -> u8 {
        let y = y - lift;
        if y < 0 || y as usize >= grid.len() {
            return 0;
        }
        grid[y as usize].as_bytes().get(x).map_or(0, |b| b - b'0')
    };
    let colour = |x: usize, y: i32| -> Color {
        let rgb = match sample(x, y) {
            0 => return ground,
            1 => {
                if sample(x, y - 1) == 0 {
                    RIM
                } else {
                    BODY
                }
            }
            3 => {
                if blink {
                    BODY
                } else {
                    eye
                }
            }
            4 => DARK,
            6 => NOSE,
            7 => {
                if open {
                    MOUTH
                } else {
                    BODY
                }
            }
            5 => {
                if open {
                    FANG
                } else {
                    BODY
                }
            }
            _ => BODY,
        };
        theme.rgb(rgb.0, rgb.1, rgb.2)
    };

    let cols = grid[0].len();
    (0..grid.len() as i32 / 2)
        .map(|row| {
            let mut spans: Vec<Span<'static>> = Vec::new();
            let mut run = String::new();
            let mut run_style = Style::new();
            for x in 0..cols {
                let style = Style::new()
                    .fg(colour(x, row * 2))
                    .bg(colour(x, row * 2 + 1));
                if style != run_style && !run.is_empty() {
                    spans.push(Span::styled(std::mem::take(&mut run), run_style));
                }
                run_style = style;
                run.push('▀');
            }
            if !run.is_empty() {
                spans.push(Span::styled(run, run_style));
            }
            Line::from(spans)
        })
        .collect()
}

/// No colour available: keep the silhouette, show the eyes as bold diamonds.
fn render_mono(size: Size, pose: Pose) -> Vec<Line<'static>> {
    let grid = size.grid();
    let lift = pose.lift();
    let sample = |x: usize, y: i32| -> u8 {
        let y = y - lift;
        if y < 0 || y as usize >= grid.len() {
            return 0;
        }
        grid[y as usize].as_bytes().get(x).map_or(0, |b| b - b'0')
    };
    let solid = |c: u8| !matches!(c, 0 | 4 | 7);
    (0..grid.len() as i32 / 2)
        .map(|row| {
            let spans: Vec<Span<'static>> = (0..grid[0].len())
                .map(|x| {
                    let (top, bottom) = (sample(x, row * 2), sample(x, row * 2 + 1));
                    if (top == 3 || bottom == 3) && !pose.blinking() {
                        return Span::styled("◆", Style::new().add_modifier(Modifier::BOLD));
                    }
                    let ch = match (solid(top), solid(bottom)) {
                        (true, true) => "█",
                        (true, false) => "▀",
                        (false, true) => "▄",
                        (false, false) => " ",
                    };
                    Span::raw(ch)
                })
                .collect();
            Line::from(spans)
        })
        .collect()
}

/// Plain-ASCII Fang for locales without block characters. `@` marks the eyes.
const ASCII: [&str; 6] = [
    "  /\\_____/\\  ",
    " /  \\\\ //  \\ ",
    "|   @   @   |",
    " \\    ^    / ",
    "  \\ VvvvV /  ",
    "   \\_____/   ",
];

fn render_ascii(pose: Pose, theme: &Theme) -> Vec<Line<'static>> {
    let eye_glyph = if pose.blinking() {
        "-"
    } else {
        match pose.mood {
            Mood::Idle => "o",
            Mood::Prowl => {
                if (pose.t() / 3).is_multiple_of(2) {
                    "o"
                } else {
                    "O"
                }
            }
            Mood::Found => "*",
            Mood::Miss => "x",
        }
    };
    let (r, g, b) = pose.eye_rgb();
    let eye_style = if theme.mono() {
        Style::new().add_modifier(Modifier::BOLD)
    } else {
        Style::new().fg(theme.rgb(r, g, b))
    };
    ASCII
        .iter()
        .map(|row| {
            let mut spans = Vec::new();
            for (i, part) in row.split('@').enumerate() {
                if i > 0 {
                    spans.push(Span::styled(eye_glyph, eye_style));
                }
                spans.push(Span::raw(part.to_string()));
            }
            Line::from(spans)
        })
        .collect()
}

/// A one-line Fang for terminals too short for the full mascot.
pub fn eyes_line(pose: Pose, theme: &Theme) -> Line<'static> {
    let (r, g, b) = pose.eye_rgb();
    let style = if theme.mono() {
        Style::new().add_modifier(Modifier::BOLD)
    } else {
        Style::new().fg(theme.rgb(r, g, b))
    };
    let (l, rr) = if theme.unicode { ("◢", "◣") } else { ("<", ">") };
    if pose.blinking() {
        Line::from(Span::styled(if theme.unicode { "▬ ▬" } else { "- -" }, style))
    } else {
        Line::from(vec![Span::styled(l, style), Span::raw(" "), Span::styled(rr, style)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::ColorLevel;

    fn theme() -> Theme {
        Theme::new(ColorLevel::True, true)
    }

    #[test]
    fn grids_have_consistent_dimensions() {
        for size in [Size::Small, Size::Big] {
            let grid = size.grid();
            let (cols, rows) = size.cells();
            assert_eq!(grid.len(), rows as usize * 2);
            assert!(grid.iter().all(|r| r.len() == cols as usize), "ragged grid");
            assert!(grid.iter().all(|r| r.bytes().all(|b| matches!(b, b'0'..=b'7'))));
        }
    }

    #[test]
    fn grids_are_left_right_symmetric() {
        for size in [Size::Small, Size::Big] {
            for row in size.grid() {
                let b = row.as_bytes();
                let mirrored: Vec<u8> = b.iter().rev().copied().collect();
                assert_eq!(b, mirrored.as_slice(), "row {row} is not symmetric");
            }
        }
    }

    #[test]
    fn every_feature_is_present() {
        // The nose is under one pixel wide at 24 x 20, so only the big drawing has it.
        let small: String = mascot_data::SMALL.concat();
        for class in ['1', '3', '4', '5', '7'] {
            assert!(small.contains(class), "class {class} missing from the small grid");
        }
        let big: String = mascot_data::BIG.concat();
        for class in ['1', '3', '4', '5', '6', '7'] {
            assert!(big.contains(class), "class {class} missing from the big grid");
        }
    }

    #[test]
    fn render_matches_declared_size() {
        for size in [Size::Small, Size::Big] {
            let lines = render(size, Pose::new(Mood::Prowl, 0, 0), &theme(), Color::Black);
            let (cols, rows) = size.cells();
            assert_eq!(lines.len(), rows as usize);
            for line in &lines {
                assert_eq!(line.width(), cols as usize);
            }
        }
    }

    #[test]
    fn eye_colour_follows_the_mood() {
        let idle = Pose::new(Mood::Idle, 0, 0).eye_rgb();
        let prowl = Pose::new(Mood::Prowl, 0, 0).eye_rgb();
        let found = Pose::new(Mood::Found, 20, 0).eye_rgb();
        let miss = Pose::new(Mood::Miss, 0, 0).eye_rgb();
        assert_eq!(prowl, (0xF7, 0xB7, 0x31));
        assert_ne!(idle, prowl);
        assert_eq!(found, (0xFF, 0xF3, 0xC4));
        assert!(miss.0 > miss.1, "no-match eyes are red");
    }

    #[test]
    fn prowl_pulses_between_two_ambers() {
        let a = Pose::new(Mood::Prowl, 0, 0).eye_rgb();
        let b = Pose::new(Mood::Prowl, 3, 0).eye_rgb();
        assert_ne!(a, b);
        assert_eq!(a, Pose::new(Mood::Prowl, 6, 0).eye_rgb());
    }

    #[test]
    fn flash_overrides_the_mood() {
        assert_eq!(Pose::new(Mood::Prowl, 0, 0).flashing(true).eye_rgb(), (255, 255, 255));
    }

    #[test]
    fn found_pounces_then_settles() {
        let lifts: Vec<i32> = (0..5).map(|t| Pose::new(Mood::Found, 10 + t, 10).lift()).collect();
        assert_eq!(lifts, vec![-3, -2, -1, 0, 0]);
    }

    #[test]
    fn idle_blinks_briefly_every_three_seconds() {
        let blinks: Vec<u64> = (0..60).filter(|&t| Pose::new(Mood::Idle, t, 0).blinking()).collect();
        assert_eq!(blinks, vec![28, 29, 58, 59]);
        assert!(!Pose::new(Mood::Prowl, 28, 0).blinking());
    }

    #[test]
    fn idle_mouth_is_shut_and_prowl_mouth_is_open() {
        let row = |pose: Pose| -> String {
            render(Size::Small, pose, &theme(), Color::Black)
                .iter()
                .flat_map(|l| l.spans.iter())
                .map(|s| format!("{:?}{:?}", s.style.fg, s.style.bg))
                .collect()
        };
        assert_ne!(row(Pose::new(Mood::Idle, 0, 0)), row(Pose::new(Mood::Prowl, 0, 0)));
        assert!(Pose::new(Mood::Prowl, 0, 0).mouth_open());
        assert!(!Pose::new(Mood::Miss, 0, 0).mouth_open());
    }

    #[test]
    fn frozen_pose_never_moves() {
        let a = Pose::new(Mood::Prowl, 0, 0).frozen(true);
        let b = Pose::new(Mood::Prowl, 99, 0).frozen(true);
        assert_eq!(a.lift(), b.lift());
        assert_eq!(a.eye_rgb(), b.eye_rgb());
    }

    #[test]
    fn waking_fades_in_from_the_body_colour() {
        let dark = Pose::new(Mood::Prowl, 0, 0).waking(0).eye_rgb();
        let lit = Pose::new(Mood::Prowl, 0, 0).waking(FADE_STEPS).eye_rgb();
        assert_eq!(dark, BODY);
        assert_eq!(lit, (0xF7, 0xB7, 0x31));
    }

    #[test]
    fn mono_and_ascii_fallbacks_render() {
        let mono = Theme::new(ColorLevel::None, true);
        let lines = render(Size::Small, Pose::new(Mood::Idle, 0, 0), &mono, Color::Reset);
        assert_eq!(lines.len(), 10);
        let text: String = lines.iter().map(|l| l.to_string()).collect();
        assert!(text.contains('◆'), "eyes survive without colour");

        let ascii = Theme::new(ColorLevel::True, false);
        let lines = render(Size::Small, Pose::new(Mood::Found, 0, 0), &ascii, Color::Reset);
        let text: String = lines.iter().map(|l| l.to_string()).collect();
        assert!(text.contains('*'));
        assert!(text.is_ascii());
    }
}
