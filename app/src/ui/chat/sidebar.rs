//! The sidebar (`sidebar.css`, `account_switcher.css`, `sidebar_pins.css`):
//! the logotype and its toggle, New chat and Chats, the
//! search field, the Recent chats, what's pinned (and a picker of every
//! project behind "All projects"), and your
//! name at the foot, with the credits when they run low, opening the menu
//! of Settings: the web's tabs, and this computer's own pages.

use egui::{Color32, Id, Key, Rect, Sense, Ui, pos2, vec2};

use cww::tui::chat::{ChatSummary, Project};

use super::composer::ctrl_key;
use super::controls::{self, FieldLook};
use super::icons::{Icon, Images};
use super::paint;
use super::state::ChatState;
use super::tokens::{self, Palette, Type, scale};
use super::widgets;

pub const SEARCH_ID: &str = "chat-search";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    NewChat,
    Open(u64),
    /// The header's button: collapse when docked, close as a drawer.
    Toggle,
    /// One of this computer's settings pages, from the menu under your name.
    Page(crate::ui::Page),
    /// A page of the web, in the browser: Chats, All projects, a Settings tab.
    OpenUrl(String),
    /// A project, under Pinned or Projects: its chats, and a new chat in it.
    Project(u64),
    /// Rename a chat, from its row's menu.
    Rename(u64, String),
    /// Delete a chat, once confirmed.
    Delete(u64),
}

/// A row under Pinned.
#[derive(Debug, Clone)]
enum Pin {
    Project(Project),
    Chat(u64, String),
}

/// The icon the web shows for a project, by the name the server gives it.
pub fn project_icon(project: &Project) -> Icon {
    project
        .icon
        .as_deref()
        .and_then(Icon::named)
        .unwrap_or(if project.hq {
            Icon::Buildings
        } else if project.all_access {
            Icon::UsersThree
        } else {
            Icon::FolderSimple
        })
}

/// `.sidebar__link`'s height: 7 above and below a 21-point line.
const LINK: f32 = 35.0;

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
        let mut y = rect.top() + 56.0;

        // The web's organization switcher shows only to someone in more than
        // one organization. The chat API names only the one this computer is
        // paired with, so the sidebar shows what the web shows someone in one.

        // New chat, and Chats on the web.
        y += 4.0;
        let new_rect = Rect::from_min_max(
            pos2(rect.left() + 8.0, y),
            pos2(inner_right - 8.0, y + 38.0),
        );
        if self.new_chat(ui, new_rect) {
            event = Some(Event::NewChat);
        }
        y += 38.0;
        if let Some(url) = state.list.links.chats.clone() {
            y += 2.0;
            let row = Rect::from_min_max(
                pos2(rect.left() + 8.0, y),
                pos2(inner_right - 8.0, y + LINK),
            );
            if self.link(
                ui,
                row,
                Icon::ChatsCircle,
                "Chats",
                false,
                Id::new("chat-history"),
            ) {
                event = Some(Event::OpenUrl(url));
            }
            y += LINK;
        }
        y += 8.0;

        // Search.
        let search_rect = Rect::from_min_max(
            pos2(rect.left() + 8.0, y + 4.0),
            pos2(inner_right - 8.0, y + 36.0),
        );
        self.search(ui, search_rect, state);
        y += 36.0 + 8.0;

        // `.sidebar__label`: Recent, and while a project's chats show, the
        // way back to every chat.
        self.section_label(ui, pos2(rect.left() + 16.0, y + 12.0), "Recent");
        if let Some(project) = state.filter.and_then(|id| state.project(id)) {
            let name = project.name.clone();
            let text = format!("{name} · Show all");
            if self.micro_link(
                ui,
                pos2(inner_right - 8.0 - 10.0, y + 12.0),
                &text,
                Id::new("chat-show-all"),
            ) {
                state.filter = None;
            }
        }
        y += 32.8;

        // Your name, at the foot, with the credits when they run low.
        let meter = state.list.credits.clone().filter(|c| c.running_low);
        let foot = if meter.is_some() { 68.8 } else { 48.0 };
        let account_rect = Rect::from_min_max(
            pos2(rect.left() + 8.0, rect.bottom() - 8.0 - foot),
            pos2(inner_right - 8.0, rect.bottom() - 8.0),
        );
        self.person(ui, account_rect, meter.as_ref(), state);

        // What's pinned.
        let pins = pins(state);
        let pins_bottom = account_rect.top() - 8.0;
        let natural = 4.0
            + 32.8
            + if pins.is_empty() {
                4.0 + 19.5 + 8.0
            } else {
                pins.len() as f32 * (LINK + 2.0)
            }
            + 4.0;
        let height = natural.min((rect.height() * 0.4).max(0.0));
        let pins_rect = Rect::from_min_max(
            pos2(rect.left(), pins_bottom - height),
            pos2(inner_right, pins_bottom),
        );
        if let Some(e) = self.pinned(ui, pins_rect, &pins, state) {
            event = Some(e);
        }

        // The chats.
        let list_rect =
            Rect::from_min_max(pos2(rect.left(), y), pos2(inner_right, pins_rect.top()));
        if let Some(e) = self.list(ui, list_rect, state) {
            event = Some(e);
        }
        if let Some(e) = self.user_menu(ui, rect, account_rect, state) {
            event = Some(e);
        }
        if let Some(e) = self.projects_menu(ui, rect, pins_rect, state) {
            event = Some(e);
        }
        event
    }

    /// A `.micro` link, ending at `right_top`: faint, inked on hover. True
    /// when clicked.
    fn micro_link(&self, ui: &mut Ui, right_top: egui::Pos2, text: &str, id: Id) -> bool {
        let p = self.palette;
        let galley = paint::layout(
            ui.painter(),
            paint::job(text, scale::MICRO, p.ink_faint, f32::INFINITY),
        );
        let at = pos2(right_top.x - galley.size().x, right_top.y);
        let link = Rect::from_min_size(at, galley.size());
        let response = ui.interact(link.expand(4.0), id, Sense::click());
        response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Link, true, text));
        let t = widgets::hover(ui, id, response.hovered(), tokens::FAST);
        ui.painter()
            .galley(at, galley, tokens::lerp_rgb(p.ink_faint, p.ink, t));
        if response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        response.clicked()
    }

    /// Behind "All projects": every project the person is on, as the web's
    /// projects page lists them, in the web's menu. Picking one shows its
    /// chats and starts a new chat in it; the last row opens the web's page.
    fn projects_menu(
        &self,
        ui: &mut Ui,
        sidebar: Rect,
        pins: Rect,
        state: &mut ChatState,
    ) -> Option<Event> {
        if !state.projects_menu {
            return None;
        }
        let p = self.palette;
        let projects = state.list.projects.clone();
        let web = state.list.links.projects.clone();
        let rows = projects.len() + usize::from(web.is_some());
        let content = 32.5 + rows as f32 * 34.8 + if web.is_some() { 9.0 } else { 0.0 };
        let width = sidebar.width() - 16.0;
        let height = (5.0 + content + 5.0).min(pins.top() - sidebar.top() - 16.0);
        let anchor_y = pins.top() + 1.0 + 4.0 + 32.8;
        let rect = Rect::from_min_size(
            pos2(
                sidebar.left() + 8.0,
                (anchor_y - height).max(sidebar.top() + 8.0),
            ),
            vec2(width, height),
        );
        let mut event = None;
        let mut close = false;
        egui::Area::new(Id::new("chat-projects-menu"))
            .order(egui::Order::Foreground)
            .fixed_pos(rect.min)
            .show(ui.ctx(), |ui| {
                controls::menu_panel(ui.painter(), rect, p);
                let inner = rect.shrink(5.0);
                let mut child = ui.new_child(egui::UiBuilder::new().max_rect(inner));
                child.set_clip_rect(inner);
                egui::ScrollArea::vertical()
                    .id_salt("chat-projects-menu-scroll")
                    .max_height(inner.height())
                    .show(&mut child, |ui| {
                        ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
                        let (title, _) =
                            ui.allocate_exact_size(vec2(inner.width(), 32.5), Sense::hover());
                        let galley = paint::layout(
                            ui.painter(),
                            paint::job(
                                "Projects",
                                Type::mono(11.0, 16.5),
                                p.ink_faint,
                                f32::INFINITY,
                            ),
                        );
                        let at = pos2(
                            title.left() + 12.0,
                            title.center().y - galley.size().y / 2.0,
                        );
                        ui.painter().galley(at, galley, p.ink_faint);
                        for project in &projects {
                            let (row, _) =
                                ui.allocate_exact_size(vec2(inner.width(), 34.8), Sense::hover());
                            let row = Rect::from_min_size(row.min, vec2(row.width(), 33.8));
                            let current = state.filter == Some(project.id);
                            let id = Id::new(("chat-projects-menu-item", project.id));
                            if controls::menu_item(
                                ui,
                                id,
                                row,
                                Some(project_icon(project)),
                                &project.name,
                                false,
                                current,
                                p,
                                self.images,
                            )
                            .clicked()
                            {
                                close = true;
                                event = Some(Event::Project(project.id));
                            }
                        }
                        if let Some(url) = &web {
                            let (divider, _) =
                                ui.allocate_exact_size(vec2(inner.width(), 9.0), Sense::hover());
                            ui.painter().hline(
                                divider.x_range(),
                                divider.center().y,
                                egui::Stroke::new(1.0, p.line),
                            );
                            let (row, _) =
                                ui.allocate_exact_size(vec2(inner.width(), 34.8), Sense::hover());
                            let row = Rect::from_min_size(row.min, vec2(row.width(), 33.8));
                            if controls::menu_item(
                                ui,
                                Id::new("chat-projects-menu-web"),
                                row,
                                Some(Icon::ArrowUpRight),
                                "Open projects on the web",
                                false,
                                false,
                                p,
                                self.images,
                            )
                            .clicked()
                            {
                                close = true;
                                event = Some(Event::OpenUrl(url.clone()));
                            }
                        }
                    });
            });
        if close
            || controls::pressed_outside(ui, &[rect])
            || ui.input(|i| i.key_pressed(Key::Escape))
        {
            state.projects_menu = false;
        }
        event
    }

    /// `.sidebar__link--new`. True when clicked.
    fn new_chat(&self, ui: &mut Ui, new_rect: Rect) -> bool {
        let p = self.palette;
        let painter = ui.painter().clone();
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
        response.clicked()
    }

    /// `.sidebar__link`: an icon and a label, muted until hovered, and
    /// washed while it's the current page. True when clicked.
    fn link(&self, ui: &mut Ui, row: Rect, icon: Icon, label: &str, current: bool, id: Id) -> bool {
        let p = self.palette;
        let response = ui.interact(row, id, Sense::click());
        response.widget_info(|| {
            egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, current, label)
        });
        let t = widgets::hover(ui, id, response.hovered() || current, tokens::INSTANT);
        let painter = ui.painter();
        if t > 0.0 {
            painter.rect_filled(row, tokens::RADIUS_CONTROL, p.ink_wash(6.0 * t));
        }
        let color = tokens::lerp_rgb(p.ink_muted, p.ink, t);
        self.images.icon_at(
            painter,
            pos2(row.left() + 10.0 + 9.0, row.center().y),
            18.0,
            icon,
            color,
        );
        let galley = paint::layout(
            painter,
            paint::line_job(
                label,
                scale::SMALL,
                color,
                row.right() - 10.0 - row.left() - 38.0,
            ),
        );
        painter.galley(
            pos2(row.left() + 38.0, row.center().y - galley.size().y / 2.0),
            galley,
            color,
        );
        if response.has_focus() {
            widgets::focus_ring(ui, row, tokens::RADIUS_CONTROL, p);
        }
        if response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        response.clicked()
    }

    /// `.sidebar__label`, in mono: Today, Pinned, Projects.
    fn section_label(&self, ui: &mut Ui, at: egui::Pos2, text: &str) {
        let p = self.palette;
        let galley = paint::layout(
            ui.painter(),
            paint::job(text, scale::MICRO, p.ink_faint, f32::INFINITY),
        );
        widgets::label(ui, Rect::from_min_size(at, galley.size()), text);
        ui.painter().galley(at, galley, p.ink_faint);
    }

    /// `.sidebar__pins`: what the person pinned on the web, projects then
    /// chats, then the other projects, in a section of its own above the
    /// person's name, scrolling once it's 40% of the sidebar.
    fn pinned(
        &self,
        ui: &mut Ui,
        rect: Rect,
        pins: &[Pin],
        state: &mut ChatState,
    ) -> Option<Event> {
        let p = self.palette;
        ui.painter().hline(
            rect.x_range(),
            rect.top() + 0.5,
            egui::Stroke::new(1.0, p.line),
        );
        let mut event = None;
        let mut opened = state.projects_menu;
        let inner = Rect::from_min_max(rect.min + vec2(0.0, 1.0), rect.max);
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(inner)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        child.set_clip_rect(inner.intersect(ui.clip_rect()));
        egui::ScrollArea::vertical()
            .id_salt("chat-sidebar-pins")
            .auto_shrink([false, false])
            .show(&mut child, |ui| {
                ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
                let width = ui.available_width();
                ui.add_space(4.0);
                // The header: "Pinned", and the web's page of every project.
                let (header, _) = ui.allocate_exact_size(vec2(width, 32.8), Sense::hover());
                self.section_label(
                    ui,
                    pos2(header.left() + 16.0, header.top() + 12.0),
                    "Pinned",
                );
                // "All projects": the app's picker of them, or without any
                // listed, the web's page.
                let has_projects = !state.list.projects.is_empty();
                let web = state.list.links.projects.clone();
                if (has_projects || web.is_some())
                    && self.micro_link(
                        ui,
                        pos2(header.right() - 8.0 - 10.0, header.top() + 12.0),
                        "All projects",
                        Id::new("chat-all-projects"),
                    )
                {
                    if has_projects {
                        // Opened on this click; the press doesn't close it.
                        opened = !state.projects_menu;
                    } else if let Some(url) = web {
                        event = Some(Event::OpenUrl(url));
                    }
                }
                if pins.is_empty() {
                    let text = "Pin a chat or project to keep it here.";
                    let galley = paint::layout(
                        ui.painter(),
                        paint::job(text, Type::sans(13.0, 19.5), p.ink_faint, width - 36.0),
                    );
                    let (r, _) = ui.allocate_exact_size(
                        vec2(width, 4.0 + galley.size().y + 8.0),
                        Sense::hover(),
                    );
                    let at = pos2(r.left() + 18.0, r.top() + 4.0);
                    widgets::label(ui, Rect::from_min_size(at, galley.size()), text);
                    ui.painter().galley(at, galley, p.ink_faint);
                }
                for pin in pins {
                    let (r, _) = ui.allocate_exact_size(vec2(width, LINK + 2.0), Sense::hover());
                    let row = Rect::from_min_max(
                        pos2(r.left() + 8.0, r.top()),
                        pos2(r.right() - 8.0, r.top() + LINK),
                    );
                    match pin {
                        Pin::Project(project) => {
                            let current = state.filter == Some(project.id);
                            let id = Id::new(("chat-pin-project", project.id));
                            if self.link(ui, row, project_icon(project), &project.name, current, id)
                            {
                                event = Some(Event::Project(project.id));
                            }
                        }
                        Pin::Chat(number, title) => {
                            let current = state.open == Some(*number);
                            let id = Id::new(("chat-pin-chat", *number));
                            if self.link(ui, row, Icon::ChatCircle, title, current, id) {
                                event = Some(Event::Open(*number));
                            }
                        }
                    }
                }
                ui.add_space(4.0);
            });
        if opened != state.projects_menu {
            state.projects_menu = opened;
        }
        event
    }

    /// `.sidebar__account`: your initials (the web shows your Gravatar,
    /// which only the browser fetches), your name, and the credits meter
    /// while they run low. It opens the menu of Settings.
    fn person(
        &self,
        ui: &mut Ui,
        account: Rect,
        meter: Option<&cww::tui::chat::Credits>,
        state: &mut ChatState,
    ) {
        let p = self.palette;
        let painter = ui.painter().clone();
        let id = Id::new("chat-account");
        let response = ui.interact(account, id, Sense::click());
        let open = state.user_menu;
        response.widget_info(|| {
            let mut info =
                egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Settings and usage");
            info.selected = Some(open);
            info
        });
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
        let x = avatar_center.x + 16.0 + 12.0;
        let width = account.right() - 10.0 - x;
        let name = paint::layout(
            &painter,
            paint::line_job(self.user, scale::SMALL, p.ink, width),
        );
        let block = name.size().y + meter.map_or(0.0, |_| 6.0 + 4.0 + 5.0 + 16.8);
        let top = account.center().y - block / 2.0;
        widgets::label(
            ui,
            Rect::from_min_size(pos2(x, top), name.size()),
            self.user,
        );
        painter.galley(pos2(x, top), name.clone(), p.ink);
        if let Some(credits) = meter {
            self.meter(
                ui,
                Rect::from_min_size(
                    pos2(x, top + name.size().y + 6.0),
                    vec2(width, 4.0 + 5.0 + 16.8),
                ),
                credits,
            );
        }
        if response.has_focus() {
            widgets::focus_ring(ui, account, tokens::RADIUS_CARD, p);
        }
        if response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        if response.clicked() {
            state.user_menu = !state.user_menu;
        }
    }

    /// `.meter--battery`: what's left bright on the left, what's used
    /// fading out to the right, and "1,180 of 5,000 credits left".
    fn meter(&self, ui: &mut Ui, rect: Rect, credits: &cww::tui::chat::Credits) {
        let p = self.palette;
        let painter = ui.painter().clone();
        let level = if credits.capacity > 0 {
            (credits.left as f32 / credits.capacity as f32).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let over = credits.left == 0;
        let track = Rect::from_min_size(rect.min, vec2(rect.width(), 4.0));
        // `linear-gradient(90deg, line-strong level%, transparent)`.
        let split = track.left() + track.width() * level;
        painter.rect_filled(
            Rect::from_min_max(track.min, pos2(split, track.bottom())),
            0.0,
            p.line_strong,
        );
        let mut mesh = egui::epaint::Mesh::default();
        mesh.colored_vertex(pos2(split, track.top()), p.line_strong);
        mesh.colored_vertex(pos2(track.right(), track.top()), Color32::TRANSPARENT);
        mesh.colored_vertex(pos2(split, track.bottom()), p.line_strong);
        mesh.colored_vertex(pos2(track.right(), track.bottom()), Color32::TRANSPARENT);
        mesh.add_triangle(0, 1, 2);
        mesh.add_triangle(1, 3, 2);
        painter.with_clip_rect(track).add(egui::Shape::mesh(mesh));
        let fill = Rect::from_min_max(track.min, pos2(split, track.bottom()));
        if fill.width() > 0.0 {
            painter.rect_filled(fill, 3.0, if over { p.negative() } else { p.ink });
        }
        let caption = format!(
            "{} of {} credits left",
            crate::ui::thousands(credits.left),
            crate::ui::thousands(credits.capacity)
        );
        let color = if over { p.negative_ink() } else { p.ink_muted };
        let galley = paint::layout(
            &painter,
            paint::line_job(&caption, scale::MICRO, color, rect.width()),
        );
        let at = pos2(rect.left(), track.bottom() + 5.0);
        widgets::label(ui, Rect::from_min_size(at, galley.size()), &caption);
        painter.galley(at, galley, color);
    }

    /// The menu under your name: the web's Settings tabs, which open in the
    /// browser, then this computer's own pages.
    fn user_menu(
        &self,
        ui: &mut Ui,
        sidebar: Rect,
        anchor: Rect,
        state: &mut ChatState,
    ) -> Option<Event> {
        if !state.user_menu {
            return None;
        }
        let p = self.palette;
        enum Item {
            Title(&'static str),
            Web(Icon, String, String),
            Local(Icon, crate::ui::Page),
            Divider,
        }
        let mut items = Vec::new();
        if !state.list.links.settings.is_empty() {
            items.push(Item::Title("Chat with Work"));
            for tab in &state.list.links.settings {
                let icon = tab
                    .icon
                    .as_deref()
                    .and_then(Icon::named)
                    .unwrap_or(Icon::GearSix);
                items.push(Item::Web(icon, tab.label.clone(), tab.url.clone()));
            }
            items.push(Item::Divider);
        }
        items.push(Item::Title("This computer"));
        for (icon, page) in [
            (Icon::FolderSimple, crate::ui::Page::Folders),
            (Icon::LockSimple, crate::ui::Page::Privacy),
            (Icon::Clock, crate::ui::Page::Activity),
            (Icon::Laptop, crate::ui::Page::Account),
            (Icon::GearSix, crate::ui::Page::General),
        ] {
            items.push(Item::Local(icon, page));
        }
        // `.menu-title` and a `.menu__divider`; items 33.8 high, 1 apart.
        let height_of = |item: &Item| match item {
            Item::Title(_) => 32.5,
            Item::Divider => 9.0,
            _ => 34.8,
        };
        let content: f32 = items.iter().map(height_of).sum();
        let width = sidebar.width() - 16.0;
        let height = (5.0 + content + 5.0).min(anchor.top() - sidebar.top() - 16.0);
        let rect = Rect::from_min_size(
            pos2(sidebar.left() + 8.0, anchor.top() - 4.0 - height),
            vec2(width, height),
        );
        let mut event = None;
        let mut close = false;
        egui::Area::new(Id::new("chat-user-menu"))
            .order(egui::Order::Foreground)
            .fixed_pos(rect.min)
            .show(ui.ctx(), |ui| {
                controls::menu_panel(ui.painter(), rect, p);
                let inner = rect.shrink(5.0);
                let mut child = ui.new_child(egui::UiBuilder::new().max_rect(inner));
                child.set_clip_rect(inner);
                egui::ScrollArea::vertical()
                    .id_salt("chat-user-menu-scroll")
                    .max_height(inner.height())
                    .show(&mut child, |ui| {
                        ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
                        for (n, item) in items.iter().enumerate() {
                            let (row, _) = ui.allocate_exact_size(
                                vec2(inner.width(), height_of(item)),
                                Sense::hover(),
                            );
                            let row = match item {
                                Item::Web(..) | Item::Local(..) => {
                                    Rect::from_min_size(row.min, vec2(row.width(), 33.8))
                                }
                                _ => row,
                            };
                            let id = Id::new(("chat-user-menu-item", n));
                            match item {
                                Item::Title(text) => {
                                    // `.menu-title`: mono, faint.
                                    let galley = paint::layout(
                                        ui.painter(),
                                        paint::job(
                                            text,
                                            Type::mono(11.0, 16.5),
                                            p.ink_faint,
                                            f32::INFINITY,
                                        ),
                                    );
                                    let at = pos2(
                                        row.left() + 12.0,
                                        row.center().y - galley.size().y / 2.0,
                                    );
                                    widgets::label(
                                        ui,
                                        Rect::from_min_size(at, galley.size()),
                                        text,
                                    );
                                    ui.painter().galley(at, galley, p.ink_faint);
                                }
                                Item::Divider => {
                                    ui.painter().hline(
                                        row.x_range(),
                                        row.center().y,
                                        egui::Stroke::new(1.0, p.line),
                                    );
                                }
                                Item::Web(icon, label, url) => {
                                    if controls::menu_item(
                                        ui,
                                        id,
                                        row,
                                        Some(*icon),
                                        label,
                                        false,
                                        false,
                                        p,
                                        self.images,
                                    )
                                    .clicked()
                                    {
                                        close = true;
                                        event = Some(Event::OpenUrl(url.clone()));
                                    }
                                }
                                Item::Local(icon, page) => {
                                    if controls::menu_item(
                                        ui,
                                        id,
                                        row,
                                        Some(*icon),
                                        page.title(),
                                        false,
                                        false,
                                        p,
                                        self.images,
                                    )
                                    .clicked()
                                    {
                                        close = true;
                                        event = Some(Event::Page(*page));
                                    }
                                }
                            }
                        }
                    });
            });
        if close
            || controls::pressed_outside(ui, &[rect, anchor])
            || ui.input(|i| i.key_pressed(Key::Escape))
        {
            state.user_menu = false;
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

    fn list(&self, ui: &mut Ui, rect: Rect, state: &mut ChatState) -> Option<Event> {
        let p = self.palette;
        let mut event = None;
        let visible: Vec<ChatSummary> = state.visible().into_iter().cloned().collect();
        let mut menu_anchor = None;
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
                for chat in &visible {
                    let (row, _) = ui.allocate_exact_size(vec2(width, 39.0), Sense::hover());
                    let row = Rect::from_min_max(
                        pos2(row.left() + 8.0, row.top()),
                        pos2(row.right() - 8.0, row.bottom() - 1.0),
                    );
                    let selected = state.open == Some(chat.number);
                    if let Some((number, title)) =
                        state.renaming.as_mut().filter(|(n, _)| *n == chat.number)
                    {
                        let number = *number;
                        if selected {
                            let radius = tokens::RADIUS_CONTROL - 2.0;
                            ui.painter().rect_filled(row, radius, p.surface);
                            ui.painter().rect_stroke(
                                row.shrink(0.5),
                                radius,
                                egui::Stroke::new(1.0, p.line),
                                egui::StrokeKind::Middle,
                            );
                        }
                        match self.rename(ui, row, number, title) {
                            Some(true) => {
                                let title = title.trim().to_string();
                                state.renaming = None;
                                if !title.is_empty() && title != chat.title {
                                    event = Some(Event::Rename(number, title));
                                }
                            }
                            Some(false) => state.renaming = None,
                            None => {}
                        }
                        continue;
                    }
                    let (chosen, menu) =
                        self.row(ui, row, chat, selected, state.menu == Some(chat.number));
                    if chosen {
                        event = Some(Event::Open(chat.number));
                    }
                    if menu {
                        if state.menu == Some(chat.number) {
                            state.menu = None;
                        } else {
                            state.menu = Some(chat.number);
                        }
                    }
                    if state.menu == Some(chat.number) {
                        menu_anchor = Some((chat.clone(), self.menu_button_rect(row)));
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
        // The open row's menu: Rename and Delete, where the chat allows them.
        if let Some((chat, anchor)) = menu_anchor
            && let Some(e) = self.row_menu(ui, &chat, anchor, state)
        {
            event = Some(e);
        }
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

    /// Where a row's menu button sits: 24 square, 4 in from the right.
    fn menu_button_rect(&self, row: Rect) -> Rect {
        Rect::from_center_size(
            pos2(row.right() - 4.0 - 12.0, row.center().y),
            vec2(24.0, 24.0),
        )
    }

    /// One chat: true when it was chosen, and whether its menu button was
    /// clicked.
    fn row(
        &self,
        ui: &mut Ui,
        rect: Rect,
        chat: &ChatSummary,
        selected: bool,
        menu_open: bool,
    ) -> (bool, bool) {
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
        let has_menu = chat.can(|c| c.rename || c.delete);
        let text_right = if has_menu {
            rect.right() - 4.0 - 24.0 - 4.0
        } else {
            rect.right() - 4.0
        };
        let galley = paint::layout(
            painter,
            paint::line_job(title, scale::SMALL, color, text_right - x),
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
        // `Chat options`: three dots, shown on hover, on focus, or open.
        let mut menu_clicked = false;
        if has_menu {
            let button = self.menu_button_rect(rect);
            let button_id = id.with("options");
            let near = ui.rect_contains_pointer(rect);
            let focused = ui.memory(|m| m.has_focus(button_id));
            let shown = widgets::hover(
                ui,
                button_id.with("shown"),
                near || menu_open || focused,
                tokens::INSTANT,
            );
            let mut faded = ui.new_child(egui::UiBuilder::new().max_rect(button));
            faded.set_opacity(shown);
            let clicked = widgets::IconButton {
                icon: Icon::DotsThree,
                icon_size: 20.0,
                label: "Chat options",
                tooltip: None,
                color: p.ink_muted,
                hover_color: p.ink_muted,
                wash: 7.0,
                radius: tokens::RADIUS_CONTROL,
                enabled: true,
            }
            .show(&mut faded, button_id, button, self.images, p)
            .clicked();
            menu_clicked = clicked;
        }
        (response.clicked() && !menu_clicked, menu_clicked)
    }

    /// `.sidebar__rename`: the title in a field and a check to save it.
    /// `Some(true)` saves, `Some(false)` cancels.
    fn rename(&self, ui: &mut Ui, row: Rect, number: u64, title: &mut String) -> Option<bool> {
        let p = self.palette;
        let id = Id::new(("chat-rename", number));
        let field = Rect::from_min_max(
            pos2(row.left() + 4.0, row.center().y - 15.0),
            pos2(row.right() - 4.0 - 24.0 - 4.0 - 4.0, row.center().y + 15.0),
        );
        let first = !ui.memory(|m| m.has_focus(id))
            && ui.data(|d| d.get_temp::<bool>(id.with("started")).is_none());
        let response = controls::text_field(
            ui,
            id,
            field,
            title,
            "",
            scale::SMALL,
            8.0,
            FieldLook::new(p),
            p,
            "Chat title",
        );
        if first {
            response.request_focus();
            ui.data_mut(|d| d.insert_temp(id.with("started"), true));
        }
        let button = Rect::from_center_size(
            pos2(row.right() - 4.0 - 4.0 - 12.0, row.center().y),
            vec2(24.0, 24.0),
        );
        let save = widgets::IconButton {
            icon: Icon::Check,
            icon_size: 16.0,
            label: "Save title",
            tooltip: None,
            color: p.ink,
            hover_color: p.ink,
            wash: 7.0,
            radius: tokens::RADIUS_CONTROL,
            enabled: true,
        }
        .show(ui, id.with("save"), button, self.images, p)
        .clicked();
        let (enter, escape) = ui.input(|i| (i.key_pressed(Key::Enter), i.key_pressed(Key::Escape)));
        let done = if save || (response.lost_focus() && enter) {
            Some(true)
        } else if escape {
            Some(false)
        } else {
            None
        };
        if done.is_some() {
            ui.data_mut(|d| d.remove::<bool>(id.with("started")));
        }
        done
    }

    /// The row's menu (`.dropdown-content.menu.w-44`), below its button.
    fn row_menu(
        &self,
        ui: &mut Ui,
        chat: &ChatSummary,
        anchor: Rect,
        state: &mut ChatState,
    ) -> Option<Event> {
        let p = self.palette;
        let mut items: Vec<(Icon, &str, bool)> = Vec::new();
        if chat.can(|c| c.rename) {
            items.push((Icon::PencilSimple, "Rename", false));
        }
        if chat.can(|c| c.delete) {
            items.push((Icon::Trash, "Delete", true));
        }
        let item_h = 33.8;
        let height = items.len() as f32 * item_h + (items.len().saturating_sub(1)) as f32 + 8.0;
        let rect = Rect::from_min_size(
            pos2(anchor.right() - 176.0, anchor.bottom() + 8.0),
            vec2(176.0, height),
        );
        let mut event = None;
        let mut close = false;
        egui::Area::new(Id::new(("chat-row-menu", chat.number)))
            .order(egui::Order::Foreground)
            .fixed_pos(rect.min)
            .show(ui.ctx(), |ui| {
                controls::menu_panel(ui.painter(), rect, p);
                for (n, (icon, label, negative)) in items.iter().enumerate() {
                    let row = Rect::from_min_size(
                        pos2(rect.left() + 8.0, rect.top() + n as f32 * (item_h + 1.0)),
                        vec2(160.0, item_h),
                    );
                    let id = Id::new(("chat-row-menu-item", chat.number, n));
                    if controls::menu_item(
                        ui,
                        id,
                        row,
                        Some(*icon),
                        label,
                        *negative,
                        false,
                        p,
                        self.images,
                    )
                    .clicked()
                    {
                        close = true;
                        if *negative {
                            event = Some(Event::Delete(chat.number));
                        } else {
                            state.renaming = Some((chat.number, chat.title.clone()));
                        }
                    }
                }
            });
        if close
            || controls::pressed_outside(ui, &[rect, anchor])
            || ui.input(|i| i.key_pressed(Key::Escape))
        {
            state.menu = None;
        }
        event
    }
}

/// What the person pinned, projects then chats, each in pin order, as far
/// as the list knows them.
fn pins(state: &ChatState) -> Vec<Pin> {
    let pins = &state.list.pins;
    let projects = pins
        .projects
        .iter()
        .filter_map(|id| state.project(*id).cloned().map(Pin::Project));
    let chats = pins.chats.iter().filter_map(|number| {
        state
            .list
            .chats
            .iter()
            .find(|c| c.number == *number)
            .map(|c| Pin::Chat(c.number, display_title(c).to_string()))
    });
    projects.chain(chats).collect()
}

/// A chat's title, or "New chat" before it has one.
fn display_title(chat: &ChatSummary) -> &str {
    if chat.title.trim().is_empty() {
        "New chat"
    } else {
        chat.title.as_str()
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
    fn initials_read_like_the_web() {
        assert_eq!(initials("Alex Rivera"), "AR");
        assert_eq!(initials("Ada"), "A");
        assert_eq!(initials("dana@example.com"), "D");
        assert_eq!(initials(""), "");
    }
}
