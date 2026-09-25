//! macOS and Windows: `tray-icon`, with native `muda` menus.
//!
//! The item must be created and changed on the main thread, which runs the
//! event loop. Menu clicks arrive through a handler that forwards them to
//! the main loop and wakes it.

use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

use super::{ids, pause_label};
use crate::events::{AppEvent, Events};
use crate::model::{Platform, TrayView};

pub struct Tray {
    icon: TrayIcon,
    menu: Menu,
    headline: MenuItem,
    detail: MenuItem,
    detail_shown: bool,
    pause: MenuItem,
    view: TrayView,
}

fn icon_for(view: &TrayView) -> Option<Icon> {
    #[cfg(target_os = "macos")]
    let rgba = crate::icons::tray_template(view.dimmed);
    #[cfg(not(target_os = "macos"))]
    let rgba = crate::icons::tray(view.dimmed, false);
    Icon::from_rgba(rgba.pixels, rgba.width, rgba.height)
        .map_err(|e| log::warn!("tray icon: {e}"))
        .ok()
}

impl Tray {
    /// Create the item. Call on the main thread once the event loop runs.
    pub fn new(events: Events, view: &TrayView, _assume_host: bool) -> Option<Self> {
        let platform = Platform::current();
        let headline = MenuItem::new(&view.headline, false, None);
        let detail = MenuItem::new(view.detail.as_deref().unwrap_or_default(), false, None);
        let pause = MenuItem::with_id(
            ids::PAUSE,
            pause_label(view.paused == Some(true)),
            view.paused.is_some(),
            None,
        );
        let settings = MenuItem::with_id(ids::SETTINGS, platform.settings_item(), true, None);
        let quit = MenuItem::with_id(ids::QUIT, platform.quit_item(), true, None);
        let menu = Menu::new();
        let built = menu.append_items(&[
            &headline,
            &PredefinedMenuItem::separator(),
            &pause,
            &settings,
            &PredefinedMenuItem::separator(),
            &quit,
        ]);
        if let Err(e) = built {
            log::warn!("tray menu: {e}");
            return None;
        }
        let mut detail_shown = false;
        if view.detail.is_some() && menu.insert(&detail, 1).is_ok() {
            detail_shown = true;
        }

        let menu_events = events.clone();
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            let event = match event.id.0.as_str() {
                ids::PAUSE => AppEvent::TogglePause,
                ids::SETTINGS => AppEvent::OpenSettings,
                ids::QUIT => AppEvent::Quit,
                _ => return,
            };
            menu_events.send(event);
        }));
        // Windows: a left click opens the settings window, a right click
        // the menu. macOS shows the menu on any click, as menu bar items do.
        #[cfg(windows)]
        tray_icon::TrayIconEvent::set_event_handler(Some(
            move |event: tray_icon::TrayIconEvent| {
                if let tray_icon::TrayIconEvent::Click {
                    button: tray_icon::MouseButton::Left,
                    button_state: tray_icon::MouseButtonState::Up,
                    ..
                } = event
                {
                    events.send(AppEvent::OpenSettings);
                }
            },
        ));
        #[cfg(not(windows))]
        drop(events);

        let builder = TrayIconBuilder::new()
            .with_tooltip(&view.tooltip)
            .with_menu(Box::new(menu.clone()));
        let builder = match icon_for(view) {
            Some(icon) => builder.with_icon(icon),
            None => builder,
        };
        #[cfg(target_os = "macos")]
        let builder = builder.with_icon_as_template(true);
        #[cfg(windows)]
        let builder = builder.with_menu_on_left_click(false);
        let icon = match builder.build() {
            Ok(icon) => icon,
            Err(e) => {
                log::warn!("no tray icon: {e}");
                return None;
            }
        };
        Some(Self {
            icon,
            menu,
            headline,
            detail,
            detail_shown,
            pause,
            view: view.clone(),
        })
    }

    pub fn update(&mut self, view: &TrayView) {
        if *view == self.view {
            return;
        }
        if view.headline != self.view.headline {
            self.headline.set_text(&view.headline);
        }
        match (&view.detail, self.detail_shown) {
            (Some(text), true) => self.detail.set_text(text),
            (Some(text), false) => {
                self.detail.set_text(text);
                self.detail_shown = self.menu.insert(&self.detail, 1).is_ok();
            }
            (None, true) => {
                let _ = self.menu.remove(&self.detail);
                self.detail_shown = false;
            }
            (None, false) => {}
        }
        if view.paused != self.view.paused {
            self.pause.set_text(pause_label(view.paused == Some(true)));
            self.pause.set_enabled(view.paused.is_some());
        }
        if view.tooltip != self.view.tooltip {
            let _ = self.icon.set_tooltip(Some(&view.tooltip));
        }
        if view.dimmed != self.view.dimmed
            && let Some(icon) = icon_for(view)
        {
            #[cfg(target_os = "macos")]
            let _ = self.icon.set_icon_with_as_template(Some(icon), true);
            #[cfg(not(target_os = "macos"))]
            let _ = self.icon.set_icon(Some(icon));
        }
        self.view = view.clone();
    }
}
