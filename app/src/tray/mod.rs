//! The menu bar item (macOS), notification-area icon (Windows) or tray item
//! (Linux). Its menu is the platform's own, not drawn by the app.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(not(target_os = "linux"))]
mod native;

#[cfg(target_os = "linux")]
pub use linux::Tray;
#[cfg(not(target_os = "linux"))]
pub use native::Tray;

/// Menu item IDs.
#[cfg_attr(target_os = "linux", allow(dead_code))]
pub(crate) mod ids {
    pub const PAUSE: &str = "pause";
    pub const SETTINGS: &str = "settings";
    pub const QUIT: &str = "quit";
}

/// The label of the pause item.
pub(crate) fn pause_label(paused: bool) -> &'static str {
    if paused {
        "Resume Sharing"
    } else {
        "Pause Sharing"
    }
}
