//! Linux: a StatusNotifierItem through `ksni`, with its menu over DBusMenu,
//! so the panel draws it (Waybar, KDE, GNOME with AppIndicator support).
//!
//! ksni runs its own thread and blocks on D-Bus; clicks are forwarded to
//! the main loop.

use ksni::blocking::TrayMethods;

use super::pause_label;
use crate::events::{AppEvent, Events};
use crate::model::{Platform, TrayView};

struct Item {
    view: TrayView,
    events: Events,
}

/// DBusMenu reads `_` as a mnemonic marker.
fn label(text: &str) -> String {
    text.replace('_', "__")
}

fn pixmap(dimmed: bool, large: bool) -> ksni::Icon {
    let rgba = crate::icons::tray(dimmed, large);
    // ksni wants ARGB32 in network byte order.
    let mut data = Vec::with_capacity(rgba.pixels.len());
    for px in rgba.pixels.as_chunks::<4>().0 {
        data.extend_from_slice(&[px[3], px[0], px[1], px[2]]);
    }
    ksni::Icon {
        width: rgba.width as i32,
        height: rgba.height as i32,
        data,
    }
}

impl ksni::Tray for Item {
    fn id(&self) -> String {
        "cww-app".into()
    }

    fn title(&self) -> String {
        "Chat with Work Local Agent".into()
    }

    fn category(&self) -> ksni::Category {
        ksni::Category::ApplicationStatus
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        vec![
            pixmap(self.view.dimmed, false),
            pixmap(self.view.dimmed, true),
        ]
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: self.view.tooltip.clone(),
            description: self.view.detail.clone().unwrap_or_default(),
            ..Default::default()
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        self.events.send(AppEvent::OpenSettings);
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::{MenuItem, StandardItem};
        let platform = Platform::current();
        let mut items: Vec<MenuItem<Self>> = vec![
            StandardItem {
                label: label(&self.view.headline),
                enabled: false,
                ..Default::default()
            }
            .into(),
        ];
        if let Some(detail) = &self.view.detail {
            items.push(
                StandardItem {
                    label: label(detail),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
            );
        }
        items.extend([
            MenuItem::Separator,
            StandardItem {
                label: pause_label(self.view.paused == Some(true)).into(),
                enabled: self.view.paused.is_some(),
                activate: Box::new(|item: &mut Self| item.events.send(AppEvent::TogglePause)),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: platform.settings_item().into(),
                activate: Box::new(|item: &mut Self| item.events.send(AppEvent::OpenSettings)),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: platform.quit_item().into(),
                activate: Box::new(|item: &mut Self| item.events.send(AppEvent::Quit)),
                ..Default::default()
            }
            .into(),
        ]);
        items
    }
}

pub struct Tray {
    handle: ksni::blocking::Handle<Item>,
    view: TrayView,
}

impl Tray {
    /// Register the item. Without a tray host this returns `None`, unless
    /// `assume_host` is set (at login the panel may start after the app).
    pub fn new(events: Events, view: &TrayView, assume_host: bool) -> Option<Self> {
        let item = Item {
            view: view.clone(),
            events,
        };
        match item.assume_sni_available(assume_host).spawn() {
            Ok(handle) => Some(Self {
                handle,
                view: view.clone(),
            }),
            Err(e) => {
                log::info!("no system tray: {e}");
                None
            }
        }
    }

    pub fn update(&mut self, view: &TrayView) {
        if *view == self.view {
            return;
        }
        let next = view.clone();
        self.handle.update(move |item| item.view = next);
        self.view = view.clone();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn underscores_are_not_mnemonics() {
        assert_eq!(label("read docs:q3_plan.md"), "read docs:q3__plan.md");
    }

    #[test]
    fn pixmaps_are_argb() {
        let icon = pixmap(false, false);
        assert_eq!(icon.data.len(), (icon.width * icon.height * 4) as usize);
        let rgba = crate::icons::tray(false, false);
        let opaque = rgba
            .pixels
            .as_chunks::<4>()
            .0
            .iter()
            .position(|p| p[3] == 255)
            .unwrap();
        assert_eq!(icon.data[opaque * 4], 255, "alpha comes first");
    }
}
