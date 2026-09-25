//! First run: start the agent, pair, and choose what to share. Nothing is
//! shared until the user clicks a button that says so.

use egui::{RichText, Ui, Vec2};

use super::widgets::{self, group};
use super::{Page, SettingsApp};
use crate::model::Status;
use crate::paths;

impl SettingsApp {
    pub(super) fn welcome_page(&mut self, ui: &mut Ui, status: Option<&Status>) {
        let theme = self.theme;
        ui.vertical_centered(|ui| {
            let texture = self.mark_texture(ui.ctx());
            ui.add(egui::Image::new(&texture).fit_to_exact_size(Vec2::splat(56.0)));
            ui.add_space(4.0);
            ui.label(
                RichText::new("Welcome to Chat with Work Local Agent")
                    .heading()
                    .color(theme.palette.text),
            );
            ui.add(
                egui::Label::new(theme.weak(
                    "Chat with Work can search and read the folders you choose on this computer. \
                     Nothing is shared until you say so.",
                ))
                .wrap(),
            );
        });
        ui.add_space(14.0);

        // 1. The background agent.
        step(
            ui,
            &theme,
            1,
            "Run the Local Agent",
            status.is_some(),
            |ui| {
                if status.is_some() {
                    ui.label(theme.weak("It's running in the background."));
                } else {
                    self.agent_missing(ui);
                }
            },
        );

        // 2. Pairing.
        let paired = status.is_some_and(|s| s.is_paired());
        step(
            ui,
            &theme,
            2,
            "Connect to Chat with Work",
            paired,
            |ui| match status {
                None => {
                    ui.label(theme.weak("Start the Local Agent first."));
                }
                Some(s) if s.is_paired() && self.account.pairing.is_none() => {
                    ui.label(theme.weak(format!(
                        "Paired with {}.",
                        s.server_host().unwrap_or_default()
                    )));
                }
                Some(s) => self.pairing_section(ui, s),
            },
        );

        // 3. Folders, with explicit consent for Documents.
        let shared = status.is_some_and(|s| !s.roots.is_empty());
        step(
            ui,
            &theme,
            3,
            "Choose what to share",
            shared,
            |ui| match status {
                None => {
                    ui.label(theme.weak("Start the Local Agent first."));
                }
                Some(s) if !s.roots.is_empty() => {
                    let labels: Vec<&str> = s.roots.iter().map(|r| r.label.as_str()).collect();
                    ui.label(theme.weak(format!("Sharing {}.", labels.join(", "))));
                }
                Some(_) => {
                    let documents = self.documents.clone();
                    if let Some(documents) = &documents {
                        ui.add(
                            egui::Label::new(RichText::new(format!(
                                "Share your Documents folder ({})? Chat with Work will be able to \
                             search and read the files in it, except private ones such as keys \
                             and passwords.",
                                paths::display(documents)
                            )))
                            .wrap(),
                        );
                    } else {
                        ui.label("Choose a folder for Chat with Work to search.");
                    }
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        let busy = self.folders.adding || self.folders.picking;
                        if let Some(documents) = documents {
                            let share = ui.add_enabled_ui(!busy, |ui| {
                                widgets::primary_button(ui, &theme, "Share Documents")
                            });
                            if share.inner.clicked() {
                                self.add_folder(ui.ctx(), documents);
                            }
                        }
                        if ui
                            .add_enabled(!busy, egui::Button::new("Choose Another Folder…"))
                            .clicked()
                        {
                            self.pick_folder(ui.ctx());
                        }
                    });
                    if let Some(error) = self.folders.error.clone() {
                        widgets::notice(ui, &theme, &error, theme.palette.danger);
                    }
                }
            },
        );

        ui.add_space(12.0);
        ui.horizontal(|ui| {
            widgets::switch(ui, &theme, &mut self.welcome_autostart, "Start at login");
            ui.label(format!(
                "Show Chat with Work Local Agent in the {} when you log in",
                super::general::tray_place(theme.platform)
            ));
        });
        ui.add_space(12.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let done = status.is_some_and(|s| s.is_paired() && !s.roots.is_empty());
            let clicked = if done {
                widgets::primary_button(ui, &theme, "Done").clicked()
            } else {
                ui.button("Skip for Now").clicked()
            };
            if clicked {
                self.finish_welcome();
            }
        });
    }

    pub(super) fn finish_welcome(&mut self) {
        self.app_state.onboarded = true;
        self.app_state.save(&self.shared.paths);
        if self.welcome_autostart != self.general.autostart {
            match crate::autostart::set_enabled(self.welcome_autostart) {
                Ok(()) => self.general.autostart = self.welcome_autostart,
                Err(e) => self.general.error = Some(format!("{e:#}")),
            }
        }
        self.page = Page::Folders;
    }
}

/// A numbered step with a check mark once it's done.
fn step(
    ui: &mut Ui,
    theme: &super::theme::Theme,
    number: u8,
    title: &str,
    done: bool,
    add: impl FnOnce(&mut Ui),
) {
    group(ui, theme, |ui| {
        ui.horizontal(|ui| {
            let (mark, color) = if done {
                ("✔".to_string(), theme.palette.success)
            } else {
                (number.to_string(), theme.palette.accent)
            };
            let size = 22.0;
            let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), egui::Sense::hover());
            ui.painter()
                .circle_filled(rect.center(), size / 2.0, color.gamma_multiply(0.18));
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                mark,
                egui::FontId::proportional(theme.metrics.small + 1.0),
                color,
            );
            ui.label(theme.strong(title));
        });
        ui.indent(title, add);
    });
    ui.add_space(8.0);
}
