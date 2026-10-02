//! The web app's icons (Phosphor, bold) and images: one alpha atlas,
//! rendered by `assets/chat/render.sh` and tinted when drawn, and the
//! logotype for each theme.

use egui::{Color32, Pos2, Rect, TextureHandle, TextureOptions, pos2, vec2};

/// In the atlas's order (see `ICONS` in `assets/chat/render.sh`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    NotePencil,
    MagnifyingGlass,
    SidebarSimple,
    X,
    Paperclip,
    ArrowUp,
    Stop,
    LockSimple,
    CaretRight,
    Copy,
    Check,
    ArrowDown,
    ArrowUpRight,
    GearSix,
    WarningCircle,
    FileText,
    FilePdf,
    FileXls,
    FileDoc,
    GlobeSimple,
    AppWindow,
    Plug,
    Desktop,
    CaretUpDown,
    DotsThree,
    PencilSimple,
    Trash,
    ArrowsClockwise,
    GitBranch,
    Export,
    HandPalm,
    HourglassMedium,
    Warning,
    File,
    FileImage,
    FilePpt,
    FileTxt,
    FileCode,
    FileAudio,
    FileVideo,
}

const COUNT: usize = Icon::FileVideo as usize + 1;
const CELL: f32 = 64.0;
/// Icons to a row of the atlas: 32 cells make 2048, the widest texture.
const COLUMNS: usize = 32;
const ROWS: usize = COUNT.div_ceil(COLUMNS);

/// Where `icon` is in the atlas, in texture coordinates.
fn uv(icon: Icon) -> Rect {
    let i = icon as usize;
    let (col, row) = ((i % COLUMNS) as f32, (i / COLUMNS) as f32);
    let (w, h) = (1.0 / COLUMNS as f32, 1.0 / ROWS as f32);
    Rect::from_min_size(pos2(col * w, row * h), vec2(w, h))
}

fn decode(png: &[u8]) -> egui::ColorImage {
    let image = image::load_from_memory_with_format(png, image::ImageFormat::Png)
        .expect("bundled images are valid PNGs")
        .into_rgba8();
    let size = [image.width() as usize, image.height() as usize];
    egui::ColorImage::from_rgba_unmultiplied(size, image.as_raw())
}

fn options() -> TextureOptions {
    TextureOptions {
        mipmap_mode: Some(egui::TextureFilter::Linear),
        ..TextureOptions::LINEAR
    }
}

/// The page's textures, loaded on first use and dropped with the window.
pub struct Images {
    icons: TextureHandle,
    logotype: TextureHandle,
    logotype_dark: TextureHandle,
    mark: TextureHandle,
}

impl Images {
    pub fn load(ctx: &egui::Context) -> Self {
        let atlas = decode(include_bytes!("../../../assets/chat/icons.png"));
        debug_assert_eq!(
            atlas.size,
            [COLUMNS * CELL as usize, ROWS * CELL as usize],
            "atlas and Icon agree"
        );
        Self {
            icons: ctx.load_texture("chat-icons", atlas, options()),
            logotype: ctx.load_texture(
                "chat-logotype",
                decode(include_bytes!("../../../assets/chat/logotype.png")),
                options(),
            ),
            logotype_dark: ctx.load_texture(
                "chat-logotype-dark",
                decode(include_bytes!("../../../assets/chat/logotype-dark.png")),
                options(),
            ),
            mark: ctx.load_texture("chat-mark", crate::icons::mark(), options()),
        }
    }

    /// Draw `icon` filling `rect`, in `color`.
    pub fn icon(&self, painter: &egui::Painter, rect: Rect, icon: Icon, color: Color32) {
        painter.image(self.icons.id(), rect, uv(icon), color);
    }

    /// Draw `icon` turned by `angle` radians around its center.
    pub fn icon_rotated(
        &self,
        painter: &egui::Painter,
        center: Pos2,
        size: f32,
        icon: Icon,
        color: Color32,
        angle: f32,
    ) {
        let mut mesh = egui::epaint::Mesh::with_texture(self.icons.id());
        mesh.add_rect_with_uv(
            Rect::from_center_size(center, vec2(size, size)),
            uv(icon),
            color,
        );
        mesh.rotate(egui::emath::Rot2::from_angle(angle), center);
        painter.add(egui::Shape::mesh(mesh));
    }

    /// Draw `icon` `size` points square, centered on `center`.
    pub fn icon_at(
        &self,
        painter: &egui::Painter,
        center: Pos2,
        size: f32,
        icon: Icon,
        color: Color32,
    ) {
        self.icon(
            painter,
            Rect::from_center_size(center, vec2(size, size)),
            icon,
            color,
        );
    }

    /// The logotype, `height` points high, from `left_center`. Returns its
    /// width.
    pub fn logotype(
        &self,
        painter: &egui::Painter,
        left_center: Pos2,
        height: f32,
        dark: bool,
    ) -> f32 {
        let texture = if dark {
            &self.logotype_dark
        } else {
            &self.logotype
        };
        let [w, h] = texture.size();
        let width = height * w as f32 / h as f32;
        let rect = Rect::from_min_size(left_center - vec2(0.0, height / 2.0), vec2(width, height));
        painter.image(
            texture.id(),
            rect,
            Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)),
            Color32::WHITE,
        );
        width
    }

    /// The Chat with Work mark.
    pub fn mark(&self, painter: &egui::Painter, rect: Rect) {
        painter.image(
            self.mark.id(),
            rect,
            Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)),
            Color32::WHITE,
        );
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_atlas_has_a_cell_for_every_icon() {
        let atlas = super::decode(include_bytes!("../../../assets/chat/icons.png"));
        assert_eq!(atlas.size, [super::COLUMNS * 64, super::ROWS * 64]);
    }
}
