//! The Chat with Work mark, for the window and the tray.
//!
//! The PNGs are rendered from `assets/mark.svg` by `assets/render.sh`.

pub struct Rgba {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

fn decode(png: &[u8]) -> Rgba {
    let image = image::load_from_memory_with_format(png, image::ImageFormat::Png)
        .expect("bundled icons are valid PNGs")
        .into_rgba8();
    Rgba {
        width: image.width(),
        height: image.height(),
        pixels: image.into_raw(),
    }
}

/// Fade the icon while nothing is being served (paused, offline, stopped).
fn dim(mut icon: Rgba) -> Rgba {
    for px in icon.pixels.as_chunks_mut::<4>().0 {
        px[3] = (u16::from(px[3]) * 2 / 5) as u8;
    }
    icon
}

/// The tray icon: the full mark (white bubble, black scribble), which reads
/// on light and dark panels. `large` is for high-density Linux panels.
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub fn tray(dimmed: bool, large: bool) -> Rgba {
    let icon = decode(if large {
        include_bytes!("../assets/tray-64.png")
    } else {
        include_bytes!("../assets/tray-32.png")
    });
    if dimmed { dim(icon) } else { icon }
}

/// The scribble alone, as a template image: macOS colors it to match the
/// menu bar.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn tray_template(dimmed: bool) -> Rgba {
    let icon = decode(include_bytes!("../assets/tray-template-36.png"));
    if dimmed { dim(icon) } else { icon }
}

pub fn window() -> egui::IconData {
    let icon = decode(include_bytes!("../assets/icon-256.png"));
    egui::IconData {
        width: icon.width,
        height: icon.height,
        rgba: icon.pixels,
    }
}

/// The mark for the welcome page.
pub fn mark() -> egui::ColorImage {
    let icon = decode(include_bytes!("../assets/tray-64.png"));
    egui::ColorImage::from_rgba_unmultiplied(
        [icon.width as usize, icon.height as usize],
        &icon.pixels,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icons_decode_and_dim() {
        let icon = tray(false, false);
        assert_eq!((icon.width, icon.height), (32, 32));
        let faded = tray(true, false);
        let max = |i: &Rgba| {
            i.pixels
                .as_chunks::<4>()
                .0
                .iter()
                .map(|p| p[3])
                .max()
                .unwrap()
        };
        assert_eq!(max(&icon), 255);
        assert!(max(&faded) < 110);
        assert_eq!(tray_template(false).width, 36);
        assert_eq!(window().width, 256);
    }
}
