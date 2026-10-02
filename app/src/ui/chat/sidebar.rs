//! The sidebar (`sidebar.css`): the logotype and its toggle, New chat, the
//! search field, the chats by day, and your name at the foot, which goes
//! to Settings.

use egui::{Color32, Id, Key, Rect, Sense, Ui, pos2, vec2};

use cww::tui::chat::ChatSummary;

use super::composer::ctrl_key;
use super::icons::{Icon, Images};
use super::paint;
use super::state::ChatState;
use super::tokens::{self, Palette, Type, scale};
use super::widgets;

pub const SEARCH_ID: &str = "chat-search";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    NewChat,
    Open(u64),
    /// The header's button: collapse when docked, close as a drawer.
    Toggle,
    Settings,
}

/// When a chat last moved, as the sidebar groups chats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Day {
    Today,
    Yesterday,
    Earlier,
}

impl Day {
    pub fn of(updated_at: &str, today: jiff::civil::Date) -> Self {
        let date = updated_at
            .parse::<jiff::Timestamp>()
            .ok()
            .map(|t| t.to_zoned(jiff::tz::TimeZone::system()).date());
        match date {
            Some(d) if d >= today => Day::Today,
            Some(d) if Some(d) == today.yesterday().ok() => Day::Yesterday,
            _ => Day::Earlier,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Day::Today => "Today",
            Day::Yesterday => "Yesterday",
            Day::Earlier => "Earlier",
        }
    }
}

pub struct Sidebar<'a> {
    pub images: &'a Images,
    pub palette: &'a Palette,
    pub time: f64,
    /// Drawn as a drawer over the conversation (narrow windows).
    pub drawer: bool,
    pub user: &'a str,
    pub focus_search: bool,
}

impl Sidebar<'_> {
    pub fn show(&self, ui: &mut Ui, rect: Rect, state: &mut ChatState) -> Option<Event> {
        let p = self.palette;
        let mut event = None;
        let painter = ui.painter().clone();
        painter.rect_filled(rect, 0.0, p.canvas_sunken);
        painter.vline(
            rect.right() - 0.5,
            rect.y_range(),
            egui::Stroke::new(1.0, p.line),
        );
        let inner_right = rect.right() - 1.0;

        // Header: the logotype, and the toggle.
        let header_center = rect.top() + 14.0 + 16.0;
        self.images.logotype(
            &painter,
            pos2(rect.left() + 16.0, header_center),
            22.0,
            p.dark,
        );
        let toggle = Rect::from_center_size(
            pos2(inner_right - 8.0 - 16.0, header_center),
            vec2(32.0, 32.0),
        );
        let (icon, label) = if self.drawer {
            (Icon::X, "Close sidebar")
        } else {
            (Icon::SidebarSimple, "Toggle sidebar")
        };
        let toggled = widgets::IconButton {
            icon,
            icon_size: 16.0,
            label,
            tooltip: None,
            color: p.ink_muted,
            hover_color: p.ink,
            wash: 7.0,
            radius: tokens::RADIUS_CONTROL - 2.0,
            enabled: true,
        }
        .show(ui, Id::new("chat-sidebar-toggle"), toggle, self.images, p)
        .clicked();
        if toggled {
            event = Some(Event::Toggle);
        }

        // New chat.
        let new_rect = Rect::from_min_max(
            pos2(rect.left() + 8.0, rect.top() + 60.0),
            pos2(inner_right - 8.0, rect.top() + 98.0),
        );
        let id = Id::new("chat-new");
        let response = ui.interact(new_rect, id, Sense::click());
        response
            .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "New chat"));
        let t = widgets::hover(ui, id, response.hovered(), tokens::INSTANT);
        let fill = tokens::lerp_rgb(p.surface, p.surface_hover, t);
        let ring = tokens::lerp_rgb(p.line, p.line_strong, t);
        painter.rect_filled(new_rect, tokens::RADIUS_CONTROL, fill);
        painter.rect_stroke(
            new_rect.shrink(0.5),
            tokens::RADIUS_CONTROL,
            egui::Stroke::new(1.0, ring),
            egui::StrokeKind::Middle,
        );
        self.images.icon_at(
            &painter,
            pos2(new_rect.left() + 10.0 + 9.0, new_rect.center().y),
            18.0,
            Icon::NotePencil,
            p.ink,
        );
        let text = paint::layout(
            &painter,
            paint::job("New chat", scale::SMALL.weight(560.0), p.ink, f32::INFINITY),
        );
        painter.galley(
            pos2(
                new_rect.left() + 38.0,
                new_rect.center().y - text.size().y / 2.0,
            ),
            text,
            p.ink,
        );
        paint::kbd_hint(
            &painter,
            pos2(new_rect.right() - 10.0, new_rect.center().y),
            &["Alt", "N"],
            p,
            t,
        );
        if response.has_focus() {
            widgets::focus_ring(ui, new_rect, tokens::RADIUS_CONTROL, p);
        }
        if response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        if response.clicked() {
            event = Some(Event::NewChat);
        }

        // Search.
        let search_rect = Rect::from_min_max(
            pos2(rect.left() + 8.0, rect.top() + 110.0),
            pos2(inner_right - 8.0, rect.top() + 142.0),
        );
        self.search(ui, search_rect, state);

        // Your name, at the foot.
        let account = Rect::from_min_max(
            pos2(rect.left() + 8.0, rect.bottom() - 8.0 - 48.0),
            pos2(inner_right - 8.0, rect.bottom() - 8.0),
        );
        let id = Id::new("chat-account");
        let response = ui.interact(account, id, Sense::click());
        response
            .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Settings"));
        let t = widgets::hover(ui, id, response.hovered(), tokens::INSTANT);
        if t > 0.0 {
            painter.rect_filled(account, tokens::RADIUS_CARD, p.ink_wash(5.0 * t));
        }
        let avatar_center = pos2(account.left() + 10.0 + 16.0, account.center().y);
        painter.circle_filled(avatar_center, 16.0, p.surface);
        painter.circle_stroke(avatar_center, 16.5, egui::Stroke::new(1.0, p.line));
        let initials = initials(self.user);
        let galley = paint::layout(
            &painter,
            paint::job(
                &initials,
                Type::sans(12.0, 16.0).weight(550.0),
                p.ink_muted,
                f32::INFINITY,
            ),
        );
        painter.galley(avatar_center - galley.size() / 2.0, galley, p.ink_muted);
        let name_width = account.width() - 10.0 - 32.0 - 12.0 - 10.0 - 20.0;
        let name = paint::layout(
            &painter,
            paint::line_job(self.user, scale::SMALL, p.ink, name_width),
        );
        painter.galley(
            pos2(
                avatar_center.x + 16.0 + 12.0,
                account.center().y - name.size().y / 2.0,
            ),
            name,
            p.ink,
        );
        self.images.icon_at(
            &painter,
            pos2(account.right() - 10.0 - 8.0, account.center().y),
            16.0,
            Icon::GearSix,
            p.ink_faint.gamma_multiply(t),
        );
        if response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        if response.clicked() {
            event = Some(Event::Settings);
        }

        // The chats.
        let list_rect = Rect::from_min_max(
            pos2(rect.left(), rect.top() + 150.0),
            pos2(inner_right, account.top() - 8.0),
        );
        if let Some(e) = self.list(ui, list_rect, state) {
            event = Some(e);
        }
        event
    }

    fn search(&self, ui: &mut Ui, rect: Rect, state: &mut ChatState) {
        let p = self.palette;
        let id = Id::new(SEARCH_ID);
        let focused = ui.memory(|m| m.has_focus(id));
        let hovered = ui.rect_contains_pointer(rect);
        let painter = ui.painter().clone();
        let focus_t = ui
            .ctx()
            .animate_bool_with_time(id.with("focus"), focused, tokens::FAST);
        let hover_t = widgets::hover(ui, id, hovered, tokens::FAST);
        painter.rect_filled(rect, tokens::RADIUS_CONTROL, p.surface);
        if focus_t > 0.0 {
            painter.rect_stroke(
                rect,
                tokens::RADIUS_CONTROL,
                egui::Stroke::new(3.0, p.focus().gamma_multiply(0.22 * focus_t)),
                egui::StrokeKind::Outside,
            );
        }
        let border = tokens::lerp_rgb(
            tokens::lerp_rgb(p.line_strong, tokens::alpha(p.ink, 0.26), hover_t),
            p.focus(),
            focus_t,
        );
        painter.rect_stroke(
            rect.shrink(0.5),
            tokens::RADIUS_CONTROL,
            egui::Stroke::new(1.0, border),
            egui::StrokeKind::Middle,
        );
        // The icon gets squeezed to 10.4 wide in the web's field.
        self.images.icon_at(
            &painter,
            pos2(rect.left() + 13.0 + 5.2, rect.center().y),
            10.4,
            Icon::MagnifyingGlass,
            p.ink_faint,
        );
        let hint_t = ui.ctx().animate_bool_with_time(
            id.with("hint"),
            !focused && state.search.is_empty(),
            tokens::FAST,
        );
        let hint_w = paint::kbd_hint(
            &painter,
            pos2(rect.right() - 12.0, rect.center().y),
            &[ctrl_key(), "K"],
            p,
            hint_t,
        );
        let field = Rect::from_min_max(
            pos2(rect.left() + 31.4, rect.center().y - 9.0),
            pos2(
                rect.right() - 12.0 - hint_w.max(0.0) - 4.0,
                rect.center().y + 9.0,
            ),
        );
        let ty = Type::sans(12.0, 18.0);
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(field));
        let output = egui::TextEdit::singleline(&mut state.search)
            .id(id)
            .frame(egui::Frame::NONE)
            .margin(egui::Margin::ZERO)
            .font(ty.font_id())
            .text_color(p.ink)
            .hint_text(
                egui::RichText::new("Search chats")
                    .font(ty.font_id())
                    .color(p.ink_faint.gamma_multiply(0.5)),
            )
            .desired_width(field.width())
            .show(&mut child);
        let response = output.response;
        if self.focus_search {
            response.request_focus();
        }
        if focused && ui.input(|i| i.key_pressed(Key::Escape)) {
            state.search.clear();
            response.surrender_focus();
        }
    }

    fn list(&self, ui: &mut Ui, rect: Rect, state: &ChatState) -> Option<Event> {
        let p = self.palette;
        let mut event = None;
        let today = jiff::Zoned::now().date();
        let visible: Vec<&ChatSummary> = state.visible();
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(rect)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        child.set_clip_rect(rect.intersect(ui.clip_rect()));
        let output = egui::ScrollArea::vertical()
            .id_salt("chat-sidebar-list")
            .auto_shrink([false, false])
            .show(&mut child, |ui| {
                ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
                let width = ui.available_width();
                let mut current: Option<Day> = None;
                for chat in &visible {
                    let day = Day::of(&chat.updated_at, today);
                    if current != Some(day) {
                        if current.is_some() {
                            ui.add_space(8.0);
                        }
                        current = Some(day);
                        let (label_rect, _) =
                            ui.allocate_exact_size(vec2(width, 32.8), Sense::hover());
                        let galley = paint::layout(
                            ui.painter(),
                            paint::job(day.label(), scale::MICRO, p.ink_faint, f32::INFINITY),
                        );
                        let pos = pos2(label_rect.left() + 16.0, label_rect.top() + 12.0);
                        widgets::label(ui, Rect::from_min_size(pos, galley.size()), day.label());
                        ui.painter().galley(pos, galley, p.ink_faint);
                    }
                    let (row, _) = ui.allocate_exact_size(vec2(width, 39.0), Sense::hover());
                    let row = Rect::from_min_max(
                        pos2(row.left() + 8.0, row.top()),
                        pos2(row.right() - 8.0, row.bottom() - 1.0),
                    );
                    if self.row(ui, row, chat, state.open == Some(chat.number)) {
                        event = Some(Event::Open(chat.number));
                    }
                }
                if visible.is_empty() && !state.search.trim().is_empty() {
                    let (r, _) = ui.allocate_exact_size(vec2(width, 40.0), Sense::hover());
                    let galley = paint::layout(
                        ui.painter(),
                        paint::job("No chats match", scale::SMALL, p.ink_faint, f32::INFINITY),
                    );
                    let pos = pos2(r.left() + 18.0, r.top() + 10.0);
                    widgets::label(
                        ui,
                        Rect::from_min_size(pos, galley.size()),
                        "No chats match",
                    );
                    ui.painter().galley(pos, galley, p.ink_faint);
                }
                ui.add_space(8.0);
            });
        // `.sidebar__fade` once the list scrolls.
        let scrolled = output.state.offset.y > 0.5;
        let fade =
            ui.ctx()
                .animate_bool_with_time(Id::new("chat-sidebar-fade"), scrolled, tokens::BASE);
        if fade > 0.0 {
            let top = rect.top();
            let mut mesh = egui::epaint::Mesh::default();
            let (c0, c1) = (p.canvas_sunken.gamma_multiply(fade), Color32::TRANSPARENT);
            mesh.colored_vertex(pos2(rect.left(), top), c0);
            mesh.colored_vertex(pos2(rect.right(), top), c0);
            mesh.colored_vertex(pos2(rect.left(), top + 16.0), c1);
            mesh.colored_vertex(pos2(rect.right(), top + 16.0), c1);
            mesh.add_triangle(0, 1, 2);
            mesh.add_triangle(1, 3, 2);
            ui.painter().add(egui::Shape::mesh(mesh));
        }
        event
    }

    /// One chat; true when it was chosen.
    fn row(&self, ui: &mut Ui, rect: Rect, chat: &ChatSummary, selected: bool) -> bool {
        let p = self.palette;
        let id = Id::new(("chat-row", chat.number));
        let response = ui.interact(rect, id, Sense::click());
        let title = if chat.title.trim().is_empty() {
            "New chat"
        } else {
            chat.title.as_str()
        };
        response.widget_info(|| {
            egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, selected, title)
        });
        let t = widgets::hover(ui, id, response.hovered(), tokens::INSTANT);
        let painter = ui.painter();
        let radius = tokens::RADIUS_CONTROL - 2.0;
        if selected {
            painter.rect_filled(rect, radius, p.surface);
            painter.rect_stroke(
                rect.shrink(0.5),
                radius,
                egui::Stroke::new(1.0, p.line),
                egui::StrokeKind::Middle,
            );
        } else if t > 0.0 {
            painter.rect_filled(rect, radius, p.ink_wash(6.0 * t));
        }
        let mut x = rect.left() + 10.0;
        let cy = rect.center().y;
        if chat.processing() {
            // A pulsing live dot.
            let phase = (self.time / 2.0).fract() as f32;
            paint::pulse_halo(painter, pos2(x + 3.0, cy), 3.0, p.live(), phase);
            painter.circle_filled(pos2(x + 3.0, cy), 3.0, p.live());
            x += 6.0 + 8.0;
        } else if chat.state == "error" {
            painter.circle_filled(pos2(x + 3.0, cy), 3.0, p.attention);
            x += 6.0 + 8.0;
        }
        let color = if selected || chat.processing() {
            p.ink
        } else {
            tokens::lerp_rgb(p.ink_muted, p.ink, t)
        };
        let galley = paint::layout(
            painter,
            paint::line_job(title, scale::SMALL, color, rect.right() - 4.0 - x),
        );
        let pos = pos2(x, cy - galley.size().y / 2.0);
        if chat.processing() {
            paint::shimmer(painter, pos, &galley, p.ink_faint, self.time);
        } else {
            painter.galley(pos, galley, color);
        }
        if response.has_focus() {
            widgets::focus_ring(ui, rect, radius, p);
        }
        if response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        response.clicked()
    }
}

/// "AR" for "Alex Rivera"; the first letter of an address.
pub fn initials(name: &str) -> String {
    let words: Vec<&str> = name
        .split(|c: char| c.is_whitespace() || c == '@')
        .filter(|w| !w.is_empty())
        .collect();
    let letters: String = match words.as_slice() {
        [] => String::new(),
        [one] => one.chars().take(1).collect(),
        [first, .., last] if !name.contains('@') => {
            first.chars().take(1).chain(last.chars().take(1)).collect()
        }
        [first, ..] => first.chars().take(1).collect(),
    };
    letters.to_uppercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chats_group_by_day() {
        let today = jiff::civil::date(2026, 10, 2);
        let tz = jiff::tz::TimeZone::system();
        let at = |d: jiff::civil::Date, h: i8| {
            d.at(h, 0, 0, 0)
                .to_zoned(tz.clone())
                .unwrap()
                .timestamp()
                .to_string()
        };
        assert_eq!(Day::of(&at(today, 9), today), Day::Today);
        assert_eq!(
            Day::of(&at(today.yesterday().unwrap(), 23), today),
            Day::Yesterday
        );
        assert_eq!(
            Day::of(&at(jiff::civil::date(2026, 9, 1), 12), today),
            Day::Earlier
        );
        assert_eq!(Day::of("not a time", today), Day::Earlier);
    }

    #[test]
    fn initials_read_like_the_web() {
        assert_eq!(initials("Alex Rivera"), "AR");
        assert_eq!(initials("Ada"), "A");
        assert_eq!(initials("dana@example.com"), "D");
        assert_eq!(initials(""), "");
    }
}
