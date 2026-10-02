//! Live Wire, in a terminal: a near-black canvas, dim hairlines, mono
//! micro-labels, signal colours for state, and the rainbow only for the
//! composer and things happening right now.
//!
//! The colours are the dark theme of Chat with Work's design tokens. Without
//! true colour they are mapped to the 256-colour palette, and with `NO_COLOR`
//! set there is no colour at all, only bold, dim and reverse.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;

pub type Rgb = (u8, u8, u8);

pub const CANVAS: Rgb = (0x06, 0x07, 0x0B);
pub const SUNKEN: Rgb = (0x0A, 0x0C, 0x12);
pub const SURFACE: Rgb = (0x0F, 0x12, 0x1A);
pub const SELECTED: Rgb = (0x1A, 0x20, 0x30);
pub const INK: Rgb = (0xE9, 0xEC, 0xF4);
pub const MUTED: Rgb = (0x8D, 0x96, 0xAA);
pub const FAINT: Rgb = (0x63, 0x6B, 0x80);
pub const LINE: Rgb = (0x1C, 0x21, 0x30);
pub const LINE_STRONG: Rgb = (0x2A, 0x31, 0x44);
pub const POSITIVE: Rgb = (0x44, 0xFF, 0x9A);
pub const LIVE: Rgb = (0x44, 0xB0, 0xFF);
pub const ATTENTION: Rgb = (0xFF, 0xC2, 0x47);
pub const NEGATIVE: Rgb = (0xFF, 0x66, 0x44);
pub const NEGATIVE_INK: Rgb = (0xFF, 0x7F, 0x61);

/// `--rainbow`: green, blue, violet, orange, lime.
const RAINBOW: [(f32, Rgb); 5] = [
    (0.0, (0x44, 0xFF, 0x9A)),
    (0.2286, (0x44, 0xB0, 0xFF)),
    (0.4836, (0x8B, 0x44, 0xFF)),
    (0.7333, (0xFF, 0x66, 0x44)),
    (1.0, (0xEB, 0xFF, 0x70)),
];

/// What a colour means. Signals mark state only, never decoration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    Positive,
    Live,
    Attention,
    Negative,
    Idle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Depth {
    /// `NO_COLOR`: modifiers only.
    Mono,
    Ansi256,
    TrueColor,
}

#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub depth: Depth,
}

impl Theme {
    pub fn new(depth: Depth) -> Self {
        Self { depth }
    }

    /// Pick the colour depth from the environment.
    pub fn detect() -> Self {
        let var = |name: &str| std::env::var(name).unwrap_or_default();
        if !var("NO_COLOR").is_empty() {
            return Self::new(Depth::Mono);
        }
        let colorterm = var("COLORTERM").to_lowercase();
        let truecolor = colorterm == "truecolor"
            || colorterm == "24bit"
            // Windows Terminal supports it but doesn't say so.
            || !var("WT_SESSION").is_empty()
            || var("TERM").contains("direct");
        Self::new(if truecolor {
            Depth::TrueColor
        } else {
            Depth::Ansi256
        })
    }

    pub fn color(&self, rgb: Rgb) -> Option<Color> {
        match self.depth {
            Depth::Mono => None,
            Depth::TrueColor => Some(Color::Rgb(rgb.0, rgb.1, rgb.2)),
            Depth::Ansi256 => Some(Color::Indexed(ansi256(rgb))),
        }
    }

    pub fn fg(&self, rgb: Rgb) -> Style {
        self.color(rgb)
            .map_or_else(Style::new, |c| Style::new().fg(c))
    }

    fn bg(&self, rgb: Rgb) -> Style {
        self.color(rgb)
            .map_or_else(Style::new, |c| Style::new().bg(c))
    }

    fn mono(&self) -> bool {
        self.depth == Depth::Mono
    }

    /// The page: ink on the canvas.
    pub fn base(&self) -> Style {
        self.fg(INK).patch(self.bg(CANVAS))
    }

    /// The sidebar well.
    pub fn sunken(&self) -> Style {
        self.bg(SUNKEN)
    }

    /// Cards and dialogs.
    pub fn surface(&self) -> Style {
        self.bg(SURFACE)
    }

    pub fn selected(&self) -> Style {
        if self.mono() {
            Style::new().add_modifier(Modifier::REVERSED)
        } else {
            self.bg(SELECTED)
        }
    }

    pub fn ink(&self) -> Style {
        self.fg(INK)
    }

    pub fn strong(&self) -> Style {
        self.fg(INK).add_modifier(Modifier::BOLD)
    }

    pub fn muted(&self) -> Style {
        self.fg(MUTED)
    }

    pub fn faint(&self) -> Style {
        if self.mono() {
            Style::new().add_modifier(Modifier::DIM)
        } else {
            self.fg(FAINT)
        }
    }

    /// Inline and fenced code in answers.
    pub fn code(&self) -> Style {
        if self.mono() {
            Style::new().add_modifier(Modifier::REVERSED)
        } else {
            self.fg(INK).patch(self.bg(SELECTED))
        }
    }

    /// A question's bubble.
    pub fn bubble(&self) -> Style {
        if self.mono() {
            Style::new().add_modifier(Modifier::BOLD)
        } else {
            self.fg(INK).patch(self.bg(SURFACE))
        }
    }

    /// Small uppercase mono labels: `ROOTS`, `DAEMON`, `ACTIVITY`.
    pub fn micro(&self) -> Style {
        self.faint()
    }

    /// Hairlines between regions.
    pub fn line(&self) -> Style {
        if self.mono() {
            Style::new().add_modifier(Modifier::DIM)
        } else {
            self.fg(LINE_STRONG)
        }
    }

    /// A dot or glyph in a signal colour.
    pub fn signal(&self, signal: Signal) -> Style {
        match signal {
            Signal::Positive => self.fg(POSITIVE),
            Signal::Live => self.fg(LIVE),
            Signal::Attention => self.fg(ATTENTION),
            Signal::Negative => self.fg(NEGATIVE),
            Signal::Idle => self.faint(),
        }
    }

    /// A card that needs someone, tinted whole as the web does (`--attention-wash`,
    /// `--negative-wash`): the signal mixed into the surface. Never a stripe down
    /// one side.
    pub fn wash(&self, signal: Signal) -> Style {
        match signal {
            Signal::Attention => self.bg(mix(ATTENTION, SURFACE, 0.14)),
            Signal::Negative => self.bg(mix(NEGATIVE, SURFACE, 0.10)),
            Signal::Positive => self.bg(mix(POSITIVE, SURFACE, 0.08)),
            Signal::Live => self.bg(mix(LIVE, SURFACE, 0.10)),
            Signal::Idle => self.surface(),
        }
    }

    /// That card's ring (`--attention-edge`, `--negative-edge`): the signal
    /// mixed into the line colour.
    pub fn edge(&self, signal: Signal) -> Style {
        match signal {
            Signal::Attention => self.fg(mix(ATTENTION, LINE, 0.42)),
            Signal::Negative => self.fg(mix(NEGATIVE, LINE, 0.45)),
            Signal::Positive => self.fg(mix(POSITIVE, LINE, 0.42)),
            Signal::Live => self.fg(mix(LIVE, LINE, 0.42)),
            Signal::Idle => self.line(),
        }
    }

    /// Small text in a signal colour (the `-ink` variants).
    pub fn signal_ink(&self, signal: Signal) -> Style {
        match signal {
            Signal::Negative if self.mono() => Style::new().add_modifier(Modifier::BOLD),
            Signal::Attention if self.mono() => Style::new().add_modifier(Modifier::BOLD),
            Signal::Negative => self.fg(NEGATIVE_INK),
            Signal::Idle => self.muted(),
            other => self.signal(other),
        }
    }

    /// The rainbow at `t` (0 to 1) along its length.
    pub fn rainbow(&self, t: f32) -> Style {
        if self.mono() {
            return Style::new().add_modifier(Modifier::BOLD);
        }
        self.fg(rainbow_at(t))
    }

    /// The rainbow blended into the canvas, for a locked composer.
    pub fn rainbow_dim(&self, t: f32) -> Style {
        if self.mono() {
            return Style::new().add_modifier(Modifier::DIM);
        }
        self.fg(mix(rainbow_at(t), CANVAS, 0.45))
    }

    /// `text` in muted ink with a band of faint ink sweeping across it, the
    /// web's `.shimmer` for status words like "Thinking". `t` (0 to 1) is
    /// how far along its two-second sweep the band is.
    pub fn shimmer_text(&self, text: &str, t: f32) -> Vec<Span<'static>> {
        if self.mono() {
            return vec![Span::styled(text.to_string(), self.muted())];
        }
        let chars: Vec<char> = text.chars().collect();
        let width = chars.len().max(1) as f32;
        chars
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let k = shimmer_at(i as f32 + 0.5, width, t);
                Span::styled(c.to_string(), self.fg(mix(FAINT, MUTED, k)))
            })
            .collect()
    }

    /// `text` with the rainbow running across it.
    pub fn rainbow_text(&self, text: &str) -> Vec<Span<'static>> {
        let chars: Vec<char> = text.chars().collect();
        let last = chars.len().saturating_sub(1).max(1) as f32;
        chars
            .iter()
            .enumerate()
            .map(|(i, c)| Span::styled(c.to_string(), self.rainbow(i as f32 / last)))
            .collect()
    }
}

pub fn rainbow_at(t: f32) -> Rgb {
    let t = t.clamp(0.0, 1.0);
    for pair in RAINBOW.windows(2) {
        let ((t0, c0), (t1, c1)) = (pair[0], pair[1]);
        if t <= t1 {
            return mix(c1, c0, (t - t0) / (t1 - t0));
        }
    }
    RAINBOW[RAINBOW.len() - 1].1
}

/// How much of the shimmer's faint band covers `x` in text `width` wide,
/// `t` of the way through its sweep: 0 outside the band, 1 at its middle.
/// As the web draws it: a gradient twice the text's width, faint from 35%
/// to 65% and peaking at 50%, moving across in one sweep.
pub fn shimmer_at(x: f32, width: f32, t: f32) -> f32 {
    let width = width.max(1.0);
    let offset = -2.0 * width + 4.0 * width * t.rem_euclid(1.0);
    let u = ((x - offset) / (2.0 * width)).rem_euclid(1.0);
    if !(0.35..=0.65).contains(&u) {
        0.0
    } else if u < 0.5 {
        (u - 0.35) / 0.15
    } else {
        (0.65 - u) / 0.15
    }
}

/// `a` weighted by `amount`, the rest `b`.
fn mix(a: Rgb, b: Rgb, amount: f32) -> Rgb {
    let m = |x: u8, y: u8| (x as f32 * amount + y as f32 * (1.0 - amount)).round() as u8;
    (m(a.0, b.0), m(a.1, b.1), m(a.2, b.2))
}

/// The nearest xterm 256-colour index: the 6x6x6 cube or the grey ramp.
fn ansi256((r, g, b): Rgb) -> u8 {
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    let level = |v: u8| match v {
        0..48 => 0,
        48..115 => 1,
        _ => (v - 35) / 40,
    };
    let (qr, qg, qb) = (level(r), level(g), level(b));
    let cube = (
        LEVELS[qr as usize],
        LEVELS[qg as usize],
        LEVELS[qb as usize],
    );
    let avg = (r as u32 + g as u32 + b as u32) / 3;
    let step = if avg > 238 {
        23
    } else {
        avg.saturating_sub(3) / 10
    };
    let grey = (8 + 10 * step) as u8;
    let distance = |(x, y, z): Rgb| {
        let d = |p: u8, q: u8| (p as i32 - q as i32).pow(2);
        d(x, r) + d(y, g) + d(z, b)
    };
    if distance((grey, grey, grey)) < distance(cube) {
        232 + step as u8
    } else {
        16 + 36 * qr + 6 * qg + qb
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rainbow_runs_from_green_to_lime() {
        assert_eq!(rainbow_at(0.0), (0x44, 0xFF, 0x9A));
        assert_eq!(rainbow_at(1.0), (0xEB, 0xFF, 0x70));
        assert_eq!(rainbow_at(0.4836), (0x8B, 0x44, 0xFF));
    }

    #[test]
    fn the_shimmer_band_sweeps_across() {
        // Somewhere along the sweep each letter is under the band, and
        // the band moves on rather than staying put.
        let width = 8.0;
        let at = |x: f32| {
            (0..100)
                .map(|i| shimmer_at(x, width, i as f32 / 100.0))
                .fold(0.0, f32::max)
        };
        for x in 0..8 {
            assert!(at(x as f32 + 0.5) > 0.9, "letter {x}");
        }
        assert_ne!(shimmer_at(0.5, width, 0.3), shimmer_at(0.5, width, 0.4));
        let theme = Theme::new(Depth::TrueColor);
        assert_eq!(theme.shimmer_text("Thinking", 0.0).len(), 8);
        assert_eq!(
            Theme::new(Depth::Mono).shimmer_text("Thinking", 0.0).len(),
            1
        );
    }

    #[test]
    fn maps_to_256_colours() {
        assert_eq!(ansi256((0, 0, 0)), 16);
        assert_eq!(ansi256((255, 255, 255)), 231);
        assert_eq!(ansi256(CANVAS), 232);
        assert_eq!(ansi256((0xFF, 0x66, 0x44)), 203);
    }

    #[test]
    fn no_color_means_no_colour() {
        let theme = Theme::new(Depth::Mono);
        assert_eq!(theme.base(), Style::new());
        assert_eq!(theme.signal(Signal::Positive), Style::new());
        assert!(theme.selected().add_modifier.contains(Modifier::REVERSED));
    }
}
