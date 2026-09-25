//! The deny list, as information: what is never shared, even inside a
//! shared folder. It can only be changed in config.toml, on purpose.

use egui::{RichText, Ui};

use super::widgets::{self, group, row_separator};
use super::{Loadable, SettingsApp, open_path};
use crate::model::Status;

impl SettingsApp {
    pub(super) fn privacy_page(&mut self, ui: &mut Ui, status: Option<&Status>) {
        let theme = self.theme;
        widgets::page_header(
            ui,
            &theme,
            "Always Private",
            "These files are never shared, even inside a shared folder. Chat with Work can't change this list.",
        );
        if status.is_none() {
            self.agent_missing(ui);
            return;
        }
        // Reload after every change: an edited config.toml can change it.
        let generation = self.shared.generation();
        if self.deny_generation != Some(generation) && !matches!(self.deny, Loadable::Loading) {
            self.deny_generation = Some(generation);
            self.load_deny(ui.ctx());
        }
        let deny = match &self.deny {
            Loadable::Loaded(deny) => deny.clone(),
            Loadable::Failed(error) => {
                widgets::notice(ui, &theme, error, theme.palette.danger);
                return;
            }
            Loadable::NotLoaded | Loadable::Loading => {
                ui.label(theme.weak("Loading…"));
                return;
            }
        };

        group(ui, &theme, |ui| {
            ui.label(theme.strong("Built in"));
            ui.add(
                egui::Label::new(theme.small(
                    "Keys, credentials, password stores, mail, messages and browser profiles. \
                     A pattern matches a file or folder name anywhere in a path.",
                ))
                .wrap(),
            );
            ui.add_space(4.0);
            patterns(ui, &theme, &deny.builtin, theme.palette.text);
            if !deny.extra.is_empty() {
                row_separator(ui, &theme);
                ui.label(theme.strong("Added by you"));
                patterns(ui, &theme, &deny.extra, theme.palette.text);
            }
            if !deny.removed.is_empty() {
                row_separator(ui, &theme);
                ui.label(theme.strong("Removed by you"));
                ui.label(theme.small("These built-in patterns no longer apply."));
                patterns(ui, &theme, &deny.removed, theme.palette.danger);
            }
            row_separator(ui, &theme);
            ui.label(theme.strong("Also private"));
            ui.add(
                egui::Label::new(theme.small(if deny.allow_hardlinks {
                    "The Local Agent's own settings, keys, index and logs. Files with more than \
                     one hard link are allowed by your configuration."
                } else {
                    "The Local Agent's own settings, keys, index and logs, and files with more \
                     than one hard link, which could point outside a shared folder."
                }))
                .wrap(),
            );
        });

        ui.add_space(10.0);
        ui.horizontal_wrapped(|ui| {
            ui.label(theme.weak("To change this list, edit the"));
            ui.label(RichText::new("[deny]").monospace());
            ui.label(theme.weak("section of config.toml, and it applies right away."));
        });
        if let Some(config) = deny.config_file.clone()
            && ui.button("Open config.toml").clicked()
        {
            open_path(&config);
        }
    }
}

fn patterns(ui: &mut Ui, theme: &super::theme::Theme, patterns: &[String], color: egui::Color32) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(8.0, 6.0);
        // Rows as tall as the text, not as a button.
        ui.spacing_mut().interact_size.y = theme.metrics.small + 6.0;
        for pattern in patterns {
            // Each pattern is one unbreakable label, so the row wraps
            // between patterns, never inside one.
            ui.add(
                egui::Label::new(
                    RichText::new(format!(" {pattern} "))
                        .code()
                        .size(theme.metrics.small + 0.5)
                        .color(color),
                )
                .wrap_mode(egui::TextWrapMode::Extend),
            );
        }
    });
}
