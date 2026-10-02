//! Live Wire, the web app's design tokens (`app/assets/tailwind/base/tokens.css`
//! in Chat with Work), as egui values: colors for light and dark, radii,
//! durations and easing curves, the rainbow, and the type.

use egui::epaint::text::VariationCoords;
use egui::{Color32, FontFamily, FontId, TextFormat};

/// The interface's face, Geist, and its labels' face, Geist Mono. Both are
/// variable fonts; runs pick their weight with the `wght` axis.
pub const SANS: &str = "geist";
pub const MONO: &str = "geist-mono";

const fn rgb(hex: u32) -> Color32 {
    Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

/// The colors of one theme.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Palette {
    pub dark: bool,
    pub canvas: Color32,
    pub canvas_sunken: Color32,
    pub surface: Color32,
    pub surface_raised: Color32,
    pub surface_hover: Color32,
    pub surface_selected: Color32,
    pub ink: Color32,
    pub ink_muted: Color32,
    pub ink_faint: Color32,
    pub ink_inverted: Color32,
    pub line: Color32,
    pub line_strong: Color32,
    /// The dot grid behind the new-chat screen.
    pub dot: Color32,
    pub green: Color32,
    pub blue: Color32,
    pub violet: Color32,
    pub orange: Color32,
    pub lime: Color32,
    pub green_ink: Color32,
    pub blue_ink: Color32,
    pub violet_ink: Color32,
    pub orange_ink: Color32,
    pub lime_ink: Color32,
    pub attention: Color32,
    pub attention_ink: Color32,
    /// The color of drop shadows.
    pub shadow: Color32,
    /// The highlight along a raised surface's top edge.
    pub shadow_inset: Color32,
    /// Tooltips: daisyUI's neutral, the same in both themes.
    pub neutral: Color32,
    pub neutral_content: Color32,
}

impl Palette {
    pub fn new(dark: bool) -> Self {
        let pick = |light: u32, dark_hex: u32| if dark { rgb(dark_hex) } else { rgb(light) };
        Self {
            dark,
            canvas: pick(0xF3F5F9, 0x06070B),
            canvas_sunken: pick(0xEAEEF4, 0x0A0C12),
            surface: pick(0xFFFFFF, 0x0F121A),
            surface_raised: pick(0xFFFFFF, 0x141824),
            surface_hover: pick(0xF1F4F8, 0x161B27),
            surface_selected: pick(0xE7EBF2, 0x1A2030),
            ink: pick(0x0A0D14, 0xE9ECF4),
            ink_muted: pick(0x5A6374, 0x8D96AA),
            ink_faint: pick(0x7C8599, 0x636B80),
            ink_inverted: pick(0xFFFFFF, 0x06070B),
            line: pick(0xD6DCE7, 0x1C2130),
            line_strong: pick(0xC3CBDA, 0x2A3144),
            dot: if dark {
                Color32::from_rgba_unmultiplied(233, 236, 244, 20)
            } else {
                Color32::from_rgba_unmultiplied(10, 13, 20, 26)
            },
            green: pick(0x13C977, 0x44FF9A),
            blue: pick(0x1E8FE6, 0x44B0FF),
            violet: pick(0x7A3CF0, 0x9B5CFF),
            orange: pick(0xF0552F, 0xFF6644),
            lime: pick(0xB9C92A, 0xEBFF70),
            green_ink: pick(0x0A8A50, 0x44FF9A),
            blue_ink: pick(0x1569B5, 0x44B0FF),
            violet_ink: pick(0x6A2FDB, 0xB58AFF),
            orange_ink: pick(0xC93A17, 0xFF7F61),
            lime_ink: pick(0x5F6B00, 0xEBFF70),
            attention: pick(0xD99A06, 0xFFC247),
            attention_ink: pick(0x8A5F00, 0xFFC247),
            shadow: if dark {
                Color32::from_rgba_unmultiplied(0, 0, 0, 230)
            } else {
                Color32::from_rgba_unmultiplied(20, 30, 60, 71)
            },
            shadow_inset: if dark {
                Color32::from_rgba_unmultiplied(255, 255, 255, 10)
            } else {
                Color32::from_rgba_unmultiplied(255, 255, 255, 204)
            },
            neutral: rgb(0x1C2130),
            neutral_content: rgb(0xE9ECF4),
        }
    }

    /// `--live`: something happening right now.
    pub fn live(&self) -> Color32 {
        self.blue
    }

    pub fn negative(&self) -> Color32 {
        self.orange
    }

    /// The focus ring: the live blue.
    pub fn focus(&self) -> Color32 {
        self.blue
    }

    pub fn negative_ink(&self) -> Color32 {
        self.orange_ink
    }

    pub fn positive(&self) -> Color32 {
        self.green
    }

    /// `--attention-wash`: a decision waiting for someone, as a card.
    pub fn attention_wash(&self) -> Color32 {
        mix(
            self.attention,
            self.surface,
            if self.dark { 0.14 } else { 0.16 },
        )
    }

    /// `--attention-edge`: that card's ring.
    pub fn attention_edge(&self) -> Color32 {
        mix(self.attention, self.line, 0.42)
    }

    /// `--attention-paper`: what will be written, set inside the card.
    pub fn attention_paper(&self) -> Color32 {
        mix(
            self.attention,
            if self.dark {
                self.canvas_sunken
            } else {
                self.surface
            },
            0.03,
        )
    }

    /// The modal backdrop: `light-dark(rgb(10 13 20 / 32%), rgb(0 0 0 / 62%))`.
    pub fn backdrop(&self) -> Color32 {
        if self.dark {
            Color32::from_black_alpha(158)
        } else {
            Color32::from_rgba_unmultiplied(10, 13, 20, 82)
        }
    }

    /// `color-mix(in oklab, var(--ink) N%, transparent)`, the hover wash.
    pub fn ink_wash(&self, percent: f32) -> Color32 {
        alpha(self.ink, percent / 100.0)
    }
}

// Radii.
pub const RADIUS_TAG: f32 = 6.0;
pub const RADIUS_CONTROL: f32 = 10.0;
pub const RADIUS_CARD: f32 = 14.0;
pub const RADIUS_PANEL: f32 = 20.0;

// Durations, in seconds.
pub const INSTANT: f32 = 0.08;
pub const FAST: f32 = 0.15;
pub const BASE: f32 = 0.22;
pub const SLOW: f32 = 0.4;
pub const WIRE: f32 = 1.2;
/// The narrow sidebar sliding in, as daisyUI's drawer does.
pub const DRAWER: f32 = 0.3;

/// The web's wide breakpoint (`lg`, 64rem): the sidebar docks from here.
pub const WIDE: f32 = 1024.0;
pub const SIDEBAR_WIDTH: f32 = 288.0;
/// `--conversation-width`.
pub const COLUMN: f32 = 736.0;

/// `--ease-snap`: quick out, soft landing.
pub fn ease_snap(t: f32) -> f32 {
    cubic_bezier(0.2, 0.7, 0.2, 1.0, t)
}

/// `--ease-glide`: for pulses.
pub fn ease_glide(t: f32) -> f32 {
    cubic_bezier(0.6, 0.0, 0.3, 1.0, t)
}

/// CSS `ease-out`.
pub fn ease_out(t: f32) -> f32 {
    cubic_bezier(0.0, 0.0, 0.58, 1.0, t)
}

/// A CSS `cubic-bezier()` timing function at `t`.
pub fn cubic_bezier(x1: f32, y1: f32, x2: f32, y2: f32, t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    let bezier = |a: f32, b: f32, s: f32| {
        let u = 1.0 - s;
        3.0 * u * u * s * a + 3.0 * u * s * s * b + s * s * s
    };
    // Solve x(s) = t by bisection: plenty precise for a few hundred pixels.
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    for _ in 0..24 {
        let mid = (lo + hi) / 2.0;
        if bezier(x1, x2, mid) < t {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    bezier(y1, y2, (lo + hi) / 2.0)
}

/// The rainbow, as `--rainbow` draws it from left to right.
pub const RAINBOW: [(f32, Color32); 5] = [
    (-0.0055, rgb(0x44FF9A)),
    (0.2286, rgb(0x44B0FF)),
    (0.4836, rgb(0x8B44FF)),
    (0.7333, rgb(0xFF6644)),
    (0.9934, rgb(0xEBFF70)),
];

/// `--rainbow-loop` and `--rainbow-conic`: the rainbow closed into a loop.
pub const RAINBOW_LOOP: [(f32, Color32); 6] = [
    (0.0, rgb(0x44FF9A)),
    (0.2, rgb(0x44B0FF)),
    (0.4, rgb(0x8B44FF)),
    (0.6, rgb(0xFF6644)),
    (0.8, rgb(0xEBFF70)),
    (1.0, rgb(0x44FF9A)),
];

/// A gradient's color at `t`, interpolated in sRGB as CSS gradients are.
pub fn gradient(stops: &[(f32, Color32)], t: f32) -> Color32 {
    let first = stops[0];
    if t <= first.0 {
        return first.1;
    }
    for pair in stops.windows(2) {
        let ((a, ca), (b, cb)) = (pair[0], pair[1]);
        if t <= b {
            return lerp_rgb(ca, cb, (t - a) / (b - a));
        }
    }
    stops[stops.len() - 1].1
}

pub fn rainbow(t: f32) -> Color32 {
    gradient(&RAINBOW, t)
}

/// The looped rainbow, repeating every 1.0.
pub fn rainbow_loop(t: f32) -> Color32 {
    gradient(&RAINBOW_LOOP, t.rem_euclid(1.0))
}

pub fn lerp_rgb(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let mix = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t).round() as u8;
    Color32::from_rgba_unmultiplied(
        mix(a.r(), b.r()),
        mix(a.g(), b.g()),
        mix(a.b(), b.b()),
        mix(a.a(), b.a()),
    )
}

/// `color` at `opacity`, as `color-mix(in oklab, color N%, transparent)`.
pub fn alpha(color: Color32, opacity: f32) -> Color32 {
    color.gamma_multiply(opacity.clamp(0.0, 1.0))
}

/// `color-mix(in oklab, a (t*100)%, b)` for two opaque colors.
pub fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let (la, lb) = (oklab(a), oklab(b));
    let m = [0, 1, 2].map(|i| la[i] * t + lb[i] * (1.0 - t));
    from_oklab(m)
}

fn to_linear(c: u8) -> f32 {
    let c = f32::from(c) / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn to_srgb(c: f32) -> u8 {
    let c = c.clamp(0.0, 1.0);
    let s = if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    };
    (s * 255.0).round() as u8
}

fn oklab(c: Color32) -> [f32; 3] {
    let (r, g, b) = (to_linear(c.r()), to_linear(c.g()), to_linear(c.b()));
    let l = (0.412_221_46 * r + 0.536_332_55 * g + 0.051_445_995 * b).cbrt();
    let m = (0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b).cbrt();
    let s = (0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b).cbrt();
    [
        0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s,
        1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s,
        0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s,
    ]
}

fn from_oklab([l, a, b]: [f32; 3]) -> Color32 {
    let l_ = (l + 0.396_337_78 * a + 0.215_803_76 * b).powi(3);
    let m_ = (l - 0.105_561_346 * a - 0.063_854_17 * b).powi(3);
    let s_ = (l - 0.089_484_18 * a - 1.291_485_5 * b).powi(3);
    Color32::from_rgb(
        to_srgb(4.076_741_7 * l_ - 3.307_711_6 * m_ + 0.230_969_94 * s_),
        to_srgb(-1.268_438 * l_ + 2.609_757_4 * m_ - 0.341_319_38 * s_),
        to_srgb(-0.004_196_086_3 * l_ - 0.703_418_6 * m_ + 1.707_614_7 * s_),
    )
}

/// A run of text in one of the web's faces.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Type {
    pub size: f32,
    pub weight: f32,
    pub mono: bool,
    /// Line height in points.
    pub line_height: f32,
    /// Tracking in ems (`letter-spacing`).
    pub tracking: f32,
}

impl Type {
    pub const fn sans(size: f32, line_height: f32) -> Self {
        Self {
            size,
            weight: 400.0,
            mono: false,
            line_height,
            tracking: 0.0,
        }
    }

    /// `.micro`: mono, 0.02em tracking.
    pub const fn mono(size: f32, line_height: f32) -> Self {
        Self {
            size,
            weight: 400.0,
            mono: true,
            line_height,
            tracking: 0.02,
        }
    }

    pub const fn weight(mut self, weight: f32) -> Self {
        self.weight = weight;
        self
    }

    pub const fn tracking(mut self, ems: f32) -> Self {
        self.tracking = ems;
        self
    }

    pub fn font_id(&self) -> FontId {
        FontId::new(
            self.size,
            FontFamily::Name(if self.mono { MONO } else { SANS }.into()),
        )
    }

    pub fn format(&self, color: Color32) -> TextFormat {
        TextFormat {
            font_id: self.font_id(),
            color,
            line_height: Some(self.line_height),
            extra_letter_spacing: self.tracking * self.size,
            coords: VariationCoords::new([(b"wght", self.weight)]),
            valign: egui::Align::Center,
            ..TextFormat::default()
        }
    }
}

/// The web's type scale.
pub mod scale {
    use super::Type;

    /// The page's base text: 16/24.
    pub const BODY: Type = Type::sans(16.0, 24.0);
    /// Answers (`.message__body.prose`): 16 with a 1.7 line height.
    pub const PROSE: Type = Type::sans(16.0, 27.2);
    /// Sidebar rows, the activity line, menus: 14/21.
    pub const SMALL: Type = Type::sans(14.0, 21.0);
    /// Steps, notices' small print, chips beside the composer: 13.
    pub const SMALLER: Type = Type::sans(13.0, 19.5);
    /// Captions, badges: 12/18.
    pub const CAPTION: Type = Type::sans(12.0, 18.0);
    /// `.micro`: section labels, counts, table headers.
    pub const MICRO: Type = Type::mono(12.0, 16.8);
    /// `kbd` in a `.kbd-hint`.
    pub const KBD: Type = Type::mono(11.0, 11.0).tracking(0.0);
    /// Citation chips.
    pub const CHIP: Type = Type::mono(11.0, 15.95);
    /// Code blocks.
    pub const CODE: Type = Type::mono(14.0, 20.0).tracking(0.0);
}

/// Add Geist and Geist Mono to `fonts`, each falling back to egui's own
/// fonts (and color emoji) for anything they lack.
pub fn add_fonts(fonts: &mut egui::FontDefinitions) {
    fonts.font_data.insert(
        SANS.into(),
        egui::FontData::from_static(include_bytes!("../../../assets/chat/fonts/Geist.ttf")).into(),
    );
    fonts.font_data.insert(
        MONO.into(),
        egui::FontData::from_static(include_bytes!("../../../assets/chat/fonts/GeistMono.ttf"))
            .into(),
    );
    let fallback = |family: FontFamily| fonts.families.get(&family).cloned().unwrap_or_default();
    let mut sans = vec![SANS.to_string()];
    sans.extend(fallback(FontFamily::Proportional));
    let mut mono = vec![MONO.to_string()];
    mono.extend(fallback(FontFamily::Monospace));
    mono.extend(fallback(FontFamily::Proportional));
    fonts.families.insert(FontFamily::Name(SANS.into()), sans);
    fonts.families.insert(FontFamily::Name(MONO.into()), mono);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oklab_mixing_matches_the_browser() {
        // color-mix(in oklab, #0A0D14 84%, #F3F5F9): the primary button's
        // hover on light, which Chromium paints as rgb(40, 43, 50).
        let hover = mix(rgb(0x0A0D14), rgb(0xF3F5F9), 0.84);
        for (got, want) in [(hover.r(), 40), (hover.g(), 43), (hover.b(), 50)] {
            assert!((i32::from(got) - want).abs() <= 2, "{hover:?}");
        }
        let dark = mix(rgb(0xE9ECF4), rgb(0x06070B), 0.84);
        for (got, want) in [(dark.r(), 191), (dark.g(), 194), (dark.b(), 201)] {
            assert!((i32::from(got) - want).abs() <= 2, "{dark:?}");
        }
        assert_eq!(mix(rgb(0x123456), rgb(0xABCDEF), 1.0), rgb(0x123456));
    }

    #[test]
    fn easing_curves_start_and_end_in_place() {
        for curve in [ease_snap, ease_glide, ease_out] {
            assert!(curve(0.0).abs() < 1e-3);
            assert!((curve(1.0) - 1.0).abs() < 1e-3);
            assert!(curve(0.5) > 0.0 && curve(0.5) < 1.1);
        }
        // ease-snap is quick out of the gate.
        assert!(ease_snap(0.25) > 0.6);
    }

    #[test]
    fn the_rainbow_loops() {
        assert_eq!(rainbow_loop(0.0), rainbow_loop(1.0));
        assert_eq!(rainbow(-0.01), rgb(0x44FF9A));
        assert_eq!(rainbow(1.0), rgb(0xEBFF70));
    }
}
