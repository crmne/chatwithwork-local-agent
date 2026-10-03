//! The menu bar item (macOS), notification-area icon (Windows) or tray item
//! (Linux), through fastframe-tray. Its menu is the platform's own, not
//! drawn by the app: an `NSMenu`, a Win32 menu, or DBusMenu.

use fastframe_tray::{Config, DrawIcon, Event, MenuItem};

use crate::events::{AppEvent, Waker};
use crate::model::{Platform, TrayView};

/// Menu entry ids.
mod ids {
    pub const HEADLINE: &str = "headline";
    pub const DETAIL: &str = "detail";
    pub const PAUSE: &str = "pause";
    pub const SETTINGS: &str = "settings";
    pub const QUIT: &str = "quit";
}

pub struct Tray {
    tray: fastframe_tray::Tray,
    view: TrayView,
}

/// The label of the pause entry.
fn pause_label(paused: bool) -> &'static str {
    if paused {
        "Resume Sharing"
    } else {
        "Pause Sharing"
    }
}

/// The menu: the state and its detail as greyed-out lines, then the
/// actions.
fn menu(view: &TrayView, platform: Platform) -> Vec<MenuItem> {
    vec![
        MenuItem::action(ids::HEADLINE, view.headline.clone()).enabled(false),
        MenuItem::action(ids::DETAIL, view.detail.clone().unwrap_or_default())
            .enabled(false)
            .visible(view.detail.is_some()),
        MenuItem::Separator,
        MenuItem::action(ids::PAUSE, pause_label(view.paused == Some(true)))
            .enabled(view.paused.is_some()),
        MenuItem::action(ids::SETTINGS, platform.settings_item()),
        MenuItem::Separator,
        MenuItem::action(ids::QUIT, platform.quit_item()),
    ]
}

/// The tooltip. Linux panels show the detail line under the state; Windows
/// and macOS show the state alone.
fn tooltip(view: &TrayView, platform: Platform) -> String {
    match (&view.detail, platform) {
        (Some(detail), Platform::Linux) => format!("{}\n{detail}", view.tooltip),
        _ => view.tooltip.clone(),
    }
}

/// The icon, faded while nothing is being served, and the macOS template
/// image macOS tints to match the menu bar.
fn icons(dimmed: bool) -> (DrawIcon, Option<DrawIcon>) {
    if dimmed {
        (
            crate::icons::tray_dimmed,
            Some(crate::icons::tray_template_dimmed),
        )
    } else {
        (crate::icons::tray, Some(crate::icons::tray_template))
    }
}

/// What a click asks of the app. A left click on the icon opens the
/// settings window (Linux and Windows); on macOS any click opens the menu.
fn app_event(event: Event) -> Option<AppEvent> {
    Some(match event {
        Event::Toggle | Event::Show | Event::Menu(ids::SETTINGS) => AppEvent::OpenSettings,
        Event::Menu(ids::PAUSE) => AppEvent::TogglePause,
        Event::Menu(ids::QUIT) => AppEvent::Quit,
        Event::Menu(_) => return None,
    })
}

impl Tray {
    /// Registers the item. Call on the main thread once the event loop
    /// runs: macOS makes the item then. `None` when the system has no tray
    /// to offer at all.
    pub fn new(waker: Waker, view: &TrayView) -> Option<Self> {
        let platform = Platform::current();
        let (icon, template_icon) = icons(view.dimmed);
        let mut tray = fastframe_tray::Tray::spawn(
            Config {
                id: "cww-app",
                title: "Chat with Work".into(),
                icon,
                template_icon,
                // The glyph changes with the state, so panels must draw it
                // rather than the installed app icon.
                themed_icon: false,
                menu_on_click: true,
                menu: menu(view, platform),
            },
            move || waker.wake(),
        )?;
        tray.set_tooltip(tooltip(view, platform));
        // Makes the macOS item; elsewhere the item runs on its own thread.
        tray.attach();
        Some(Self {
            tray,
            view: view.clone(),
        })
    }

    /// What the person chose since the last call.
    pub fn events(&self) -> Vec<AppEvent> {
        self.tray
            .events()
            .into_iter()
            .filter_map(app_event)
            .collect()
    }

    /// Whether a panel shows the item now. Without one, closing the window
    /// would leave no way back, so it quits the app.
    pub fn is_shown(&self) -> bool {
        self.tray.is_shown()
    }

    pub fn update(&mut self, view: &TrayView) {
        if *view == self.view {
            return;
        }
        let platform = Platform::current();
        let old = std::mem::replace(&mut self.view, view.clone());
        if view.headline != old.headline {
            self.tray.set_label(ids::HEADLINE, view.headline.clone());
        }
        if view.detail != old.detail {
            if let Some(detail) = &view.detail {
                self.tray.set_label(ids::DETAIL, detail.clone());
            }
            self.tray.set_visible(ids::DETAIL, view.detail.is_some());
        }
        if view.paused != old.paused {
            self.tray
                .set_label(ids::PAUSE, pause_label(view.paused == Some(true)));
            self.tray.set_enabled(ids::PAUSE, view.paused.is_some());
        }
        if tooltip(view, platform) != tooltip(&old, platform) {
            self.tray.set_tooltip(tooltip(view, platform));
        }
        if view.dimmed != old.dimmed {
            let (icon, template_icon) = icons(view.dimmed);
            self.tray.set_icon(icon, template_icon);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Health;

    fn view(detail: Option<&str>, paused: Option<bool>) -> TrayView {
        TrayView {
            health: Health::Online,
            headline: "Online, sharing 2 folders".into(),
            detail: detail.map(Into::into),
            paused,
            tooltip: "Chat with Work: Online".into(),
            dimmed: false,
        }
    }

    fn action(item: &MenuItem) -> (&'static str, &str, bool, bool) {
        match item {
            MenuItem::Action {
                id,
                label,
                enabled,
                visible,
            } => (id, label, *enabled, *visible),
            MenuItem::Separator => ("-", "", false, true),
        }
    }

    #[test]
    fn the_state_lines_are_greyed_out_and_the_actions_are_not() {
        let items = menu(
            &view(Some("Indexing 1 folder…"), Some(false)),
            Platform::Linux,
        );
        let items: Vec<_> = items.iter().map(action).collect();
        assert_eq!(
            items,
            [
                ("headline", "Online, sharing 2 folders", false, true),
                ("detail", "Indexing 1 folder…", false, true),
                ("-", "", false, true),
                ("pause", "Pause Sharing", true, true),
                ("settings", "Settings", true, true),
                ("-", "", false, true),
                ("quit", "Quit", true, true),
            ]
        );
    }

    #[test]
    fn without_a_detail_or_a_daemon_to_pause_those_entries_step_back() {
        let items = menu(&view(None, None), Platform::MacOs);
        let items: Vec<_> = items.iter().map(action).collect();
        assert!(!items[1].3, "no detail line");
        assert_eq!(items[3], ("pause", "Pause Sharing", false, true));
        assert_eq!(items[4].1, "Settings…");
        assert_eq!(items[6].1, "Quit Chat with Work");
        let paused = menu(&view(None, Some(true)), Platform::Windows);
        assert_eq!(action(&paused[3]), ("pause", "Resume Sharing", true, true));
        assert_eq!(action(&paused[6]).1, "Exit");
    }

    /// DBusMenu reads `_` as a shortcut marker; fastframe-tray escapes it,
    /// so the label goes in as written.
    #[test]
    fn labels_go_in_as_written() {
        let items = menu(
            &view(Some("Last access read q3_plan.md"), None),
            Platform::Linux,
        );
        assert_eq!(action(&items[1]).1, "Last access read q3_plan.md");
    }

    #[test]
    fn linux_tooltips_carry_the_detail() {
        let indexing = view(Some("Indexing 1 folder…"), None);
        assert_eq!(
            tooltip(&indexing, Platform::Linux),
            "Chat with Work: Online\nIndexing 1 folder…"
        );
        assert_eq!(
            tooltip(&indexing, Platform::Windows),
            "Chat with Work: Online"
        );
        assert_eq!(
            tooltip(&view(None, None), Platform::Linux),
            "Chat with Work: Online"
        );
    }

    #[test]
    fn clicks_open_the_window_and_entries_do_what_they_say() {
        assert_eq!(app_event(Event::Toggle), Some(AppEvent::OpenSettings));
        assert_eq!(app_event(Event::Show), Some(AppEvent::OpenSettings));
        assert_eq!(
            app_event(Event::Menu("settings")),
            Some(AppEvent::OpenSettings)
        );
        assert_eq!(app_event(Event::Menu("pause")), Some(AppEvent::TogglePause));
        assert_eq!(app_event(Event::Menu("quit")), Some(AppEvent::Quit));
        assert_eq!(app_event(Event::Menu("headline")), None);
    }

    #[test]
    fn the_icon_fades_while_nothing_is_served() {
        let (bright, template) = icons(false);
        let (faded, faded_template) = icons(true);
        assert_ne!(bright(32), faded(32));
        assert_ne!(template.unwrap()(36), faded_template.unwrap()(36));
    }
}
