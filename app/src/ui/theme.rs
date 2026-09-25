//! Looks that follow each platform: its UI font, accent color, light and
//! dark palettes, control sizes and corner radii, after macOS System
//! Settings, Windows 11 Settings, and GNOME (libadwaita).

use egui::{Color32, CornerRadius, FontFamily, FontId, Margin, Stroke, TextStyle, Vec2};

use crate::model::Platform;

pub const BOLD: &str = "bold";

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Palette {
    /// The content area.
    pub window: Color32,
    pub sidebar: Color32,
    /// Grouped rows (macOS form groups, Windows cards, GNOME boxed lists).
    pub group: Color32,
    pub group_stroke: Color32,
    pub text: Color32,
    pub weak: Color32,
    pub separator: Color32,
    pub accent: Color32,
    /// Text on an accent fill.
    pub on_accent: Color32,
    pub control: Color32,
    pub control_hover: Color32,
    pub control_stroke: Color32,
    pub field: Color32,
    pub nav_selected: Color32,
    pub nav_selected_text: Color32,
    pub success: Color32,
    pub warning: Color32,
    pub danger: Color32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Metrics {
    pub body: f32,
    pub small: f32,
    pub title: f32,
    pub button_padding: Vec2,
    pub control_height: f32,
    pub radius: u8,
    pub group_radius: u8,
    pub sidebar_width: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Theme {
    pub platform: Platform,
    pub dark: bool,
    pub palette: Palette,
    pub metrics: Metrics,
}

const fn rgb(hex: u32) -> Color32 {
    Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

/// `hex` over `base` at `alpha`, as an opaque color.
fn over(base: Color32, hex: u32, alpha: f32) -> Color32 {
    let top = rgb(hex);
    let mix = |a: u8, b: u8| (f32::from(a) * (1.0 - alpha) + f32::from(b) * alpha).round() as u8;
    Color32::from_rgb(
        mix(base.r(), top.r()),
        mix(base.g(), top.g()),
        mix(base.b(), top.b()),
    )
}

impl Theme {
    pub fn new(platform: Platform, dark: bool, accent: Option<[u8; 3]>) -> Self {
        let mut palette = palette(
            platform,
            dark,
            accent.map(|[r, g, b]| Color32::from_rgb(r, g, b)),
        );
        // The user's accent can be light or dark; pick readable text for it.
        let a = palette.accent;
        let luma =
            0.2126 * f32::from(a.r()) + 0.7152 * f32::from(a.g()) + 0.0722 * f32::from(a.b());
        palette.on_accent = if luma > 160.0 {
            Color32::BLACK
        } else {
            Color32::WHITE
        };
        let metrics = match platform {
            Platform::MacOs => Metrics {
                body: 13.0,
                small: 11.0,
                title: 20.0,
                button_padding: Vec2::new(12.0, 3.0),
                control_height: 24.0,
                radius: 6,
                group_radius: 10,
                sidebar_width: 200.0,
            },
            Platform::Windows => Metrics {
                body: 14.0,
                small: 12.0,
                title: 26.0,
                button_padding: Vec2::new(12.0, 6.0),
                control_height: 32.0,
                radius: 4,
                group_radius: 8,
                sidebar_width: 220.0,
            },
            Platform::Linux => Metrics {
                body: 14.5,
                small: 12.0,
                title: 20.0,
                button_padding: Vec2::new(14.0, 7.0),
                control_height: 34.0,
                radius: 6,
                group_radius: 12,
                sidebar_width: 210.0,
            },
        };
        Self {
            platform,
            dark,
            palette,
            metrics,
        }
    }

    /// Detect the platform's colors now.
    pub fn detect(ctx: &egui::Context) -> Self {
        let dark = match ctx.system_theme() {
            Some(theme) => theme == egui::Theme::Dark,
            None => crate::platform::prefers_dark().unwrap_or(false),
        };
        Self::new(Platform::current(), dark, crate::platform::accent_color())
    }

    pub fn apply(&self, ctx: &egui::Context) {
        let theme = if self.dark {
            egui::Theme::Dark
        } else {
            egui::Theme::Light
        };
        ctx.set_theme(theme);
        ctx.set_style_of(theme, self.style());
    }

    pub fn style(&self) -> egui::Style {
        let p = &self.palette;
        let m = &self.metrics;
        let mut style = egui::Style::default();
        let mut visuals = if self.dark {
            egui::Visuals::dark()
        } else {
            egui::Visuals::light()
        };
        visuals.panel_fill = p.window;
        visuals.window_fill = p.window;
        visuals.extreme_bg_color = p.field;
        visuals.text_edit_bg_color = Some(p.field);
        visuals.faint_bg_color = p.group;
        visuals.code_bg_color = if self.dark {
            Color32::from_gray(64)
        } else {
            Color32::from_gray(232)
        };
        visuals.override_text_color = Some(p.text);
        visuals.weak_text_color = Some(p.weak);
        visuals.hyperlink_color = p.accent;
        visuals.selection.bg_fill = p.accent.gamma_multiply(if self.dark { 0.55 } else { 0.35 });
        visuals.selection.stroke = Stroke::new(1.0, p.accent);
        visuals.warn_fg_color = p.warning;
        visuals.error_fg_color = p.danger;
        visuals.window_corner_radius = CornerRadius::same(m.group_radius);
        visuals.menu_corner_radius = CornerRadius::same(m.radius);
        visuals.striped = false;
        visuals.indent_has_left_vline = false;
        let radius = CornerRadius::same(m.radius);
        let w = &mut visuals.widgets;
        w.noninteractive.bg_stroke = Stroke::new(1.0, p.separator);
        w.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
        w.noninteractive.corner_radius = radius;
        for (state, fill) in [
            (&mut w.inactive, p.control),
            (&mut w.hovered, p.control_hover),
            (&mut w.active, p.control_hover),
            (&mut w.open, p.control_hover),
        ] {
            state.bg_fill = fill;
            state.weak_bg_fill = fill;
            state.bg_stroke = Stroke::new(1.0, p.control_stroke);
            state.fg_stroke = Stroke::new(1.0, p.text);
            state.corner_radius = radius;
            state.expansion = 0.0;
        }
        w.hovered.bg_stroke = Stroke::new(1.0, p.control_stroke);
        w.active.bg_stroke = Stroke::new(1.0, p.accent);
        style.visuals = visuals;

        style.spacing.item_spacing = Vec2::new(8.0, 8.0);
        style.spacing.button_padding = m.button_padding;
        style.spacing.interact_size = Vec2::new(40.0, m.control_height);
        style.spacing.window_margin = Margin::same(16);
        style.spacing.icon_width = m.body + 2.0;
        style.spacing.icon_width_inner = m.body - 4.0;
        style.spacing.icon_spacing = 6.0;
        style.spacing.text_edit_width = 280.0;

        let prop = |size: f32| FontId::new(size, FontFamily::Proportional);
        style.text_styles = [
            (TextStyle::Small, prop(m.small)),
            (TextStyle::Body, prop(m.body)),
            (TextStyle::Button, prop(m.body)),
            (
                TextStyle::Heading,
                FontId::new(m.title, FontFamily::Name(BOLD.into())),
            ),
            (
                TextStyle::Monospace,
                FontId::new(m.body - 1.0, FontFamily::Monospace),
            ),
        ]
        .into();
        style
    }

    /// Text in the bold face, where the platform has one.
    pub fn strong(&self, text: impl Into<String>) -> egui::RichText {
        egui::RichText::new(text)
            .family(FontFamily::Name(BOLD.into()))
            .color(self.palette.text)
    }

    pub fn weak(&self, text: impl Into<String>) -> egui::RichText {
        egui::RichText::new(text).color(self.palette.weak)
    }

    pub fn small(&self, text: impl Into<String>) -> egui::RichText {
        egui::RichText::new(text)
            .size(self.metrics.small)
            .color(self.palette.weak)
    }
}

fn palette(platform: Platform, dark: bool, accent: Option<Color32>) -> Palette {
    let white = Color32::WHITE;
    match (platform, dark) {
        (Platform::MacOs, false) => {
            let accent = accent.unwrap_or(rgb(0x007aff));
            Palette {
                window: rgb(0xf2f2f2),
                sidebar: rgb(0xe8e8e8),
                group: white,
                group_stroke: rgb(0xe0e0e0),
                text: rgb(0x1d1d1f),
                weak: rgb(0x6e6e73),
                separator: rgb(0xe5e5e5),
                accent,
                on_accent: white,
                control: white,
                control_hover: rgb(0xf5f5f5),
                control_stroke: rgb(0xcfcfcf),
                field: white,
                nav_selected: accent,
                nav_selected_text: white,
                success: rgb(0x28a745),
                warning: rgb(0xc27c00),
                danger: rgb(0xd70015),
            }
        }
        (Platform::MacOs, true) => {
            let accent = accent.unwrap_or(rgb(0x0a84ff));
            Palette {
                window: rgb(0x1e1e1e),
                sidebar: rgb(0x282828),
                group: rgb(0x2a2a2a),
                group_stroke: rgb(0x353535),
                text: rgb(0xf5f5f7),
                weak: rgb(0x98989d),
                separator: rgb(0x383838),
                accent,
                on_accent: white,
                control: rgb(0x4a4a4a),
                control_hover: rgb(0x565656),
                control_stroke: rgb(0x5a5a5a),
                field: rgb(0x1c1c1c),
                nav_selected: accent,
                nav_selected_text: white,
                success: rgb(0x32d74b),
                warning: rgb(0xffb340),
                danger: rgb(0xff453a),
            }
        }
        (Platform::Windows, false) => {
            let accent = accent.unwrap_or(rgb(0x005fb8));
            Palette {
                window: rgb(0xf3f3f3),
                sidebar: rgb(0xf3f3f3),
                group: rgb(0xfbfbfb),
                group_stroke: rgb(0xe5e5e5),
                text: rgb(0x1a1a1a),
                weak: rgb(0x5d5d5d),
                separator: rgb(0xeaeaea),
                accent,
                on_accent: white,
                control: rgb(0xfdfdfd),
                control_hover: rgb(0xf6f6f6),
                control_stroke: rgb(0xd6d6d6),
                field: white,
                nav_selected: rgb(0xe9e9e9),
                nav_selected_text: rgb(0x1a1a1a),
                success: rgb(0x0f7b0f),
                warning: rgb(0x9d5d00),
                danger: rgb(0xc42b1c),
            }
        }
        (Platform::Windows, true) => {
            let accent = accent.unwrap_or(rgb(0x60cdff));
            Palette {
                window: rgb(0x202020),
                sidebar: rgb(0x202020),
                group: rgb(0x2b2b2b),
                group_stroke: rgb(0x1d1d1d),
                text: white,
                weak: rgb(0xc5c5c5),
                separator: rgb(0x3a3a3a),
                accent,
                on_accent: rgb(0x000000),
                control: rgb(0x2d2d2d),
                control_hover: rgb(0x323232),
                control_stroke: rgb(0x3d3d3d),
                field: rgb(0x2d2d2d),
                nav_selected: rgb(0x2d2d2d),
                nav_selected_text: white,
                success: rgb(0x6ccb5f),
                warning: rgb(0xfce100),
                danger: rgb(0xff99a4),
            }
        }
        (Platform::Linux, false) => {
            let accent = accent.unwrap_or(rgb(0x3584e4));
            let window = rgb(0xfafafb);
            Palette {
                window,
                sidebar: rgb(0xebebed),
                group: white,
                group_stroke: rgb(0xdedee0),
                text: rgb(0x1e1e22),
                weak: rgb(0x6b6b70),
                separator: rgb(0xe6e6e8),
                accent,
                on_accent: white,
                control: over(window, 0x000006, 0.08),
                control_hover: over(window, 0x000006, 0.12),
                control_stroke: Color32::TRANSPARENT,
                field: over(window, 0x000006, 0.06),
                nav_selected: over(rgb(0xebebed), 0x000006, 0.10),
                nav_selected_text: rgb(0x1e1e22),
                success: rgb(0x1b8553),
                warning: rgb(0x9c6e03),
                danger: rgb(0xc01c28),
            }
        }
        (Platform::Linux, true) => {
            let accent = accent.unwrap_or(rgb(0x3584e4));
            let window = rgb(0x222226);
            Palette {
                window,
                sidebar: rgb(0x2e2e32),
                group: rgb(0x36363a),
                group_stroke: rgb(0x2a2a2e),
                text: white,
                weak: rgb(0xa8a8ad),
                separator: rgb(0x2a2a2e),
                accent,
                on_accent: white,
                control: over(window, 0xffffff, 0.10),
                control_hover: over(window, 0xffffff, 0.15),
                control_stroke: Color32::TRANSPARENT,
                field: over(window, 0xffffff, 0.08),
                nav_selected: over(rgb(0x2e2e32), 0xffffff, 0.10),
                nav_selected_text: white,
                success: rgb(0x78e9ab),
                warning: rgb(0xffc252),
                danger: rgb(0xff7b63),
            }
        }
    }
}

/// Load the platform's UI font, with egui's own fonts behind it for any
/// character it lacks.
pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let fallback = fonts
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    let mut bold_family = fallback.clone();
    let load = |face: &crate::platform::UiFont| {
        let bytes = crate::platform::map_font(&face.path)?;
        let mut data = egui::FontData::from_static(bytes);
        data.tweak.coords = egui::epaint::text::VariationCoords::new(face.axes.iter().copied());
        Some(data)
    };
    if let Some(ui) = crate::platform::ui_fonts() {
        if let Some(regular) = load(&ui.regular) {
            fonts.font_data.insert("system".into(), regular.into());
            if let Some(family) = fonts.families.get_mut(&FontFamily::Proportional) {
                family.insert(0, "system".into());
            }
            bold_family.insert(0, "system".into());
        }
        if let Some(bold) = ui.bold.as_ref().and_then(load) {
            fonts.font_data.insert("system-bold".into(), bold.into());
            bold_family.insert(0, "system-bold".into());
        }
    }
    fonts
        .families
        .insert(FontFamily::Name(BOLD.into()), bold_family);
    ctx.set_fonts(fonts);
}

/// Without the system fonts, as in tests, the bold family still exists.
pub fn install_default_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let proportional = fonts
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    fonts
        .families
        .insert(FontFamily::Name(BOLD.into()), proportional);
    ctx.set_fonts(fonts);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_platform_has_readable_palettes() {
        for platform in [Platform::MacOs, Platform::Windows, Platform::Linux] {
            for dark in [false, true] {
                let theme = Theme::new(platform, dark, None);
                let p = theme.palette;
                let luma = |c: Color32| {
                    0.2126 * f32::from(c.r())
                        + 0.7152 * f32::from(c.g())
                        + 0.0722 * f32::from(c.b())
                };
                let contrast = (luma(p.text) - luma(p.window)).abs();
                assert!(contrast > 150.0, "{platform:?} dark={dark}: {contrast}");
                assert_eq!(luma(p.window) < 128.0, dark, "{platform:?} dark={dark}");
            }
        }
    }

    #[test]
    fn the_accent_color_is_the_users() {
        let theme = Theme::new(Platform::MacOs, false, Some([1, 2, 3]));
        assert_eq!(theme.palette.accent, Color32::from_rgb(1, 2, 3));
    }
}
