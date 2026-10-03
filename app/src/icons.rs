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
fn dim(pixels: &mut [u8]) {
    for px in pixels.as_chunks_mut::<4>().0 {
        px[3] = (u16::from(px[3]) * 2 / 5) as u8;
    }
}

/// The square RGBA icon `size` pixels on a side, from the closest PNG,
/// scaled when no PNG has that size.
fn tray_sized(pngs: &[&[u8]], size: usize, dimmed: bool) -> Vec<u8> {
    let mut icons: Vec<Rgba> = pngs.iter().map(|png| decode(png)).collect();
    icons.sort_by_key(|icon| icon.width);
    // The smallest at least as large, or else the largest.
    let at = icons
        .iter()
        .position(|icon| icon.width as usize >= size)
        .unwrap_or(icons.len() - 1);
    let icon = icons.swap_remove(at);
    let mut pixels = if icon.width as usize == size && icon.height as usize == size {
        icon.pixels
    } else {
        let image = image::RgbaImage::from_raw(icon.width, icon.height, icon.pixels)
            .expect("decoded pixels fill the image");
        let side = size as u32;
        image::imageops::resize(&image, side, side, image::imageops::FilterType::Lanczos3)
            .into_raw()
    };
    if dimmed {
        dim(&mut pixels);
    }
    pixels
}

const TRAY: [&[u8]; 2] = [
    include_bytes!("../assets/tray-32.png"),
    include_bytes!("../assets/tray-64.png"),
];
const TRAY_TEMPLATE: [&[u8]; 1] = [include_bytes!("../assets/tray-template-36.png")];

/// The tray icon: the full mark (white bubble, black scribble), which reads
/// on light and dark panels.
pub fn tray(size: usize) -> Vec<u8> {
    tray_sized(&TRAY, size, false)
}

/// The tray icon, faded.
pub fn tray_dimmed(size: usize) -> Vec<u8> {
    tray_sized(&TRAY, size, true)
}

/// The scribble alone, as a template image: macOS colors it to match the
/// menu bar.
pub fn tray_template(size: usize) -> Vec<u8> {
    tray_sized(&TRAY_TEMPLATE, size, false)
}

/// The template image, faded.
pub fn tray_template_dimmed(size: usize) -> Vec<u8> {
    tray_sized(&TRAY_TEMPLATE, size, true)
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
        let max = |pixels: &[u8]| {
            pixels
                .as_chunks::<4>()
                .0
                .iter()
                .map(|p| p[3])
                .max()
                .unwrap()
        };
        for (size, icon) in [(32, tray(32)), (64, tray(64)), (36, tray_template(36))] {
            assert_eq!(icon.len(), size * size * 4);
            assert_eq!(max(&icon), 255);
        }
        assert!(max(&tray_dimmed(32)) < 110);
        assert!(max(&tray_template_dimmed(36)) < 110);
        assert_eq!(window().width, 256);
    }

    /// fastframe-tray asks for whatever size its platform draws.
    #[test]
    fn other_sizes_are_scaled() {
        for size in [16, 48, 128] {
            assert_eq!(tray(size).len(), size * size * 4);
            assert_eq!(tray_template(size).len(), size * size * 4);
        }
    }
}
