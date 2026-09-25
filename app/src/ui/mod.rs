//! The settings window: shared folders, the deny list, the activity log,
//! pairing, and general settings, plus a welcome page on first run.
//!
//! Everything goes through the daemon's control socket. Requests run on
//! short-lived threads so the window never blocks, and the window only
//! repaints when something changes: input, a finished request, or a status
//! line from the daemon.

mod account;
mod activity;
mod folders;
mod general;
mod privacy;
pub mod theme;
mod welcome;
pub mod widgets;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

use egui::{Color32, CornerRadius, Margin, RichText, Sense, Ui, Vec2};
use serde::{Deserialize, Serialize};

use crate::control::Client;
use crate::model::{AuditEntry, DenyList, Health, Platform, Status};
use crate::paths::Paths;
use theme::Theme;

/// State shared between the main loop, the watch thread, and the window.
pub struct Shared {
    pub client: Client,
    pub paths: Paths,
    /// Pairs and forgets the pairing: `cww login` and `cww logout`.
    pub account: Arc<dyn crate::pairing::Account>,
    status: Mutex<Option<Status>>,
    /// False until the first answer (or silence) from the daemon.
    known: AtomicBool,
    generation: AtomicU64,
    /// The window asks the main loop to close it (Cmd+W, Ctrl+W).
    pub close_requested: AtomicBool,
    ctx: Mutex<Option<egui::Context>>,
}

impl Shared {
    pub fn new(
        client: Client,
        paths: Paths,
        account: Arc<dyn crate::pairing::Account>,
    ) -> Arc<Self> {
        Arc::new(Self {
            client,
            paths,
            account,
            status: Mutex::new(None),
            known: AtomicBool::new(false),
            generation: AtomicU64::new(0),
            close_requested: AtomicBool::new(false),
            ctx: Mutex::new(None),
        })
    }

    pub fn set_status(&self, status: Option<Status>) {
        *self.status.lock().expect("status lock") = status;
        self.known.store(true, Ordering::SeqCst);
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.repaint();
    }

    pub fn status(&self) -> Option<Status> {
        self.status.lock().expect("status lock").clone()
    }

    pub fn is_known(&self) -> bool {
        self.known.load(Ordering::SeqCst)
    }

    fn generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    pub fn set_ctx(&self, ctx: Option<egui::Context>) {
        *self.ctx.lock().expect("ctx lock") = ctx;
    }

    /// Repaint the window, if one is open.
    pub fn repaint(&self) {
        if let Some(ctx) = &*self.ctx.lock().expect("ctx lock") {
            ctx.request_repaint();
        }
    }

    /// Bring the open window forward.
    pub fn focus(&self) {
        if let Some(ctx) = &*self.ctx.lock().expect("ctx lock") {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            ctx.request_repaint();
        }
    }
}

/// The app's own settings, next to the daemon's config.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppState {
    /// The welcome page was finished or skipped.
    pub onboarded: bool,
}

impl AppState {
    pub fn load(paths: &Paths) -> Self {
        std::fs::read(paths.app_state_file())
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, paths: &Paths) {
        let file = paths.app_state_file();
        let result = file
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| {
                std::fs::write(&file, serde_json::to_vec_pretty(self).unwrap_or_default())
            });
        if let Err(e) = result {
            log::warn!("saving {}: {e}", file.display());
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Welcome,
    Folders,
    Privacy,
    Activity,
    Account,
    General,
}

impl Page {
    const NAV: [Page; 5] = [
        Page::Folders,
        Page::Privacy,
        Page::Activity,
        Page::Account,
        Page::General,
    ];

    fn title(self) -> &'static str {
        match self {
            Page::Welcome => "Welcome",
            Page::Folders => "Shared Folders",
            Page::Privacy => "Always Private",
            Page::Activity => "Activity",
            Page::Account => "Account",
            Page::General => "General",
        }
    }

    fn icon(self) -> widgets::Icon {
        match self {
            Page::Welcome | Page::Folders => widgets::Icon::Folder,
            Page::Privacy => widgets::Icon::Lock,
            Page::Activity => widgets::Icon::Clock,
            Page::Account => widgets::Icon::Person,
            Page::General => widgets::Icon::Gear,
        }
    }
}

/// A request's result, back from its thread.
enum Done {
    Added(Result<String, String>),
    Removed(Result<(), String>),
    Labeled(Result<(), String>),
    Paused(Result<(), String>),
    Deny(Result<DenyList, String>),
    Log(Result<Vec<AuditEntry>, String>),
    Pairing(crate::pairing::PairEvent),
    LoggedOut(Result<(), String>),
    Started(Result<String, String>),
    Picked(Option<PathBuf>),
}

struct Jobs {
    tx: Sender<Done>,
    rx: Receiver<Done>,
}

impl Jobs {
    fn new() -> Self {
        let (tx, rx) = channel();
        Self { tx, rx }
    }

    /// Run `work` on its own thread and repaint when it's done.
    fn spawn(&self, ctx: &egui::Context, work: impl FnOnce() -> Done + Send + 'static) {
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("cww-request".into())
            .spawn(move || {
                let _ = tx.send(work());
                ctx.request_repaint();
            });
        if let Err(e) = spawned {
            log::error!("can't start a request thread: {e}");
        }
    }
}

enum Loadable<T> {
    NotLoaded,
    Loading,
    Loaded(T),
    Failed(String),
}

#[derive(Default)]
struct FoldersState {
    adding: bool,
    picking: bool,
    renaming: Option<(String, String)>,
    focus_rename: bool,
    confirm_remove: Option<String>,
    busy: Option<String>,
    error: Option<String>,
    note: Option<String>,
}

#[derive(Default, PartialEq, Eq, Clone, Copy)]
enum LogFilter {
    #[default]
    All,
    Refused,
}

#[derive(Default)]
struct ActivityState {
    filter: LogFilter,
    loaded_generation: Option<u64>,
    loading: bool,
    entries: Vec<AuditEntry>,
    error: Option<String>,
}

#[derive(Default)]
struct AccountState {
    requesting: bool,
    server: String,
    custom_server: bool,
    confirm_logout: bool,
    error: Option<String>,
    /// A pairing in progress: the code, once the server has given one.
    pairing: Option<Option<crate::pairing::Code>>,
    cancel: Option<crate::pairing::Cancel>,
}

#[derive(Default)]
struct GeneralState {
    autostart: bool,
    starting: bool,
    agent_error: Option<String>,
    error: Option<String>,
    pausing: bool,
}

pub struct SettingsApp {
    shared: Arc<Shared>,
    theme: Theme,
    page: Page,
    jobs: Jobs,
    app_state: AppState,
    folders: FoldersState,
    deny: Loadable<DenyList>,
    deny_generation: Option<u64>,
    activity: ActivityState,
    account: AccountState,
    general: GeneralState,
    welcome_autostart: bool,
    mark: Option<egui::TextureHandle>,
    /// The Documents folder to offer, if there is one.
    documents: Option<PathBuf>,
    /// The theme and the repaint handle are set up on the first frame.
    attached: bool,
    /// Use the platform's UI font; tests use egui's own, which never changes.
    system_fonts: bool,
}

impl SettingsApp {
    pub fn new(shared: Arc<Shared>, theme: Theme) -> Self {
        let app_state = AppState::load(&shared.paths);
        let page = if app_state.onboarded {
            Page::Folders
        } else {
            Page::Welcome
        };
        Self {
            documents: crate::paths::documents_dir(),
            attached: false,
            system_fonts: !cfg!(test),
            theme,
            page,
            jobs: Jobs::new(),
            app_state,
            folders: FoldersState::default(),
            deny: Loadable::NotLoaded,
            deny_generation: None,
            activity: ActivityState::default(),
            account: AccountState::default(),
            general: GeneralState {
                autostart: crate::autostart::is_enabled(),
                ..GeneralState::default()
            },
            welcome_autostart: true,
            mark: None,
            shared,
        }
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn page(&self) -> Page {
        self.page
    }

    pub fn shared(&self) -> &Arc<Shared> {
        &self.shared
    }

    pub fn set_page(&mut self, page: Page) {
        self.page = page;
    }

    /// Offer this folder as the Documents folder (tests use a temporary one).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn set_documents(&mut self, documents: Option<PathBuf>) {
        self.documents = documents;
    }

    /// The whole window.
    pub fn show(&mut self, ui: &mut Ui) {
        if !self.attached {
            // Fonts take effect from the next pass, so draw nothing now.
            self.attached = true;
            if self.system_fonts {
                theme::install_fonts(ui.ctx(), &self.theme.text);
            } else {
                theme::install_default_fonts(ui.ctx(), &self.theme.text);
            }
            self.theme.apply(ui.ctx());
            self.shared.set_ctx(Some(ui.ctx().clone()));
            ui.ctx().request_repaint();
            return;
        }
        self.poll_jobs(ui.ctx());
        self.follow_system_theme(ui.ctx());
        let close = egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::W);
        if ui.ctx().input_mut(|i| i.consume_shortcut(&close)) {
            self.shared.close_requested.store(true, Ordering::SeqCst);
        }
        let status = self.shared.status();
        let p = self.theme.palette;

        if self.page != Page::Welcome {
            egui::Panel::left("navigation")
                .exact_size(self.theme.metrics.sidebar_width)
                .resizable(false)
                .show_separator_line(false)
                .frame(
                    egui::Frame::new()
                        .fill(p.sidebar)
                        .inner_margin(Margin::symmetric(10, 14)),
                )
                .show(ui, |ui| self.sidebar(ui, status.as_ref()));
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(p.window))
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        egui::Frame::new()
                            .inner_margin(Margin {
                                left: 28,
                                right: 28,
                                top: 22,
                                bottom: 24,
                            })
                            .show(ui, |ui| {
                                // set_max_width can grow a Ui; only ever narrow it.
                                ui.set_max_width(ui.available_width().min(680.0));
                                self.page_contents(ui, status.as_ref());
                            });
                    });
            });
    }

    fn page_contents(&mut self, ui: &mut Ui, status: Option<&Status>) {
        match self.page {
            Page::Welcome => self.welcome_page(ui, status),
            Page::Folders => self.folders_page(ui, status),
            Page::Privacy => self.privacy_page(ui, status),
            Page::Activity => self.activity_page(ui, status),
            Page::Account => self.account_page(ui, status),
            Page::General => self.general_page(ui, status),
        }
    }

    fn sidebar(&mut self, ui: &mut Ui, status: Option<&Status>) {
        let theme = self.theme;
        let p = theme.palette;
        ui.horizontal(|ui| {
            let texture = self.mark_texture(ui.ctx());
            ui.add(egui::Image::new(&texture).fit_to_exact_size(Vec2::splat(28.0)));
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                ui.label(theme.strong("Chat with Work"));
                ui.label(theme.small("Local Agent"));
            });
        });
        ui.add_space(14.0);
        for page in Page::NAV {
            let selected = self.page == page;
            let height = theme.metrics.control_height.max(28.0);
            let (rect, response) =
                ui.allocate_exact_size(Vec2::new(ui.available_width(), height), Sense::click());
            response.widget_info(|| {
                egui::WidgetInfo::selected(
                    egui::WidgetType::SelectableLabel,
                    true,
                    selected,
                    page.title(),
                )
            });
            if response.clicked() {
                self.page = page;
            }
            let painter = ui.painter();
            let radius = CornerRadius::same(theme.metrics.radius);
            if selected {
                painter.rect_filled(rect, radius, p.nav_selected);
                if theme.platform == Platform::Windows {
                    let bar = egui::Rect::from_center_size(
                        egui::pos2(rect.left() + 1.5, rect.center().y),
                        Vec2::new(3.0, height * 0.5),
                    );
                    painter.rect_filled(bar, CornerRadius::same(2), p.accent);
                }
            } else if response.hovered() {
                painter.rect_filled(rect, radius, p.nav_selected.gamma_multiply(0.5));
            }
            let color = if selected {
                p.nav_selected_text
            } else {
                p.text
            };
            let icon_color = if selected && theme.platform == Platform::MacOs {
                p.nav_selected_text
            } else if theme.platform == Platform::Windows {
                p.text
            } else {
                p.accent
            };
            let body = egui::FontId::proportional(theme.metrics.body);
            widgets::paint_icon(
                painter,
                egui::pos2(rect.left() + 20.0, rect.center().y),
                16.0,
                page.icon(),
                icon_color,
            );
            painter.text(
                egui::pos2(rect.left() + 38.0, rect.center().y),
                egui::Align2::LEFT_CENTER,
                page.title(),
                body,
                color,
            );
            ui.add_space(2.0);
        }
        ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
            let health = Health::of(status);
            let text = if self.shared.is_known() {
                health.short_label()
            } else {
                "Checking…"
            };
            status_dot_for(ui, &theme, health, text);
        });
    }

    fn mark_texture(&mut self, ctx: &egui::Context) -> egui::TextureHandle {
        self.mark
            .get_or_insert_with(|| {
                ctx.load_texture("mark", crate::icons::mark(), egui::TextureOptions::LINEAR)
            })
            .clone()
    }

    /// Follow a switch between light and dark while the window is open.
    fn follow_system_theme(&mut self, ctx: &egui::Context) {
        if let Some(system) = ctx.system_theme() {
            let dark = system == egui::Theme::Dark;
            if dark != self.theme.dark {
                self.theme = Theme {
                    text: self.theme.text,
                    ..Theme::new(self.theme.platform, dark, crate::platform::accent_color())
                };
                self.theme.apply(ctx);
            }
        }
    }

    fn poll_jobs(&mut self, ctx: &egui::Context) {
        while let Ok(done) = self.jobs.rx.try_recv() {
            match done {
                Done::Added(result) => {
                    self.folders.adding = false;
                    match result {
                        Ok(label) => {
                            self.folders.error = None;
                            self.folders.note = Some(format!(
                                "Sharing “{label}”. Chat with Work can search it once it's indexed."
                            ));
                        }
                        Err(e) => self.folders.error = Some(e),
                    }
                }
                Done::Removed(result) | Done::Labeled(result) => {
                    self.folders.busy = None;
                    if let Err(e) = result {
                        self.folders.error = Some(e);
                    }
                }
                Done::Paused(result) => {
                    self.general.pausing = false;
                    self.general.error = result.err();
                }
                Done::Deny(result) => {
                    self.deny = match result {
                        Ok(deny) => Loadable::Loaded(deny),
                        Err(e) => Loadable::Failed(e),
                    };
                }
                Done::Log(result) => {
                    self.activity.loading = false;
                    match result {
                        Ok(mut entries) => {
                            entries.reverse();
                            self.activity.entries = entries;
                            self.activity.error = None;
                        }
                        Err(e) => self.activity.error = Some(e),
                    }
                }
                Done::Pairing(event) => {
                    use crate::pairing::PairEvent;
                    self.account.requesting = false;
                    match event {
                        // `cww login` opens the approval page itself.
                        PairEvent::Code(code) => self.account.pairing = Some(Some(code)),
                        PairEvent::Paired => {
                            self.account.pairing = None;
                            self.account.cancel = None;
                        }
                        PairEvent::Failed(e) => {
                            self.account.pairing = None;
                            self.account.cancel = None;
                            self.account.error = Some(e);
                        }
                    }
                }
                Done::LoggedOut(result) => {
                    self.account.requesting = false;
                    self.account.error = result.err();
                }
                Done::Started(result) => {
                    self.general.starting = false;
                    self.general.agent_error = result.err();
                }
                Done::Picked(path) => {
                    self.folders.picking = false;
                    if let Some(path) = path {
                        self.add_folder(ctx, path);
                    }
                }
            }
        }
    }

    // Requests. Each runs on its own thread; results come back in poll_jobs.

    pub fn add_folder(&mut self, ctx: &egui::Context, path: PathBuf) {
        self.folders.adding = true;
        self.folders.error = None;
        self.folders.note = None;
        let client = self.shared.client.clone();
        self.jobs.spawn(ctx, move || {
            Done::Added(
                client
                    .add_root(&path, None)
                    .map(|v| v["root"]["label"].as_str().unwrap_or_default().to_string())
                    .map_err(|e| e.to_string()),
            )
        });
    }

    fn pick_folder(&mut self, ctx: &egui::Context) {
        self.folders.picking = true;
        let mut dialog = rfd::AsyncFileDialog::new().set_title("Share a Folder");
        if let Some(home) = crate::paths::home_dir() {
            dialog = dialog.set_directory(home);
        }
        // Built on this thread (AppKit wants that), awaited on another.
        let picked = dialog.pick_folder();
        self.jobs.spawn(ctx, move || {
            Done::Picked(pollster::block_on(picked).map(|f| f.path().to_path_buf()))
        });
    }

    fn remove_folder(&mut self, ctx: &egui::Context, id: String) {
        self.folders.busy = Some(id.clone());
        self.folders.error = None;
        self.folders.note = None;
        let client = self.shared.client.clone();
        self.jobs.spawn(ctx, move || {
            Done::Removed(client.remove_root(&id).map_err(|e| e.to_string()))
        });
    }

    fn label_folder(&mut self, ctx: &egui::Context, id: String, label: String) {
        self.folders.busy = Some(id.clone());
        self.folders.error = None;
        let client = self.shared.client.clone();
        self.jobs.spawn(ctx, move || {
            Done::Labeled(client.label_root(&id, &label).map_err(|e| e.to_string()))
        });
    }

    fn set_paused(&mut self, ctx: &egui::Context, paused: bool) {
        self.general.pausing = true;
        let client = self.shared.client.clone();
        self.jobs.spawn(ctx, move || {
            Done::Paused(client.set_paused(paused).map_err(|e| e.to_string()))
        });
    }

    fn load_deny(&mut self, ctx: &egui::Context) {
        self.deny = Loadable::Loading;
        let client = self.shared.client.clone();
        self.jobs.spawn(ctx, move || {
            Done::Deny(client.deny().map_err(|e| e.to_string()))
        });
    }

    fn load_log(&mut self, ctx: &egui::Context) {
        self.activity.loading = true;
        let client = self.shared.client.clone();
        self.jobs.spawn(ctx, move || {
            Done::Log(client.log(300).map_err(|e| e.to_string()))
        });
    }

    fn pair(&mut self, ctx: &egui::Context) {
        self.account.requesting = true;
        self.account.error = None;
        self.account.pairing = Some(None);
        let server = self
            .account
            .custom_server
            .then(|| self.account.server.trim().to_string())
            .filter(|s| !s.is_empty());
        let (tx, ctx) = (self.jobs.tx.clone(), ctx.clone());
        let report: crate::pairing::Report = Box::new(move |event| {
            let _ = tx.send(Done::Pairing(event));
            ctx.request_repaint();
        });
        self.account.cancel = Some(self.shared.account.pair(server, report));
    }

    fn cancel_pairing(&mut self) {
        if let Some(cancel) = self.account.cancel.take() {
            cancel();
        }
        self.account.pairing = None;
        self.account.requesting = false;
    }

    fn logout(&mut self, ctx: &egui::Context) {
        self.account.requesting = true;
        self.account.confirm_logout = false;
        let account = Arc::clone(&self.shared.account);
        self.jobs
            .spawn(ctx, move || Done::LoggedOut(account.logout()));
    }

    fn start_agent(&mut self, ctx: &egui::Context) {
        self.general.starting = true;
        self.general.agent_error = None;
        self.jobs.spawn(ctx, move || {
            Done::Started(crate::agent::start().map_err(|e| format!("{e:#}")))
        });
    }

    /// Shown on pages that need the daemon while it isn't running.
    fn agent_missing(&mut self, ui: &mut Ui) {
        let theme = self.theme;
        if !self.shared.is_known() {
            ui.label(theme.weak("Checking the Local Agent…"));
            return;
        }
        widgets::group(ui, &theme, |ui| {
            ui.label(theme.strong("The Local Agent isn't running"));
            ui.add(
                egui::Label::new(theme.weak(
                    "It runs in the background and answers Chat with Work. \
                     Start it to share folders and see its activity.",
                ))
                .wrap(),
            );
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let label = if self.general.starting {
                    "Starting…"
                } else {
                    "Start Local Agent"
                };
                let button = ui.add_enabled_ui(!self.general.starting, |ui| {
                    widgets::primary_button(ui, &theme, label)
                });
                if button.inner.clicked() {
                    self.start_agent(ui.ctx());
                }
            });
            if let Some(error) = self.general.agent_error.clone() {
                widgets::notice(ui, &theme, &error, theme.palette.danger);
            }
        });
    }
}

impl eframe::App for SettingsApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        self.show(ui);
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        egui::Rgba::from(self.theme.palette.window).to_array()
    }
}

impl Drop for SettingsApp {
    fn drop(&mut self) {
        self.shared.set_ctx(None);
    }
}

pub fn status_color(theme: &Theme, health: Health) -> Color32 {
    let p = &theme.palette;
    match health {
        Health::Online => p.success,
        Health::Connecting | Health::Paused => p.warning,
        Health::Offline | Health::Revoked => p.danger,
        Health::Stopped | Health::NotPaired => p.weak,
    }
}

fn status_dot_for(ui: &mut Ui, theme: &Theme, health: Health, text: &str) {
    widgets::status_dot(
        ui,
        status_color(theme, health),
        RichText::new(text).color(theme.palette.text),
    );
}

/// Open a web page, with the same checks as `cww login` and the terminal UI.
pub fn open_url(url: &str) {
    #[cfg(test)]
    tests::OPENED.with(|opened| opened.borrow_mut().push(url.to_string()));
    #[cfg(not(test))]
    if !cww::browser::open(url) {
        log::warn!("not opening {url}");
    }
}

pub fn open_path(path: &std::path::Path) {
    if let Err(e) = open::that_detached(path) {
        log::warn!("opening {}: {e}", path.display());
    }
}

/// "1,532".
pub fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests;
