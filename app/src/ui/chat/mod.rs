//! The Chat page: your Chat with Work chats, drawn as the web app draws
//! them (docs/chat-window.md), through the daemon's chat API, as `cww tui`
//! uses it (CONTROL.md, "Chats").
//!
//! The page asks for frames only while something on it moves: a
//! transition, a streaming answer, running work's rings and shimmer. New
//! text arrives from the follower thread, which wakes the window.

pub mod cards;
pub mod composer;
pub mod controls;
pub mod conversation;
pub mod dialogs;
pub mod highlight;
pub mod icons;
pub mod logos;
pub mod markdown;
pub mod mcp_app;
pub mod paint;
pub mod prose;
pub mod runner;
pub mod sidebar;
pub mod state;
pub mod tokens;
pub mod widgets;

use egui::{Id, Key, Modifiers, Rect, Sense, Ui, UiBuilder, pos2, vec2};

use crate::model::Status;
use composer::Composer;
use conversation::{Conversation, ConversationView};
use icons::{Icon, Images};
use runner::Runner;
use sidebar::Sidebar;
use state::{Access, ChatState, Command};
use tokens::{Palette, Type};

/// What the page asks of the window around it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Go to the settings, on this page.
    Settings(super::Page),
    StartAgent,
}

/// What the page needs to know about the daemon and the window.
pub struct Env<'a> {
    pub status: Option<&'a Status>,
    /// The daemon answered (or didn't) at least once.
    pub known: bool,
    pub dark: bool,
    /// `Start Local Agent` is under way.
    pub starting: bool,
}

const GREETINGS: [&str; 8] = [
    "What's up, {}?",
    "Hey {}, what's next?",
    "What can I do for you, {}?",
    "Hi {}, what's the task?",
    "What are we working on, {}?",
    "Hey {}, what needs doing?",
    "What's the job, {}?",
    "Hi {}, let's get to it.",
];

pub struct ChatPage {
    state: ChatState,
    runner: Runner,
    images: Option<Images>,
    conversation: ConversationView,
    /// The docked sidebar is collapsed (wide windows).
    collapsed: bool,
    /// The sidebar is open as a drawer (narrow windows).
    drawer: bool,
    focus_composer: bool,
    focus_search: bool,
    /// Frames left to keep scrolling to the latest message: the scroll
    /// area learns how tall the chat is a frame late.
    scroll_to_bottom: u8,
    /// The chat shown last frame, to start fresh on another.
    shown: Option<Option<u64>>,
    greeting: usize,
    /// When the new-chat screen last appeared, for its rise.
    new_chat_since: Option<f64>,
    /// Hold every animation at one moment, for snapshots.
    still: bool,
    /// The file picker is open; what's picked arrives here.
    picking: Option<std::sync::mpsc::Receiver<Vec<std::path::PathBuf>>>,
    /// The window, to wake when the file picker closes.
    ctx: Option<egui::Context>,
    /// When the list was last read because a chat in it was running.
    relisted: f64,
    /// The daemon's endpoint and cww's data folder, where the web's images
    /// come from and are kept.
    socket: std::path::PathBuf,
    data_dir: std::path::PathBuf,
}

/// How often to read the list again while a chat in it is being answered
/// elsewhere, so its dot and shimmer stop when it's done.
const RELIST: f64 = 4.0;

/// The moment a still page shows: the shimmer mid-sweep, the caret on.
const STILL_TIME: f64 = 0.35;

impl ChatPage {
    pub fn new(socket: &std::path::Path, data_dir: std::path::PathBuf) -> Self {
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as usize);
        Self {
            state: ChatState::default(),
            runner: Runner::new(socket),
            images: None,
            conversation: ConversationView::default(),
            collapsed: false,
            drawer: false,
            focus_composer: false,
            focus_search: false,
            scroll_to_bottom: 3,
            shown: None,
            greeting: seed % GREETINGS.len(),
            new_chat_since: None,
            still: false,
            picking: None,
            ctx: None,
            relisted: 0.0,
            socket: socket.to_path_buf(),
            data_dir,
        }
    }

    /// Hold every animation still, for snapshots.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn set_still(&mut self, still: bool) {
        self.still = still;
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn state(&self) -> &ChatState {
        &self.state
    }

    #[cfg(test)]
    pub fn conversation_mut(&mut self) -> &mut ConversationView {
        &mut self.conversation
    }

    #[cfg(test)]
    pub fn state_mut(&mut self) -> &mut ChatState {
        &mut self.state
    }

    /// The web's images still on their way.
    #[cfg(test)]
    pub fn logos_pending(&self) -> usize {
        self.images.as_ref().map_or(0, |i| i.logos.pending())
    }

    /// Pick the greeting (tests want the same one each time).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn set_greeting(&mut self, index: usize) {
        self.greeting = index % GREETINGS.len();
    }

    /// Open a chat, as choosing it in the sidebar does.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn open(&mut self, number: u64) {
        let commands = self.state.open_chat(number);
        self.run(commands);
    }

    /// The window closes: stop following.
    pub fn stop(&self) {
        self.runner.stop();
    }

    fn run(&mut self, commands: Vec<Command>) {
        for command in commands {
            self.runner.run(command);
        }
    }

    pub fn show(&mut self, ui: &mut Ui, env: &Env) -> Option<Action> {
        let ctx = ui.ctx().clone();
        self.runner.attach(&ctx);
        if self.ctx.is_none() {
            self.ctx = Some(ctx.clone());
        }
        // Files picked, or dropped on the window.
        if let Some(rx) = &self.picking
            && let Ok(paths) = rx.try_recv()
        {
            self.picking = None;
            self.attach(paths);
        }
        let dropped: Vec<std::path::PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .filter(|p| p.is_absolute())
                .collect()
        });
        if !dropped.is_empty() {
            self.attach(dropped);
        }
        let paired = env
            .status
            .is_some_and(|s| s.is_paired() && s.connection.connection != "revoked");
        let commands = self.state.set_paired(paired);
        self.run(commands);
        for msg in self.runner.drain() {
            let commands = self.state.update(msg);
            self.run(commands);
        }
        // A chat being answered elsewhere, while the server doesn't keep
        // the list current (an older daemon or server): read the list again
        // now and then, until it's done. Nothing runs, nothing is read.
        let running_elsewhere = self.state.ready()
            && !self.state.list_live
            && self
                .state
                .list
                .chats
                .iter()
                .any(|c| c.processing() && Some(c.number) != self.state.open);
        if running_elsewhere {
            let now = ctx.input(|i| i.time);
            if now - self.relisted >= RELIST {
                self.relisted = now;
                self.run(vec![Command::List]);
            }
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(RELIST));
        }
        if self.shown != Some(self.state.open) {
            self.shown = Some(self.state.open);
            self.conversation.reset();
            self.scroll_to_bottom = 3;
            self.new_chat_since = None;
        }
        let images = self.images.take().unwrap_or_else(|| Images::load(&ctx));
        images.logos.set_source(
            &self.socket,
            env.status
                .filter(|_| paired)
                .and_then(|s| s.server.as_deref()),
            Some(&self.data_dir),
        );
        images.logos.poll(&ctx);
        let action = self.page(ui, env, &images);
        self.images = Some(images);
        action
    }

    fn page(&mut self, ui: &mut Ui, env: &Env, images: &Images) -> Option<Action> {
        let ctx = ui.ctx().clone();
        let p = Palette::new(env.dark);
        let time = if self.still {
            STILL_TIME
        } else {
            ctx.input(|i| i.time)
        };
        let full = ui.max_rect();
        let mut action = None;
        let wide = full.width() >= tokens::WIDE;
        let docked = wide && !self.collapsed;
        if wide {
            self.drawer = false;
        }

        // Shortcuts, as on the web.
        let (search, new_chat, focus, escape) = ctx.input_mut(|i| {
            (
                i.consume_shortcut(&egui::KeyboardShortcut::new(Modifiers::COMMAND, Key::K)),
                i.consume_shortcut(&egui::KeyboardShortcut::new(Modifiers::ALT, Key::N)),
                i.consume_shortcut(&egui::KeyboardShortcut::new(Modifiers::COMMAND, Key::Slash)),
                i.key_pressed(Key::Escape),
            )
        });
        if search {
            self.focus_search = true;
            if !docked {
                self.drawer = true;
                self.collapsed = false;
            }
        }
        if new_chat && self.state.ready() {
            let commands = self.state.new_chat();
            self.run(commands);
            self.focus_composer = true;
        }
        if focus {
            self.focus_composer = true;
        }
        if escape && self.drawer {
            self.drawer = false;
        }

        ui.painter().rect_filled(full, 0.0, p.canvas);
        let main = if docked {
            Rect::from_min_max(
                pos2(full.left() + tokens::SIDEBAR_WIDTH, full.top()),
                full.max,
            )
        } else {
            full
        };

        let conversing = self.state.open.is_some() || self.state.pending_question.is_some();
        let mut animating = false;
        if conversing {
            animating |= self.conversation_area(ui, main, &p, images, time, env, &mut action);
        } else {
            animating |= self.new_chat_area(ui, main, &p, images, time, env, &mut action);
        }

        // Moving things: running work, streaming, uploads, the drawer.
        if !self.still
            && (self.state.working()
                || self.state.uploading()
                || self.state.list.chats.iter().any(|c| c.processing()))
        {
            animating = true;
        }

        // The sidebar's own button, while it isn't docked.
        if !docked {
            let rect = Rect::from_min_size(main.min + vec2(8.0, 8.0), vec2(32.0, 32.0));
            egui::Area::new(Id::new("chat-open-sidebar"))
                .order(egui::Order::Middle)
                .fixed_pos(rect.min)
                .show(&ctx, |ui| {
                    let response = widgets::IconButton {
                        icon: Icon::SidebarSimple,
                        icon_size: 20.0,
                        label: "Open sidebar",
                        tooltip: None,
                        color: p.ink,
                        hover_color: p.ink,
                        wash: 7.0,
                        radius: tokens::RADIUS_CONTROL - 2.0,
                        enabled: true,
                    }
                    .show(
                        ui,
                        Id::new("chat-open-sidebar-button"),
                        rect,
                        images,
                        &p,
                    );
                    if response.clicked() {
                        if wide {
                            self.collapsed = false;
                        } else {
                            self.drawer = true;
                        }
                    }
                });
        }

        let user = if self.state.list.user.name.is_empty() {
            "Settings".to_string()
        } else {
            self.state.list.user.name.clone()
        };
        let drawer_t = ctx.animate_bool_with_time_and_easing(
            Id::new("chat-drawer"),
            self.drawer,
            tokens::DRAWER,
            tokens::ease_out,
        );
        if docked {
            let rect = Rect::from_min_size(full.min, vec2(tokens::SIDEBAR_WIDTH, full.height()));
            let mut child = ui.new_child(UiBuilder::new().max_rect(rect));
            let event = Sidebar {
                images,
                palette: &p,
                time,
                drawer: false,
                user: &user,
                focus_search: std::mem::take(&mut self.focus_search),
            }
            .show(&mut child, rect, &mut self.state);
            self.sidebar_event(event, &mut action);
        } else if drawer_t > 0.0 {
            animating |= drawer_t < 1.0;
            let width = tokens::SIDEBAR_WIDTH.min(full.width() * 0.85);
            egui::Area::new(Id::new("chat-drawer-area"))
                .order(egui::Order::Foreground)
                .fixed_pos(full.min)
                .show(&ctx, |ui| {
                    // The backdrop dims the conversation and closes on a click.
                    let backdrop =
                        ui.interact(full, Id::new("chat-drawer-backdrop"), Sense::click());
                    ui.painter().rect_filled(
                        full,
                        0.0,
                        egui::Color32::from_black_alpha((0.4 * 255.0 * drawer_t) as u8),
                    );
                    if backdrop.clicked() {
                        self.drawer = false;
                    }
                    let rect = Rect::from_min_size(
                        pos2(full.left() - width * (1.0 - drawer_t), full.top()),
                        vec2(width, full.height()),
                    );
                    let mut child = ui.new_child(UiBuilder::new().max_rect(rect));
                    // Swallow clicks under the drawer.
                    child.interact(rect, Id::new("chat-drawer-body"), Sense::click());
                    let event = Sidebar {
                        images,
                        palette: &p,
                        time,
                        drawer: true,
                        user: &user,
                        focus_search: std::mem::take(&mut self.focus_search),
                    }
                    .show(&mut child, rect, &mut self.state);
                    if matches!(
                        event,
                        Some(
                            sidebar::Event::Open(_)
                                | sidebar::Event::NewChat
                                | sidebar::Event::Settings
                                | sidebar::Event::Toggle
                        )
                    ) {
                        self.drawer = false;
                    }
                    self.sidebar_event(event, &mut action);
                });
        }

        // The dialog over it all, and the clipboard.
        for outcome in dialogs::show(&ctx, &mut self.state, &p, images) {
            match outcome {
                dialogs::Outcome::Commands(commands) => self.run(commands),
                dialogs::Outcome::Copy(text) => ctx.copy_text(text),
            }
        }
        if let Some(text) = self.state.clipboard.take() {
            ctx.copy_text(text);
        }

        if animating {
            ctx.request_repaint();
        }
        action
    }

    fn sidebar_event(&mut self, event: Option<sidebar::Event>, action: &mut Option<Action>) {
        match event {
            Some(sidebar::Event::NewChat) => {
                let commands = self.state.new_chat();
                self.run(commands);
                self.focus_composer = true;
            }
            Some(sidebar::Event::Open(number)) => {
                let commands = self.state.open_chat(number);
                self.run(commands);
                self.focus_composer = true;
            }
            Some(sidebar::Event::Toggle) => {
                if self.drawer {
                    self.drawer = false;
                } else {
                    self.collapsed = true;
                }
            }
            Some(sidebar::Event::Settings) => {
                *action = Some(Action::Settings(super::Page::Folders))
            }
            Some(sidebar::Event::Rename(chat, title)) => {
                let commands = self.state.act(chat, state::ChatAction::Rename(title));
                self.run(commands);
            }
            Some(sidebar::Event::Delete(chat)) => {
                self.state.dialog = Some(state::Dialog::Delete { chat });
            }
            None => {}
        }
    }

    /// Why the composer is locked right now, and the way out.
    fn lock(&self, env: &Env) -> Option<(String, Option<&'static str>)> {
        if !env.known {
            return Some(("Looking for the Local Agent…".into(), None));
        }
        let Some(status) = env.status else {
            let action = if env.starting { None } else { Some("Start it") };
            let reason = if env.starting {
                "Starting the Local Agent…"
            } else {
                "The Local Agent isn't running."
            };
            return Some((reason.into(), action));
        };
        if status.connection.connection == "revoked" {
            return Some((
                "This computer's pairing was revoked.".into(),
                Some("Pair again"),
            ));
        }
        if !status.is_paired() {
            return Some((
                "This computer isn't paired with Chat with Work.".into(),
                Some("Pair it"),
            ));
        }
        match &self.state.access {
            Access::Unknown | Access::Loading => Some(("Loading your chats…".into(), None)),
            Access::NeedsApproval {
                requested: false, ..
            } => Some((
                "Chat with Work hasn't allowed this computer to use your chats.".into(),
                Some("Ask for access"),
            )),
            Access::NeedsApproval {
                requested: true,
                url,
            } => Some((
                "Asked. Allow it in Chat with Work: Settings, Computers.".into(),
                url.as_ref().map(|_| "Open Settings"),
            )),
            Access::Unavailable(failure) => Some((failure.message.clone(), Some("Try again"))),
            Access::Ready => self.state.locked_reason().map(|r| (r.to_string(), None)),
        }
    }

    fn lock_action(&mut self, env: &Env, action: &mut Option<Action>) {
        if env.status.is_none() {
            *action = Some(Action::StartAgent);
            return;
        }
        if env
            .status
            .is_some_and(|s| !s.is_paired() || s.connection.connection == "revoked")
        {
            *action = Some(Action::Settings(super::Page::Account));
            return;
        }
        match &self.state.access {
            Access::NeedsApproval {
                requested: false, ..
            } => {
                let commands = self.state.request_access();
                self.run(commands);
            }
            Access::NeedsApproval { url: Some(url), .. } => {
                let url = url.clone();
                self.open_on_server(env, &url);
            }
            Access::Unavailable(_) => {
                let commands = self.state.retry();
                self.run(commands);
            }
            _ => {}
        }
    }

    /// Open a page of the paired server, the only kind of page the app
    /// opens on the server's say-so.
    fn open_on_server(&self, env: &Env, url: &str) {
        let on_server = env
            .status
            .and_then(|s| s.server.as_deref())
            .is_some_and(|server| url.starts_with(&format!("{}/", server.trim_end_matches('/'))));
        if on_server {
            super::open_url(url);
        }
    }

    /// The open chat in the browser, or the server's new chat.
    fn open_chat_in_browser(&self, env: &Env) {
        match self.state.open_summary().map(|c| c.url.clone()) {
            Some(url) if !url.is_empty() => self.open_on_server(env, &url),
            _ => {
                if let Some(server) = env.status.and_then(|s| s.server.as_deref()) {
                    super::open_url(server);
                }
            }
        }
    }

    /// The native file picker, for files to attach. It's built on this
    /// thread (AppKit wants that) and awaited on another; what's picked
    /// comes back through `picked`.
    fn pick_files(&mut self) {
        if self.picking.is_some() || !self.state.ready() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        self.picking = Some(rx);
        #[cfg(test)]
        {
            // Tests never open a window: they say what was picked.
            let _ = tx.send(
                tests::PICKED
                    .with(|p| p.borrow_mut().take())
                    .unwrap_or_default(),
            );
        }
        #[cfg(not(test))]
        {
            let mut dialog = rfd::AsyncFileDialog::new().set_title("Attach Files");
            if let Some(home) = crate::paths::home_dir() {
                dialog = dialog.set_directory(home);
            }
            let picked = dialog.pick_files();
            let ctx = self.ctx.clone();
            let spawned = std::thread::Builder::new()
                .name("cww-chat-pick".into())
                .spawn(move || {
                    let files = pollster::block_on(picked)
                        .map(|files| files.iter().map(|f| f.path().to_path_buf()).collect())
                        .unwrap_or_default();
                    let _ = tx.send(files);
                    if let Some(ctx) = ctx {
                        ctx.request_repaint();
                    }
                });
            if let Err(e) = spawned {
                log::error!("can't open the file picker: {e}");
                self.picking = None;
            }
        }
    }

    /// Files to attach: picked, or dropped on the window.
    pub fn attach(&mut self, paths: Vec<std::path::PathBuf>) {
        let commands = self.state.attach(paths);
        self.run(commands);
    }

    fn composer_event(
        &mut self,
        event: Option<composer::Event>,
        env: &Env,
        action: &mut Option<Action>,
    ) {
        match event {
            Some(composer::Event::Send) => {
                let commands = self.state.send();
                self.run(commands);
                self.scroll_to_bottom = 3;
            }
            Some(composer::Event::Stop) => {
                let commands = self.state.stop();
                self.run(commands);
            }
            Some(composer::Event::Action) => self.lock_action(env, action),
            Some(composer::Event::Attach) => self.pick_files(),
            Some(composer::Event::Detach(key)) => self.state.detach(key),
            Some(composer::Event::PickModel(id)) => self.state.pick_model(&id),
            None => {}
        }
    }

    fn composer<'a>(
        &self,
        lock: &'a Option<(String, Option<&'static str>)>,
        time: f64,
    ) -> Composer<'a> {
        let notice_free = lock
            .as_ref()
            .map(|(reason, action)| (reason.as_str(), *action));
        Composer {
            placeholder: if self.state.open.is_some() {
                "Reply to Chat with Work"
            } else {
                "Ask anything…"
            },
            locked: notice_free.map(|(r, _)| r),
            action: notice_free.and_then(|(_, a)| a),
            working: self.state.working(),
            stopping: self.state.stopping,
            sending: self.state.sending,
            can_send: self.state.can_send().is_ok(),
            time,
            attachments: self.state.attachments.clone(),
            models: match &self.state.models {
                state::ModelList::Ready(models) => models.models.clone(),
                _ => Vec::new(),
            },
            model: self.state.current_model(),
            model_logo: self
                .state
                .open_summary()
                .and_then(|c| c.model.as_ref())
                .and_then(|m| m.logo.clone()),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn conversation_area(
        &mut self,
        ui: &mut Ui,
        main: Rect,
        p: &Palette,
        images: &Images,
        time: f64,
        env: &Env,
        action: &mut Option<Action>,
    ) -> bool {
        let ctx = ui.ctx().clone();
        let column = (main.width() - 32.0).min(tokens::COLUMN);
        let mut animating = false;
        let mut events = Vec::new();
        let mut child = ui.new_child(UiBuilder::new().max_rect(main));
        // On the chat's first read: start at the latest message.
        let scroll_to_bottom = self.scroll_to_bottom > 0 && !self.state.loading;
        if scroll_to_bottom {
            self.scroll_to_bottom -= 1;
            ctx.request_repaint();
        }
        let output = egui::ScrollArea::vertical()
            .id_salt(("chat-conversation", self.state.open))
            .auto_shrink([false, false])
            .stick_to_bottom(true)
            .show(&mut child, |ui| {
                ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
                ui.add_space(24.0);
                let left = main.center().x - column / 2.0;
                let mut col = ui.new_child(
                    UiBuilder::new()
                        .max_rect(Rect::from_min_size(
                            pos2(left, ui.cursor().top()),
                            vec2(column, f32::INFINITY),
                        ))
                        .layout(egui::Layout::top_down(egui::Align::Min)),
                );
                col.spacing_mut().item_spacing = vec2(0.0, 0.0);
                let (found, moving) = Conversation {
                    palette: p,
                    images,
                    time,
                    still: self.still,
                }
                .show(&mut col, &self.state, &mut self.conversation);
                events = found;
                animating |= moving;
                if let Some(error) = &self.state.error {
                    col.add_space(12.0);
                    empty_state(
                        &mut col,
                        images,
                        p,
                        Icon::WarningCircle,
                        "This chat isn't available",
                        error,
                    );
                }
                let height = col.min_rect().height();
                ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
                ui.add_space(176.0);
                if scroll_to_bottom {
                    ui.scroll_to_cursor_animation(
                        Some(egui::Align::BOTTOM),
                        egui::style::ScrollAnimation::none(),
                    );
                }
            });
        for event in events {
            match event {
                conversation::Event::OpenUrl(url) => super::open_url(&url),
                conversation::Event::OpenChat => self.open_chat_in_browser(env),
                conversation::Event::Copy(text) => ctx.copy_text(text),
                conversation::Event::Retry { message, user } => {
                    if let Some(chat) = self.state.open {
                        self.state.dialog = Some(state::Dialog::Retry {
                            chat,
                            message,
                            user,
                        });
                    }
                }
                conversation::Event::Branch(message) => {
                    if let Some(chat) = self.state.open {
                        let commands = self.state.act(chat, state::ChatAction::Branch(message));
                        self.run(commands);
                    }
                }
                conversation::Event::Share => {
                    if let Some(chat) = self.state.open {
                        self.state.dialog = Some(state::Dialog::Share {
                            chat,
                            copied: false,
                        });
                    }
                }
                conversation::Event::Decide(decision) => {
                    let commands = self.state.decide(decision);
                    self.run(commands);
                }
            }
        }
        let behind = output.content_size.y - output.inner_rect.height() - output.state.offset.y;
        let near_bottom = behind < 96.0;

        // The dock: the composer over the bottom of the conversation.
        let lock = self.lock(env);
        let composer = self.composer(&lock, time);
        let width = main.width().min(tokens::COLUMN + 16.0) - 16.0;
        let height = composer.height(ui, &self.state.input, width);
        let rect = Rect::from_min_size(
            pos2(main.center().x - width / 2.0, main.bottom() - 12.0 - height),
            vec2(width, height),
        );
        let focus = std::mem::take(&mut self.focus_composer);
        let mut event = None;
        let mut jump = false;
        // Not constrained to the window: a constrained area keeps last
        // frame's size and creeps up from the bottom edge, covering the
        // conversation above the composer.
        egui::Area::new(Id::new("chat-dock"))
            .order(egui::Order::Middle)
            .constrain(false)
            .fixed_pos(rect.min)
            .show(&ctx, |ui| {
                ui.allocate_rect(rect, Sense::hover());
                event = composer.show(ui, rect, &mut self.state.input, images, p, focus);
                jump = self.scroll_button(ui, rect, near_bottom, p, images, time);
            });
        if jump {
            self.scroll_to_bottom = 3;
        }
        if let Some(notice) = self.state.notice.clone() {
            notice_toast(&ctx, rect, &notice, p, images);
            if ctx.input(|i| i.key_pressed(Key::Escape)) {
                self.state.notice = None;
            }
        }
        self.composer_event(event, env, action);
        animating
    }

    /// `.scroll-button`: back to the latest message, with a live dot while
    /// more streams in. True when clicked.
    fn scroll_button(
        &self,
        ui: &mut Ui,
        composer: Rect,
        near_bottom: bool,
        p: &Palette,
        images: &Images,
        time: f64,
    ) -> bool {
        let id = Id::new("chat-scroll-button");
        let t = ui.ctx().animate_bool_with_time_and_easing(
            id,
            !near_bottom,
            tokens::BASE,
            tokens::ease_snap,
        );
        if t <= 0.0 {
            return false;
        }
        let center = pos2(
            composer.center().x,
            composer.top() - 48.0 + 18.0 + 8.0 * (1.0 - t),
        );
        let rect = Rect::from_center_size(center, vec2(36.0, 36.0));
        let response = ui.interact(
            rect,
            id.with("button"),
            if t > 0.5 {
                Sense::click()
            } else {
                Sense::hover()
            },
        );
        response.widget_info(|| {
            egui::WidgetInfo::labeled(
                egui::WidgetType::Button,
                true,
                "Scroll to the latest message",
            )
        });
        let hover = widgets::hover(ui, id, response.hovered(), tokens::FAST);
        let painter = ui.painter();
        paint::float_shadow(
            painter,
            rect,
            18.0,
            &Palette {
                shadow: p.shadow.gamma_multiply(t),
                ..*p
            },
        );
        painter.circle_filled(center, 18.0, p.surface_raised.gamma_multiply(t));
        painter.circle_stroke(
            center,
            18.5,
            egui::Stroke::new(1.0, p.line_strong.gamma_multiply(t)),
        );
        let color = tokens::lerp_rgb(p.ink_muted, p.ink, hover).gamma_multiply(t);
        images.icon_at(painter, center, 16.0, Icon::ArrowDown, color);
        if self.state.working() {
            let dot = pos2(rect.right() - 2.0 - 4.0, rect.top() + 2.0 + 4.0);
            painter.circle_filled(dot, 6.0, p.surface_raised.gamma_multiply(t));
            painter.circle_filled(dot, 4.0, p.live().gamma_multiply(t));
        }
        let _ = time;
        if response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        response.clicked()
    }

    #[allow(clippy::too_many_arguments)]
    fn new_chat_area(
        &mut self,
        ui: &mut Ui,
        main: Rect,
        p: &Palette,
        images: &Images,
        time: f64,
        env: &Env,
        action: &mut Option<Action>,
    ) -> bool {
        let ctx = ui.ctx().clone();
        let full_width = ctx.content_rect().width();
        let since = *self.new_chat_since.get_or_insert(time);
        let rise = if self.still {
            1.0
        } else {
            ((time - since) as f32 / tokens::SLOW).clamp(0.0, 1.0)
        };
        let eased = tokens::ease_snap(rise);

        // The faded dot grid behind it all.
        let backdrop = Rect::from_min_max(
            pos2(main.left() + 16.0 - 0.3 * full_width, main.top() - 32.0),
            pos2(main.right() - 16.0 + 0.3 * full_width, main.bottom() + 32.0),
        );
        paint::dot_grid(
            &ui.painter().with_clip_rect(main),
            backdrop,
            22.0,
            p.dot,
            true,
        );

        let column = (main.width() - 32.0).min(672.0);
        let lock = self.lock(env);
        let composer = self.composer(&lock, time);
        let composer_h = composer.height(ui, &self.state.input, column);
        let size = (0.036 * full_width).clamp(28.0, 40.0);
        let ty = Type::sans(size, size * 1.1).weight(650.0).tracking(-0.045);
        let name = self
            .state
            .list
            .user
            .name
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_string();
        let greeting = if name.is_empty() {
            "What are we working on?".to_string()
        } else {
            GREETINGS[self.greeting].replace("{}", &name)
        };
        let galley = paint::layout(ui.painter(), paint::job(&greeting, ty, p.ink, column));
        let content = galley.size().y + 28.0 + composer_h;
        // The column's padding, and the page's 8 at the bottom.
        let available = main.height() - 64.0 - 96.0 - 8.0;
        let top = main.top() + 64.0 + ((available - content) / 2.0).max(0.0);
        if std::env::var_os("CWW_DEBUG_LAYOUT").is_some() {
            eprintln!(
                "new chat: main {main:?} greeting {:?} composer {composer_h} top {top}",
                galley.size()
            );
        }
        let left = main.center().x - column / 2.0;
        let greeting_pos = pos2(
            main.center().x - galley.size().x / 2.0,
            top + 8.0 * (1.0 - eased),
        );
        let greeting_rect = Rect::from_min_size(greeting_pos, galley.size());
        widgets::label(ui, greeting_rect, &greeting);
        let greeting_height = galley.size().y;
        ui.painter().galley_with_override_text_color(
            greeting_pos,
            galley,
            p.ink.gamma_multiply(eased),
        );
        let rect = Rect::from_min_size(
            pos2(left, top + greeting_height + 28.0),
            vec2(column, composer_h),
        );
        let focus = std::mem::take(&mut self.focus_composer);
        let event = composer.show(ui, rect, &mut self.state.input, images, p, focus);
        if let Some(notice) = self.state.notice.clone() {
            notice_toast(&ctx, rect, &notice, p, images);
        }
        self.composer_event(event, env, action);
        rise < 1.0
    }
}

/// A failure over the composer: a question that didn't go, a file or a
/// model refused.
fn notice_toast(ctx: &egui::Context, composer: Rect, text: &str, p: &Palette, images: &Images) {
    let painter = ctx.layer_painter(egui::LayerId::background());
    let galley = paint::layout(
        &painter,
        paint::job(text, tokens::scale::SMALL, p.ink, composer.width() - 64.0),
    );
    let height = galley.size().y + 20.0;
    let rect = Rect::from_min_size(
        pos2(composer.left(), composer.top() - 12.0 - height),
        vec2(composer.width(), height),
    );
    egui::Area::new(Id::new("chat-notice"))
        .order(egui::Order::Foreground)
        .constrain(false)
        .fixed_pos(rect.min)
        .show(ctx, |ui| {
            let response = ui.allocate_rect(rect, Sense::hover());
            response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, text));
            let painter = ui.painter();
            painter.rect_filled(rect, tokens::RADIUS_CARD, p.surface);
            painter.rect_stroke(
                rect.shrink(0.5),
                tokens::RADIUS_CARD,
                egui::Stroke::new(1.0, p.line),
                egui::StrokeKind::Middle,
            );
            painter
                .with_clip_rect(Rect::from_min_size(rect.min, vec2(2.0, rect.height())))
                .rect_filled(rect, tokens::RADIUS_CARD, p.negative());
            images.icon_at(
                painter,
                pos2(rect.left() + 16.0 + 9.0, rect.top() + 10.0 + 10.0),
                18.0,
                Icon::WarningCircle,
                p.negative(),
            );
            painter.galley(pos2(rect.left() + 46.0, rect.top() + 10.0), galley, p.ink);
        });
}

/// `.empty-state`: a dashed card on the dot grid with an icon, one bold
/// line and one sentence.
pub fn empty_state(ui: &mut Ui, images: &Images, p: &Palette, icon: Icon, title: &str, text: &str) {
    let width = ui.available_width();
    let painter = ui.painter().clone();
    let title_galley = paint::layout(
        &painter,
        paint::job(
            title,
            tokens::scale::SMALL.weight(550.0),
            p.ink,
            width - 48.0,
        ),
    );
    let text_galley = paint::layout(
        &painter,
        paint::job(text, tokens::scale::SMALL, p.ink_muted, width - 48.0),
    );
    let height = 40.0 + 24.0 + 8.0 + title_galley.size().y + 8.0 + text_galley.size().y + 40.0;
    let (rect, response) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Label, true, format!("{title} {text}"))
    });
    paint::dot_grid(
        &painter.with_clip_rect(rect.shrink(1.0)),
        rect,
        18.0,
        p.dot,
        false,
    );
    // A dashed hairline.
    let outline = paint::rounded_outline(rect.shrink(0.5), [tokens::RADIUS_CARD; 4], 8);
    let mut closed = outline.clone();
    closed.push(outline[0]);
    painter.add(egui::Shape::dashed_line(
        &closed,
        egui::Stroke::new(1.0, p.line_strong),
        4.0,
        3.0,
    ));
    let mut y = rect.top() + 40.0;
    images.icon_at(
        &painter,
        pos2(rect.center().x, y + 12.0),
        24.0,
        icon,
        p.ink_faint,
    );
    y += 24.0 + 8.0;
    painter.galley(
        pos2(rect.center().x - title_galley.size().x / 2.0, y),
        title_galley.clone(),
        p.ink,
    );
    y += title_galley.size().y + 8.0;
    painter.galley(
        pos2(rect.center().x - text_galley.size().x / 2.0, y),
        text_galley,
        p.ink_muted,
    );
}

/// The window's style on the Chat page: the platform's text rendering,
/// with the web's selection, caret and thin scroll bars.
pub fn style(mut style: egui::Style, dark: bool) -> egui::Style {
    let p = Palette::new(dark);
    let v = &mut style.visuals;
    v.override_text_color = None;
    v.panel_fill = p.canvas;
    v.window_fill = p.surface_raised;
    v.extreme_bg_color = p.surface;
    v.text_edit_bg_color = Some(p.surface);
    v.selection.bg_fill = tokens::alpha(p.blue, 0.3);
    v.selection.stroke = egui::Stroke::new(1.0, p.ink);
    v.text_cursor.stroke = egui::Stroke::new(1.5, p.ink);
    // A blinking caret redraws twice a second; a steady one costs nothing.
    v.text_cursor.blink = false;
    v.hyperlink_color = p.ink;
    for (state, fill) in [
        (&mut v.widgets.inactive, p.line_strong),
        (&mut v.widgets.hovered, p.ink_faint),
        (&mut v.widgets.active, p.ink_faint),
    ] {
        state.bg_fill = fill;
        state.weak_bg_fill = fill;
        state.fg_stroke = egui::Stroke::new(1.0, p.ink);
    }
    v.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0, p.ink);
    style.spacing.scroll = egui::style::ScrollStyle {
        bar_width: 6.0,
        floating_width: 3.0,
        floating_allocated_width: 0.0,
        dormant_background_opacity: 0.0,
        active_background_opacity: 0.0,
        interact_background_opacity: 0.0,
        dormant_handle_opacity: 0.0,
        active_handle_opacity: 0.8,
        interact_handle_opacity: 1.0,
        ..egui::style::ScrollStyle::floating()
    };
    style.spacing.item_spacing = vec2(0.0, 0.0);
    style
}

#[cfg(test)]
mod tests;
