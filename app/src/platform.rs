//! The few things each desktop answers differently: the accent color, dark
//! mode where the windowing library can't tell, the UI font and how text is
//! rendered, and bringing the app to the front.

use std::path::PathBuf;

/// Bring the app forward so its window takes focus. A menu bar app on
/// macOS is not active until it asks.
pub fn activate() {
    #[cfg(target_os = "macos")]
    {
        use objc2::MainThreadMarker;
        use objc2_app_kit::NSApplication;
        if let Some(mtm) = MainThreadMarker::new() {
            let app = NSApplication::sharedApplication(mtm);
            #[allow(deprecated)]
            app.activateIgnoringOtherApps(true);
        }
    }
}

/// The user's accent color, where the platform has one.
pub fn accent_color() -> Option<[u8; 3]> {
    #[cfg(target_os = "macos")]
    {
        use objc2_app_kit::{NSColor, NSColorSpace};
        let color = NSColor::controlAccentColor();
        let srgb = color.colorUsingColorSpace(&NSColorSpace::sRGBColorSpace())?;
        let to_u8 = |c: f64| (c.clamp(0.0, 1.0) * 255.0).round() as u8;
        Some([
            to_u8(srgb.redComponent()),
            to_u8(srgb.greenComponent()),
            to_u8(srgb.blueComponent()),
        ])
    }
    #[cfg(windows)]
    {
        // 0xAABBGGRR, what the taskbar and Settings use.
        let value = crate::autostart::windows_registry::read_dword(
            r"Software\Microsoft\Windows\DWM",
            "AccentColor",
        )?;
        Some([value as u8, (value >> 8) as u8, (value >> 16) as u8])
    }
    #[cfg(target_os = "linux")]
    {
        // GNOME 47 and later name one of a fixed set of accents.
        let name = gsettings("org.gnome.desktop.interface", "accent-color")?;
        Some(match name.as_str() {
            "teal" => [0x21, 0x90, 0xa4],
            "green" => [0x3a, 0x94, 0x4a],
            "yellow" => [0xc8, 0x88, 0x00],
            "orange" => [0xed, 0x5b, 0x00],
            "red" => [0xe6, 0x2d, 0x42],
            "pink" => [0xd5, 0x61, 0x99],
            "purple" => [0x91, 0x41, 0xac],
            "slate" => [0x6f, 0x83, 0x96],
            _ => return None,
        })
    }
}

/// How the desktop renders text, read once per process: the portal or
/// fontconfig on Linux (a D-Bus call, then `fc-match`), fixed elsewhere.
pub fn text_rendering() -> fastframe_text::TextRendering {
    static RENDERING: std::sync::OnceLock<fastframe_text::TextRendering> =
        std::sync::OnceLock::new();
    *RENDERING.get_or_init(fastframe_text::detect)
}

/// Dark mode on Linux desktops, which winit can't always read on Wayland:
/// the freedesktop color-scheme setting, as GNOME, KDE and Omarchy set it.
pub fn prefers_dark() -> Option<bool> {
    #[cfg(target_os = "linux")]
    {
        if let Some(scheme) = gsettings("org.gnome.desktop.interface", "color-scheme") {
            return Some(scheme == "prefer-dark");
        }
        std::env::var("GTK_THEME")
            .ok()
            .map(|t| t.to_ascii_lowercase().ends_with(":dark"))
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

#[cfg(target_os = "linux")]
fn gsettings(schema: &str, key: &str) -> Option<String> {
    let out = std::process::Command::new("gsettings")
        .args(["get", schema, key])
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(
        String::from_utf8_lossy(&out.stdout)
            .trim()
            .trim_matches('\'')
            .to_string(),
    )
}

/// One face of the system UI font: a file, and for variable fonts the axis
/// values to use (optical size, weight).
pub struct UiFont {
    pub path: PathBuf,
    pub axes: Vec<(&'static [u8; 4], f32)>,
}

/// The system UI font: regular, and a bold face if there is one.
pub struct UiFonts {
    pub regular: UiFont,
    pub bold: Option<UiFont>,
}

pub fn ui_fonts() -> Option<UiFonts> {
    #[cfg(target_os = "macos")]
    {
        // San Francisco is one variable font. Its default optical size is
        // the display cut, which sets small text too tight; opsz 17 is the
        // text cut macOS uses for body text.
        let path = PathBuf::from("/System/Library/Fonts/SFNS.ttf");
        path.exists().then(|| UiFonts {
            regular: UiFont {
                path: path.clone(),
                axes: vec![(b"opsz", 17.0), (b"wght", 400.0)],
            },
            bold: Some(UiFont {
                path,
                axes: vec![(b"opsz", 20.0), (b"wght", 600.0)],
            }),
        })
    }
    #[cfg(windows)]
    {
        let fonts = std::env::var_os("WINDIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
            .join("Fonts");
        // Windows 11's Segoe UI Variable, then Windows 10's Segoe UI.
        let variable = fonts.join("SegUIVar.ttf");
        if variable.exists() {
            return Some(UiFonts {
                regular: UiFont {
                    path: variable.clone(),
                    axes: vec![(b"opsz", 10.5), (b"wght", 400.0)],
                },
                bold: Some(UiFont {
                    path: variable,
                    axes: vec![(b"opsz", 20.0), (b"wght", 600.0)],
                }),
            });
        }
        let regular = fonts.join("segoeui.ttf");
        let bold = fonts.join("seguisb.ttf");
        regular.exists().then(|| UiFonts {
            regular: UiFont {
                path: regular,
                axes: Vec::new(),
            },
            bold: bold.exists().then_some(UiFont {
                path: bold,
                axes: Vec::new(),
            }),
        })
    }
    #[cfg(target_os = "linux")]
    {
        let find = |pattern: &str| -> Option<PathBuf> {
            let out = std::process::Command::new("fc-match")
                .args(["-f", "%{file}", pattern])
                .output()
                .ok()?;
            let path = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
            // egui reads TrueType and OpenType files, not Type 1 or bitmaps.
            let ext = path.extension()?.to_str()?.to_ascii_lowercase();
            (["ttf", "otf"].contains(&ext.as_str()) && path.exists()).then_some(path)
        };
        let regular = find("sans-serif:style=Regular")?;
        let bold = find("sans-serif:weight=bold").filter(|b| *b != regular);
        Some(UiFonts {
            regular: UiFont {
                path: regular,
                axes: Vec::new(),
            },
            bold: bold.map(|path| UiFont {
                path,
                axes: Vec::new(),
            }),
        })
    }
}

/// Map a font file into memory once per process. The pages are the file's
/// own, shared with every other program using the font, and the window can
/// be opened again without reading it again.
pub fn map_font(path: &std::path::Path) -> Option<&'static [u8]> {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};

    static MAPS: OnceLock<Mutex<HashMap<PathBuf, &'static [u8]>>> = OnceLock::new();
    let mut maps = MAPS.get_or_init(Mutex::default).lock().ok()?;
    if let Some(bytes) = maps.get(path) {
        return Some(bytes);
    }
    let file = std::fs::File::open(path).ok()?;
    // SAFETY: system font files aren't modified while programs use them.
    let map = unsafe { memmap2::Mmap::map(&file) }.ok()?;
    let bytes: &'static [u8] = Box::leak(Box::new(map));
    maps.insert(path.to_path_buf(), bytes);
    Some(bytes)
}
