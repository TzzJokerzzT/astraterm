//! The application: event loop, command dispatch and subsystem wiring.
//!
//! This is the only layer that knows about every other subsystem. The event loop
//! stays event-driven: PTY threads wake it through an `EventLoopProxy`, redraws
//! happen only when something changed, and the only timers are the cursor blink and
//! the configuration-watcher debounce.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use astraterm_config::keys::{ActionId, Keymap};
use astraterm_config::schema::Config;
use astraterm_config::service::{ConfigService, ConfigWatcher, ReloadOutcome, SaveError};
use astraterm_config::themes::{Theme, available_themes};
use astraterm_core::events::{AppCommand, ExitReason, SessionEvent};
use astraterm_core::{PaneId, ProfileId, SessionId, Size, SplitAxis, TabId, WindowId};
use astraterm_platform::clipboard::{Clipboard, Selection as ClipboardSelection};
use astraterm_platform::linux;
use astraterm_platform::paths::StatePaths;
use astraterm_platform::system::SystemMonitor;
use astraterm_settings_ui::panel::{PanelAction, PanelKey, SettingsPanel};
use astraterm_terminal::input::{
    self, KeyEventType, MouseEvent, MouseKind, encode_key, encode_mouse,
};
use astraterm_terminal::snapshot::ModeFlags;
use astraterm_workspace::snapshot::{WorkspaceSnapshot, restore_window, snapshot_window};
use astraterm_workspace::{Pane, Tab, WindowState};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoopProxy};
use winit::keyboard::ModifiersState;
use winit::window::{Window, WindowId as WinitWindowId};

use crate::cli::Cli;
use crate::keys;
use crate::paste;
use crate::prefs::{FONT_SIZE_STEP, Preferences, next_font_size};
use crate::ui::{
    ClickInfo, ConfirmDialog, DragState, ExitCleanup, Overlay, PendingAction, RenameOverlay,
    SearchOverlay, SessionImages, SplitIdRef, WindowUi, plan_exit_cleanup, url_at,
};

/// Events the application sends itself from other threads.
#[derive(Clone, Debug)]
pub enum UserEvent {
    /// New terminal output or an exit status is available.
    Wake,
    /// The configuration file changed on disk.
    ConfigChanged,
    /// A clipboard read finished off the event loop and is ready to send.
    PasteReady {
        window: WinitWindowId,
        pane: PaneId,
        result: Result<String, String>,
    },
}

/// Application state shared by all windows.
pub struct Application {
    config: ConfigService,
    keymap: Keymap,
    clipboard: Clipboard,
    sessions: astraterm_session::SessionManager,
    windows: BTreeMap<WinitWindowId, WindowUi>,
    focused: Option<WinitWindowId>,
    proxy: EventLoopProxy<UserEvent>,
    cli: Cli,
    mods: ModifiersState,
    /// Next key is delivered verbatim instead of being interpreted as a shortcut.
    literal_next: bool,
    state_paths: StatePaths,
    _config_watcher: Option<ConfigWatcher>,
    watcher_deadline: Option<Instant>,
    font_size_override: Option<f32>,
    quitting: bool,
    started: bool,
    /// Last known pointer position, in device pixels.
    cursor: Option<(f64, f64)>,
    /// When the cursor blink should toggle next.
    blink_deadline: Option<Instant>,
    /// CPU/RAM/clock readings for the status bar, sampled at most once per second.
    system: SystemMonitor,
    /// When the status bar readings should be refreshed next.
    system_deadline: Option<Instant>,
    /// Decoded images of each session, anchored to buffer lines. Only filled when
    /// `terminal.images` is on; dropped when the session closes.
    images: BTreeMap<SessionId, SessionImages>,
}

impl Application {
    /// Builds the application from the CLI.
    pub fn new(cli: Cli, proxy: EventLoopProxy<UserEvent>) -> anyhow::Result<Self> {
        let config_path = cli
            .config
            .clone()
            .unwrap_or_else(astraterm_config::default_config_path);
        // Make the first run discoverable: write a documented default file.
        if cli.config.is_none()
            && let Err(error) = astraterm_config::defaults::ensure_config_file(&config_path)
        {
            tracing::warn!(error = %error, "could not write the default configuration");
        }

        let service = ConfigService::load(&config_path);
        if let Some(error) = service.load_error() {
            tracing::error!(error = %error, "configuration could not be parsed; using defaults");
        }
        for issue in service.issues() {
            tracing::warn!("{}", issue.describe());
        }

        let keymap = Keymap::build(&service.config().keys);

        let (watcher_tx, watcher_rx) = crossbeam_channel::unbounded();
        let config_watcher = match ConfigWatcher::watch(&config_path, watcher_tx) {
            Ok(watcher) => Some(watcher),
            Err(error) => {
                tracing::warn!(error = %error, "configuration file watching is unavailable");
                None
            }
        };

        {
            let rx = watcher_rx;
            let proxy = proxy.clone();
            if let Err(error) = std::thread::Builder::new()
                .name("astraterm-config-watch".to_owned())
                .spawn(move || {
                    while rx.recv().is_ok() {
                        if proxy.send_event(UserEvent::ConfigChanged).is_err() {
                            break;
                        }
                    }
                })
            {
                tracing::warn!(error = %error, "configuration watcher thread could not start");
            }
        }

        let state_paths = StatePaths::resolve();
        if let Err(error) = state_paths.ensure() {
            tracing::warn!(error = %error, "could not create the state directory");
        }
        let preferences = Preferences::load(&state_paths.preferences_file());

        Ok(Self {
            config: service,
            keymap,
            clipboard: Clipboard::detect(),
            sessions: astraterm_session::SessionManager::new(),
            windows: BTreeMap::new(),
            focused: None,
            proxy,
            cli,
            mods: ModifiersState::empty(),
            literal_next: false,
            state_paths,
            _config_watcher: config_watcher,
            watcher_deadline: None,
            font_size_override: preferences.sanitized_font_size(),
            quitting: false,
            started: false,
            cursor: None,
            blink_deadline: None,
            system: SystemMonitor::new(),
            system_deadline: None,
            images: BTreeMap::new(),
        })
    }

    fn theme(&self) -> Theme {
        astraterm_config::themes::resolve_theme(
            &self.config.config().appearance.theme,
            self.config.themes_dir(),
        )
    }

    // --- Window creation -----------------------------------------------------

    fn create_window(&mut self, event_loop: &ActiveEventLoop, restore: bool) -> anyhow::Result<()> {
        let config = self.effective_config();
        let attributes = PlatformName::apply(Window::default_attributes())
            .with_title("AstraTerm")
            .with_inner_size(LogicalSize::new(
                config.window.width as f64,
                config.window.height as f64,
            ))
            .with_maximized(config.window.startup_maximized)
            // Always translucent: the surface alpha mode is fixed at creation, and the
            // painted alpha (from `[window] opacity`) decides how much shows through,
            // so the settings panel can change opacity without a new window.
            .with_transparent(true)
            .with_decorations(config.window.show_window_controls);
        let window = Arc::new(event_loop.create_window(attributes)?);
        // IME preedit is not implemented yet; text input arrives through key events.
        window.set_ime_allowed(false);
        // The icon comes from the same logo the desktop entry installs, and the app
        // identity matches `StartupWMClass`/`Icon` so the launcher and the window
        // switcher show it instead of a generic placeholder.
        window.set_window_icon(window_icon());

        let renderer = astraterm_renderer::Renderer::new(
            window.clone(),
            event_loop.owned_display_handle(),
            astraterm_renderer::RendererOptions {
                font: config.font.clone(),
                scale: window.scale_factor() as f32,
                transparent: true,
                low_power: config.performance.low_power_mode,
            },
        )?;
        let metrics = renderer.cell_metrics();

        // A profile named on the command line applies to the first window.
        let cli_profile = self
            .cli
            .profile
            .as_deref()
            .and_then(|name| {
                config
                    .profiles
                    .iter()
                    .position(|profile| profile.name == name)
            })
            .map(|index| ProfileId::from_raw(index as u64 + 1));
        if self.cli.profile.is_some() && cli_profile.is_none() {
            tracing::warn!(
                profile = ?self.cli.profile,
                "the requested profile does not exist in the configuration"
            );
        }
        let (workspace, panes) = if restore {
            self.restored_workspace(&config, metrics)
        } else {
            self.fresh_workspace(&config, metrics, cli_profile)
        };

        let mut ui = WindowUi::new(
            window,
            renderer,
            workspace,
            panes,
            self.theme(),
            config,
            metrics,
        );
        for pane in ui.panes.keys().copied().collect::<Vec<_>>() {
            if let Some(session) = self.sessions.for_pane(pane) {
                ui.session_bindings.insert(pane, session.id);
            }
        }
        let winit_id = ui.window.id();
        self.focused = Some(winit_id);
        self.windows.insert(winit_id, ui);
        self.sync_sizes(winit_id);
        if let Some(ui) = self.windows.get(&winit_id) {
            ui.window.request_redraw();
        }
        Ok(())
    }

    /// Creates a workspace with a single tab and one session.
    fn fresh_workspace(
        &mut self,
        config: &Config,
        metrics: astraterm_core::CellMetrics,
        profile: Option<ProfileId>,
    ) -> (WindowState, BTreeMap<PaneId, Pane>) {
        let pane = PaneId::next();
        let profile_name = profile
            .and_then(|id| config.profiles.get(id.raw().saturating_sub(1) as usize))
            .map(|profile| profile.name.clone());
        let profile_ref = profile_name
            .as_deref()
            .and_then(|name| config.profiles.iter().find(|profile| profile.name == name));
        let cell = (
            metrics.width.round().max(1.0) as u16,
            metrics.height.max(1) as u16,
        );
        let mut spec = astraterm_session::session_spec(
            config,
            profile_ref,
            profile,
            self.cli.working_directory.clone(),
            80,
            24,
            cell,
        );
        if !self.cli.command.is_empty() {
            // A CLI command is an argument vector, never a shell string.
            spec.pty.program = self.cli.command[0].clone();
            spec.pty.args = self.cli.command[1..].to_vec();
        }
        let session = self.sessions.create(pane, &spec, 80, 24);

        let mut meta = Pane::new(pane);
        meta.profile = profile;
        meta.cwd = spec.pty.cwd.clone();
        if let Some(title) = &self.cli.title {
            meta.title = title.clone();
        } else if let Some(label) = spec.label.clone() {
            meta.title = label;
        }
        if let Err(error) = &session {
            tracing::error!(error = %error, "could not start the shell; the pane shows an empty session");
            meta.title = format!("failed: {error}");
        }

        let mut panes = BTreeMap::new();
        panes.insert(pane, meta);
        let mut tab = Tab::new(String::new(), pane);
        if let Some(title) = &self.cli.title {
            tab.title = title.clone();
        }
        (WindowState::new(WindowId::next(), tab), panes)
    }

    /// Rebuilds the persisted layout, recreating sessions (never processes).
    fn restored_workspace(
        &mut self,
        config: &Config,
        metrics: astraterm_core::CellMetrics,
    ) -> (WindowState, BTreeMap<PaneId, Pane>) {
        if !config.workspace.restore_on_startup {
            return self.fresh_workspace(config, metrics, None);
        }
        let path = self.state_paths.workspace_file();
        let Ok(text) = std::fs::read_to_string(&path) else {
            return self.fresh_workspace(config, metrics, None);
        };
        let Ok(snapshot) = toml::from_str::<WorkspaceSnapshot>(&text) else {
            tracing::warn!(path = %path.display(), "workspace metadata is invalid; starting fresh");
            return self.fresh_workspace(config, metrics, None);
        };
        let Some(window_snapshot) = snapshot.windows.first() else {
            return self.fresh_workspace(config, metrics, None);
        };
        let Some(restored) = restore_window(window_snapshot, PaneId::next) else {
            return self.fresh_workspace(config, metrics, None);
        };

        let mut panes = BTreeMap::new();
        for (pane, meta) in &restored.panes {
            let profile_id = meta
                .profile
                .as_deref()
                .and_then(|name| {
                    config
                        .profiles
                        .iter()
                        .position(|profile| profile.name == name)
                })
                .map(|index| ProfileId::from_raw(index as u64 + 1));
            let profile_ref =
                profile_id.and_then(|id| config.profiles.get(id.raw().saturating_sub(1) as usize));
            let cell = (
                metrics.width.round().max(1.0) as u16,
                metrics.height.max(1) as u16,
            );
            let mut spec = astraterm_session::session_spec(
                config,
                profile_ref,
                profile_id,
                meta.cwd.clone(),
                80,
                24,
                cell,
            );
            if !self.cli.command.is_empty() {
                spec.pty.program = self.cli.command[0].clone();
                spec.pty.args = self.cli.command[1..].to_vec();
            }
            if let Err(error) = self.sessions.create(*pane, &spec, 80, 24) {
                tracing::warn!(error = %error, "could not recreate a session");
            }
            let mut pane_meta = Pane::new(*pane);
            pane_meta.cwd = meta.cwd.clone();
            pane_meta.profile = profile_id;
            pane_meta.title = meta.label.clone().unwrap_or_default();
            panes.insert(*pane, pane_meta);
        }

        let mut workspace = WindowState {
            id: restored.window_id,
            tabs: restored.tabs,
            active_tab: TabId::from_raw(0),
        };
        if let Some(tab) = workspace.tabs.get(restored.active_index) {
            workspace.active_tab = tab.id;
        }
        (workspace, panes)
    }

    // --- Session plumbing ----------------------------------------------------

    /// Pumps output for every session and handles the resulting events.
    fn pump(&mut self) {
        let report = self.sessions.pump();
        for text in report.clipboard_stores {
            // OSC 52 is disabled by default; this is the only clipboard write path.
            let _ = self.clipboard.set(ClipboardSelection::Clipboard, &text);
        }
        for (session, bytes) in report.replies {
            self.sessions.reply(session, &bytes);
        }
        for event in self.sessions.drain_events() {
            self.handle_session_event(event);
        }
        self.intake_images();
        if report.bytes_processed > 0 || report.exits > 0 {
            for ui in self.windows.values() {
                ui.window.request_redraw();
            }
        }
    }

    /// Drains the image sequences every session captured in the last pump.
    ///
    /// This is the only path from untrusted bytes to the GPU, so it is gated on
    /// `terminal.images`: with the option off the payloads are drained and dropped
    /// without being decoded. A refused payload is a dropped image, never a panic.
    fn intake_images(&mut self) {
        let enabled = self.config.config().terminal.images;
        for session in self.sessions.iter_mut() {
            let pane = session.pane;
            let id = session.id;
            // The cell metrics of the pane decide how many cells an image covers;
            // a session whose window is already gone falls back to the defaults.
            let metrics = self
                .windows
                .values()
                .find(|ui| ui.panes.contains_key(&pane))
                .map(|ui| ui.metrics)
                .unwrap_or_default();
            let top_line = session.engine.line_range().0;
            let images = self
                .images
                .entry(id)
                .or_insert_with(|| SessionImages::new(id, top_line));
            images.intake(&mut session.engine, metrics, enabled);
        }
        // A closed session must not keep its decoded pixels alive. Session ids are
        // never reused, so a stale entry can never become live again. The map is
        // only walked when it holds something, which is never the case while the
        // option is off.
        if !self.images.is_empty() {
            let live: Vec<SessionId> = self.sessions.iter().map(|session| session.id).collect();
            self.images.retain(|id, _| live.contains(id));
        }
    }

    fn handle_session_event(&mut self, event: SessionEvent) {
        match event {
            SessionEvent::Output { session } => {
                if let Some(session) = self.sessions.get_mut(session) {
                    session.refresh_cwd();
                }
            }
            SessionEvent::Title { session, title } => {
                if let Some(session) = self.sessions.get_mut(session) {
                    session.title = title.clone();
                }
                self.refresh_titles();
            }
            SessionEvent::Bell { session } => {
                let label = self
                    .sessions
                    .get_mut(session)
                    .map(|session| {
                        session.bell = true;
                        session
                            .title
                            .clone()
                            .or_else(|| session.label.clone())
                            .unwrap_or_default()
                    })
                    .unwrap_or_default();
                for ui in self.windows.values_mut() {
                    ui.status = Some((format!("bell: {label}"), Instant::now()));
                    ui.window.request_redraw();
                }
            }
            SessionEvent::ColorQuery { session, index } => {
                let theme = self.theme();
                let rgb = match index {
                    256 => theme.palette.foreground,
                    257 => theme.palette.background,
                    index => theme.palette.ansi_color(index as u8),
                };
                let bytes = astraterm_terminal::TerminalEngine::encode_color_reply(
                    index,
                    (rgb.r, rgb.g, rgb.b),
                );
                self.sessions.reply(session, &bytes);
            }
            SessionEvent::Exited { session, reason } => self.handle_exit(session, reason),
        }
    }

    fn handle_exit(&mut self, session_id: SessionId, reason: ExitReason) {
        let close_pane = self.config.config().workspace.close_pane_when_shell_exits;
        let Some(pane) = self.sessions.get(session_id).map(|session| session.pane) else {
            return;
        };
        let description = reason.describe();

        if close_pane {
            let mut changed = Vec::new();
            let mut kept_open = false;
            let mut remove_session = false;
            for window_id in self.windows.keys().copied().collect::<Vec<_>>() {
                let plan = self
                    .windows
                    .get(&window_id)
                    .map(|ui| plan_exit_cleanup(&ui.workspace, pane));
                match plan {
                    Some(ExitCleanup::ClosePane(pane)) => {
                        let closed = self
                            .with_window(window_id, |ui| {
                                let closed = ui
                                    .workspace
                                    .active_tab_mut()
                                    .map(|tab| tab.close_pane(pane))
                                    .unwrap_or(false);
                                if closed {
                                    ui.panes.remove(&pane);
                                    ui.session_bindings.remove(&pane);
                                    ui.window.request_redraw();
                                }
                                closed
                            })
                            .unwrap_or(false);
                        if closed {
                            changed.push(window_id);
                            remove_session = true;
                        }
                    }
                    Some(ExitCleanup::CloseTab(tab)) => {
                        let closed = self
                            .with_window(window_id, |ui| {
                                let panes = ui
                                    .workspace
                                    .tab(tab)
                                    .map(|tab| tab.panes())
                                    .unwrap_or_default();
                                let closed = ui.workspace.close_tab(tab);
                                if closed {
                                    for pane in panes {
                                        ui.panes.remove(&pane);
                                        ui.session_bindings.remove(&pane);
                                    }
                                    ui.window.request_redraw();
                                }
                                closed
                            })
                            .unwrap_or(false);
                        if closed {
                            changed.push(window_id);
                            remove_session = true;
                        }
                    }
                    Some(ExitCleanup::KeepOpen) | None => {
                        // The last pane of the last tab keeps its session and metadata so
                        // the final screen stays readable; nothing is stripped.
                        kept_open = true;
                    }
                }
            }
            for window_id in changed {
                self.sync_sizes(window_id);
                self.with_window(window_id, |ui| {
                    ui.status = Some((format!("session {description}"), Instant::now()));
                });
            }
            if kept_open {
                for ui in self.windows.values_mut() {
                    if ui.panes.contains_key(&pane) {
                        ui.status = Some((
                            format!("session {description} — close the tab to dismiss"),
                            Instant::now(),
                        ));
                        ui.window.request_redraw();
                    }
                }
            }
            if remove_session {
                self.sessions.close(session_id);
            }
            self.persist_workspace();
        }
    }

    fn refresh_titles(&mut self) {
        let titles: Vec<(WinitWindowId, String)> = self
            .windows
            .values()
            .filter_map(|ui| {
                let pane = ui.focused_pane()?;
                let session = self.sessions.for_pane(pane)?;
                let label = session
                    .title
                    .clone()
                    .or_else(|| session.label.clone())
                    .unwrap_or_else(|| "AstraTerm".to_owned());
                Some((ui.window.id(), format!("{label} — AstraTerm")))
            })
            .collect();
        for (window_id, title) in titles {
            if let Some(ui) = self.windows.get(&window_id) {
                ui.window.set_title(&title);
            }
        }
    }

    /// Pushes the grid size of every visible pane to its PTY.
    fn sync_sizes(&mut self, window_id: WinitWindowId) {
        let Some((sizes, cell)) = self.windows.get(&window_id).map(|ui| {
            let sizes: Vec<(PaneId, (u16, u16))> = ui
                .pane_rects()
                .into_iter()
                .map(|(pane, rect)| (pane, ui.grid_size(rect)))
                .collect();
            let cell = (
                ui.metrics.width.round().max(1.0) as u16,
                ui.metrics.height.max(1) as u16,
            );
            (sizes, cell)
        }) else {
            return;
        };
        let mut resized = false;
        let mut resized_grids: Vec<(SessionId, u16, u16)> = Vec::new();
        for (pane, (cols, rows)) in sizes {
            if let Some(session) = self.sessions.for_pane_mut(pane)
                && session.engine.size() != (cols, rows)
            {
                session.resize(cols, rows, cell.0, cell.1);
                resized_grids.push((session.id, cols, rows));
                resized = true;
            }
        }
        // A resize reflows the buffer, and an image anchor is a bare line number:
        // nothing can be re-anchored, so the anchors the new grid cannot show are
        // discarded instead of being drawn on the wrong line.
        for (session, cols, rows) in resized_grids {
            if let Some(images) = self.images.get_mut(&session) {
                images.drop_outside(rows, cols);
            }
        }
        if resized && let Some(ui) = self.windows.get(&window_id) {
            ui.window.request_redraw();
        }
    }

    fn sync_all_sizes(&mut self) {
        for id in self.windows.keys().copied().collect::<Vec<_>>() {
            self.sync_sizes(id);
        }
    }

    /// Runs `f` with mutable access to one window, without holding the borrow while
    /// other subsystems are touched.
    fn with_window<R>(
        &mut self,
        window_id: WinitWindowId,
        f: impl FnOnce(&mut WindowUi) -> R,
    ) -> Option<R> {
        self.windows.get_mut(&window_id).map(f)
    }

    fn active_context(&self) -> Option<(WinitWindowId, Option<PaneId>, TabId)> {
        let window_id = self.focused?;
        let ui = self.windows.get(&window_id)?;
        Some((window_id, ui.focused_pane(), ui.workspace.active_tab))
    }

    // --- Commands ------------------------------------------------------------

    /// Dispatches one application command.
    pub fn dispatch(&mut self, event_loop: &ActiveEventLoop, command: AppCommand) {
        let Some((window_id, focused_pane, active_tab)) = self.active_context() else {
            return;
        };

        match command {
            AppCommand::Quit => self.request_quit(event_loop, PendingAction::Quit),
            AppCommand::CloseWindow { .. } => self.request_close_window(event_loop, window_id),
            AppCommand::NewWindow { .. } => {
                if let Err(error) = self.create_window(event_loop, false) {
                    tracing::error!(error = %error, "could not open a new window");
                }
            }
            AppCommand::NewTab { profile } => {
                if let Err(error) = self.open_tab(window_id, profile) {
                    tracing::error!(error = %error, "could not create a tab");
                }
            }
            AppCommand::CloseTab { .. } => self.request_close_tab(window_id, active_tab),
            AppCommand::ClosePane { .. } => {
                if let Some(pane) = focused_pane {
                    self.request_close_pane(window_id, pane);
                }
            }
            AppCommand::SplitPane { axis, .. } => {
                tracing::debug!(?axis, ?focused_pane, "split requested");
                match focused_pane {
                    Some(pane) => {
                        if let Err(error) = self.split_pane(window_id, pane, axis) {
                            tracing::error!(error = %error, "could not split the pane");
                        }
                    }
                    None => tracing::debug!("no focused pane to split"),
                }
            }
            AppCommand::FocusPane { pane } => {
                self.with_window(window_id, |ui| {
                    if let Some(tab) = ui.workspace.active_tab_mut() {
                        tab.focus(pane);
                    }
                    ui.window.request_redraw();
                });
            }
            AppCommand::FocusDirection { direction } => {
                self.with_window(window_id, |ui| {
                    let rect = ui.terminal_rect();
                    let layout = ui.layout;
                    if let Some(tab) = ui.workspace.active_tab_mut() {
                        tab.focus_direction(direction, rect, layout);
                    }
                    ui.window.request_redraw();
                });
            }
            AppCommand::NextTab => {
                self.with_window(window_id, |ui| ui.workspace.next_tab());
                self.after_tab_change(window_id);
            }
            AppCommand::PreviousTab => {
                self.with_window(window_id, |ui| ui.workspace.previous_tab());
                self.after_tab_change(window_id);
            }
            AppCommand::SelectTab { index } => {
                self.with_window(window_id, |ui| ui.workspace.select_index(index));
                self.after_tab_change(window_id);
            }
            AppCommand::MoveTab { from, to } => {
                self.with_window(window_id, |ui| {
                    ui.workspace.move_tab(from, to);
                    ui.window.request_redraw();
                });
                self.persist_workspace();
            }
            AppCommand::RenameTab { tab, title } => {
                // An empty title means "ask the user"; a non-empty one is a direct set.
                if title.is_empty() {
                    let current = self
                        .windows
                        .get(&window_id)
                        .and_then(|ui| ui.workspace.tab(tab))
                        .map(|tab| tab.title.clone())
                        .unwrap_or_default();
                    self.with_window(window_id, |ui| {
                        ui.overlay = Overlay::Rename(RenameOverlay::new(tab, &current));
                        ui.window.request_redraw();
                    });
                } else {
                    self.with_window(window_id, |ui| {
                        if let Some(tab) = ui.workspace.tab_mut(tab) {
                            tab.title = title;
                        }
                        ui.window.request_redraw();
                    });
                }
            }
            AppCommand::MoveTabRelative { delta } => {
                let moved = self
                    .with_window(window_id, |ui| {
                        let Some(from) = ui.workspace.active_index() else {
                            return false;
                        };
                        let count = ui.workspace.tabs.len() as i32;
                        let to = (from as i32 + delta).clamp(0, count - 1) as usize;
                        ui.workspace.move_tab(from, to)
                    })
                    .unwrap_or(false);
                if moved {
                    self.with_window(window_id, |ui| ui.window.request_redraw());
                    self.persist_workspace();
                }
            }
            AppCommand::ResizeDivider {
                axis,
                delta_percent,
            } => {
                let split = self.focused_split(window_id, axis);
                if let Some(split) = split {
                    self.with_window(window_id, |ui| {
                        if let Some(tab) = ui.workspace.active_tab_mut() {
                            tab.root.adjust_ratio(split, delta_percent as f32 / 100.0);
                        }
                    });
                    self.sync_sizes(window_id);
                    self.with_window(window_id, |ui| ui.window.request_redraw());
                }
            }
            AppCommand::EqualizeSplits => {
                self.with_window(window_id, |ui| {
                    if let Some(tab) = ui.workspace.active_tab_mut() {
                        tab.root.equalize();
                    }
                });
                self.sync_sizes(window_id);
                self.with_window(window_id, |ui| ui.window.request_redraw());
            }
            AppCommand::ToggleZoom => {
                self.with_window(window_id, |ui| {
                    if let Some(tab) = ui.workspace.active_tab_mut() {
                        tab.toggle_zoom();
                    }
                });
                self.sync_sizes(window_id);
                self.with_window(window_id, |ui| ui.window.request_redraw());
            }
            AppCommand::CopySelection => self.copy_selection(window_id),
            AppCommand::Paste => self.paste(window_id),
            AppCommand::Search { .. } => {
                self.with_window(window_id, |ui| {
                    ui.overlay = Overlay::Search(SearchOverlay::default());
                    ui.window.request_redraw();
                });
            }
            AppCommand::SearchNext => self.step_search(window_id, true),
            AppCommand::SearchPrevious => self.step_search(window_id, false),
            AppCommand::CloseSearch => {
                self.with_window(window_id, |ui| {
                    ui.overlay = Overlay::None;
                    ui.window.request_redraw();
                });
            }
            AppCommand::ScrollLines { delta } => {
                if let Some(pane) = focused_pane
                    && let Some(session) = self.sessions.for_pane_mut(pane)
                {
                    let rows = session.engine.size().1 as i32;
                    let delta = match delta {
                        i32::MAX => rows,
                        i32::MIN => -rows,
                        other => other,
                    };
                    session.engine.scroll_display(delta);
                }
                self.with_window(window_id, |ui| ui.window.request_redraw());
            }
            AppCommand::ScrollToBottom => {
                if let Some(pane) = focused_pane
                    && let Some(session) = self.sessions.for_pane_mut(pane)
                {
                    session.engine.scroll_to(true);
                }
                self.with_window(window_id, |ui| ui.window.request_redraw());
            }
            AppCommand::IncreaseFontSize => self.change_font_size(FONT_SIZE_STEP),
            AppCommand::DecreaseFontSize => self.change_font_size(-FONT_SIZE_STEP),
            AppCommand::ResetFontSize => {
                self.font_size_override = None;
                self.apply_config();
                self.persist_preferences();
            }
            AppCommand::OpenSettings => self.open_settings(window_id),
            AppCommand::ReloadConfig => self.reload_config(),
            AppCommand::ToggleStatusBar => {
                let value = !self.config.config().appearance.show_status_bar;
                if let Err(error) = self.config.update(|config| {
                    config.appearance.show_status_bar = value;
                }) {
                    tracing::warn!(error = %error, "could not persist the status bar setting");
                }
                self.apply_config();
            }
            AppCommand::ConfirmClose => {
                self.with_window(window_id, |ui| {
                    ui.overlay = Overlay::None;
                    ui.window.request_redraw();
                });
            }
            AppCommand::SendLiteralNext => {
                self.literal_next = true;
                self.with_window(window_id, |ui| {
                    ui.set_status("next key goes to the terminal");
                });
            }
        }
    }

    fn focused_split(
        &self,
        window_id: WinitWindowId,
        axis: SplitAxis,
    ) -> Option<astraterm_workspace::tree::SplitId> {
        let ui = self.windows.get(&window_id)?;
        let tab = ui.workspace.active_tab()?;
        let rect = ui.terminal_rect();
        let dividers = tab.dividers(rect, ui.layout);
        let pane = ui.focused_pane();
        let matching: Vec<_> = dividers
            .into_iter()
            .filter(|(_, divider_axis, _)| *divider_axis == axis)
            .collect();
        // Prefer a divider that touches the focused pane, otherwise use the first.
        matching
            .iter()
            .find(|(_, _, divider)| {
                pane.and_then(|pane| tab.pane_rect(pane, rect, ui.layout))
                    .map(|pane_rect| {
                        (pane_rect.bottom() - divider.y).abs() <= 2
                            || (pane_rect.y - divider.y).abs() <= 2
                            || (pane_rect.right() - divider.x).abs() <= 2
                            || (pane_rect.x - divider.x).abs() <= 2
                    })
                    .unwrap_or(false)
            })
            .or_else(|| matching.first())
            .map(|(split, _, _)| *split)
    }

    fn after_tab_change(&mut self, window_id: WinitWindowId) {
        self.sync_sizes(window_id);
        self.with_window(window_id, |ui| ui.window.request_redraw());
        self.refresh_titles();
    }

    fn step_search(&mut self, window_id: WinitWindowId, forward: bool) {
        // The borrow of `ui` must not outlive the call into the session manager, so
        // the search state is copied out and back.
        let Some(mut overlay) = self
            .windows
            .get(&window_id)
            .and_then(|ui| match &ui.overlay {
                Overlay::Search(search) => Some(search.clone()),
                _ => None,
            })
        else {
            return;
        };
        if overlay.highlights.is_empty() {
            return;
        }
        let count = overlay.highlights.len();
        overlay.current = if forward {
            (overlay.current + 1) % count
        } else {
            (overlay.current + count - 1) % count
        };
        let row = overlay.highlights[overlay.current].0;
        if let Some(pane) = self
            .windows
            .get(&window_id)
            .and_then(WindowUi::focused_pane)
            && let Some(session) = self.sessions.for_pane_mut(pane)
        {
            let rows = session.engine.size().1 as i32;
            let offset = session.engine.display_offset() as i32;
            let buffer_line = row as i32 - offset;
            let desired = (rows / 2 - buffer_line).max(0);
            let delta = desired - offset;
            if delta != 0 {
                session.engine.scroll_display(delta);
            }
        }
        self.with_window(window_id, |ui| {
            ui.overlay = Overlay::Search(overlay);
            ui.window.request_redraw();
        });
    }

    fn open_tab(
        &mut self,
        window_id: WinitWindowId,
        profile: Option<ProfileId>,
    ) -> anyhow::Result<()> {
        let config = self.config.config().clone();
        let Some((cols, rows, cell)) = self.windows.get(&window_id).map(|ui| {
            let (cols, rows) = ui.grid_size(ui.terminal_rect());
            (
                cols,
                rows,
                (
                    ui.metrics.width.round().max(1.0) as u16,
                    ui.metrics.height.max(1) as u16,
                ),
            )
        }) else {
            return Ok(());
        };

        let profile_ref =
            profile.and_then(|id| config.profiles.get(id.raw().saturating_sub(1) as usize));
        let spec =
            astraterm_session::session_spec(&config, profile_ref, profile, None, cols, rows, cell);
        let pane = PaneId::next();
        let session = self.sessions.create(pane, &spec, cols, rows)?;

        let mut meta = Pane::new(pane);
        meta.profile = profile;
        meta.cwd = spec.pty.cwd.clone();
        if let Some(label) = spec.label.clone() {
            meta.title = label;
        }
        self.with_window(window_id, |ui| {
            ui.workspace.add_tab(Tab::new(String::new(), pane));
            ui.panes.insert(pane, meta);
            ui.session_bindings.insert(pane, session);
            ui.window.request_redraw();
        });
        self.sync_sizes(window_id);
        self.persist_workspace();
        self.refresh_titles();
        Ok(())
    }

    fn split_pane(
        &mut self,
        window_id: WinitWindowId,
        pane: PaneId,
        axis: SplitAxis,
    ) -> anyhow::Result<()> {
        let config = self.config.config().clone();
        let Some((cols, rows, cell, cwd, profile_id)) = self.windows.get(&window_id).map(|ui| {
            let (cols, rows) = ui.grid_size(ui.terminal_rect());
            let cwd = ui.panes.get(&pane).and_then(|meta| meta.cwd.clone());
            let profile_id = ui.panes.get(&pane).and_then(|meta| meta.profile);
            (
                cols,
                rows,
                (
                    ui.metrics.width.round().max(1.0) as u16,
                    ui.metrics.height.max(1) as u16,
                ),
                cwd,
                profile_id,
            )
        }) else {
            return Ok(());
        };

        let cwd = cwd.or_else(|| {
            self.sessions
                .for_pane(pane)
                .and_then(|session| session.cwd.clone())
        });
        let profile_ref =
            profile_id.and_then(|id| config.profiles.get(id.raw().saturating_sub(1) as usize));
        let new_pane = PaneId::next();
        let spec = astraterm_session::session_spec(
            &config,
            profile_ref,
            profile_id,
            cwd.clone(),
            (cols / 2).max(1),
            rows,
            cell,
        );
        let session = self
            .sessions
            .create(new_pane, &spec, (cols / 2).max(1), rows)?;

        let split = self
            .with_window(window_id, |ui| {
                ui.workspace
                    .active_tab_mut()
                    .and_then(|tab| tab.split_focused(axis, new_pane))
                    .is_some()
            })
            .unwrap_or(false);
        if !split {
            tracing::warn!(?pane, ?axis, "split_focused refused the new pane");
            self.sessions.close(session);
            return Ok(());
        }

        let mut meta = Pane::new(new_pane);
        meta.cwd = cwd;
        meta.profile = profile_id;
        self.with_window(window_id, |ui| {
            ui.panes.insert(new_pane, meta);
            ui.session_bindings.insert(new_pane, session);
            ui.window.request_redraw();
        });
        self.sync_sizes(window_id);
        self.persist_workspace();
        Ok(())
    }

    fn request_close_tab(&mut self, window_id: WinitWindowId, tab: TabId) {
        let Some((panes, single, running)) = self.windows.get(&window_id).and_then(|ui| {
            let tab_ref = ui.workspace.tab(tab)?;
            let panes = tab_ref.panes();
            let running = panes.iter().any(|pane| {
                self.sessions
                    .for_pane(*pane)
                    .map(|session| session.is_running())
                    .unwrap_or(false)
            });
            Some((panes, ui.workspace.tabs.len() == 1, running))
        }) else {
            return;
        };
        let confirm = self.config.config().terminal.confirm_on_close && running && !single;
        if confirm {
            self.with_window(window_id, |ui| {
                ui.overlay = Overlay::Confirm(ConfirmDialog {
                    message: format!("Close this tab and its {} pane(s)?", panes.len()),
                    action: PendingAction::CloseTab(tab),
                });
                ui.window.request_redraw();
            });
            return;
        }
        if single {
            self.with_window(window_id, |ui| {
                ui.set_status("closing the last tab keeps the window; use the quit shortcut");
            });
            return;
        }
        self.close_tab(window_id, tab);
    }

    fn close_tab(&mut self, window_id: WinitWindowId, tab: TabId) {
        let Some(panes) = self
            .with_window(window_id, |ui| {
                let panes = ui.workspace.tab(tab)?.panes();
                Some(if ui.workspace.close_tab(tab) {
                    Some(panes)
                } else {
                    None
                })
            })
            .flatten()
            .flatten()
        else {
            return;
        };
        let sessions: Vec<SessionId> = panes
            .iter()
            .filter_map(|pane| self.sessions.for_pane(*pane).map(|session| session.id))
            .collect();
        self.with_window(window_id, |ui| {
            for pane in &panes {
                ui.panes.remove(pane);
                ui.session_bindings.remove(pane);
            }
            ui.window.request_redraw();
        });
        for session in sessions {
            self.sessions.close(session);
        }
        self.sync_sizes(window_id);
        self.persist_workspace();
        self.refresh_titles();
    }

    fn request_close_pane(&mut self, window_id: WinitWindowId, pane: PaneId) {
        let Some((single, tab_id, running)) = self.windows.get(&window_id).and_then(|ui| {
            let tab = ui.workspace.active_tab()?;
            let running = self
                .sessions
                .for_pane(pane)
                .map(|session| session.is_running())
                .unwrap_or(false);
            Some((tab.pane_count() == 1, tab.id, running))
        }) else {
            return;
        };
        let confirm = self.config.config().terminal.confirm_on_close && running;
        if confirm {
            let action = if single {
                PendingAction::CloseTab(tab_id)
            } else {
                PendingAction::ClosePane(pane)
            };
            self.with_window(window_id, |ui| {
                ui.overlay = Overlay::Confirm(ConfirmDialog {
                    message: if single {
                        "Close this tab and end its session?".to_owned()
                    } else {
                        "Close this pane and end its session?".to_owned()
                    },
                    action,
                });
                ui.window.request_redraw();
            });
            return;
        }
        if single {
            self.close_tab(window_id, tab_id);
        } else {
            self.close_pane(window_id, pane);
        }
    }

    fn close_pane(&mut self, window_id: WinitWindowId, pane: PaneId) {
        let closed = self
            .with_window(window_id, |ui| {
                let closed = ui
                    .workspace
                    .active_tab_mut()
                    .map(|tab| tab.close_pane(pane))
                    .unwrap_or(false);
                if closed {
                    ui.panes.remove(&pane);
                    ui.session_bindings.remove(&pane);
                    ui.window.request_redraw();
                }
                closed
            })
            .unwrap_or(false);
        if !closed {
            return;
        }
        if let Some(session) = self.sessions.for_pane(pane).map(|session| session.id) {
            self.sessions.close(session);
        }
        self.sync_sizes(window_id);
        self.persist_workspace();
    }

    /// Closes one window: its sessions end, the other windows keep running.
    ///
    /// The application exits only when the last window is gone, so a window-scoped
    /// action can never destroy another window's shells.
    fn request_close_window(&mut self, event_loop: &ActiveEventLoop, window_id: WinitWindowId) {
        let Some(ui) = self.windows.get(&window_id) else {
            return;
        };
        let running = ui.panes.values().any(|pane| {
            self.sessions
                .for_pane(pane.id)
                .map(|session| session.is_running())
                .unwrap_or(false)
        });
        let confirm = running && self.config.config().terminal.confirm_on_close;
        if confirm {
            self.with_window(window_id, |ui| {
                ui.overlay = Overlay::Confirm(ConfirmDialog {
                    message: "Close this window and end its sessions?".to_owned(),
                    action: PendingAction::CloseWindow(ui.id),
                });
                ui.window.request_redraw();
            });
            return;
        }
        self.close_window(event_loop, window_id);
    }

    /// Tears down exactly one window.
    fn close_window(&mut self, event_loop: &ActiveEventLoop, window_id: WinitWindowId) {
        let Some(ui) = self.windows.remove(&window_id) else {
            return;
        };
        let sessions: Vec<SessionId> = ui.session_bindings.values().copied().collect();
        for session in sessions {
            self.sessions.close(session);
        }
        ui.window.set_visible(false);
        drop(ui);

        if self.focused == Some(window_id) {
            self.focused = self.windows.keys().next().copied();
        }
        if self.windows.is_empty() {
            self.finish(event_loop);
        } else {
            // Persist immediately so the closed window disappears from the layout.
            self.persist_workspace();
            self.refresh_titles();
        }
    }

    fn request_quit(&mut self, event_loop: &ActiveEventLoop, action: PendingAction) {
        let running = self.sessions.iter().any(|session| session.is_running());
        let confirm = running && self.config.config().terminal.confirm_on_close;
        if confirm {
            if let Some(window_id) = self.focused {
                self.with_window(window_id, |ui| {
                    ui.overlay = Overlay::Confirm(ConfirmDialog {
                        message: "Quit AstraTerm and end all sessions?".to_owned(),
                        action,
                    });
                    ui.window.request_redraw();
                });
            }
            return;
        }
        self.finish(event_loop);
    }

    fn finish(&mut self, event_loop: &ActiveEventLoop) {
        if self.quitting {
            return;
        }
        self.quitting = true;
        self.persist_workspace();
        self.sessions.shutdown_all();
        event_loop.exit();
    }

    fn copy_selection(&mut self, window_id: WinitWindowId) {
        let Some(pane) = self
            .windows
            .get(&window_id)
            .and_then(WindowUi::focused_pane)
        else {
            return;
        };
        let Some(text) = self
            .sessions
            .for_pane(pane)
            .and_then(|session| session.selection_text())
        else {
            return;
        };
        let copied = self.clipboard.set(ClipboardSelection::Clipboard, &text);
        let message = if copied {
            format!("copied {} character(s)", text.chars().count())
        } else {
            "clipboard is unavailable (install wl-clipboard or xclip)".to_owned()
        };
        self.with_window(window_id, |ui| ui.set_status(message));
    }

    fn paste(&mut self, window_id: WinitWindowId) {
        let Some(pane) = self
            .windows
            .get(&window_id)
            .and_then(WindowUi::focused_pane)
        else {
            return;
        };
        // Reading the selection spawns a helper process and waits for the owner to
        // answer, so it runs off the event loop; the text comes back as an event.
        let clipboard = self.clipboard.clone();
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            let result = clipboard.get(ClipboardSelection::Clipboard).ok_or_else(|| {
                "clipboard is unavailable (install wl-clipboard or xclip)".to_owned()
            });
            let _ = proxy.send_event(UserEvent::PasteReady {
                window: window_id,
                pane,
                result,
            });
        });
    }

    /// Sends a clipboard payload that was read off the event loop.
    fn finish_paste(
        &mut self,
        window_id: WinitWindowId,
        pane: PaneId,
        result: Result<String, String>,
    ) {
        let text = match result {
            Ok(text) => text,
            Err(message) => {
                self.with_window(window_id, |ui| ui.set_status(message));
                return;
            }
        };
        let Some(modes) = self
            .sessions
            .for_pane(pane)
            .map(|session| session.engine.modes())
        else {
            return;
        };
        match paste::encode_pasted_text(&text, modes) {
            Ok(bytes) => {
                if let Some(session) = self.sessions.for_pane_mut(pane) {
                    session.write(&bytes);
                }
            }
            Err(error) => {
                self.with_window(window_id, |ui| ui.set_status(error.message()));
            }
        }
    }

    fn change_font_size(&mut self, delta: f32) {
        let current = self
            .font_size_override
            .unwrap_or(self.config.config().font.size);
        let next = next_font_size(current, delta);
        self.font_size_override = Some(next);
        self.apply_config();
        self.persist_preferences();
    }

    fn open_settings(&mut self, window_id: WinitWindowId) {
        let Some((config, size, metrics)) = self.windows.get(&window_id).map(|ui| {
            (
                ui.config.clone(),
                Size::new(ui.size.width, ui.size.height),
                ui.metrics,
            )
        }) else {
            return;
        };
        let (themes, _errors) = available_themes(self.config.themes_dir());
        let mut panel = SettingsPanel::new(&config, themes, self.config.issues().to_vec());
        panel.set_paths(
            self.config.path().display().to_string(),
            self.config.themes_dir().display().to_string(),
        );
        // Each row occupies two lines (label + value) in the projection.
        let rows = (size.height as f32 / (metrics.height as f32 * 2.0)) as usize;
        panel.set_visible_rows(rows.saturating_sub(5).max(4));
        self.with_window(window_id, |ui| {
            ui.overlay = Overlay::Settings(Box::new(panel));
            ui.window.request_redraw();
        });
    }

    fn reload_config(&mut self) {
        match self.config.reload_if_changed() {
            ReloadOutcome::Unchanged => {}
            ReloadOutcome::Reloaded(_) | ReloadOutcome::Reset(_) => {
                tracing::info!("configuration reloaded from disk");
                self.apply_config();
            }
            ReloadOutcome::Invalid(issues) => {
                let message = issues
                    .first()
                    .map(|issue| issue.describe())
                    .unwrap_or_default();
                tracing::warn!(%message, "external configuration edit is invalid; keeping the previous configuration");
                if let Some(window_id) = self.focused {
                    self.with_window(window_id, |ui| {
                        ui.set_status(format!("config error: {message}"))
                    });
                }
            }
        }
    }

    fn apply_config(&mut self) {
        let config = self.effective_config();
        self.apply_config_value(&config);
    }

    /// The live configuration with the zoom override applied.
    ///
    /// The override stays in memory: writing it into `config.toml` would retrigger
    /// the file watcher and re-apply the configuration on every zoom step.
    fn effective_config(&self) -> Config {
        let mut config = self.config.config().clone();
        if let Some(size) = self.font_size_override {
            config.font.size = size;
        }
        config
    }

    /// Persists the zoom level in the state directory, reporting but not failing.
    fn persist_preferences(&self) {
        let preferences = Preferences::with_font_size(self.font_size_override);
        if let Err(error) = preferences.save(&self.state_paths.preferences_file()) {
            tracing::warn!(error = %error, "could not persist the zoom level");
        }
    }

    fn apply_config_value(&mut self, config: &Config) {
        self.keymap = Keymap::build(&config.keys);
        let theme = astraterm_config::themes::resolve_theme(
            &config.appearance.theme,
            self.config.themes_dir(),
        );
        for ui in self.windows.values_mut() {
            ui.apply_config(config, theme.clone());
        }
        self.sync_all_sizes();
        self.refresh_titles();
    }

    // --- Workspace persistence ----------------------------------------------

    fn persist_workspace(&self) {
        // An empty window list means the app never got a window (for example a GPU
        // failure at startup): writing it would destroy the user's saved layout.
        let Some(snapshot) = workspace_snapshot(&self.windows) else {
            return;
        };
        let Ok(text) = toml::to_string_pretty(&snapshot) else {
            return;
        };
        if let Err(error) =
            astraterm_config::writer::write_atomic(&self.state_paths.workspace_file(), &text)
        {
            tracing::warn!(error = %error, "could not persist workspace metadata");
        }
    }

    // --- Input ---------------------------------------------------------------

    fn handle_key(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WinitWindowId,
        event: &winit::event::KeyEvent,
    ) {
        if event.state != ElementState::Pressed {
            // A release only has a meaning for an application that asked for event
            // types; panels never see it and keep their own key handling.
            self.handle_key_release(window_id, event);
            return;
        }
        let overlay = self.windows.get(&window_id).map(|ui| match ui.overlay {
            Overlay::None => 0u8,
            Overlay::Search(_) => 1,
            Overlay::Settings(_) => 2,
            Overlay::Confirm(_) => 3,
            Overlay::Rename(_) => 4,
        });
        match overlay {
            Some(2) => self.handle_settings_key(window_id, event),
            Some(3) => self.handle_confirm_key(event_loop, window_id, event),
            Some(1) => self.handle_search_key(window_id, event),
            Some(4) => self.handle_rename_key(window_id, event),
            _ => self.handle_terminal_key(event_loop, window_id, event),
        }
    }

    fn handle_settings_key(&mut self, window_id: WinitWindowId, event: &winit::event::KeyEvent) {
        use winit::keyboard::{Key, NamedKey as WK};
        let key = match &event.logical_key {
            Key::Named(WK::ArrowUp) => PanelKey::Up,
            Key::Named(WK::ArrowDown) => PanelKey::Down,
            Key::Named(WK::ArrowLeft) => PanelKey::Left,
            Key::Named(WK::ArrowRight) => PanelKey::Right,
            Key::Named(WK::PageUp) => PanelKey::PageUp,
            Key::Named(WK::PageDown) => PanelKey::PageDown,
            Key::Named(WK::Home) => PanelKey::Home,
            Key::Named(WK::End) => PanelKey::End,
            Key::Named(WK::Enter) => PanelKey::Enter,
            Key::Named(WK::Escape) => PanelKey::Escape,
            Key::Named(WK::Tab) => PanelKey::Tab,
            Key::Named(WK::Backspace) => PanelKey::Backspace,
            Key::Named(WK::Space) => PanelKey::Char(' '),
            Key::Character(text) => match text.chars().next() {
                Some(ch) => PanelKey::Char(ch),
                None => return,
            },
            _ => return,
        };

        let Some(action) = self
            .with_window(window_id, |ui| match &mut ui.overlay {
                Overlay::Settings(panel) => Some(panel.handle_key(key)),
                _ => None,
            })
            .flatten()
        else {
            return;
        };

        match action {
            PanelAction::Consumed => {}
            PanelAction::Close => {
                self.with_window(window_id, |ui| ui.overlay = Overlay::None);
                // A previewed palette that was never saved must not survive the panel.
                self.apply_config();
            }
            PanelAction::Revert => {
                self.with_window(window_id, |ui| {
                    if let Overlay::Settings(panel) = &mut ui.overlay {
                        panel.revert();
                    }
                });
                // Undo any palette that was being previewed.
                self.apply_config();
            }
            PanelAction::PreviewPalette(palette) => {
                for ui in self.windows.values_mut() {
                    ui.theme.palette = palette.as_ref().clone();
                    ui.renderer.invalidate_text_cache();
                    ui.window.request_redraw();
                }
            }
            PanelAction::SaveTheme { name, palette } => {
                let dir = self.config.themes_dir().to_path_buf();
                match astraterm_config::save_theme(&dir, &name, *palette) {
                    Ok((stem, path)) => {
                        self.config.refresh_themes();
                        match self
                            .config
                            .update(|config| config.appearance.theme = stem.clone())
                        {
                            Ok(_) => {
                                self.apply_config();
                                let themes = self.config.themes().to_vec();
                                let issues = self.config.issues().to_vec();
                                let config = self.config.config().clone();
                                let message = format!("saved {}", path.display());
                                self.with_window(window_id, |ui| {
                                    if let Overlay::Settings(panel) = &mut ui.overlay {
                                        panel.set_themes(themes);
                                        panel.on_saved(&config, issues);
                                        panel.set_message(message);
                                    }
                                });
                            }
                            Err(error) => {
                                let message = format!("theme saved but not selected: {error}");
                                self.with_window(window_id, |ui| {
                                    if let Overlay::Settings(panel) = &mut ui.overlay {
                                        panel.set_message(message);
                                    }
                                });
                            }
                        }
                    }
                    Err(error) => {
                        let message = error.to_string();
                        self.with_window(window_id, |ui| {
                            if let Overlay::Settings(panel) = &mut ui.overlay {
                                panel.set_message(message);
                            }
                        });
                    }
                }
            }
            PanelAction::Message(message) => {
                self.with_window(window_id, |ui| {
                    if let Overlay::Settings(panel) = &mut ui.overlay {
                        panel.set_message(message);
                    }
                });
            }
            PanelAction::Save(draft) => match self.config.replace(*draft) {
                Ok(_) => {
                    let config = self.config.config().clone();
                    let issues = self.config.issues().to_vec();
                    self.with_window(window_id, |ui| {
                        if let Overlay::Settings(panel) = &mut ui.overlay {
                            panel.on_saved(&config, issues);
                        }
                    });
                    self.apply_config();
                }
                Err(SaveError::Invalid(issues)) => {
                    let message = issues
                        .first()
                        .map(|issue| issue.describe())
                        .unwrap_or_default();
                    self.with_window(window_id, |ui| {
                        if let Overlay::Settings(panel) = &mut ui.overlay {
                            panel.set_message(format!("invalid setting: {message}"));
                            panel.set_issues(issues);
                        }
                    });
                }
                Err(SaveError::ExternalChange) => {
                    self.with_window(window_id, |ui| {
                        if let Overlay::Settings(panel) = &mut ui.overlay {
                            panel.set_message(
                                "the config file changed on disk; reload before saving",
                            );
                        }
                    });
                }
                Err(SaveError::Write(error)) => {
                    self.with_window(window_id, |ui| {
                        if let Overlay::Settings(panel) = &mut ui.overlay {
                            panel.set_message(format!("could not save: {error}"));
                        }
                    });
                }
            },
        }
        self.with_window(window_id, |ui| ui.window.request_redraw());
    }

    fn handle_confirm_key(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WinitWindowId,
        event: &winit::event::KeyEvent,
    ) {
        use winit::keyboard::{Key, NamedKey as WK};
        let answer = match &event.logical_key {
            Key::Named(WK::Escape) => Some(false),
            Key::Named(WK::Enter) => Some(true),
            Key::Character(text) => {
                let ch = text.chars().next().unwrap_or(' ').to_ascii_lowercase();
                if ch == 'y' {
                    Some(true)
                } else if ch == 'n' {
                    Some(false)
                } else {
                    None
                }
            }
            _ => None,
        };
        let Some(confirmed) = answer else { return };
        let action = self
            .with_window(window_id, |ui| {
                let action = match &ui.overlay {
                    Overlay::Confirm(dialog) => Some(dialog.action),
                    _ => None,
                };
                ui.overlay = Overlay::None;
                ui.window.request_redraw();
                action
            })
            .flatten();
        let Some(action) = action else { return };
        if !confirmed {
            return;
        }
        match action {
            PendingAction::ClosePane(pane) => self.close_pane(window_id, pane),
            PendingAction::CloseTab(tab) => self.close_tab(window_id, tab),
            PendingAction::CloseWindow(id) => {
                let target = self
                    .windows
                    .values()
                    .find(|ui| ui.id == id)
                    .map(|ui| ui.window.id());
                match target {
                    Some(winit_id) => self.close_window(event_loop, winit_id),
                    // The window disappeared while the prompt was open.
                    None => self.finish(event_loop),
                }
            }
            PendingAction::Quit => self.finish(event_loop),
        }
    }

    fn handle_rename_key(&mut self, window_id: WinitWindowId, event: &winit::event::KeyEvent) {
        use winit::keyboard::{Key, NamedKey as WK};
        enum RenameKey {
            Char(char),
            Backspace,
            Commit,
            Cancel,
        }
        let key = match &event.logical_key {
            Key::Named(WK::Escape) => RenameKey::Cancel,
            Key::Named(WK::Enter) => RenameKey::Commit,
            Key::Named(WK::Backspace) => RenameKey::Backspace,
            Key::Named(WK::Space) => RenameKey::Char(' '),
            Key::Character(text) => match text.chars().next() {
                Some(ch) => RenameKey::Char(ch),
                None => return,
            },
            _ => return,
        };

        let committed = self
            .with_window(window_id, |ui| {
                let Overlay::Rename(rename) = &mut ui.overlay else {
                    return None;
                };
                match key {
                    RenameKey::Cancel => {
                        ui.overlay = Overlay::None;
                        ui.window.request_redraw();
                        None
                    }
                    RenameKey::Backspace => {
                        rename.pop();
                        ui.window.request_redraw();
                        None
                    }
                    RenameKey::Char(ch) => {
                        rename.push(ch);
                        ui.window.request_redraw();
                        None
                    }
                    RenameKey::Commit => {
                        let tab = rename.tab;
                        let title = rename.committed_title();
                        ui.overlay = Overlay::None;
                        Some((tab, title))
                    }
                }
            })
            .flatten();
        if let Some((tab, title)) = committed {
            self.with_window(window_id, |ui| {
                if let Some(tab) = ui.workspace.tab_mut(tab) {
                    tab.title = title;
                }
                ui.window.request_redraw();
            });
            self.persist_workspace();
        }
    }

    fn handle_search_key(&mut self, window_id: WinitWindowId, event: &winit::event::KeyEvent) {
        use winit::keyboard::{Key, NamedKey as WK};
        enum SearchKey {
            Char(char),
            Backspace,
            Next,
            Previous,
            Close,
        }
        let key = match &event.logical_key {
            Key::Named(WK::Escape) => SearchKey::Close,
            Key::Named(WK::Enter) => SearchKey::Next,
            Key::Named(WK::Backspace) => SearchKey::Backspace,
            Key::Named(WK::ArrowDown) => SearchKey::Next,
            Key::Named(WK::ArrowUp) => SearchKey::Previous,
            Key::Character(text) => match text.chars().next() {
                Some(ch) => SearchKey::Char(ch),
                None => return,
            },
            _ => return,
        };

        match key {
            SearchKey::Close => {
                self.with_window(window_id, |ui| {
                    ui.overlay = Overlay::None;
                    ui.window.request_redraw();
                });
            }
            SearchKey::Next => self.step_search(window_id, true),
            SearchKey::Previous => self.step_search(window_id, false),
            SearchKey::Backspace | SearchKey::Char(_) => {
                let query = self
                    .with_window(window_id, |ui| {
                        let Overlay::Search(search) = &mut ui.overlay else {
                            return None;
                        };
                        match key {
                            SearchKey::Backspace => {
                                search.query.pop();
                            }
                            SearchKey::Char(ch) => search.query.push(ch),
                            _ => {}
                        }
                        search.current = 0;
                        Some(search.query.clone())
                    })
                    .flatten();
                let Some(query) = query else { return };
                self.refresh_highlights(window_id, &query);
            }
        }
    }

    fn refresh_highlights(&mut self, window_id: WinitWindowId, query: &str) {
        let case_sensitive = self
            .windows
            .get(&window_id)
            .and_then(|ui| match &ui.overlay {
                Overlay::Search(search) => Some(search.case_sensitive),
                _ => None,
            })
            .unwrap_or(false);
        let Some(pane) = self
            .windows
            .get(&window_id)
            .and_then(WindowUi::focused_pane)
        else {
            return;
        };
        let highlights = self
            .sessions
            .for_pane_mut(pane)
            .map(|session| {
                let snapshot = session.snapshot();
                crate::ui::find_in_snapshot(&snapshot, query, case_sensitive)
            })
            .unwrap_or_default();
        self.with_window(window_id, |ui| {
            if let Overlay::Search(search) = &mut ui.overlay {
                search.highlights = highlights;
                if search.current >= search.highlights.len() {
                    search.current = 0;
                }
            }
            ui.window.request_redraw();
        });
    }

    fn handle_terminal_key(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WinitWindowId,
        event: &winit::event::KeyEvent,
    ) {
        if !self.literal_next
            && let Some(chord) = keys::chord_of(event, self.mods)
            && let Some(action) = self.keymap.action_for(&chord)
            && let Some(command) = keys::command_for(action)
        {
            tracing::debug!(?chord, %action, "shortcut");
            let command = self.substitute_context(window_id, action, command);
            self.dispatch(event_loop, command);
            return;
        }
        self.literal_next = false;

        let Some((pane, modes)) = self.focused_pane_modes(window_id) else {
            return;
        };
        let Some(press) = keys::key_press(event, self.mods) else {
            return;
        };
        // A held key is a repeat only for the Kitty protocol. Everywhere else a
        // repeat is just another press, exactly as xterm always behaved.
        let press = if event.repeat && keys::forwards_release(modes) {
            press.with_event(KeyEventType::Repeat)
        } else {
            press
        };
        let bytes = encode_key(press, modes);
        if bytes.is_empty() {
            return;
        }
        if let Some(session) = self.sessions.for_pane_mut(pane) {
            session.write(&bytes);
        }
    }

    /// Sends a key release to the focused PTY when the application asked for it.
    ///
    /// `REPORT_EVENT_TYPES` (Kitty flag 2) is the only mode that gives a release an
    /// encoding; without it `encode_key` returns nothing, so an xterm session sees
    /// exactly one event per key, as before.
    fn handle_key_release(&mut self, window_id: WinitWindowId, event: &winit::event::KeyEvent) {
        let Some(ui) = self.windows.get(&window_id) else {
            return;
        };
        if !matches!(ui.overlay, Overlay::None) {
            return;
        }
        let Some((pane, modes)) = self.focused_pane_modes(window_id) else {
            return;
        };
        if !keys::forwards_release(modes) {
            return;
        }
        let Some(press) = keys::key_press(event, self.mods) else {
            return;
        };
        let bytes = encode_key(press.with_event(KeyEventType::Release), modes);
        if bytes.is_empty() {
            return;
        }
        if let Some(session) = self.sessions.for_pane_mut(pane) {
            session.write(&bytes);
        }
    }

    /// Focused pane of `window_id` plus the modes of its session.
    fn focused_pane_modes(&self, window_id: WinitWindowId) -> Option<(PaneId, ModeFlags)> {
        let pane = self.windows.get(&window_id)?.focused_pane()?;
        let modes = self.sessions.for_pane(pane)?.engine.modes();
        Some((pane, modes))
    }

    /// Replaces placeholder identifiers in a mapped command with the focused pane or
    /// active tab, so [`keys::command_for`] stays a pure mapping.
    fn substitute_context(
        &self,
        window_id: WinitWindowId,
        _action: ActionId,
        command: AppCommand,
    ) -> AppCommand {
        let Some(ui) = self.windows.get(&window_id) else {
            return command;
        };
        let pane = ui.focused_pane().unwrap_or(keys::PaneIdPlaceholder::VALUE);
        let tab = ui.workspace.active_tab;
        match command {
            AppCommand::SplitPane { axis, .. } => AppCommand::SplitPane { pane, axis },
            AppCommand::ClosePane { .. } => AppCommand::ClosePane { pane },
            AppCommand::CloseTab { .. } => AppCommand::CloseTab { tab },
            AppCommand::RenameTab { .. } => AppCommand::RenameTab {
                tab,
                title: String::new(),
            },
            other => other,
        }
    }

    // --- Pointer -------------------------------------------------------------

    fn handle_mouse_input(
        &mut self,
        window_id: WinitWindowId,
        state: ElementState,
        button: MouseButton,
    ) {
        let Some((x, y)) = self.cursor else { return };
        let Some(ui) = self.windows.get(&window_id) else {
            return;
        };
        if matches!(ui.overlay, Overlay::Confirm(_) | Overlay::Settings(_)) {
            return;
        }
        if state == ElementState::Pressed
            && button == MouseButton::Left
            && let Some(index) = ui.tab_at(x, y)
        {
            self.with_window(window_id, |ui| ui.workspace.select_index(index));
            self.after_tab_change(window_id);
            return;
        }

        let Some(pane) = ui.pane_at(x, y) else { return };
        let Some((column, row, modes)) = ui.cell_at(pane, x, y).and_then(|(column, row)| {
            self.sessions
                .for_pane(pane)
                .map(|session| (column, row, session.engine.modes()))
        }) else {
            return;
        };

        // The application asked for mouse input: forward it instead of selecting.
        if modes.mouse_reporting() && !self.mods.shift_key() {
            let kind = if state == ElementState::Pressed {
                MouseKind::Press
            } else {
                MouseKind::Release
            };
            let button = match button {
                MouseButton::Left => input::MouseButton::Left,
                MouseButton::Middle => input::MouseButton::Middle,
                MouseButton::Right => input::MouseButton::Right,
                _ => input::MouseButton::None,
            };
            let event = MouseEvent {
                kind,
                button,
                column,
                row,
                ctrl: self.mods.control_key(),
                alt: self.mods.alt_key(),
                shift: self.mods.shift_key(),
            };
            let bytes = encode_mouse(event, modes);
            if !bytes.is_empty()
                && let Some(session) = self.sessions.for_pane_mut(pane)
            {
                session.write(&bytes);
            }
            return;
        }

        if state == ElementState::Pressed {
            if button != MouseButton::Left {
                return;
            }
            if let Some(drag) = self.divider_at(window_id, x, y) {
                self.with_window(window_id, |ui| ui.drag = drag);
                return;
            }
            if self.mods.control_key() && self.try_open_url(window_id, pane, column, row) {
                return;
            }
            self.begin_selection(window_id, pane, column, row);
        } else {
            let selecting = self
                .windows
                .get(&window_id)
                .map(|ui| matches!(ui.drag, DragState::Selection { .. }))
                .unwrap_or(false);
            self.with_window(window_id, |ui| ui.drag = DragState::None);
            if selecting && self.config.config().terminal.copy_on_select {
                self.copy_selection(window_id);
            }
        }
    }

    fn divider_at(&self, window_id: WinitWindowId, x: f64, y: f64) -> Option<DragState> {
        let ui = self.windows.get(&window_id)?;
        let tab = ui.workspace.active_tab()?;
        let rect = ui.terminal_rect();
        let tolerance = (ui.layout.divider.max(2) as f64) + 2.0;
        for (split, axis, divider) in tab.dividers(rect, ui.layout) {
            let (hit, start, extent) = match axis {
                SplitAxis::Vertical => (
                    (x - divider.x as f64).abs() <= tolerance
                        && y >= divider.y as f64
                        && y <= divider.bottom() as f64,
                    x,
                    rect.width as f64,
                ),
                SplitAxis::Horizontal => (
                    (y - divider.y as f64).abs() <= tolerance
                        && x >= divider.x as f64
                        && x <= divider.right() as f64,
                    y,
                    rect.height as f64,
                ),
            };
            if hit {
                return Some(DragState::Divider {
                    split: SplitIdRef(split.raw()),
                    axis,
                    start,
                    extent: extent.max(1.0),
                    applied: 0.0,
                });
            }
        }
        None
    }

    fn begin_selection(&mut self, window_id: WinitWindowId, pane: PaneId, column: u16, row: u16) {
        let now = Instant::now();
        let previous = self.windows.get(&window_id).and_then(|ui| ui.last_click);
        let count = match previous {
            Some(click)
                if now.duration_since(click.at) < Duration::from_millis(400)
                    && click.column == column
                    && click.row == row =>
            {
                (click.count + 1).min(3)
            }
            _ => 1,
        };
        let kind = match count {
            2 => astraterm_terminal::SelectionKind::Semantic,
            3 => astraterm_terminal::SelectionKind::Lines,
            _ => astraterm_terminal::SelectionKind::Simple,
        };
        if let Some(session) = self.sessions.for_pane_mut(pane) {
            session
                .engine
                .update_selection(column, row, kind, astraterm_terminal::Side::Left);
        }
        self.with_window(window_id, |ui| {
            ui.drag = DragState::Selection { pane };
            ui.last_click = Some(ClickInfo {
                at: now,
                column,
                row,
                count,
            });
            ui.window.request_redraw();
        });
    }

    fn try_open_url(
        &mut self,
        window_id: WinitWindowId,
        pane: PaneId,
        column: u16,
        row: u16,
    ) -> bool {
        let open_enabled = self
            .windows
            .get(&window_id)
            .map(|ui| ui.config.terminal.open_urls_on_click)
            .unwrap_or(false);
        if !open_enabled {
            return false;
        }
        let url = self.sessions.for_pane_mut(pane).and_then(|session| {
            let snapshot = session.snapshot();
            snapshot
                .lines
                .get(row as usize)
                .and_then(|line| url_at(&line.text(), column as usize))
        });
        let Some(url) = url else { return false };
        let message = match linux::open_url(&url) {
            Ok(()) => format!("opened {url}"),
            Err(error) => error,
        };
        self.with_window(window_id, |ui| ui.set_status(message));
        true
    }

    fn handle_cursor_moved(&mut self, window_id: WinitWindowId, x: f64, y: f64) {
        self.cursor = Some((x, y));
        let Some(drag) = self.windows.get(&window_id).map(|ui| ui.drag.clone()) else {
            return;
        };
        match drag {
            DragState::Selection { pane } => {
                let cell = self
                    .windows
                    .get(&window_id)
                    .and_then(|ui| ui.cell_at(pane, x, y));
                if let Some((column, row)) = cell
                    && let Some(session) = self.sessions.for_pane_mut(pane)
                {
                    session.engine.update_selection(
                        column,
                        row,
                        astraterm_terminal::SelectionKind::SimpleOrExisting,
                        astraterm_terminal::Side::Right,
                    );
                }
                self.with_window(window_id, |ui| ui.window.request_redraw());
            }
            DragState::Divider {
                split,
                axis,
                start,
                extent,
                applied,
            } => {
                let current = match axis {
                    SplitAxis::Vertical => x,
                    SplitAxis::Horizontal => y,
                };
                let total = ((current - start) / extent) as f32;
                let delta = total - applied;
                if delta.abs() > 0.002 {
                    self.with_window(window_id, |ui| {
                        let rect = ui.terminal_rect();
                        let layout = ui.layout;
                        let target = ui.workspace.active_tab().and_then(|tab| {
                            tab.dividers(rect, layout)
                                .into_iter()
                                .find(|(split_id, _, _)| split_id.raw() == split.0)
                                .map(|(split_id, _, _)| split_id)
                        });
                        if let Some(split_id) = target
                            && let Some(tab) = ui.workspace.active_tab_mut()
                        {
                            tab.root.adjust_ratio(split_id, delta);
                        }
                        ui.drag = DragState::Divider {
                            split,
                            axis,
                            start,
                            extent,
                            applied: total,
                        };
                        ui.window.request_redraw();
                    });
                    self.sync_sizes(window_id);
                }
            }
            DragState::None => {}
        }
    }

    fn handle_wheel(&mut self, window_id: WinitWindowId, delta: MouseScrollDelta) {
        let Some(ui) = self.windows.get(&window_id) else {
            return;
        };
        let pane = self
            .cursor
            .and_then(|(x, y)| ui.pane_at(x, y))
            .or_else(|| ui.focused_pane());
        let Some(pane) = pane else { return };
        let Some(modes) = self
            .sessions
            .for_pane(pane)
            .map(|session| session.engine.modes())
        else {
            return;
        };
        let lines = match delta {
            MouseScrollDelta::LineDelta(_, lines) => lines,
            MouseScrollDelta::PixelDelta(position) => position.y as f32 / 20.0,
        };
        if lines == 0.0 {
            return;
        }
        if modes.mouse_reporting() {
            let Some((x, y)) = self.cursor else { return };
            let Some((column, row)) = ui.cell_at(pane, x, y) else {
                return;
            };
            let button: u8 = if lines > 0.0 { 64 } else { 65 };
            let mut bytes = Vec::new();
            if modes.sgr_mouse {
                bytes.extend_from_slice(
                    format!("\x1b[<{};{};{}M", button, column + 1, row + 1).as_bytes(),
                );
            } else {
                let column = (column + 1).min(223) as u8;
                let row = (row + 1).min(223) as u8;
                bytes.extend_from_slice(&[0x1b, b'[', b'M', 32 + button, 32 + column, 32 + row]);
            }
            if let Some(session) = self.sessions.for_pane_mut(pane) {
                session.write(&bytes);
            }
        } else if let Some(session) = self.sessions.for_pane_mut(pane) {
            let steps = lines.round() as i32;
            session.engine.scroll_display(steps * 3);
        }
        self.with_window(window_id, |ui| ui.window.request_redraw());
    }

    /// Reports focus in/out when the application asked for focus reporting (1004).
    fn report_focus(&mut self, window_id: WinitWindowId, focused: bool) {
        let Some(pane) = self
            .windows
            .get(&window_id)
            .and_then(WindowUi::focused_pane)
        else {
            return;
        };
        let wants = self
            .sessions
            .for_pane(pane)
            .map(|session| session.engine.modes().focus_reporting)
            .unwrap_or(false);
        if !wants {
            return;
        }
        let bytes: &[u8] = if focused { b"\x1b[I" } else { b"\x1b[O" };
        if let Some(session) = self.sessions.for_pane_mut(pane) {
            session.write(bytes);
        }
    }
}

/// Applies the platform window identity (`app_id` on Wayland, `WM_CLASS` on X11).
struct PlatformName;

impl PlatformName {
    fn apply(attributes: winit::window::WindowAttributes) -> winit::window::WindowAttributes {
        #[cfg(any(
            target_os = "linux",
            target_os = "dragonfly",
            target_os = "freebsd",
            target_os = "netbsd",
            target_os = "openbsd"
        ))]
        {
            // Both backends read the same field: Wayland uses `general` as `app_id`,
            // X11 uses the pair as `WM_CLASS`. The value matches the installed desktop
            // file, so the compositor can associate the window with its icon.
            use winit::platform::wayland::WindowAttributesExtWayland;
            return WindowAttributesExtWayland::with_name(attributes, "astraterm", "astraterm");
        }
        #[allow(unreachable_code)]
        attributes
    }
}

/// Window icon decoded from the logo committed in `assets/icons`.
///
/// The PNG is embedded in the binary so the icon works from any installation
/// directory; a decode failure is not fatal and simply leaves the window with the
/// compositor-provided icon.
fn window_icon() -> Option<winit::window::Icon> {
    const ICON_PNG: &[u8] = include_bytes!("../../../assets/icons/astraterm.png");
    let mut decoder = png::Decoder::new(std::io::Cursor::new(ICON_PNG));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().ok()?;
    let mut buffer = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buffer).ok()?;
    let data = match info.color_type {
        png::ColorType::Rgba => buffer[..info.buffer_size()].to_vec(),
        png::ColorType::Rgb => buffer[..info.buffer_size()]
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|pixel| [pixel[0], pixel[1], pixel[2], 0xff])
            .collect(),
        _ => return None,
    };
    winit::window::Icon::from_rgba(data, info.width, info.height).ok()
}

/// Builds the persisted workspace, or `None` when there is nothing to persist.
///
/// Writing an empty snapshot would replace a saved layout with nothing, which is
/// exactly what a failed window creation must never do.
fn workspace_snapshot(windows: &BTreeMap<WinitWindowId, WindowUi>) -> Option<WorkspaceSnapshot> {
    if windows.is_empty() {
        return None;
    }
    let windows = windows
        .values()
        .map(|ui| {
            let mut snapshot = snapshot_window(&ui.workspace, &ui.panes);
            snapshot.width = ui.size.width;
            snapshot.height = ui.size.height;
            snapshot
        })
        .collect();
    Some(WorkspaceSnapshot {
        version: astraterm_workspace::snapshot::SNAPSHOT_VERSION,
        windows,
    })
}

impl ApplicationHandler<UserEvent> for Application {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.started {
            return;
        }
        self.started = true;

        // Wire the PTY wake path: output from any thread wakes the event loop.
        let proxy = self.proxy.clone();
        self.sessions.set_waker(Arc::new(move || {
            let _ = proxy.send_event(UserEvent::Wake);
        }));

        let restore = !self.cli.new_window;
        if let Err(error) = self.create_window(event_loop, restore) {
            tracing::error!(error = %error, "could not create the window; the GPU backend may be unavailable");
            event_loop.exit();
        }
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::Wake => self.pump(),
            // Debounce: editors write in bursts, so the reload waits for quiet.
            UserEvent::ConfigChanged => {
                self.watcher_deadline = Some(Instant::now() + Duration::from_millis(250));
            }
            UserEvent::PasteReady {
                window,
                pane,
                result,
            } => self.finish_paste(window, pane, result),
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WinitWindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => self.request_close_window(event_loop, window_id),
            WindowEvent::Resized(size) => {
                self.with_window(window_id, |ui| ui.resize(size));
                self.sync_sizes(window_id);
            }
            WindowEvent::ScaleFactorChanged { .. } => {
                let scale = self
                    .windows
                    .get(&window_id)
                    .map(|ui| ui.window.scale_factor() as f32);
                if let Some(scale) = scale {
                    self.with_window(window_id, |ui| {
                        let font = ui.config.font.clone();
                        ui.renderer.set_scale(scale, &font);
                        ui.metrics = ui.renderer.cell_metrics();
                        ui.scale = scale;
                        ui.window.request_redraw();
                    });
                }
                self.sync_sizes(window_id);
            }
            WindowEvent::Focused(focused) => {
                if focused {
                    self.focused = Some(window_id);
                    self.with_window(window_id, |ui| {
                        ui.cursor_phase = true;
                        ui.window.request_redraw();
                    });
                    self.report_focus(window_id, true);
                } else {
                    self.report_focus(window_id, false);
                }
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                self.mods = modifiers.state();
            }
            WindowEvent::KeyboardInput { event, .. } => {
                self.handle_key(event_loop, window_id, &event)
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.handle_cursor_moved(window_id, position.x, position.y);
            }
            WindowEvent::MouseInput { state, button, .. } => {
                self.handle_mouse_input(window_id, state, button);
            }
            WindowEvent::MouseWheel { delta, .. } => self.handle_wheel(window_id, delta),
            WindowEvent::CursorLeft { .. } => self.cursor = None,
            WindowEvent::RedrawRequested => {
                let system = self.system.snapshot();
                let scene = match self.windows.get_mut(&window_id) {
                    Some(ui) => ui.compose(&mut self.sessions, &system, &self.images),
                    None => return,
                };
                if let Some(ui) = self.windows.get_mut(&window_id)
                    && let Err(error) = ui.renderer.render(&scene)
                {
                    // A redraw requested from inside the redraw handler would spin the
                    // event loop on a persistent failure; the next real event retries.
                    tracing::warn!(error = %error, "frame was not drawn");
                }
            }
            WindowEvent::Occluded(occluded) => {
                if !occluded && let Some(ui) = self.windows.get(&window_id) {
                    ui.window.request_redraw();
                }
            }
            WindowEvent::Destroyed => {
                // The compositor destroyed the window without a close request, so its
                // sessions must not be left running without a UI.
                let sessions: Vec<SessionId> = self
                    .windows
                    .get(&window_id)
                    .map(|ui| ui.session_bindings.values().copied().collect())
                    .unwrap_or_default();
                for session in sessions {
                    self.sessions.close(session);
                }
                self.windows.remove(&window_id);
                if self.focused == Some(window_id) {
                    self.focused = self.windows.keys().next().copied();
                }
                if self.windows.is_empty() {
                    self.finish(event_loop);
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(deadline) = self.watcher_deadline
            && Instant::now() >= deadline
        {
            self.watcher_deadline = None;
            self.reload_config();
        }

        // System readings for the status bar: one sample per second, and only while
        // the bar is visible. With the bar hidden there is no timer for them at all.
        let status_visible = self
            .windows
            .values()
            .any(|ui| ui.config.appearance.show_status_bar);
        let mut status_wait: Option<Duration> = None;
        if status_visible {
            let now = Instant::now();
            let due = self.system_deadline.unwrap_or(now);
            if now >= due {
                if self.system.sample(Duration::from_millis(900)) {
                    for ui in self.windows.values() {
                        ui.window.request_redraw();
                    }
                }
                let next = now + Duration::from_secs(1);
                self.system_deadline = Some(next);
                status_wait = Some(next.saturating_duration_since(now));
            } else {
                status_wait = Some(due.saturating_duration_since(now));
            }
        } else {
            self.system_deadline = None;
        }

        // Cursor blink is the only periodic timer, and only while blinking is on.
        let blink = self
            .focused
            .and_then(|id| self.windows.get(&id))
            .map(|ui| ui.should_blink())
            .unwrap_or(false);
        if blink {
            let now = Instant::now();
            let (toggle, next) = crate::ui::blink_due(now, self.blink_deadline);
            self.blink_deadline = Some(next);
            if toggle
                && let Some(window_id) = self.focused
                && let Some(ui) = self.windows.get_mut(&window_id)
            {
                ui.toggle_cursor_phase();
            }
            let wait = next.saturating_duration_since(now);
            if let Some(status_wait) = status_wait {
                event_loop.set_control_flow(winit::event_loop::ControlFlow::wait_duration(
                    wait.min(status_wait),
                ));
            } else {
                event_loop.set_control_flow(winit::event_loop::ControlFlow::wait_duration(wait));
            }
        } else if let Some(status_wait) = status_wait {
            event_loop.set_control_flow(winit::event_loop::ControlFlow::wait_duration(status_wait));
        } else if self.watcher_deadline.is_some() {
            event_loop.set_control_flow(winit::event_loop::ControlFlow::wait_duration(
                Duration::from_millis(100),
            ));
        } else {
            event_loop.set_control_flow(winit::event_loop::ControlFlow::Wait);
        }
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        self.persist_workspace();
        self.sessions.shutdown_all();
    }
}

#[cfg(test)]
mod persistence_tests {
    use std::collections::BTreeMap;

    use super::workspace_snapshot;

    #[test]
    fn an_empty_window_list_is_never_persisted() {
        let windows = BTreeMap::new();
        assert!(
            workspace_snapshot(&windows).is_none(),
            "a failed startup must not overwrite the saved layout"
        );
    }
}
