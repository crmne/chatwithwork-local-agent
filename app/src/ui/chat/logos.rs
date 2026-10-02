//! The web's own images: each service's and model maker's logo, and the
//! file-type icons, fetched from the paired server through the daemon
//! (`asset`, CONTROL.md) and drawn as the web draws them, monochrome logos
//! white in dark mode (`dark:brightness-0 dark:invert`).
//!
//! Their paths are fingerprinted, so an image never changes: each is kept
//! in memory for as long as the window is open, and on disk, owner-only,
//! under cww's data folder, so it's fetched once. While one loads, or if it
//! can't be had, the page draws what it drew before: a letter chip or a
//! Phosphor icon. Loading asks for no frames: the fetching thread wakes the
//! window once, when the image is in.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::mpsc::{Receiver, Sender, channel};

use egui::{Color32, Rect, TextureHandle, TextureOptions, pos2};

use cww::tui::chat::{Asset, Chats};

use super::paint;
use super::tokens::Palette;

/// The longest side a logo is drawn into a texture at: the largest is 36
/// points, so this stays sharp at 3x.
const RASTER: u32 = 128;
/// The longest side kept of a PNG, which the server keeps small anyway.
const MAX_SIDE: u32 = 1024;

/// An image, decoded on the fetching thread.
struct Decoded {
    image: egui::ColorImage,
    /// The same shape in white, for a monochrome logo in dark mode.
    white: Option<egui::ColorImage>,
}

enum Entry {
    Loading,
    Ready {
        image: TextureHandle,
        white: Option<TextureHandle>,
        aspect: f32,
    },
    Failed,
}

/// Where images come from: the daemon, and the paired server's folder in
/// the disk cache.
#[derive(Clone)]
struct Source {
    chats: Chats,
    server: String,
    dir: Option<PathBuf>,
}

struct Inner {
    source: Option<Source>,
    entries: HashMap<String, Entry>,
    tx: Sender<(String, Result<Decoded, String>)>,
    rx: Receiver<(String, Result<Decoded, String>)>,
    ctx: Option<egui::Context>,
}

pub struct Logos {
    inner: Mutex<Inner>,
}

impl Default for Logos {
    fn default() -> Self {
        let (tx, rx) = channel();
        Self {
            inner: Mutex::new(Inner {
                source: None,
                entries: HashMap::new(),
                tx,
                rx,
                ctx: None,
            }),
        }
    }
}

impl Logos {
    /// Fetch from this daemon, for the server it's paired with, keeping
    /// images under `data_dir`. Another server forgets what was loaded.
    pub fn set_source(&self, socket: &Path, server: Option<&str>, data_dir: Option<&Path>) {
        let mut inner = self.inner.lock().expect("logos");
        let Some(server) = server else {
            inner.source = None;
            return;
        };
        if inner.source.as_ref().is_some_and(|s| s.server == server) {
            return;
        }
        inner.entries.clear();
        inner.source = Some(Source {
            chats: Chats::new(socket),
            server: server.to_string(),
            dir: data_dir.map(|d| d.join("chat-assets").join(host_folder(server))),
        });
    }

    /// Take the images fetched since the last frame into textures.
    pub fn poll(&self, ctx: &egui::Context) {
        let mut inner = self.inner.lock().expect("logos");
        if inner.ctx.is_none() {
            inner.ctx = Some(ctx.clone());
        }
        let done: Vec<_> = inner.rx.try_iter().collect();
        for (path, result) in done {
            let entry = match result {
                Ok(decoded) => {
                    let [w, h] = decoded.image.size;
                    let name = format!("chat-logo{path}");
                    Entry::Ready {
                        image: ctx.load_texture(&name, decoded.image, options()),
                        white: decoded
                            .white
                            .map(|white| ctx.load_texture(name + "-white", white, options())),
                        aspect: w as f32 / h.max(1) as f32,
                    }
                }
                Err(e) => {
                    log::warn!("can't show {path}: {e}");
                    Entry::Failed
                }
            };
            // A source changed meanwhile forgot it: keep it only if asked.
            if inner.entries.contains_key(&path) {
                inner.entries.insert(path, entry);
            }
        }
    }

    /// Images still on their way.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn pending(&self) -> usize {
        let inner = self.inner.lock().expect("logos");
        inner
            .entries
            .values()
            .filter(|e| matches!(e, Entry::Loading))
            .count()
    }

    /// Draw `asset` contained in `rect`, its corners rounded by `radius`
    /// (half the side for a round one). False when it isn't there (yet):
    /// draw the fallback then.
    pub fn draw(
        &self,
        painter: &egui::Painter,
        asset: Option<&Asset>,
        rect: Rect,
        radius: f32,
        p: &Palette,
        opacity: f32,
    ) -> bool {
        let Some(asset) = asset else { return false };
        let mut inner = self.inner.lock().expect("logos");
        match inner.entries.get(&asset.path) {
            Some(Entry::Ready {
                image,
                white,
                aspect,
            }) => {
                let texture = match white {
                    Some(white) if asset.monochrome && p.dark => white,
                    _ => image,
                };
                let size = if *aspect >= 1.0 {
                    egui::vec2(rect.width(), rect.width() / aspect)
                } else {
                    egui::vec2(rect.height() * aspect, rect.height())
                };
                let rect = Rect::from_center_size(rect.center(), size);
                image_rounded(
                    painter,
                    texture.id(),
                    rect,
                    radius,
                    Color32::WHITE.gamma_multiply(opacity),
                );
                true
            }
            Some(Entry::Loading | Entry::Failed) => false,
            None => {
                let Some(source) = inner.source.clone() else {
                    return false;
                };
                inner.entries.insert(asset.path.clone(), Entry::Loading);
                let (tx, ctx, path) = (inner.tx.clone(), inner.ctx.clone(), asset.path.clone());
                let spawned = std::thread::Builder::new()
                    .name("cww-chat-logo".into())
                    .spawn(move || {
                        let result = fetch(&source, &path);
                        let _ = tx.send((path, result));
                        if let Some(ctx) = ctx {
                            ctx.request_repaint();
                        }
                    });
                if spawned.is_err() {
                    inner.entries.insert(asset.path.clone(), Entry::Failed);
                }
                false
            }
        }
    }
}

fn options() -> TextureOptions {
    TextureOptions {
        mipmap_mode: Some(egui::TextureFilter::Linear),
        ..TextureOptions::LINEAR
    }
}

/// A folder name for a server: `chatwithwork.com`, `localhost_3000`.
fn host_folder(server: &str) -> String {
    let host = server
        .split_once("://")
        .map_or(server, |(_, rest)| rest)
        .trim_end_matches('/');
    host.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Where an image is kept on disk, for a path the daemon would fetch.
fn cache_file(dir: &Path, path: &str) -> Option<PathBuf> {
    let path = cww::chats::asset_path(path).ok()?;
    let name = path.trim_start_matches("/assets/").replace('/', "__");
    Some(dir.join(name))
}

/// The image at `path`: from the disk cache, or from the daemon, then kept.
fn fetch(source: &Source, path: &str) -> Result<Decoded, String> {
    let file = source.dir.as_deref().and_then(|dir| cache_file(dir, path));
    if let Some(file) = &file
        && let Ok(bytes) = std::fs::read(file)
        && let Ok(decoded) = decode(&bytes)
    {
        return Ok(decoded);
    }
    let data = source.chats.asset(path).map_err(|f| f.message)?;
    let decoded = decode(&data.bytes)?;
    if let Some(file) = &file
        && let Err(e) = cww::paths::write_private_file(file, &data.bytes)
    {
        log::warn!("can't keep {path}: {e:#}");
    }
    Ok(decoded)
}

/// An SVG or a PNG, as a texture's pixels, and in white for a monochrome
/// logo.
fn decode(bytes: &[u8]) -> Result<Decoded, String> {
    let image = if bytes.starts_with(b"\x89PNG") {
        png(bytes)?
    } else {
        svg(bytes)?
    };
    let white = {
        let pixels = image
            .pixels
            .iter()
            .map(|c| Color32::from_rgba_premultiplied(c.a(), c.a(), c.a(), c.a()))
            .collect();
        egui::ColorImage {
            size: image.size,
            source_size: image.source_size,
            pixels,
        }
    };
    Ok(Decoded {
        image,
        white: Some(white),
    })
}

fn png(bytes: &[u8]) -> Result<egui::ColorImage, String> {
    let mut image = image::load_from_memory_with_format(bytes, image::ImageFormat::Png)
        .map_err(|e| e.to_string())?;
    if image.width().max(image.height()) > MAX_SIDE {
        image = image.thumbnail(MAX_SIDE, MAX_SIDE);
    }
    let image = image.into_rgba8();
    let size = [image.width() as usize, image.height() as usize];
    Ok(egui::ColorImage::from_rgba_unmultiplied(
        size,
        image.as_raw(),
    ))
}

fn svg(bytes: &[u8]) -> Result<egui::ColorImage, String> {
    let tree = resvg::usvg::Tree::from_data(bytes, &resvg::usvg::Options::default())
        .map_err(|e| e.to_string())?;
    let size = tree.size();
    let scale = RASTER as f32 / size.width().max(size.height()).max(1.0);
    let (w, h) = (
        (size.width() * scale).round().max(1.0) as u32,
        (size.height() * scale).round().max(1.0) as u32,
    );
    let mut pixmap = resvg::tiny_skia::Pixmap::new(w, h).ok_or("an empty image")?;
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    Ok(egui::ColorImage::from_rgba_premultiplied(
        [w as usize, h as usize],
        pixmap.data(),
    ))
}

/// Draw a texture over `rect` with its corners rounded, as `border-radius`
/// clips an `<img>`.
pub fn image_rounded(
    painter: &egui::Painter,
    texture: egui::TextureId,
    rect: Rect,
    radius: f32,
    tint: Color32,
) {
    if radius <= 0.0 {
        painter.image(
            texture,
            rect,
            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
            tint,
        );
        return;
    }
    let radius = radius.min(rect.width() / 2.0).min(rect.height() / 2.0);
    let outline = paint::rounded_outline(rect, [radius; 4], 8);
    let uv = |p: egui::Pos2| {
        pos2(
            (p.x - rect.left()) / rect.width(),
            (p.y - rect.top()) / rect.height(),
        )
    };
    let mut mesh = egui::epaint::Mesh::with_texture(texture);
    let center = rect.center();
    mesh.vertices.push(egui::epaint::Vertex {
        pos: center,
        uv: uv(center),
        color: tint,
    });
    for p in &outline {
        mesh.vertices.push(egui::epaint::Vertex {
            pos: *p,
            uv: uv(*p),
            color: tint,
        });
    }
    let n = outline.len() as u32;
    for k in 0..n {
        mesh.add_triangle(0, 1 + k, 1 + (k + 1) % n);
    }
    painter.add(egui::Shape::mesh(mesh));
}

#[cfg(test)]
mod tests {
    use super::*;

    const SVG: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" width="64" height="32"><rect width="64" height="32" fill="#ff0000"/></svg>"##;

    #[test]
    fn svgs_are_drawn_at_the_longest_side() {
        let decoded = decode(SVG).unwrap();
        assert_eq!(decoded.image.size, [128, 64]);
        assert_eq!(decoded.image.pixels[0], Color32::from_rgb(255, 0, 0));
        // In white for dark mode, keeping the shape.
        assert_eq!(decoded.white.unwrap().pixels[0], Color32::WHITE);
    }

    #[test]
    fn pngs_are_read_and_anything_else_is_refused() {
        let mut png = Vec::new();
        image::RgbaImage::from_pixel(3, 2, image::Rgba([0, 0, 255, 255]))
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        assert_eq!(decode(&png).unwrap().image.size, [3, 2]);
        assert!(decode(b"GIF89a").is_err());
    }

    #[test]
    fn images_are_kept_under_the_servers_folder() {
        assert_eq!(host_folder("https://chatwithwork.com"), "chatwithwork.com");
        assert_eq!(host_folder("http://localhost:3000/"), "localhost_3000");
        let dir = Path::new("/cache");
        assert_eq!(
            cache_file(dir, "/assets/providers/slack-0c9450af.svg"),
            Some(dir.join("providers__slack-0c9450af.svg"))
        );
        assert_eq!(cache_file(dir, "/assets/../secrets"), None);
        assert_eq!(cache_file(dir, "/etc/passwd"), None);
    }

    #[test]
    fn a_kept_image_is_read_without_asking_the_daemon() {
        let dir = tempfile::tempdir().unwrap();
        let source = Source {
            chats: Chats::new(&dir.path().join("no-daemon.sock")),
            server: "https://chatwithwork.com".into(),
            dir: Some(dir.path().to_path_buf()),
        };
        let path = "/assets/providers/slack-0c9450af.svg";
        assert!(fetch(&source, path).is_err(), "nothing kept, no daemon");
        cww::paths::write_private_file(&cache_file(dir.path(), path).unwrap(), SVG).unwrap();
        assert_eq!(fetch(&source, path).unwrap().image.size, [128, 64]);
    }

    #[cfg(unix)]
    #[test]
    fn a_fetched_image_is_kept_on_disk_for_its_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let file = cache_file(&dir.path().join("host"), "/assets/a/b-1.svg").unwrap();
        cww::paths::write_private_file(&file, SVG).unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&file), 0o600);
        assert_eq!(mode(file.parent().unwrap()), 0o700);
    }
}
