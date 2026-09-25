//! Start at login, pausing, the background agent, and where things are.

use egui::{RichText, Ui};

use super::widgets::{self, group, row_separator, setting_row};
use super::{SettingsApp, open_path, open_url};
use crate::model::{Platform, Status};
use crate::paths;

pub const REPOSITORY: &str = "https://github.com/crmne/chatwithwork-local-agent";

/// Where the tray item lives, in the platform's words.
pub fn tray_place(platform: Platform) -> &'static str {
    match platform {
        Platform::MacOs => "menu bar",
        Platform::Windows => "notification area",
        Platform::Linux => "system tray",
    }
}

impl SettingsApp {
    pub(super) fn general_page(&mut self, ui: &mut Ui, status: Option<&Status>) {
        let theme = self.theme;
        widgets::page_header(ui, &theme, "General", "");

        group(ui, &theme, |ui| {
            let description = format!(
                "Show Chat with Work Local Agent in the {} when you log in. The agent itself \
                 always runs in the background once it's started.",
                tray_place(theme.platform)
            );
            setting_row(ui, &theme, "Start at login", &description, |ui| {
                let mut on = self.general.autostart;
                if widgets::switch(ui, &theme, &mut on, "Start at login").changed() {
                    match crate::autostart::set_enabled(on) {
                        Ok(()) => {
                            self.general.autostart = on;
                            self.general.error = None;
                        }
                        Err(e) => self.general.error = Some(format!("{e:#}")),
                    }
                }
            });
            row_separator(ui, &theme);
            let paused = status.is_some_and(|s| s.paused);
            setting_row(
                ui,
                &theme,
                "Pause sharing",
                "While paused, the Local Agent refuses every request from Chat with Work.",
                |ui| {
                    let mut on = paused;
                    let enabled = status.is_some() && !self.general.pausing;
                    let changed = ui
                        .add_enabled_ui(enabled, |ui| {
                            widgets::switch(ui, &theme, &mut on, "Pause sharing")
                        })
                        .inner
                        .changed();
                    if changed {
                        self.set_paused(ui.ctx(), on);
                    }
                },
            );
        });
        if let Some(error) = self.general.error.clone() {
            ui.add_space(6.0);
            widgets::notice(ui, &theme, &error, theme.palette.danger);
        }

        widgets::section(ui, &theme, "Local Agent");
        match status {
            None => self.agent_missing(ui),
            Some(status) => {
                group(ui, &theme, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Background agent").color(theme.palette.text));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(theme.weak(format!(
                                "Running · version {} · process {}",
                                status.version, status.pid
                            )));
                        });
                    });
                    for (title, path) in [
                        ("Configuration", status.config_file.clone()),
                        ("Activity log", status.audit_file.clone()),
                    ] {
                        let Some(path) = path else { continue };
                        row_separator(ui, &theme);
                        ui.horizontal(|ui| {
                            ui.vertical(|ui| {
                                ui.set_width((ui.available_width() - 90.0).max(160.0));
                                ui.label(RichText::new(title).color(theme.palette.text));
                                ui.add_space(-4.0);
                                ui.add(
                                    egui::Label::new(theme.small(paths::display(&path))).truncate(),
                                );
                            });
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.button("Open").clicked() {
                                        open_path(&path);
                                    }
                                },
                            );
                        });
                    }
                });
            }
        }

        widgets::section(ui, &theme, "About");
        group(ui, &theme, |ui| {
            ui.label(RichText::new(format!(
                "Chat with Work Local Agent {}",
                env!("CARGO_PKG_VERSION")
            )));
            ui.add(
                egui::Label::new(theme.small(
                    "Open source under the MIT or Apache 2.0 license. The code, the wire \
                     protocol and the threat model are public.",
                ))
                .wrap(),
            );
            if ui.link("Source code and security notes").clicked() {
                open_url(REPOSITORY);
            }
        });
    }
}
