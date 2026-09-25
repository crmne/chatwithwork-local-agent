//! The main loop: one winit event loop for the tray and the settings
//! window, asleep until something happens.
//!
//! The settings window is an eframe app created when it opens and dropped
//! when it closes, so a closed window costs no memory for GL buffers or
//! fonts, and nothing redraws while it's hidden. The loop is driven with
//! `pump_app_events` so a new window can be created between iterations.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::Receiver;
use std::time::Instant;

use eframe::{EframeWinitApplication, UserEvent};
use winit::application::ApplicationHandler;
use winit::event::{StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::platform::pump_events::{EventLoopExtPumpEvents, PumpStatus};

use crate::control::Client;
use crate::events::{AppEvent, Events, Waker};
use crate::instance::{self, Instance};
use crate::model::{Status, TrayView};
use crate::paths::Paths;
use crate::tray::Tray;
use crate::ui::{AppState, Page, SettingsApp, Shared, theme};

pub struct Options {
    /// Open the settings window at start (not when started at login).
    pub open_settings: bool,
    /// Started at login: the panel may not be up yet.
    pub background: bool,
    /// Open on this page.
    pub page: Option<Page>,
    /// Save a picture of the window here and quit (for docs and tests).
    pub screenshot: Option<std::path::PathBuf>,
}

/// Our own wake-ups travel as a repaint request for a viewport that
/// doesn't exist, so they fit eframe's event type.
fn wake_viewport() -> egui::ViewportId {
    egui::ViewportId::from_hash_of("cww-app-wake")
}

fn is_wake(event: &UserEvent) -> bool {
    matches!(event, UserEvent::RequestRepaint { viewport_id, .. } if *viewport_id == wake_viewport())
}

struct Shell<'a> {
    shared: Arc<Shared>,
    events: Events,
    rx: Receiver<AppEvent>,
    status: Option<Status>,
    tray: Option<Tray>,
    tray_failed: bool,
    background: bool,
    window: Option<EframeWinitApplication<'a>>,
    pending: Option<EframeWinitApplication<'a>>,
    want_window: bool,
    start_page: Option<Page>,
    screenshot: Option<std::path::PathBuf>,
    quit: bool,
}

pub fn run(paths: Paths, options: Options) -> anyhow::Result<()> {
    let mut builder = EventLoop::<UserEvent>::with_user_event();
    #[cfg(target_os = "macos")]
    {
        // A menu bar app: no Dock icon, and no menu bar of its own.
        use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS};
        builder.with_activation_policy(ActivationPolicy::Accessory);
    }
    let mut event_loop = builder.build()?;
    let proxy = event_loop.create_proxy();
    let waker = Waker::new(move || {
        let _ = proxy.send_event(UserEvent::RequestRepaint {
            viewport_id: wake_viewport(),
            when: Instant::now(),
            cumulative_pass_nr: 0,
        });
    });
    let (events, rx) = Events::new(waker);

    if options.screenshot.is_none() {
        match instance::claim(&paths.instance_path(), !options.background) {
            Ok(Instance::Secondary) => {
                log::info!("already running; asked it to open its window");
                return Ok(());
            }
            Ok(Instance::Primary(primary)) => primary.listen(events.clone()),
            Err(e) => log::warn!("single-instance check failed: {e}"),
        }
    }

    let onboarded = AppState::load(&paths).onboarded;
    let shared = Shared::new(Client::new(paths.socket_path()), paths);
    {
        let events = events.clone();
        shared
            .client
            .spawn_watch(move |status| events.send(AppEvent::Status(status)));
    }

    let mut shell = Shell {
        shared,
        events,
        rx,
        status: None,
        tray: None,
        tray_failed: false,
        background: options.background,
        window: None,
        pending: None,
        want_window: options.open_settings || !onboarded || options.screenshot.is_some(),
        start_page: options.page,
        screenshot: options.screenshot,
        quit: false,
    };

    loop {
        if let PumpStatus::Exit(code) = event_loop.pump_app_events(None, &mut shell) {
            log::debug!("event loop exited ({code})");
            break;
        }
        if shell.quit {
            break;
        }
        if shell.want_window && shell.window.is_none() && shell.pending.is_none() {
            shell.want_window = false;
            shell.pending = Some(create_window(
                &event_loop,
                Arc::clone(&shell.shared),
                shell.start_page.take(),
                shell.screenshot.clone(),
            ));
            // Come back around at once so new_events can create it.
            shell.events.waker().wake();
        }
        let nothing_left = shell.window.is_none() && shell.pending.is_none() && !shell.want_window;
        if nothing_left && shell.tray.is_none() && shell.tray_failed {
            // Without a tray there is no way back to a closed window.
            break;
        }
    }
    Ok(())
}

fn create_window<'a>(
    event_loop: &EventLoop<UserEvent>,
    shared: Arc<Shared>,
    page: Option<Page>,
    screenshot: Option<std::path::PathBuf>,
) -> EframeWinitApplication<'a> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Chat with Work Local Agent")
            .with_app_id("cww-app")
            .with_inner_size([840.0, 620.0])
            .with_min_inner_size([660.0, 460.0])
            .with_icon(crate::icons::window()),
        centered: true,
        ..Default::default()
    };
    eframe::create_native(
        "Chat with Work Local Agent",
        options,
        Box::new(move |cc| {
            let theme = theme::Theme::detect(&cc.egui_ctx);
            let mut app = SettingsApp::new(shared, theme);
            if let Some(page) = page {
                app.set_page(page);
            }
            if let Some(path) = screenshot {
                return Ok(Box::new(crate::screenshot::Capture::new(app, path)));
            }
            Ok(Box::new(app))
        }),
        event_loop,
    )
}

impl Shell<'_> {
    fn init_tray(&mut self) {
        if self.screenshot.is_some() {
            return;
        }
        let view = TrayView::new(self.status.as_ref());
        self.tray = Tray::new(self.events.clone(), &view, self.background);
        if self.tray.is_none() {
            self.tray_failed = true;
            // The window is the only way in.
            self.want_window = self.window.is_none() && self.pending.is_none();
        }
    }

    fn drain(&mut self, event_loop: &ActiveEventLoop) {
        while let Ok(event) = self.rx.try_recv() {
            match event {
                AppEvent::Status(status) => {
                    self.shared.set_status(status.clone());
                    if let Some(tray) = &mut self.tray {
                        tray.update(&TrayView::new(status.as_ref()));
                    }
                    self.status = status;
                }
                AppEvent::OpenSettings => {
                    if self.window.is_some() {
                        self.shared.focus();
                        crate::platform::activate();
                    } else {
                        self.want_window = true;
                    }
                }
                AppEvent::TogglePause => {
                    if let Some(status) = &self.status {
                        let paused = !status.paused;
                        let client = self.shared.client.clone();
                        std::thread::spawn(move || {
                            if let Err(e) = client.set_paused(paused) {
                                log::warn!("pausing: {e}");
                            }
                        });
                    }
                }
                AppEvent::Quit => {
                    self.close_window(event_loop);
                    self.quit = true;
                    event_loop.exit();
                }
            }
        }
    }

    fn close_window(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(mut window) = self.window.take() {
            // Destroys the window and the GL context while the loop runs.
            window.exiting(event_loop);
            drop(window);
            release_memory();
        }
        event_loop.set_control_flow(ControlFlow::Wait);
        if self.tray.is_none() || self.screenshot.is_some() {
            self.quit = true;
            event_loop.exit();
        }
    }

    fn after_window_event(&mut self, event_loop: &ActiveEventLoop) {
        if self.shared.close_requested.swap(false, Ordering::SeqCst) {
            self.close_window(event_loop);
        }
    }
}

/// Give the memory the window used back to the system. glibc keeps freed
/// heap for reuse otherwise, and the app may sit in the tray for weeks.
fn release_memory() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        unsafe extern "C" {
            fn malloc_trim(pad: usize) -> i32;
        }
        // SAFETY: malloc_trim only returns free heap pages to the kernel.
        unsafe { malloc_trim(0) };
    }
}

impl ApplicationHandler<UserEvent> for Shell<'_> {
    fn new_events(&mut self, event_loop: &ActiveEventLoop, cause: StartCause) {
        if cause == StartCause::Init {
            // macOS wants the status item made once the app is running.
            self.init_tray();
        }
        if let Some(mut window) = self.pending.take() {
            window.resumed(event_loop);
            self.window = Some(window);
            crate::platform::activate();
        }
        if let Some(window) = &mut self.window {
            window.new_events(event_loop, cause);
        }
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(window) = &mut self.window {
            window.resumed(event_loop);
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        if is_wake(&event) {
            self.drain(event_loop);
            return;
        }
        if let Some(window) = &mut self.window {
            window.user_event(event_loop, event);
        }
        self.after_window_event(event_loop);
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: winit::window::WindowId,
        event: WindowEvent,
    ) {
        if matches!(event, WindowEvent::CloseRequested) {
            self.close_window(event_loop);
            return;
        }
        if let Some(window) = &mut self.window {
            window.window_event(event_loop, window_id, event);
        }
        self.after_window_event(event_loop);
    }

    fn device_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        device_id: winit::event::DeviceId,
        event: winit::event::DeviceEvent,
    ) {
        if let Some(window) = &mut self.window {
            window.device_event(event_loop, device_id, event);
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        match &mut self.window {
            Some(window) => window.about_to_wait(event_loop),
            None => event_loop.set_control_flow(ControlFlow::Wait),
        }
        self.after_window_event(event_loop);
    }

    fn suspended(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(window) = &mut self.window {
            window.suspended(event_loop);
        }
    }

    fn exiting(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(window) = &mut self.window {
            window.exiting(event_loop);
        }
    }
}
