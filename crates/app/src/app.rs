//! The application: event loop, command dispatch and subsystem wiring.
//!
//! This is the only layer that knows about every other subsystem. The event loop
//! stays event-driven: PTY threads wake it through an `EventLoopProxy`, redraws
//! happen only when something changed, and the only timers are the cursor blink and
//! the configuration-watcher debounce.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use astraterm_config::keys::{ActionId, Keymap};
use astraterm_config::schema::{Config, should_notify};
use astraterm_config::service::{ConfigService, ConfigWatcher, ReloadOutcome, SaveError};
use astraterm_config::themes::{Theme, available_themes};
use astraterm_core::events::{AppCommand, ExitReason, SessionEvent};
use astraterm_core::{PaneId, ProfileId, SessionId, Size, SplitAxis, TabId, WindowId};
use astraterm_platform::clipboard::{
    Clipboard, ClipboardWriter, Paste as ClipboardPaste, Selection as ClipboardSelection,
};
use astraterm_platform::linux;
use astraterm_platform::notifications::{MAX_BODY_CHARS, MAX_TITLE_CHARS, Notifications, sanitize};
use astraterm_platform::paths::StatePaths;
use astraterm_platform::system::SystemMonitor;
use astraterm_settings_ui::panel::{PanelAction, PanelKey, SettingsPanel};
use astraterm_terminal::input::{
    self, KeyEventType, MouseEvent, MouseKind, TermKey, encode_key, encode_mouse,
};
use astraterm_terminal::snapshot::ModeFlags;
use astraterm_workspace::snapshot::{WorkspaceSnapshot, restore_window, snapshot_window};
use astraterm_workspace::{Pane, Tab, WindowState};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, Ime, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoopProxy};
use winit::keyboard::ModifiersState;
use winit::window::{ImePurpose, UserAttentionType, Window, WindowId as WinitWindowId};

use crate::cli::Cli;
use crate::copy_on_select::CopyOnSelect;
use crate::keys;
use crate::paste;
use crate::prefs::{FONT_SIZE_STEP, Preferences, next_font_size};
use crate::ui::{
    ClickInfo, ConfirmDialog, DragState, ExitCleanup, Floating, FloatingPress, Overlay,
    PendingAction, RenameOverlay, SearchOverlay, SessionImages, WindowUi,
    closes_floating, divider_drag_at, open_clicked_url, plan_exit_cleanup, plan_floating_press,
    split_lands_in_floating,
};

/// Characters of notification body repeated in the status bar.
///
/// The body itself is bounded by [`MAX_BODY_CHARS`], but the status bar has one
/// line, so the status message keeps only its beginning.
const STATUS_NOTIFICATION_CHARS: usize = 80;

/// Grid a session starts at when the window has no usable size yet.
///
/// Only used before the compositor has reported a size; the retry in `sync_sizes`
/// replaces it with the real grid on the first event that carries one.
const FALLBACK_GRID: (u16, u16) = (80, 24);

/// Minimum gap between two delivered notifications from the same session.
///
/// A program can print thousands of `OSC 9` sequences in one 32 KiB PTY read.
/// Without this gate each one would sanitise its text, repaint every window and
/// spawn a helper process, so a remote host could freeze the window.
const NOTIFICATION_MIN_INTERVAL: Duration = Duration::from_millis(250);

/// Whether a session may deliver a notification at `now`.
fn notification_due(last: Option<&Instant>, now: Instant) -> bool {
    last.is_none_or(|last| now.duration_since(*last) >= NOTIFICATION_MIN_INTERVAL)
}

/// Minimum gap before an identical save or reload notice is repeated.
///
/// The watcher already debounces file events, but one burst can still reach the
/// event loop more than once. Repeating an identical notice would repaint the
/// window and file another desktop notification for the same event.
const FEEDBACK_MIN_INTERVAL: Duration = Duration::from_millis(500);

/// Whether the notice `message` may be shown again at `now`.
///
/// A different message always passes: only an identical repeat is coalesced.
fn feedback_due(last: Option<&(String, Instant)>, message: &str, now: Instant) -> bool {
    match last {
        Some((previous, at)) if previous == message => {
            now.duration_since(*at) >= FEEDBACK_MIN_INTERVAL
        }
        _ => true,
    }
}

/// Text for one settings save: what was written, or the real error.
///
/// The error is the underlying failure (validation, the external-change guard,
/// the writer, the theme file), so a failed save says why instead of a generic
/// "could not save".
fn save_feedback(what: &str, result: Result<(), String>) -> String {
    match result {
        Ok(()) => format!("{what} saved"),
        Err(error) => format!("could not save {what}: {error}"),
    }
}

/// Text for one configuration reload; `None` when the file did not change.
///
/// An invalid edit keeps its previous wording: the parse error is what tells the
/// user why the old configuration is still in force.
fn reload_feedback(outcome: &ReloadOutcome) -> Option<String> {
    match outcome {
        ReloadOutcome::Unchanged => None,
        ReloadOutcome::Reloaded(_) | ReloadOutcome::Reset(_) => Some("config reloaded".to_owned()),
        ReloadOutcome::Invalid(issues) => Some(match issues.first() {
            Some(issue) => format!("config error: {}", issue.describe()),
            None => "config error".to_owned(),
        }),
    }
}

/// Directory a pane created from another pane starts in.
///
/// The live value wins: `Pane::cwd` is written once when the pane is created and is
/// never refreshed, while `Session::cwd` follows the child's own process. `recorded`
/// is the fallback for a pane whose session is already gone.
fn parent_directory(recorded: Option<PathBuf>, live: Option<PathBuf>) -> Option<PathBuf> {
    live.or(recorded)
}

/// Events the application sends itself from other threads.
#[derive(Clone, Debug)]
pub enum UserEvent {
    /// New terminal output or an exit status is available.
    Wake,
    /// The configuration file changed on disk.
    ConfigChanged,
    /// A clipboard read finished off the event loop and is ready to send.
    ///
    /// `Ok` is the text the clipboard held or the path an image was written to, so an
    /// image paste reaches the terminal as a path any program can read.
    PasteReady {
        window: WinitWindowId,
        pane: PaneId,
        result: Result<ClipboardPaste, String>,
    },
    /// A queued clipboard write failed; the message belongs in the status bar.
    ///
    /// The write itself runs on the clipboard worker, so its outcome arrives after the
    /// frame that asked for it and cannot be returned by the call.
    ClipboardWriteFailed {
        window: WinitWindowId,
        message: String,
    },
}

/// Application state shared by all windows.
pub struct Application {
    config: ConfigService,
    keymap: Keymap,
    clipboard: Clipboard,
    /// Every clipboard write goes through this worker: opening the native handle,
    /// setting the selection and any wait inside the library all happen off the UI
    /// thread, and the handle stays alive there for as long as the worker does.
    clipboard_writer: ClipboardWriter,
    /// The copy-on-select trigger: a selection is copied when the gesture ends.
    copy_on_select: CopyOnSelect,
    sessions: astraterm_session::SessionManager,
    windows: BTreeMap<WinitWindowId, WindowUi>,
    focused: Option<WinitWindowId>,
    /// Windows that currently hold the input focus.
    ///
    /// Deliberately distinct from `focused`: that one names the window keybindings
    /// act on, is set once and is never cleared when focus moves away, so it cannot
    /// answer "is the user looking at this window right now?".
    focused_windows: BTreeSet<WinitWindowId>,
    /// Last delivered notification per session, so a flood of OSC 9 costs one
    /// comparison each instead of a redraw and a process per sequence.
    last_notification: BTreeMap<SessionId, Instant>,
    /// Last save or reload notice, so a burst of identical ones is shown once.
    last_feedback: Option<(String, Instant)>,
    /// Desktop notification backend. Degrades silently when `notify-send` is absent.
    notifications: Notifications,
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

        // One worker for every clipboard write, started before the first frame so a
        // selection never has to wait for it.
        let clipboard = Clipboard::detect();
        let clipboard_writer = clipboard.writer();

        Ok(Self {
            config: service,
            keymap,
            clipboard,
            clipboard_writer,
            copy_on_select: CopyOnSelect::default(),
            sessions: astraterm_session::SessionManager::new(),
            windows: BTreeMap::new(),
            focused: None,
            focused_windows: BTreeSet::new(),
            last_notification: BTreeMap::new(),
            last_feedback: None,
            notifications: Notifications::new(),
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
        // Composed text reaches the terminal as raw bytes, not as key events:
        // dead keys (`´` then `a`), and every character an input method composes.
        // The terminal purpose tells the platform this is not prose, so no
        // candidate box is offered for ordinary typing.
        window.set_ime_allowed(true);
        window.set_ime_purpose(ImePurpose::Terminal);
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
        // Sessions are created before the window's UI state exists, so the winsize they
        // start with is derived from the window rectangle with the same geometry the
        // first `sync_sizes` will use (S5). The renderer's size is what `WindowUi::new`
        // stores, so the two cannot disagree; when the compositor has not reported a
        // usable size yet, fall back to the historical 80x24 and let the retry in
        // `sync_sizes` correct it once a real size arrives.
        let grid = crate::ui::initial_grid(&config, metrics, renderer.scale(), renderer.size())
            .unwrap_or(FALLBACK_GRID);

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
            self.restored_workspace(&config, metrics, grid)
        } else {
            self.fresh_workspace(&config, metrics, cli_profile, grid)
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
    ///
    /// `grid` is the grid the window paints, so it is also the winsize the child
    /// starts with.
    fn fresh_workspace(
        &mut self,
        config: &Config,
        metrics: astraterm_core::CellMetrics,
        profile: Option<ProfileId>,
        grid: (u16, u16),
    ) -> (WindowState, BTreeMap<PaneId, Pane>) {
        let (cols, rows) = grid;
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
            cols,
            rows,
            cell,
        );
        if !self.cli.command.is_empty() {
            // A CLI command is an argument vector, never a shell string.
            spec.pty.program = self.cli.command[0].clone();
            spec.pty.args = self.cli.command[1..].to_vec();
        }
        let session = self.sessions.create(pane, &spec, cols, rows);

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
        grid: (u16, u16),
    ) -> (WindowState, BTreeMap<PaneId, Pane>) {
        if !config.workspace.restore_on_startup {
            return self.fresh_workspace(config, metrics, None, grid);
        }
        let path = self.state_paths.workspace_file();
        let Ok(text) = std::fs::read_to_string(&path) else {
            return self.fresh_workspace(config, metrics, None, grid);
        };
        let Ok(snapshot) = toml::from_str::<WorkspaceSnapshot>(&text) else {
            tracing::warn!(path = %path.display(), "workspace metadata is invalid; starting fresh");
            return self.fresh_workspace(config, metrics, None, grid);
        };
        let Some(window_snapshot) = snapshot.windows.first() else {
            return self.fresh_workspace(config, metrics, None, grid);
        };
        let Some(restored) = restore_window(window_snapshot, PaneId::next) else {
            return self.fresh_workspace(config, metrics, None, grid);
        };

        let (cols, rows) = grid;
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
                cols,
                rows,
                cell,
            );
            if !self.cli.command.is_empty() {
                spec.pty.program = self.cli.command[0].clone();
                spec.pty.args = self.cli.command[1..].to_vec();
            }
            if let Err(error) = self.sessions.create(*pane, &spec, cols, rows) {
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
            // OSC 52 is disabled by default; it queues on the same worker as every other
            // write, so an application cannot freeze the window with a clipboard store.
            let _ = self
                .clipboard_writer
                .set(ClipboardSelection::Clipboard, &text);
        }
        for (session, bytes) in report.replies {
            self.sessions.reply(session, &bytes);
        }
        for event in self.sessions.drain_events() {
            self.handle_session_event(event);
        }
        self.intake_images();
        if report.bytes_processed > 0 || report.exits > 0 {
            for ui in self.windows.values_mut() {
                ui.window.request_redraw();
                // Output is what moves the cursor, so the candidate window follows it
                // here instead of on a poll.
                ui.sync_ime_area(&self.sessions, false);
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
                .find(|ui| {
                    ui.panes.contains_key(&pane)
                        || ui
                            .floating
                            .as_ref()
                            .is_some_and(|floating| floating.owns_session(id))
                })
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
            SessionEvent::Notification {
                session,
                title,
                body,
            } => self.handle_notification(session, title, body),
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

    /// Turns one OSC 9 / OSC 777 request into a status message and, when the
    /// policy allows it, a window attention hint plus a desktop notification.
    ///
    /// The text comes from whatever runs in the terminal, including a remote host:
    /// it is sanitized here, before it reaches the status bar or `notify-send`, and
    /// it is never executed. The default policy only fires while the window showing
    /// the session does not have focus, so a program cannot interrupt the work in
    /// progress.
    fn handle_notification(&mut self, session: SessionId, title: Option<String>, body: String) {
        // Gate first: sanitising, painting every window and spawning a helper are
        // what turn a stream of OSC 9 sequences into a frozen UI.
        let now = Instant::now();
        if !notification_due(self.last_notification.get(&session), now) {
            return;
        }
        self.last_notification.insert(session, now);

        let label = self
            .sessions
            .get(session)
            .map(|session| {
                session
                    .title
                    .clone()
                    .or_else(|| session.label.clone())
                    .unwrap_or_default()
            })
            .unwrap_or_default();
        let body = sanitize(&body, MAX_BODY_CHARS);
        let title = title
            .map(|title| sanitize(&title, MAX_TITLE_CHARS))
            .filter(|title| !title.is_empty());

        // The status line follows the bell: every window shows what happened, even
        // when the notification itself is suppressed.
        let status = if body.is_empty() {
            format!("notify: {label}")
        } else {
            format!("notify: {}", sanitize(&body, STATUS_NOTIFICATION_CHARS))
        };
        for ui in self.windows.values_mut() {
            ui.status = Some((status.clone(), Instant::now()));
            ui.window.request_redraw();
        }

        let window_id = self.window_for_session(session);
        let window_focused = window_id.is_some_and(|id| self.focused_windows.contains(&id));
        if !should_notify(self.config.config().notifications.on, window_focused) {
            return;
        }
        if let Some(ui) = window_id.and_then(|id| self.windows.get(&id)) {
            ui.window
                .request_user_attention(Some(UserAttentionType::Informational));
        }
        let title = title.unwrap_or_else(|| {
            if label.is_empty() {
                "AstraTerm".to_owned()
            } else {
                sanitize(&label, MAX_TITLE_CHARS)
            }
        });
        self.notifications.send(&title, &body);
    }

    /// Shows a transient notice in `window_id`, dropping an identical repeat
    /// inside [`FEEDBACK_MIN_INTERVAL`].
    ///
    /// Returns whether the notice was shown, so the caller only files a desktop
    /// notification for a notice that actually happened.
    fn show_feedback(&mut self, window_id: Option<WinitWindowId>, message: &str) -> bool {
        let now = Instant::now();
        if !feedback_due(self.last_feedback.as_ref(), message, now) {
            return false;
        }
        self.last_feedback = Some((message.to_owned(), now));
        if let Some(window_id) = window_id {
            self.with_window(window_id, |ui| ui.set_status(message));
        }
        true
    }

    /// Shows a save or reload notice and, under the `[notifications] on` policy,
    /// files one desktop notification when the window does not have focus.
    ///
    /// The status line is the in-app feedback; the desktop notification is for a
    /// window the user is not looking at, which is the usual case for a reload
    /// triggered by an external edit.
    fn announce_feedback(
        &mut self,
        window_id: Option<WinitWindowId>,
        title: &str,
        message: String,
    ) {
        if !self.show_feedback(window_id, &message) {
            return;
        }
        let window_focused = window_id.is_some_and(|id| self.focused_windows.contains(&id));
        if !should_notify(self.config.config().notifications.on, window_focused) {
            return;
        }
        if let Some(ui) = window_id.and_then(|id| self.windows.get(&id)) {
            ui.window
                .request_user_attention(Some(UserAttentionType::Informational));
        }
        self.notifications.send(title, &message);
    }

    /// Shows a settings-panel message in the panel footer and announces it.
    ///
    /// The footer keeps the text for as long as the panel is open, while the
    /// status line expires; a failed save keeps both.
    fn panel_feedback(&mut self, window_id: WinitWindowId, message: String) {
        self.with_window(window_id, |ui| {
            if let Overlay::Settings(panel) = &mut ui.overlay {
                panel.set_message(message.clone());
            }
        });
        self.announce_feedback(Some(window_id), "AstraTerm", message);
    }

    /// First window whose panes are bound to `session`, if any.
    fn window_for_session(&self, session: SessionId) -> Option<WinitWindowId> {
        self.windows
            .iter()
            .find(|(_, ui)| ui.session_bindings.values().any(|bound| *bound == session))
            .map(|(id, _)| *id)
    }

    fn handle_exit(&mut self, session_id: SessionId, reason: ExitReason) {
        let close_pane = self.config.config().workspace.close_pane_when_shell_exits;
        let Some(pane) = self.sessions.get(session_id).map(|session| session.pane) else {
            return;
        };
        let description = reason.describe();

        // The floating terminal is outside the split tree, so its exit cannot be
        // planned as a mosaic pane close: the exited pane leaves the float's own
        // layout, and the last one destroys the float so the next toggle opens a
        // fresh shell.
        let floating_window = self
            .windows
            .iter()
            .find(|(_, ui)| {
                ui.floating
                    .as_ref()
                    .is_some_and(|floating| floating.owns_session(session_id))
            })
            .map(|(window_id, _)| *window_id);
        if let Some(window_id) = floating_window {
            self.close_floating_pane(window_id, pane);
            self.with_window(window_id, |ui| {
                ui.set_status(format!("floating session {description}"));
            });
            return;
        }

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
    ///
    /// The floating terminal's panes see their own sizes: the rectangles its own split
    /// tree lays out inside `float_rect`, never the size of the window (S7). A hidden
    /// floating terminal is not resized at all, so hiding it never reflows its
    /// buffers.
    fn sync_sizes(&mut self, window_id: WinitWindowId) {
        let Some((sizes, cell)) = self.windows.get(&window_id).map(|ui| {
            let sizes: Vec<(PaneId, (u16, u16))> = ui
                .pane_rects()
                .into_iter()
                .chain(ui.floating_pane_rects())
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
        let mut resized_grids: Vec<(SessionId, (i32, i32))> = Vec::new();
        // The engine matching the grid is not evidence that the child's winsize does,
        // so a session whose PTY never took the last winsize is retried here (S5).
        for (pane, (cols, rows)) in sizes {
            if let Some(session) = self.sessions.for_pane_mut(pane)
                && (session.engine.size() != (cols, rows) || !session.pty_matches(cols, rows))
            {
                session.resize(cols, rows, cell.0, cell.1);
                resized_grids.push((session.id, session.engine.line_range()));
                resized = true;
            }
        }
        // A resize reflows the buffer and an image anchor is a bare line number, so
        // the anchors the new grid kept are kept with it, off screen or not: the
        // drawing path already culls what the view does not show, and scrolling back
        // brings the image along (S5.1). Only the anchors whose line the reflow
        // removed go, and that is counted on the session (S5.2).
        for (session, range) in resized_grids {
            if let Some(images) = self.images.get_mut(&session) {
                let dropped = images.drop_evicted(range);
                if dropped > 0 {
                    tracing::debug!(
                        session = ?session,
                        dropped,
                        total = images.dropped_anchors(),
                        lines = ?range,
                        "resize removed image anchors whose buffer line is gone"
                    );
                }
            }
        }
        if resized && let Some(ui) = self.windows.get(&window_id) {
            ui.window.request_redraw();
        }
        // Every caller got here because the rectangles the terminal is painted in
        // changed, and the IME candidate window follows one of those cells, so it is
        // refreshed from the same call instead of from each site.
        self.sync_ime_area(window_id);
    }

    fn sync_all_sizes(&mut self) {
        for id in self.windows.keys().copied().collect::<Vec<_>>() {
            self.sync_sizes(id);
        }
    }

    /// Pushes the rectangle the input method's candidate window follows, when it moved.
    fn sync_ime_area(&mut self, window_id: WinitWindowId) {
        self.update_ime_area(window_id, false);
    }

    /// Pushes it even when it did not move.
    ///
    /// The platform forgets the caret at the start of every input-method session, and a
    /// preedit re-anchors the candidate window, so those two moments re-send it.
    fn resend_ime_area(&mut self, window_id: WinitWindowId) {
        self.update_ime_area(window_id, true);
    }

    fn update_ime_area(&mut self, window_id: WinitWindowId, force: bool) {
        if let Some(ui) = self.windows.get_mut(&window_id) {
            ui.sync_ime_area(&self.sessions, force);
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
                    // The floating terminal is not in the split tree, so closing one of
                    // its panes closes it inside the float, and closing the last one
                    // destroys the float and its sessions (S7).
                    let floats = self
                        .windows
                        .get(&window_id)
                        .is_some_and(|ui| closes_floating(ui.floating.as_ref(), pane));
                    if floats {
                        self.close_floating_pane(window_id, pane);
                    } else {
                        self.request_close_pane(window_id, pane);
                    }
                }
            }
            AppCommand::SplitPane { axis, .. } => {
                tracing::debug!(?axis, ?focused_pane, "split requested");
                // A visible floating terminal owns the keys, so the split lands inside
                // it and the mosaic keeps its panes; otherwise a new pane joins the
                // mosaic (S7).
                let floats = self
                    .windows
                    .get(&window_id)
                    .is_some_and(|ui| split_lands_in_floating(ui.floating.as_ref()));
                let result = if floats {
                    self.split_floating(window_id, axis)
                } else {
                    match self
                        .windows
                        .get(&window_id)
                        .and_then(WindowUi::mosaic_pane)
                        .or(focused_pane)
                    {
                        Some(pane) => self.split_pane(window_id, pane, axis),
                        None => {
                            tracing::debug!("no focused pane to split");
                            Ok(())
                        }
                    }
                };
                if let Err(error) = result {
                    tracing::error!(error = %error, "could not split the pane");
                }
            }
            AppCommand::FocusPane { pane } => {
                // The focus follows the layout that holds the pane: a floating pane
                // focuses inside the float and a mosaic pane inside its tab, so the
                // keyboard keeps reaching the pane that was asked for (S7).
                let focused = self
                    .with_window(window_id, |ui| ui.focus_pane(pane))
                    .unwrap_or(false);
                if focused {
                    self.with_window(window_id, |ui| ui.window.request_redraw());
                }
            }
            AppCommand::FocusDirection { direction } => {
                // While the float is visible it owns the keys, so the focus moves
                // between its panes and stops at its edge instead of reaching the
                // mosaic behind it (S7).
                self.with_window(window_id, |ui| {
                    ui.focus_direction(direction);
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
                        if let Some(tab) = ui.driven_tab_mut() {
                            tab.root.adjust_ratio(split, delta_percent as f32 / 100.0);
                        }
                    });
                    self.sync_sizes(window_id);
                    self.with_window(window_id, |ui| ui.window.request_redraw());
                }
            }
            AppCommand::EqualizeSplits => {
                self.with_window(window_id, |ui| {
                    if let Some(tab) = ui.driven_tab_mut() {
                        tab.root.equalize();
                    }
                });
                self.sync_sizes(window_id);
                self.with_window(window_id, |ui| ui.window.request_redraw());
            }
            AppCommand::ToggleZoom => {
                self.with_window(window_id, |ui| {
                    if let Some(tab) = ui.driven_tab_mut() {
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
            AppCommand::ToggleFloating => self.toggle_floating(window_id),
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

        // Any command can move the focus, the cursor, the split rectangles or the cell
        // metrics the candidate window follows; doing it once here keeps every arm from
        // having to remember it. It is a no-op when nothing moved.
        self.sync_ime_area(window_id);
    }

    /// Divider the resize shortcuts act on, in the layout the keyboard drives.
    fn focused_split(
        &self,
        window_id: WinitWindowId,
        axis: SplitAxis,
    ) -> Option<astraterm_workspace::tree::SplitId> {
        let ui = self.windows.get(&window_id)?;
        let tab = ui.driven_tab()?;
        // The dividers live in the rectangle their layout was computed in: the
        // floating box for the float, the terminal area otherwise.
        let rect = if split_lands_in_floating(ui.floating.as_ref()) {
            ui.floating_rect()?
        } else {
            ui.terminal_rect()
        };
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

    /// Live working directory of `pane`, falling back to the directory recorded
    /// when the pane was created.
    ///
    /// `Session::cwd` is refreshed from `/proc/<pid>/cwd`, while `Pane::cwd` is
    /// written once at creation: only the live value says where the user is now.
    fn parent_live_cwd(&self, pane: Option<PaneId>, recorded: Option<PathBuf>) -> Option<PathBuf> {
        let live = pane
            .and_then(|pane| self.sessions.for_pane(pane))
            .and_then(|session| session.cwd.clone());
        parent_directory(recorded, live)
    }

    fn open_tab(
        &mut self,
        window_id: WinitWindowId,
        profile: Option<ProfileId>,
    ) -> anyhow::Result<()> {
        let config = self.config.config().clone();
        let Some((cols, rows, cell, parent, recorded_cwd)) =
            self.windows.get(&window_id).map(|ui| {
                let (cols, rows) = ui.grid_size(ui.terminal_rect());
                // The parent is the active tab's pane, not the pane that owns the
                // keyboard: while the floating terminal is visible the keyboard is the
                // float's and its directory must not leak into a mosaic tab (S6).
                let parent = ui.mosaic_pane();
                let recorded_cwd = parent
                    .and_then(|pane| ui.panes.get(&pane))
                    .and_then(|meta| meta.cwd.clone());
                (
                    cols,
                    rows,
                    (
                        ui.metrics.width.round().max(1.0) as u16,
                        ui.metrics.height.max(1) as u16,
                    ),
                    parent,
                    recorded_cwd,
                )
            })
        else {
            return Ok(());
        };

        // A new tab starts where the active tab's focused pane is now, not where that
        // pane was created (S3, S4, S6).
        let live_cwd = self.parent_live_cwd(parent, recorded_cwd);
        let profile_ref =
            profile.and_then(|id| config.profiles.get(id.raw().saturating_sub(1) as usize));
        let spec = astraterm_session::session_spec_inheriting(
            &config,
            profile_ref,
            profile,
            live_cwd,
            cols,
            rows,
            cell,
        );
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

    /// `toggle-floating`: creates, shows or hides the window's floating terminal.
    ///
    /// The three states are deliberate and small:
    ///
    /// * nothing open yet — create its own session and show it;
    /// * visible — hide it, leaving the session and its PTY untouched;
    /// * hidden — show it again with the shell state it kept.
    ///
    /// Hiding never resizes or writes to the session, so a long-running job in the
    /// floating shell keeps running at the size it had.
    fn toggle_floating(&mut self, window_id: WinitWindowId) {
        let Some(press) = self
            .windows
            .get(&window_id)
            .map(|ui| plan_floating_press(ui.floating.as_ref()))
        else {
            return;
        };
        match press {
            FloatingPress::Create => {
                if let Err(error) = self.open_floating(window_id) {
                    tracing::error!(error = %error, "could not open the floating terminal");
                    self.with_window(window_id, |ui| {
                        ui.set_status(format!("floating terminal failed: {error}"));
                    });
                }
            }
            FloatingPress::Hide => {
                self.with_window(window_id, |ui| {
                    if let Some(floating) = ui.floating.as_mut() {
                        floating.set_visible(false);
                    }
                    ui.window.request_redraw();
                });
            }
            FloatingPress::Show => {
                self.with_window(window_id, |ui| {
                    if let Some(floating) = ui.floating.as_mut() {
                        floating.set_visible(true);
                    }
                    ui.window.request_redraw();
                });
                // The window may have changed size while it was hidden, so the PTY
                // catches up with the rectangle before the first frame is drawn.
                self.sync_sizes(window_id);
            }
        }
    }

    /// Creates the floating terminal: its own session, the spec a new pane gets.
    ///
    /// The session is created at the grid that fits the floating rectangle, never at
    /// the size of the window, so it starts at the size it is drawn at.
    fn open_floating(&mut self, window_id: WinitWindowId) -> anyhow::Result<()> {
        let config = self.config.config().clone();
        let Some((cols, rows, cell, mosaic, recorded_cwd, profile_id)) =
            self.windows.get(&window_id).map(|ui| {
                let (cols, rows) = ui.grid_size(crate::chrome::float_rect(ui.terminal_rect()));
                let mosaic = ui.mosaic_pane();
                let recorded_cwd = mosaic
                    .and_then(|pane| ui.panes.get(&pane))
                    .and_then(|meta| meta.cwd.clone());
                let profile_id = mosaic
                    .and_then(|pane| ui.panes.get(&pane))
                    .and_then(|meta| meta.profile);
                (
                    cols,
                    rows,
                    (
                        ui.metrics.width.round().max(1.0) as u16,
                        ui.metrics.height.max(1) as u16,
                    ),
                    mosaic,
                    recorded_cwd,
                    profile_id,
                )
            })
        else {
            return Ok(());
        };

        // The floating terminal opens where the pane behind it is now (S1, S4).
        let live_cwd = self.parent_live_cwd(mosaic, recorded_cwd);
        let profile_ref =
            profile_id.and_then(|id| config.profiles.get(id.raw().saturating_sub(1) as usize));
        let pane = PaneId::next();
        let spec = astraterm_session::session_spec_inheriting(
            &config,
            profile_ref,
            profile_id,
            live_cwd,
            cols,
            rows,
            cell,
        );
        let session = self.sessions.create(pane, &spec, cols, rows)?;

        // The metadata and the binding keep the floating session in the window's
        // lifecycle: closing or destroying the window closes it like any other pane.
        let mut meta = Pane::new(pane);
        meta.cwd = spec.pty.cwd.clone();
        meta.profile = profile_id;
        if let Some(label) = spec.label.clone() {
            meta.title = label;
        }
        self.with_window(window_id, |ui| {
            ui.panes.insert(pane, meta);
            ui.session_bindings.insert(pane, session);
            ui.floating = Some(Floating::new(pane, session));
            ui.window.request_redraw();
        });
        self.sync_sizes(window_id);
        Ok(())
    }

    /// Splits the focused pane of the floating terminal (S7).
    ///
    /// The new pane joins the float's own layout, so the mosaic's pane count, ratios
    /// and sizes stay exactly as they were, and nothing about the float is persisted
    /// (S5).
    fn split_floating(&mut self, window_id: WinitWindowId, axis: SplitAxis) -> anyhow::Result<()> {
        let config = self.config.config().clone();
        let Some((cols, rows, cell, parent, recorded_cwd, profile_id)) = self
            .windows
            .get(&window_id)
            .and_then(|ui| {
                let rect = ui.floating_rect()?;
                let (cols, rows) = ui.grid_size(rect);
                let parent = ui.floating.as_ref()?.active_pane();
                let meta = ui.panes.get(&parent);
                Some((
                    cols,
                    rows,
                    (
                        ui.metrics.width.round().max(1.0) as u16,
                        ui.metrics.height.max(1) as u16,
                    ),
                    parent,
                    meta.and_then(|meta| meta.cwd.clone()),
                    meta.and_then(|meta| meta.profile),
                ))
            })
        else {
            return Ok(());
        };

        // The policy decides first, exactly as it does for a mosaic split: a profile
        // or `[terminal] working_directory` directory wins, and the split pane's live
        // directory only resolves a `current`/`inherit` policy (S4). Under the default
        // `home` policy the new floating pane therefore starts in `$HOME`, like a
        // mosaic split and unlike a new tab or the float itself, which inherit.
        let cwd = self.parent_live_cwd(Some(parent), recorded_cwd);
        let profile_ref =
            profile_id.and_then(|id| config.profiles.get(id.raw().saturating_sub(1) as usize));
        let new_pane = PaneId::next();
        // The first guess is the half a split of this axis leaves behind; the exact
        // grid follows in `sync_sizes` once the layout exists.
        let (start_cols, start_rows) = match axis {
            SplitAxis::Vertical => ((cols / 2).max(1), rows),
            SplitAxis::Horizontal => (cols, (rows / 2).max(1)),
        };
        let spec = astraterm_session::session_spec(
            &config,
            profile_ref,
            profile_id,
            cwd.clone(),
            start_cols,
            start_rows,
            cell,
        );
        let session = self.sessions.create(new_pane, &spec, start_cols, start_rows)?;

        let split = self
            .with_window(window_id, |ui| {
                ui.floating
                    .as_mut()
                    .is_some_and(|floating| floating.split_active(axis, new_pane, session))
            })
            .unwrap_or(false);
        if !split {
            tracing::warn!(?axis, "the floating terminal refused the new pane");
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
        Ok(())
    }

    /// Closes one pane of the floating terminal.
    ///
    /// Closing a pane that is not the last one leaves the float with its remaining
    /// panes; closing the last one destroys the float and its sessions, exactly as the
    /// single-pane float did (S7).
    fn close_floating_pane(&mut self, window_id: WinitWindowId, pane: PaneId) {
        let Some(session) = self.windows.get(&window_id).and_then(|ui| {
            let floating = ui.floating.as_ref()?;
            // A pane the float does not hold is not closed here: failing to close it
            // must not be mistaken for "this was the last pane".
            if !floating.contains(pane) {
                return None;
            }
            floating.session_of(pane)
        }) else {
            return;
        };
        let closed = self
            .with_window(window_id, |ui| {
                let closed = ui
                    .floating
                    .as_mut()
                    .is_some_and(|floating| floating.close_pane(pane));
                if closed {
                    ui.panes.remove(&pane);
                    ui.session_bindings.remove(&pane);
                    ui.window.request_redraw();
                }
                closed
            })
            .unwrap_or(false);
        if !closed {
            self.close_floating(window_id);
            return;
        }
        self.sessions.close(session);
        self.last_notification.remove(&session);
        self.sync_sizes(window_id);
    }

    /// Destroys the floating terminal and every session it holds.
    ///
    /// Used when its last pane closes and by the exit of a single-pane float's shell.
    /// Focus needs no bookkeeping: with no floating terminal left the keyboard resolves
    /// to the mosaic, exactly as before it was opened.
    fn close_floating(&mut self, window_id: WinitWindowId) {
        let Some((panes, sessions)) = self.windows.get(&window_id).and_then(|ui| {
            let floating = ui.floating.as_ref()?;
            Some((
                floating.panes(),
                floating.session_ids().collect::<Vec<_>>(),
            ))
        }) else {
            return;
        };
        self.with_window(window_id, |ui| {
            ui.floating = None;
            for pane in panes {
                ui.panes.remove(&pane);
                ui.session_bindings.remove(&pane);
            }
            ui.window.request_redraw();
        });
        for session in sessions {
            self.sessions.close(session);
            self.last_notification.remove(&session);
        }
        self.sync_sizes(window_id);
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
            self.last_notification.remove(&session);
        }
        ui.window.set_visible(false);
        drop(ui);

        self.focused_windows.remove(&window_id);
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

    /// Copies the selection of the focused pane.
    ///
    /// This is the explicit copy (`ctrl+shift+c`) and it never waits: the write runs on
    /// the clipboard worker, which is what stopped an explicit copy from freezing the
    /// window when the clipboard stalled.
    fn copy_selection(&mut self, window_id: WinitWindowId) {
        let Some(text) = self.selection_text(window_id) else {
            return;
        };
        self.queue_copy(window_id, text);
    }

    /// The selection text of the focused pane, if there is one.
    fn selection_text(&self, window_id: WinitWindowId) -> Option<String> {
        let pane = self
            .windows
            .get(&window_id)
            .and_then(WindowUi::focused_pane)?;
        self.sessions.for_pane(pane).and_then(|s| s.selection_text())
    }

    /// Queues `text` on the clipboard worker and reports it in the status bar.
    ///
    /// The call returns as soon as the text is queued; the write's own failure, if it
    /// comes at all, arrives later as [`UserEvent::ClipboardWriteFailed`].
    fn queue_copy(&mut self, window_id: WinitWindowId, text: String) {
        let proxy = self.proxy.clone();
        let queued = self.clipboard_writer.set_reported(
            ClipboardSelection::Clipboard,
            &text,
            move |result| {
                if let Err(message) = result {
                    let _ = proxy.send_event(UserEvent::ClipboardWriteFailed {
                        window: window_id,
                        message,
                    });
                }
            },
        );
        let message = match queued {
            Ok(()) => format!("copied {} character(s)", text.chars().count()),
            // The platform distinguishes "no clipboard backend" from "the write ran and
            // failed", so the message says which one happened.
            Err(error) => error,
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
        // Reading the selection waits for the owner to answer — and an image takes longer
        // than text — so it runs off the event loop; the payload comes back as an event.
        let clipboard = self.clipboard.clone();
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            let result = clipboard.read(ClipboardSelection::Clipboard);
            let _ = proxy.send_event(UserEvent::PasteReady {
                window: window_id,
                pane,
                result,
            });
        });
    }

    /// Sends a clipboard payload that was read off the event loop.
    ///
    /// Text is pasted as text; an image was already written to a temporary file by the
    /// platform, so what reaches the PTY is that path, encoded exactly like typed text.
    fn finish_paste(
        &mut self,
        window_id: WinitWindowId,
        pane: PaneId,
        result: Result<ClipboardPaste, String>,
    ) {
        let payload = match result {
            Ok(payload) => payload,
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
        let encoded = match &payload {
            ClipboardPaste::Text(text) => paste::encode_pasted_text(text, modes),
            ClipboardPaste::Image { path } => paste::encode_pasted_image(path, modes),
        };
        match encoded {
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
        let outcome = self.config.reload_if_changed();
        match &outcome {
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
            }
        }

        let Some(message) = reload_feedback(&outcome) else {
            return;
        };
        // An invalid edit keeps its previous behaviour: it is reported in the
        // window the user works in and never raises the desktop, because nothing
        // changed and the notice is a correction, not an interruption.
        if matches!(&outcome, ReloadOutcome::Invalid(_)) {
            self.show_feedback(self.focused, &message);
        } else {
            self.announce_feedback(self.focused, "AstraTerm", message);
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
            self.sync_ime_area(window_id);
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
        // A key can move the cursor through the child's echo (which arrives as output),
        // through a shortcut that switched pane or tab, or through a scroll; the area
        // is re-derived here and sent only if the cell actually changed.
        self.sync_ime_area(window_id);
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
                    Ok((stem, _)) => {
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
                                self.with_window(window_id, |ui| {
                                    if let Overlay::Settings(panel) = &mut ui.overlay {
                                        panel.set_themes(themes);
                                        panel.on_saved(&config, issues);
                                    }
                                });
                                self.announce_feedback(
                                    Some(window_id),
                                    "AstraTerm",
                                    save_feedback("theme", Ok(())),
                                );
                            }
                            Err(error) => {
                                let message = format!("theme saved but not selected: {error}");
                                self.panel_feedback(window_id, message);
                            }
                        }
                    }
                    Err(error) => {
                        let message = save_feedback("theme", Err(error.to_string()));
                        self.panel_feedback(window_id, message);
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
                    self.announce_feedback(
                        Some(window_id),
                        "AstraTerm",
                        save_feedback("settings", Ok(())),
                    );
                }
                Err(SaveError::Invalid(issues)) => {
                    let detail = issues
                        .first()
                        .map(|issue| issue.describe())
                        .unwrap_or_default();
                    let message = save_feedback("settings", Err(detail));
                    self.with_window(window_id, |ui| {
                        if let Overlay::Settings(panel) = &mut ui.overlay {
                            panel.set_message(message.clone());
                            panel.set_issues(issues);
                        }
                    });
                    self.announce_feedback(Some(window_id), "AstraTerm", message);
                }
                Err(SaveError::ExternalChange) => {
                    let message =
                        save_feedback("settings", Err(SaveError::ExternalChange.to_string()));
                    self.panel_feedback(window_id, message);
                }
                Err(SaveError::Write(error)) => {
                    let message = save_feedback("settings", Err(error.to_string()));
                    self.panel_feedback(window_id, message);
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
        // Remembered so a platform that also commits this character through the
        // input method does not make the PTY insert it twice.
        if let TermKey::Char(_) = press.key
            && let Some(text) = event.text.as_ref()
        {
            let text = text.to_string();
            let now = Instant::now();
            self.with_window(window_id, |ui| ui.last_key_text = Some((text, now)));
        }
    }

    /// Delivers text the platform composed (dead keys, input methods) to the pane.
    ///
    /// The text is written as raw UTF-8: it never passes through the shortcut or
    /// key-encoding paths, so `|` from `AltGr+1` and `á` from a dead key reach the
    /// child process unchanged.
    fn handle_ime(&mut self, window_id: WinitWindowId, ime: Ime) {
        match ime {
            Ime::Commit(text) => {
                self.commit_composed_text(window_id, &text);
                self.sync_ime_area(window_id);
            }
            // Preedit is not drawn: the composition is visible once committed.
            Ime::Preedit(text, _) => {
                tracing::trace!(preedit = %text, "ime preedit");
                // A preedit update re-anchors the candidate window at the caret, so the
                // platform is told again even though the cell did not move.
                self.resend_ime_area(window_id);
            }
            // A fresh input-method session starts with no caret at all.
            Ime::Enabled => self.resend_ime_area(window_id),
            Ime::Disabled => {}
        }
    }

    fn commit_composed_text(&mut self, window_id: WinitWindowId, text: &str) {
        if text.is_empty() {
            return;
        }
        let Some(ui) = self.windows.get(&window_id) else {
            return;
        };
        // Overlays (search, settings, rename, confirmation) read their own text
        // input from key events and never receive composed terminal text.
        if !matches!(ui.overlay, Overlay::None) {
            return;
        }
        let Some(pane) = ui.focused_pane() else {
            return;
        };
        let last = ui
            .last_key_text
            .as_ref()
            .map(|(written, at)| (written.as_str(), *at));
        if keys::commit_repeats_key(text, last, Instant::now()) {
            return;
        }
        let bytes = text.as_bytes();
        if let Some(session) = self.sessions.for_pane_mut(pane) {
            session.write(bytes);
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

        // The pointer resolves the pane under the cursor through `click_pane`, so a
        // click inside the floating terminal also makes that pane the float's active
        // pane: the keys and the next split then follow the pane the user picked
        // instead of whichever pane a split or a close left focused (S7).
        let Some((pane, column, row)) = self.windows.get_mut(&window_id).and_then(|ui| {
            let pane = ui.click_pane(x, y)?;
            let (column, row) = ui.cell_at(pane, x, y)?;
            Some((pane, column, row))
        }) else {
            return;
        };
        // A click can move the focus to another pane, mosaic or floating, and the
        // candidate window follows the cursor of the pane that owns the keyboard.
        self.sync_ime_area(window_id);
        let Some(modes) = self
            .sessions
            .for_pane(pane)
            .map(|session| session.engine.modes())
        else {
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
            // The release ends the gesture, so this is the moment a selection exists to
            // copy: the motions that grew it never reach the clipboard.
            if selecting {
                let enabled = self.config.config().terminal.copy_on_select;
                let text = self.selection_text(window_id);
                if let Some(text) = self.copy_on_select.completed(enabled, text) {
                    self.queue_copy(window_id, text);
                }
            }
        }
    }

    /// Divider drag under the pointer, if any.
    ///
    /// The floating terminal is drawn over the mosaic, so a point inside its box
    /// resolves against its own dividers and never against one that lies behind it
    /// (S7).
    fn divider_at(&self, window_id: WinitWindowId, x: f64, y: f64) -> Option<DragState> {
        let ui = self.windows.get(&window_id)?;
        let tolerance = (ui.layout.divider.max(2) as f64) + 2.0;
        if let Some(rect) = ui.floating_rect()
            && rect.contains(x, y)
        {
            return divider_drag_at(&ui.floating_dividers(), rect, tolerance, x, y);
        }
        let tab = ui.workspace.active_tab()?;
        let rect = ui.terminal_rect();
        divider_drag_at(&tab.dividers(rect, ui.layout), rect, tolerance, x, y)
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
        let opened = self.sessions.for_pane_mut(pane).and_then(|session| {
            let snapshot = session.snapshot();
            let line = snapshot.lines.get(row as usize)?;
            open_clicked_url(
                snapshot.cell(column, row),
                line,
                column as usize,
                linux::open_url,
            )
        });
        let Some(opened) = opened else { return false };
        let message = match opened.result {
            Ok(()) => format!("opened {}", opened.url),
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
                // Motion only grows the selection. The clipboard write belongs to the
                // release that completes the gesture (`CopyOnSelect`), so a drag that
                // moves the pointer fifty times queues nothing.
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
                        // A split identifier is unique in the process, so the tree that
                        // owns the divider is unambiguous: a float drag resizes only the
                        // float's panes, a mosaic drag only the mosaic's (S7).
                        ui.adjust_split_ratio(split.0, delta);
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
        // Scrolling the viewport moves or hides the cursor row the candidate window
        // follows.
        self.sync_ime_area(window_id);
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
            UserEvent::ClipboardWriteFailed { window, message } => {
                self.with_window(window, |ui| ui.set_status(message));
            }
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
                    self.focused_windows.insert(window_id);
                    self.focused = Some(window_id);
                    self.with_window(window_id, |ui| {
                        ui.cursor_phase = true;
                        // The user is looking at this window now, so no attention
                        // hint is owed. Wayland treats `None` as a no-op; X11 clears
                        // the urgency flag with it.
                        ui.window.request_user_attention(None);
                        ui.window.request_redraw();
                    });
                    self.report_focus(window_id, true);
                    // The input method starts a new session with no caret, so the area
                    // is sent again even though the cursor did not move.
                    self.resend_ime_area(window_id);
                } else {
                    self.focused_windows.remove(&window_id);
                    self.report_focus(window_id, false);
                }
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                self.mods = modifiers.state();
            }
            WindowEvent::KeyboardInput { event, .. } => {
                self.handle_key(event_loop, window_id, &event)
            }
            WindowEvent::Ime(ime) => self.handle_ime(window_id, ime),
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
                let frame = match self.windows.get_mut(&window_id) {
                    Some(ui) => ui.compose(&mut self.sessions, &system, &self.images),
                    None => return,
                };
                if let Some(ui) = self.windows.get_mut(&window_id)
                    && let Err(error) = ui.renderer.render(&frame.scene, frame.overlay.as_ref())
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
mod notification_gate_tests {
    use std::time::{Duration, Instant};

    use super::{NOTIFICATION_MIN_INTERVAL, notification_due};

    #[test]
    fn a_flood_of_notifications_is_coalesced_per_session() {
        let now = Instant::now();
        assert!(notification_due(None, now), "the first one always passes");
        assert!(
            !notification_due(Some(&now), now),
            "a second sequence in the same instant is dropped"
        );
        assert!(!notification_due(
            Some(&(now - Duration::from_millis(1))),
            now
        ));
        assert!(notification_due(
            Some(&(now - NOTIFICATION_MIN_INTERVAL)),
            now
        ));
    }
}

#[cfg(test)]
mod feedback_tests {
    use std::time::Instant;

    use astraterm_config::ConfigIssue;
    use astraterm_config::service::ReloadOutcome;

    use super::{FEEDBACK_MIN_INTERVAL, feedback_due, reload_feedback, save_feedback};

    #[test]
    fn a_successful_save_names_what_was_written() {
        assert_eq!(save_feedback("settings", Ok(())), "settings saved");
        assert_eq!(save_feedback("theme", Ok(())), "theme saved");
    }

    #[test]
    fn a_failed_save_keeps_the_real_error() {
        assert_eq!(
            save_feedback("settings", Err("permission denied".to_owned())),
            "could not save settings: permission denied"
        );
    }

    #[test]
    fn a_valid_reload_says_so_and_an_unchanged_file_stays_silent() {
        assert_eq!(
            reload_feedback(&ReloadOutcome::Reloaded(Box::default())),
            Some("config reloaded".to_owned())
        );
        assert_eq!(
            reload_feedback(&ReloadOutcome::Reset(Box::default())),
            Some("config reloaded".to_owned())
        );
        assert_eq!(reload_feedback(&ReloadOutcome::Unchanged), None);
    }

    #[test]
    fn an_invalid_reload_keeps_the_parse_error() {
        let issue = ConfigIssue::error("terminal.font_size", "must be between 6 and 72");
        assert_eq!(
            reload_feedback(&ReloadOutcome::Invalid(vec![issue])),
            Some("config error: error: terminal.font_size: must be between 6 and 72".to_owned())
        );
        assert_eq!(
            reload_feedback(&ReloadOutcome::Invalid(Vec::new())),
            Some("config error".to_owned()),
            "an invalid file without issues still reports the failure"
        );
    }

    #[test]
    fn an_identical_notice_in_a_burst_is_shown_once() {
        let now = Instant::now();
        assert!(feedback_due(None, "config reloaded", now));
        assert!(!feedback_due(
            Some(&("config reloaded".to_owned(), now)),
            "config reloaded",
            now
        ));
        assert!(
            feedback_due(
                Some(&("settings saved".to_owned(), now)),
                "config reloaded",
                now
            ),
            "a different notice is never coalesced"
        );
        assert!(feedback_due(
            Some(&("config reloaded".to_owned(), now)),
            "config reloaded",
            now + FEEDBACK_MIN_INTERVAL
        ));
    }
}

#[cfg(test)]
mod parent_directory_tests {
    use std::path::PathBuf;

    use super::parent_directory;

    #[test]
    fn the_live_directory_wins_over_the_one_recorded_at_creation() {
        // `Pane::cwd` is a creation-time snapshot; a shell that ran `cd` only shows
        // up in `Session::cwd`. A child pane must inherit the live value (S1, S3).
        assert_eq!(
            parent_directory(
                Some(PathBuf::from("/started/here")),
                Some(PathBuf::from("/moved/here"))
            ),
            Some(PathBuf::from("/moved/here"))
        );
    }

    #[test]
    fn a_pane_without_a_live_session_falls_back_to_the_recorded_directory() {
        assert_eq!(
            parent_directory(Some(PathBuf::from("/started/here")), None),
            Some(PathBuf::from("/started/here"))
        );
    }

    #[test]
    fn a_window_with_no_parent_pane_leaves_the_choice_to_the_policy() {
        assert_eq!(parent_directory(None, None), None);
    }
}

#[cfg(test)]
mod new_tab_parent_tests {
    use std::path::PathBuf;

    use astraterm_core::{PaneId, SessionId, WindowId};
    use astraterm_workspace::{Tab, WindowState};

    use super::parent_directory;
    use crate::ui::{Floating, keyboard_pane, tab_parent};

    #[test]
    fn a_new_tab_opens_where_the_active_tab_is_while_the_float_has_the_focus() {
        let mosaic = PaneId::from_raw(3);
        let mut workspace = WindowState::new(WindowId::from_raw(1), Tab::new("mosaic", mosaic));
        // A second tab exists, so "the active tab" is a real choice and not just the
        // only tab there is.
        workspace.add_tab(Tab::new("other", PaneId::from_raw(5)));
        assert!(workspace.select_index(0));

        let mut floating = Floating::new(PaneId::from_raw(7), SessionId::from_raw(21));
        floating.set_visible(true);

        // Premise of the bug: the visible float owns the keyboard, so a parent taken
        // from the focus would name the floating pane.
        assert_eq!(
            keyboard_pane(Some(&floating), tab_parent(&workspace)),
            Some(PaneId::from_raw(7))
        );

        // A new tab is opened from the active tab instead (T4, S6).
        let parent = tab_parent(&workspace);
        assert_eq!(parent, Some(mosaic));

        let live = |pane: PaneId| -> Option<PathBuf> {
            match pane {
                pane if pane == mosaic => Some(PathBuf::from("/work/mosaic")),
                pane if pane == floating.active_pane() => Some(PathBuf::from("/work/float")),
                _ => None,
            }
        };
        let inherited = parent_directory(None, parent.and_then(live));
        assert_eq!(
            inherited,
            Some(PathBuf::from("/work/mosaic")),
            "the tab starts in the active tab's directory"
        );
        assert_ne!(
            inherited,
            live(floating.active_pane()),
            "the floating pane's directory must not leak into a mosaic tab"
        );
        // The regression this pins: taking the parent from the pane that owns the keys
        // — what the tab path did before — resolves to the floating pane and inherits
        // the wrong directory.
        assert_eq!(
            parent_directory(
                None,
                keyboard_pane(Some(&floating), parent).and_then(live)
            ),
            Some(PathBuf::from("/work/float"))
        );
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
