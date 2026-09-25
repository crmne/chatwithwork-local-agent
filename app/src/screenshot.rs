//! `--screenshot FILE`: render the settings window, save a PNG of it, and
//! quit. Used for the README and to check the look on each platform.

use std::path::PathBuf;
use std::sync::atomic::Ordering;

use crate::ui::SettingsApp;

pub struct Capture {
    app: SettingsApp,
    path: PathBuf,
    frames: u32,
    requested: bool,
}

impl Capture {
    pub fn new(app: SettingsApp, path: PathBuf) -> Self {
        Self {
            app,
            path,
            frames: 0,
            requested: false,
        }
    }
}

impl eframe::App for Capture {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        eframe::App::ui(&mut self.app, ui, frame);
        let ctx = ui.ctx().clone();
        let shot = ctx.input(|i| {
            i.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(image) = shot {
            let [w, h] = image.size;
            let rgba: Vec<u8> = image.pixels.iter().flat_map(|c| c.to_array()).collect();
            match image::RgbaImage::from_raw(w as u32, h as u32, rgba)
                .map(|img| img.save(&self.path))
            {
                Some(Ok(())) => eprintln!("saved {}", self.path.display()),
                Some(Err(e)) => eprintln!("saving {}: {e}", self.path.display()),
                None => eprintln!("bad screenshot size"),
            }
            self.app
                .shared()
                .close_requested
                .store(true, Ordering::SeqCst);
            ctx.request_repaint();
            return;
        }
        // Let fonts, textures and the daemon's first answers settle.
        self.frames += 1;
        if self.frames > 20 && !self.requested {
            self.requested = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(50));
    }

    fn clear_color(&self, visuals: &egui::Visuals) -> [f32; 4] {
        self.app.clear_color(visuals)
    }
}
