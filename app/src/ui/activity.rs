//! The local audit log: every request from Chat with Work, and the
//! daemon's own events, newest first.

use egui::{RichText, Ui};

use super::theme::Theme;
use super::widgets::{self, group, row_separator};
use super::{LogFilter, SettingsApp, open_path};
use crate::model::{AuditEntry, Status, describe_tool};
use crate::time;

impl SettingsApp {
    pub(super) fn activity_page(&mut self, ui: &mut Ui, status: Option<&Status>) {
        let theme = self.theme;
        widgets::page_header(
            ui,
            &theme,
            "Activity",
            "Every request from Chat with Work, as logged on this computer. The log never leaves it.",
        );
        let Some(status) = status else {
            self.agent_missing(ui);
            return;
        };
        let generation = self.shared.generation();
        if self.activity.loaded_generation != Some(generation) && !self.activity.loading {
            self.activity.loaded_generation = Some(generation);
            self.load_log(ui.ctx());
        }

        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.activity.filter, LogFilter::All, "All");
            ui.selectable_value(&mut self.activity.filter, LogFilter::Refused, "Refused");
            if let Some(audit) = status.audit_file.clone() {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Open Log File").clicked() {
                        open_path(&audit);
                    }
                });
            }
        });
        ui.add_space(4.0);
        if let Some(error) = self.activity.error.clone() {
            widgets::notice(ui, &theme, &error, theme.palette.danger);
            return;
        }

        let filter = self.activity.filter;
        let shown: Vec<&AuditEntry> = self
            .activity
            .entries
            .iter()
            .filter(|e| filter == LogFilter::All || refused(e))
            .collect();
        if shown.is_empty() {
            group(ui, &theme, |ui| {
                ui.label(theme.weak(match (filter, self.activity.loading) {
                    (_, true) if self.activity.entries.is_empty() => "Loading…",
                    (LogFilter::All, _) => "No activity yet.",
                    (LogFilter::Refused, _) => "Nothing was refused.",
                }));
            });
            return;
        }

        let mut day = String::new();
        for chunk in shown.chunk_by(|a, b| time::day(&a.ts) == time::day(&b.ts)) {
            let heading = time::day(&chunk[0].ts);
            if heading != day {
                widgets::section(ui, &theme, &heading);
                day = heading;
            }
            group(ui, &theme, |ui| {
                for (i, entry) in chunk.iter().enumerate() {
                    entry_row(ui, &theme, entry);
                    if i + 1 < chunk.len() {
                        row_separator(ui, &theme);
                    }
                }
            });
        }
    }
}

fn refused(entry: &AuditEntry) -> bool {
    entry.event == "tool" && entry.decision.as_deref() != Some("allowed")
}

fn entry_row(ui: &mut Ui, theme: &Theme, entry: &AuditEntry) {
    let p = &theme.palette;
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(time::clock(&entry.ts))
                .monospace()
                .size(theme.metrics.small + 0.5)
                .color(p.weak),
        );
        ui.add_space(4.0);
        let (text, detail) = describe(entry);
        ui.vertical(|ui| {
            ui.set_width((ui.available_width() - 110.0).max(160.0));
            ui.add(egui::Label::new(RichText::new(text).color(p.text)).truncate());
            if let Some(detail) = detail {
                ui.add_space(-4.0);
                ui.add(egui::Label::new(theme.small(detail)).truncate());
            }
        });
        if entry.event == "tool" {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let (label, color) = match entry.decision.as_deref() {
                    Some("allowed") => ("Answered", p.success),
                    Some("denied") => ("Refused", p.danger),
                    _ => ("Failed", p.warning),
                };
                widgets::badge(ui, theme, label, color);
            });
        }
    });
}

/// A sentence for the entry, and a detail line.
fn describe(entry: &AuditEntry) -> (String, Option<String>) {
    if entry.event == "tool" {
        let text = capitalize(&describe_tool(entry));
        let mut details = Vec::new();
        if let Some(reason) = &entry.reason {
            details.push(reason.clone());
        } else if let Some(code) = &entry.code {
            details.push(code.replace('_', " "));
        }
        if let Some(n) = entry.results {
            details.push(if n == 1 {
                "1 result".into()
            } else {
                format!("{n} results")
            });
        }
        if let Some(bytes) = entry.bytes {
            details.push(format!("{} sent", size(bytes)));
        }
        if let Some(chat) = &entry.chat_id {
            details.push(format!("chat {chat}"));
        }
        let detail = (!details.is_empty()).then(|| details.join(" · "));
        return (text, detail);
    }
    let text = match entry.event.as_str() {
        "started" => "The Local Agent started",
        "stopped" => "The Local Agent stopped",
        "connected" => "Connected to Chat with Work",
        "disconnected" => "Disconnected from Chat with Work",
        "paused" => "Sharing paused",
        "resumed" => "Sharing resumed",
        "reloaded" => "Settings applied",
        "paired" => "Paired with Chat with Work",
        "logged_out" => "Pairing removed from this computer",
        "revoked" => "Chat with Work revoked this computer",
        other => return (capitalize(&other.replace('_', " ")), entry.detail.clone()),
    };
    (text.to_string(), entry.detail.clone())
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

fn size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_read_as_sentences() {
        let read = AuditEntry {
            event: "tool".into(),
            tool: Some("read".into()),
            path: Some("docs:plans/q3.md".into()),
            decision: Some("allowed".into()),
            bytes: Some(2048),
            chat_id: Some("7".into()),
            ..AuditEntry::default()
        };
        assert_eq!(
            describe(&read),
            (
                "Read docs:plans/q3.md".into(),
                Some("2.0 KB sent · chat 7".into())
            )
        );
        assert!(!refused(&read));

        let denied = AuditEntry {
            event: "tool".into(),
            tool: Some("read".into()),
            path: Some("docs:.env".into()),
            decision: Some("denied".into()),
            code: Some("denied".into()),
            reason: Some("this path is on the deny list (.env*)".into()),
            ..AuditEntry::default()
        };
        assert!(refused(&denied));
        assert_eq!(
            describe(&denied).1.as_deref(),
            Some("this path is on the deny list (.env*)")
        );

        let paused = AuditEntry {
            event: "paused".into(),
            ..AuditEntry::default()
        };
        assert_eq!(describe(&paused).0, "Sharing paused");
        assert!(!refused(&paused));
    }
}
