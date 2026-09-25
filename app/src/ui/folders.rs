//! Shared folders: add with the native picker, rename, remove, and see
//! each folder's index state.

use egui::{RichText, Ui};

use super::widgets::{self, group, row_separator};
use super::{SettingsApp, thousands};
use crate::model::{RootStatus, Status};
use crate::paths;

impl SettingsApp {
    pub(super) fn folders_page(&mut self, ui: &mut Ui, status: Option<&Status>) {
        let theme = self.theme;
        widgets::page_header(
            ui,
            &theme,
            "Shared Folders",
            "Chat with Work can search and read the files in these folders, and nothing else on this computer.",
        );
        let Some(status) = status else {
            self.agent_missing(ui);
            return;
        };

        if status.roots.is_empty() {
            group(ui, &theme, |ui| {
                ui.label(theme.strong("Nothing is shared yet"));
                ui.add(
                    egui::Label::new(theme.weak(
                        "Add a folder to let Chat with Work search it. Share narrow folders: \
                         anything inside can be read, except the private files listed under \
                         Always Private.",
                    ))
                    .wrap(),
                );
            });
        } else {
            group(ui, &theme, |ui| {
                let count = status.roots.len();
                for (i, root) in status.roots.iter().enumerate() {
                    self.folder_row(ui, root);
                    if i + 1 < count {
                        row_separator(ui, &theme);
                    }
                }
            });
        }

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let busy = self.folders.adding || self.folders.picking;
            let add = ui.add_enabled_ui(!busy, |ui| {
                widgets::primary_button(
                    ui,
                    &theme,
                    if self.folders.adding {
                        "Adding…"
                    } else {
                        "Add Folder…"
                    },
                )
            });
            if add.inner.clicked() {
                self.pick_folder(ui.ctx());
            }
            if let Some(documents) = documents_to_offer(self.documents.as_deref(), status) {
                let share = ui.add_enabled(!busy, egui::Button::new("Share Documents"));
                if share.clicked() {
                    self.add_folder(ui.ctx(), documents);
                }
            }
        });
        if let Some(error) = self.folders.error.clone() {
            ui.add_space(6.0);
            widgets::notice(ui, &theme, &error, theme.palette.danger);
        }
        if let Some(note) = self.folders.note.clone() {
            ui.add_space(6.0);
            widgets::notice(ui, &theme, &note, theme.palette.accent);
        }
        ui.add_space(12.0);
        ui.add(
            egui::Label::new(theme.small(
                "Labels are what Chat with Work sees. It never sees where a folder is on \
                 this computer.",
            ))
            .wrap(),
        );
    }

    fn folder_row(&mut self, ui: &mut Ui, root: &RootStatus) {
        let theme = self.theme;
        let busy = self.folders.busy.as_deref() == Some(root.id.as_str());
        let renaming = self
            .folders
            .renaming
            .as_ref()
            .is_some_and(|(id, _)| *id == root.id);
        let confirming = self.folders.confirm_remove.as_deref() == Some(root.id.as_str());

        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.set_width((ui.available_width() - 190.0).max(200.0));
                ui.horizontal(|ui| {
                    ui.label(theme.strong(&root.label));
                    let (text, color) = index_badge(&theme, root);
                    widgets::badge(ui, &theme, &text, color);
                });
                ui.add_space(-4.0);
                let path = root
                    .local_path
                    .as_deref()
                    .map(paths::display)
                    .unwrap_or_default();
                ui.add(egui::Label::new(theme.small(path)).truncate())
                    .on_hover_text(format!("Chat with Work sees this folder as “{}:”", root.id));
            });
            if !renaming && !confirming {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_enabled_ui(!busy, |ui| {
                        if ui.button("Remove…").clicked() {
                            self.folders.confirm_remove = Some(root.id.clone());
                            self.folders.renaming = None;
                        }
                        if ui.button("Rename…").clicked() {
                            self.folders.renaming = Some((root.id.clone(), root.label.clone()));
                            self.folders.focus_rename = true;
                            self.folders.confirm_remove = None;
                        }
                    });
                });
            }
        });

        if renaming {
            let mut save = false;
            let mut cancel = false;
            ui.horizontal(|ui| {
                if let Some((_, text)) = self.folders.renaming.as_mut() {
                    let edit = ui.add(
                        egui::TextEdit::singleline(text)
                            .desired_width(260.0)
                            .char_limit(80)
                            .hint_text("Label"),
                    );
                    if std::mem::take(&mut self.folders.focus_rename) {
                        edit.request_focus();
                    }
                    if edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        save = true;
                    }
                }
                let (c, s) = widgets::dialog_buttons(ui, &theme, "Save", false);
                cancel |= c;
                save |= s;
            });
            if cancel {
                self.folders.renaming = None;
            } else if save && let Some((id, label)) = self.folders.renaming.take() {
                self.label_folder(ui.ctx(), id, label);
            }
        }

        if confirming {
            ui.add_space(4.0);
            ui.add(
                egui::Label::new(RichText::new(format!(
                    "Stop sharing “{}”? Chat with Work will no longer be able to search or \
                     read these files. Nothing on this computer is deleted.",
                    root.label
                )))
                .wrap(),
            );
            let (cancel, confirm) = widgets::dialog_buttons(ui, &theme, "Stop Sharing", true);
            if cancel {
                self.folders.confirm_remove = None;
            } else if confirm {
                self.folders.confirm_remove = None;
                self.remove_folder(ui.ctx(), root.id.clone());
            }
        }
    }
}

/// The index state of a folder, as a label and a color.
fn index_badge(theme: &super::theme::Theme, root: &RootStatus) -> (String, egui::Color32) {
    let p = &theme.palette;
    if !root.available {
        return ("Folder not found".into(), p.warning);
    }
    match root.index.as_str() {
        "ready" => {
            let files = root.indexed_files.unwrap_or(0);
            let noun = if files == 1 { "file" } else { "files" };
            (format!("Indexed · {} {noun}", thousands(files)), p.success)
        }
        "indexing" => ("Indexing…".into(), p.accent),
        "pending" => ("Waiting to index".into(), p.weak),
        "error" => ("Index error".into(), p.danger),
        "disabled" => ("Searched live".into(), p.weak),
        other => (other.to_string(), p.weak),
    }
}

/// The Documents folder, if it exists and isn't shared yet.
fn documents_to_offer(
    documents: Option<&std::path::Path>,
    status: &Status,
) -> Option<std::path::PathBuf> {
    let documents = documents?.to_path_buf();
    let canonical = documents.canonicalize().unwrap_or(documents.clone());
    let shared = status.roots.iter().any(|r| {
        r.local_path
            .as_deref()
            .is_some_and(|p| p == canonical || p == documents)
    });
    (!shared).then_some(documents)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Platform;
    use crate::ui::theme::Theme;

    #[test]
    fn index_states_read_well() {
        let theme = Theme::new(Platform::Linux, false, None);
        let mut root = RootStatus {
            available: true,
            index: "ready".into(),
            indexed_files: Some(1532),
            ..RootStatus::default()
        };
        assert_eq!(index_badge(&theme, &root).0, "Indexed · 1,532 files");
        root.indexed_files = Some(1);
        assert_eq!(index_badge(&theme, &root).0, "Indexed · 1 file");
        root.index = "indexing".into();
        assert_eq!(index_badge(&theme, &root).0, "Indexing…");
        root.available = false;
        assert_eq!(index_badge(&theme, &root).0, "Folder not found");
    }
}
