//! Pairing with Chat with Work: the device flow opens the approval page in
//! the browser and shows the code to compare. The daemon does the rest.

use egui::{RichText, Ui};

use super::widgets::{self, group, row_separator};
use super::{SettingsApp, open_url, status_color};
use crate::model::{Health, Status, host_of};
use crate::pairing::Code;
use crate::time;

impl SettingsApp {
    pub(super) fn account_page(&mut self, ui: &mut Ui, status: Option<&Status>) {
        let theme = self.theme;
        widgets::page_header(
            ui,
            &theme,
            "Account",
            "Pair this computer with your Chat with Work account.",
        );
        let Some(status) = status else {
            self.agent_missing(ui);
            return;
        };
        self.pairing_section(ui, status);
    }

    /// Pairing state and actions, shared with the welcome page.
    pub(super) fn pairing_section(&mut self, ui: &mut Ui, status: &Status) {
        let theme = self.theme;
        if let Some(pairing) = self.account.pairing.clone() {
            match pairing {
                Some(code) => self.pairing_card(ui, &code),
                None => {
                    ui.label(theme.weak("Asking Chat with Work for a code…"));
                    if ui.button("Cancel").clicked() {
                        self.cancel_pairing();
                    }
                }
            }
        } else if status.is_paired() {
            self.paired_card(ui, status);
        } else {
            group(ui, &theme, |ui| {
                ui.label(theme.strong("This computer isn't paired"));
                ui.add(
                    egui::Label::new(theme.weak(
                        "Pairing opens Chat with Work in your browser, where you approve this \
                         computer. It gets its own key, which never leaves it.",
                    ))
                    .wrap(),
                );
                ui.add_space(4.0);
                self.pair_button(ui, "Pair with Chat with Work…");
                ui.add_space(2.0);
                ui.checkbox(&mut self.account.custom_server, "Use a self-hosted server");
                if self.account.custom_server {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.account.server)
                            .hint_text("https://chat.example.com")
                            .desired_width(320.0),
                    );
                }
            });
        }
        if let Some(error) = self.account.error.clone() {
            ui.add_space(6.0);
            widgets::notice(ui, &theme, &error, theme.palette.danger);
        }
    }

    fn pair_button(&mut self, ui: &mut Ui, label: &str) {
        let theme = self.theme;
        let busy = self.account.requesting;
        let response = ui.add_enabled_ui(!busy, |ui| {
            widgets::primary_button(
                ui,
                &theme,
                if busy {
                    "Asking Chat with Work…"
                } else {
                    label
                },
            )
        });
        if response.inner.clicked() {
            self.pair(ui.ctx());
        }
    }

    fn pairing_card(&mut self, ui: &mut Ui, pairing: &Code) {
        let theme = self.theme;
        group(ui, &theme, |ui| {
            ui.label(theme.strong("Approve this computer in your browser"));
            ui.add(
                egui::Label::new(theme.weak(format!(
                    "Open {} and check that it shows this code, then approve “{}”.",
                    host_of(&pairing.verification_uri),
                    pairing.device_name
                )))
                .wrap(),
            );
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(&pairing.user_code)
                        .monospace()
                        .size(theme.metrics.title + 6.0)
                        .color(theme.palette.text),
                );
                if ui.button("Copy").clicked() {
                    ui.ctx().copy_text(pairing.user_code.clone());
                }
            });
            ui.add_space(6.0);
            // No spinner: it would repaint the window many times a second.
            ui.label(theme.weak("Waiting for approval…"));
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let again = pairing.browser_url.clone();
                if ui
                    .add_enabled(again.is_some(), egui::Button::new("Open Page Again"))
                    .clicked()
                    && let Some(url) = again
                {
                    open_url(&url);
                }
                if ui.button("Cancel").clicked() {
                    self.cancel_pairing();
                }
            });
            ui.add_space(2.0);
            ui.label(theme.small(format!("Key fingerprint {}", pairing.fingerprint)));
        });
    }

    fn paired_card(&mut self, ui: &mut Ui, status: &Status) {
        let theme = self.theme;
        let health = Health::of(Some(status));
        group(ui, &theme, |ui| {
            let row = |ui: &mut Ui, title: &str, value: RichText| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(title).color(theme.palette.text));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(value);
                    });
                });
            };
            row(
                ui,
                "Chat with Work",
                theme.weak(status.server_host().unwrap_or_default()),
            );
            row_separator(ui, &theme);
            ui.horizontal(|ui| {
                ui.label(RichText::new("Connection").color(theme.palette.text));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let mut text = health.label().to_string();
                    if health == Health::Online && !status.connection.since.is_empty() {
                        text = format!("Online since {}", time::short(&status.connection.since));
                    }
                    widgets::status_dot(ui, status_color(&theme, health), theme.weak(text));
                });
            });
            if let Some(error) = status.connection.last_error.as_deref()
                && health != Health::Online
            {
                ui.add(egui::Label::new(theme.small(error)).wrap());
            }
            row_separator(ui, &theme);
            row(
                ui,
                "Device ID",
                theme.weak(status.device_id.clone().unwrap_or_default()),
            );
        });
        ui.add_space(8.0);
        if health == Health::Revoked {
            widgets::notice(
                ui,
                &theme,
                "Chat with Work no longer accepts this computer. Pair it again to keep sharing.",
                theme.palette.warning,
            );
            ui.add_space(6.0);
            self.pair_button(ui, "Pair Again…");
            ui.add_space(6.0);
        }
        if self.account.confirm_logout {
            group(ui, &theme, |ui| {
                ui.add(
                    egui::Label::new(
                        "Disconnect this computer? Chat with Work stops reaching it right away. \
                         Also remove it in Chat with Work under Settings ▸ Computers.",
                    )
                    .wrap(),
                );
                let (cancel, confirm) = widgets::dialog_buttons(ui, &theme, "Disconnect", true);
                if cancel {
                    self.account.confirm_logout = false;
                } else if confirm {
                    self.logout(ui.ctx());
                }
            });
        } else if ui
            .add_enabled(
                !self.account.requesting,
                egui::Button::new("Disconnect This Computer…"),
            )
            .clicked()
        {
            self.account.confirm_logout = true;
        }
    }
}
