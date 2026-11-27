//! Per-window UI state and scene composition.
//!
//! `WindowUi` owns one window: its renderer, workspace (tabs and split tree), pane
//! metadata, chrome state (tab strip, status bar, overlays) and pointer
//! interaction state. It never owns PTY sessions; those live in the application so
//! that several windows can share one manager.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use astraterm_config::schema::{Config, TabBarPosition};
use astraterm_config::themes::Theme;
use astraterm_core::{
    CellMetrics, FocusDirection, PaneId, Rect, SessionId, Size, SplitAxis, TabId, WindowId,
};
use astraterm_platform::linux;
use astraterm_platform::system::SystemSnapshot;
use astraterm_renderer::Renderer;
use astraterm_renderer::paint::{self, ImageItem, ImagePixels, PaintOptions, Scene, TextItem};
use astraterm_session::SessionManager;
use astraterm_settings_ui::panel::{PanelStyle, SettingsPanel};
use astraterm_terminal::TerminalEngine;
use astraterm_terminal::image::{ImagePlacement, ImageStore};
use astraterm_terminal::image_decode::{DecodeLimits, ImageDecoder};
use astraterm_terminal::snapshot::{CellSnapshot, RowSnapshot, Snapshot};
use astraterm_workspace::tree::SplitId;
use astraterm_workspace::{LayoutConfig, Pane, Tab, WindowState};

use crate::chrome;
use crate::ime::{self, CursorCell};
use crate::prompt;
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::window::Window;

/// Height of the tab strip in cell rows.
const TAB_STRIP_ROWS: f32 = 1.0;
/// Base padding added to the tab strip and status bar, in device pixels.
const CHROME_PADDING: u32 = 6;
/// Width of the active-tab indicator, in device pixels.
const TAB_INDICATOR_WIDTH: u32 = 3;
/// How long a transient status message stays on screen.
const STATUS_LIFETIME: Duration = Duration::from_secs(4);

/// Blink period of the text cursor.
pub const CURSOR_BLINK_PERIOD: Duration = Duration::from_millis(530);

/// How long the bell's border flash stays on screen (S6).
///
/// Long enough to be noticed on a 60 Hz screen — about two dozen frames — and short
/// enough that it never competes with the message in the status bar.
pub const BELL_FLASH_DURATION: Duration = Duration::from_millis(400);

/// Thickness of the bell's border flash, in device pixels.
const BELL_FLASH_THICKNESS: u32 = 2;

/// Scene item id of the input method's composition (S4).
///
/// Chrome starts at 1 000 000 and the floating terminal at 0x20_0000, so this keeps
/// the composition in a range of its own; the shaping cache therefore never confuses
/// it with a row, a label or the cursor.
const PREEDIT_ID: u64 = 0x0E_0000;

/// Scene item id base of the floating terminal.
///
/// Chrome uses the fixed ranges from 1_000_000 up and mosaic rows use
/// `(pane_index + 1) * 0x1000`, so this range is unique in the scene and its shaped
/// rows stay cached across frames.
const FLOATING_ID_BASE: u64 = 0x20_0000;

/// Distance between the scene item id bases of two floating panes.
///
/// `paint::paint_pane` numbers one item per terminal line and puts the cursor at
/// `base + 0x10_000`, so the stride has to stay wider than that offset plus the
/// tallest pane: 0x20_0000 lines is far more than any window shows. With it, the
/// shaping cache can never confuse two floating panes with each other or with a
/// mosaic pane.
const FLOATING_PANE_STRIDE: u64 = 0x20_0000;

/// Scene item id base of the floating pane at `index`.
///
/// The first pane keeps the historical base, so a single-pane floating terminal
/// paints exactly the identifiers it always did.
pub fn floating_id_base(index: usize) -> u64 {
    FLOATING_ID_BASE + index as u64 * FLOATING_PANE_STRIDE
}

/// Decides whether the cursor blink should toggle now, and when to check again.
///
/// The period is passed in rather than read from the configuration, because the idle
/// frame budget may stretch it (S10). A blink that re-armed itself on every frame would
/// spin the event loop at frame rate; this gate keeps it at exactly two toggles per
/// period, and `CURSOR_BLINK_PERIOD` is what the application passes when the budget
/// leaves the blink alone.
pub fn blink_due_with(
    now: Instant,
    deadline: Option<Instant>,
    period: Duration,
) -> (bool, Instant) {
    match deadline {
        Some(deadline) if now < deadline => (false, deadline),
        Some(deadline) => {
            // Catch up without ever firing more than once per call.
            let next = deadline + period;
            (true, if next <= now { now + period } else { next })
        }
        None => (true, now + period),
    }
}

/// What currently captures keyboard input.
pub enum Overlay {
    None,
    Search(SearchOverlay),
    Rename(RenameOverlay),
    Settings(Box<SettingsPanel>),
    Confirm(ConfirmDialog),
}

/// Inline rename prompt for the active tab.
#[derive(Clone, Debug)]
pub struct RenameOverlay {
    pub tab: TabId,
    pub buffer: String,
}

impl RenameOverlay {
    pub fn new(tab: TabId, current: &str) -> Self {
        Self {
            tab,
            buffer: current.to_owned(),
        }
    }

    /// Title to apply: trimmed, and empty means "go back to the derived title".
    pub fn committed_title(&self) -> String {
        self.buffer.trim().to_owned()
    }

    pub fn push(&mut self, ch: char) {
        if self.buffer.chars().count() < 120 {
            self.buffer.push(ch);
        }
    }

    pub fn pop(&mut self) {
        self.buffer.pop();
    }
}

impl std::fmt::Debug for Overlay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Overlay::None => write!(f, "Overlay::None"),
            Overlay::Search(search) => write!(f, "Overlay::Search({:?})", search.query),
            Overlay::Rename(rename) => write!(f, "Overlay::Rename({:?})", rename.buffer),
            Overlay::Settings(_) => write!(f, "Overlay::Settings"),
            Overlay::Confirm(confirm) => write!(f, "Overlay::Confirm({:?})", confirm.message),
        }
    }
}

/// Interactive scrollback search state.
#[derive(Clone, Debug, Default)]
pub struct SearchOverlay {
    pub query: String,
    pub case_sensitive: bool,
    /// Matches found on the visible screen, as `(row, start column, length)`.
    pub highlights: Vec<(usize, usize, usize)>,
    /// Index of the current highlight.
    pub current: usize,
}

/// A pending confirmation shown modally.
#[derive(Clone, Debug)]
pub struct ConfirmDialog {
    pub message: String,
    pub action: PendingAction,
}

/// What happens when the user accepts a confirmation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PendingAction {
    ClosePane(PaneId),
    CloseTab(TabId),
    CloseWindow(WindowId),
    Quit,
}

/// The window's floating terminal.
///
/// It is deliberately *outside* the workspace: the mosaic layout and the reflow
/// never see it, so the mosaic's splits, zooms and divider drags ignore it, and it
/// is never persisted (S5). It keeps its own layout, so it can hold several panes
/// with their own dividers and sizes (S7); several *tabs* inside it are a separate
/// task (T6).
///
/// A window owns at most one, and it keeps its own sessions, which survive being
/// hidden: hiding only flips `visible` and never touches a PTY.
#[derive(Clone, Debug, PartialEq)]
pub struct Floating {
    /// Its own split layout: the focused pane, the zoom and the divider ratios of
    /// the floating panes. One tab only.
    pub tab: Tab,
    /// Session of every floating pane, by pane, which is how the keyboard, paste
    /// and the mouse reach them. A hide/show cycle never replaces a session.
    pub sessions: BTreeMap<PaneId, SessionId>,
    /// Whether it is currently drawn and owns the keyboard.
    pub visible: bool,
}

impl Floating {
    /// A floating terminal showing `session` in a single pane, created by the first
    /// toggle press.
    pub fn new(pane: PaneId, session: SessionId) -> Self {
        let mut sessions = BTreeMap::new();
        sessions.insert(pane, session);
        Self {
            tab: Tab::new(String::new(), pane),
            sessions,
            visible: true,
        }
    }

    /// Shows or hides it without touching any session.
    pub fn set_visible(&mut self, visible: bool) {
        self.visible = visible;
    }

    /// Whether it exists but is not on screen: the status bar reports this state.
    pub fn is_hidden(&self) -> bool {
        !self.visible
    }

    /// Pane that has the focus inside the floating terminal.
    pub fn active_pane(&self) -> PaneId {
        self.tab.zoomed.unwrap_or(self.tab.active_pane)
    }

    /// Makes `pane` the floating terminal's focused pane.
    ///
    /// Returns `false` for a pane the float does not hold, so no caller can hand the
    /// float's focus to a mosaic pane. Focus survives a hide/show cycle because it
    /// lives in the float's tab, which hiding never touches.
    pub fn focus(&mut self, pane: PaneId) -> bool {
        self.tab.focus(pane)
    }

    /// Focuses the floating pane under a pointer position.
    ///
    /// Hit testing and focusing are one step so the pointer path can never report a
    /// floating pane without making it the float's active pane. `None` means the
    /// point hit no floating pane, in which case the focus is left alone.
    pub fn focus_at(&mut self, rects: &[(PaneId, Rect)], x: f64, y: f64) -> Option<PaneId> {
        let pane = pane_at_point(rects, x, y)?;
        self.focus(pane).then_some(pane)
    }

    /// Moves the float's focus one pane in `direction`.
    ///
    /// `rect` is the float's box and `layout` the layout config, exactly as a mosaic
    /// tab does it, so the traversal is geometric. Returns `false` when nothing lies
    /// that way: the focus stays inside the float and never jumps to the mosaic
    /// behind it.
    pub fn focus_direction(
        &mut self,
        direction: FocusDirection,
        rect: Rect,
        layout: LayoutConfig,
    ) -> bool {
        self.tab.focus_direction(direction, rect, layout)
    }

    /// Panes of the floating terminal, in layout order.
    pub fn panes(&self) -> Vec<PaneId> {
        self.tab.panes()
    }

    pub fn pane_count(&self) -> usize {
        self.tab.pane_count()
    }

    /// Whether `pane` belongs to the floating terminal instead of the mosaic.
    pub fn contains(&self, pane: PaneId) -> bool {
        self.tab.root.contains(pane)
    }

    /// Session bound to one floating pane.
    pub fn session_of(&self, pane: PaneId) -> Option<SessionId> {
        self.sessions.get(&pane).copied()
    }

    /// Sessions of every floating pane.
    pub fn session_ids(&self) -> impl Iterator<Item = SessionId> + '_ {
        self.sessions.values().copied()
    }

    /// Whether `session` is one of the floating terminal's sessions.
    pub fn owns_session(&self, session: SessionId) -> bool {
        self.sessions.values().any(|bound| *bound == session)
    }

    /// Splits the focused floating pane and binds `session` to the new one.
    ///
    /// Returns `false` when the layout refused the new pane, in which case the
    /// caller must close the session it created.
    pub fn split_active(&mut self, axis: SplitAxis, pane: PaneId, session: SessionId) -> bool {
        if self.tab.split_focused(axis, pane).is_none() {
            return false;
        }
        self.sessions.insert(pane, session);
        true
    }

    /// Closes one floating pane, leaving the others on screen.
    ///
    /// Returns `false` for the last pane, which is the caller's cue to destroy the
    /// floating terminal and its sessions instead.
    pub fn close_pane(&mut self, pane: PaneId) -> bool {
        if !self.tab.close_pane(pane) {
            return false;
        }
        self.sessions.remove(&pane);
        true
    }
}

/// What a `toggle-floating` press does next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FloatingPress {
    /// Nothing open yet: create a session and show it.
    Create,
    /// Hidden: show it again with the shell state it kept.
    Show,
    /// Visible: hide it, leaving the session and its PTY alone.
    Hide,
}

/// Decides what `toggle-floating` does, given the current floating terminal.
pub fn plan_floating_press(floating: Option<&Floating>) -> FloatingPress {
    match floating {
        None => FloatingPress::Create,
        Some(floating) if floating.visible => FloatingPress::Hide,
        Some(_) => FloatingPress::Show,
    }
}

/// Pane that owns the keyboard: the floating terminal while it is visible,
/// otherwise the mosaic pane of the active tab.
///
/// Hiding the floating terminal hands the keyboard back to the mosaic pane that
/// kept the focus all along, so no focus has to be remembered across a hide.
pub fn keyboard_pane(floating: Option<&Floating>, mosaic: Option<PaneId>) -> Option<PaneId> {
    match floating {
        Some(floating) if floating.visible => Some(floating.active_pane()),
        _ => mosaic,
    }
}

/// Whether `ClosePane` aims at the floating terminal instead of a mosaic pane.
pub fn closes_floating(floating: Option<&Floating>, pane: PaneId) -> bool {
    floating.is_some_and(|floating| floating.contains(pane))
}

/// Whether a split asked for right now lands inside the floating terminal.
///
/// The floating terminal owns the keyboard while it is visible, so a split asked
/// for then joins its own layout and leaves the mosaic's pane count alone (S7). A
/// new tab never does: it always joins the mosaic.
pub fn split_lands_in_floating(floating: Option<&Floating>) -> bool {
    floating.is_some_and(|floating| floating.visible)
}

/// Pane a new tab inherits its directory from: the focused pane of the tab that is
/// active when it is opened (S6).
///
/// It is read from the workspace instead of from the pane that owns the keyboard,
/// because while the floating terminal is visible the keyboard belongs to a
/// floating pane and its directory must not leak into a mosaic tab.
pub fn tab_parent(workspace: &WindowState) -> Option<PaneId> {
    workspace
        .active_tab()
        .map(|tab| tab.zoomed.unwrap_or(tab.active_pane))
}

/// Whether one pane of a layout draws its border, which is also whether its images
/// are inset by that border's pixel.
///
/// `paint_pane` and `compose_images` must agree on this value or an image lands a
/// pixel off the cells it was anchored to. A mosaic pane shows a border only when
/// its tab holds more than one pane, so a lone mosaic pane fills its area. The
/// floating terminal always shows one — even when it holds a single pane — because
/// the float is a box drawn over the mosaic and the border is what separates it
/// from the text behind it. That is what the single-pane float painted before the
/// float could be split, and a split must not move its images or its glyphs.
///
/// `floating` is `true` for a floating pane and `panes` is the pane count of the
/// layout the pane belongs to.
pub fn pane_shows_border(floating: bool, panes: usize) -> bool {
    floating || panes > 1
}

/// Pointer drag in progress.
#[derive(Clone, Debug)]
pub enum DragState {
    None,
    /// Text selection inside a pane.
    Selection {
        pane: PaneId,
    },
    /// Divider being dragged.
    Divider {
        split: SplitIdRef,
        axis: SplitAxis,
        start: f64,
        extent: f64,
        applied: f32,
    },
}

/// Reference to a split node (the workspace crate keeps the real identifier).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SplitIdRef(pub u64);

/// Pane of `rects` under the pointer, if any.
pub fn pane_at_point(rects: &[(PaneId, Rect)], x: f64, y: f64) -> Option<PaneId> {
    rects
        .iter()
        .find(|(_, rect)| rect.contains(x, y))
        .map(|(pane, _)| *pane)
}

/// Divider drag under the pointer, if any.
///
/// `owner` is the rectangle the dividers were computed in: the mosaic terminal for
/// the workspace tree, the floating box for the floating terminal. The drag is
/// normalised by that rectangle's extent, so a floating divider moves the floating
/// panes and nothing else (S7).
pub fn divider_drag_at(
    dividers: &[(SplitId, SplitAxis, Rect)],
    owner: Rect,
    tolerance: f64,
    x: f64,
    y: f64,
) -> Option<DragState> {
    for (split, axis, divider) in dividers {
        let (hit, start, extent) = match axis {
            SplitAxis::Vertical => (
                (x - divider.x as f64).abs() <= tolerance
                    && y >= divider.y as f64
                    && y <= divider.bottom() as f64,
                x,
                owner.width as f64,
            ),
            SplitAxis::Horizontal => (
                (y - divider.y as f64).abs() <= tolerance
                    && x >= divider.x as f64
                    && x <= divider.right() as f64,
                y,
                owner.height as f64,
            ),
        };
        if hit {
            return Some(DragState::Divider {
                split: SplitIdRef(split.raw()),
                axis: *axis,
                start,
                extent: extent.max(1.0),
                applied: 0.0,
            });
        }
    }
    None
}

/// Repeated-click tracking for double/triple click selection.
#[derive(Clone, Copy, Debug)]
pub struct ClickInfo {
    pub at: Instant,
    pub column: u16,
    pub row: u16,
    pub count: u8,
}

/// One composed frame, ready for the renderer.
///
/// The mosaic always owns the whole window and clears it. `overlay` is the layer
/// drawn afterwards on top of the mosaic's glyphs: the floating terminal, with the
/// modal overlays above it. It is `None` when neither is on screen, so a window
/// with nothing above the panes records a single render pass.
pub struct Frame {
    pub scene: Scene,
    pub overlay: Option<Scene>,
}

/// Rectangle the layer above the mosaic is confined to, or `None` when there is
/// nothing to draw above it.
///
/// A visible floating terminal covers only its own box, and the renderer scissors
/// the layer to it. A modal overlay dims and paints the whole window, so while one
/// is open the layer keeps the window and the dialog is not cut off at the float's
/// edge.
pub fn upper_layer_clip(float: Option<Rect>, modal: bool, size: Size) -> Option<Rect> {
    match (float, modal) {
        (None, false) => None,
        (Some(rect), false) => Some(rect),
        _ => Some(Rect::new(0, 0, size.width, size.height)),
    }
}

/// Padding in device pixels.
fn padding_of(config: &Config, scale: f32) -> astraterm_core::Padding {
    astraterm_core::Padding {
        x: (config.appearance.padding_x as f32 * scale) as u32,
        y: (config.appearance.padding_y as f32 * scale) as u32,
    }
}

/// Height of the tab strip in device pixels, zero when it is hidden.
fn tab_strip_height_of(config: &Config, metrics: CellMetrics) -> u32 {
    match config.appearance.tab_bar_position {
        TabBarPosition::Hidden => 0,
        _ => (metrics.height as f32 * TAB_STRIP_ROWS) as u32 + CHROME_PADDING,
    }
}

/// Height of the status bar in device pixels, zero when it is hidden.
fn status_bar_height_of(config: &Config, metrics: CellMetrics) -> u32 {
    if config.appearance.show_status_bar {
        metrics.height + CHROME_PADDING
    } else {
        0
    }
}

/// Width of the vertical tab strip in device pixels, zero for the other positions.
fn tab_strip_width_of(config: &Config, scale: f32, size: Size) -> u32 {
    match config.appearance.tab_bar_position {
        TabBarPosition::Right => {
            let requested = (config.appearance.tab_strip_width as f32 * scale) as u32;
            requested.min(size.width.saturating_sub(160))
        }
        _ => 0,
    }
}

/// Rectangle available to the terminal panes of a window of `size`.
fn terminal_rect_of(config: &Config, metrics: CellMetrics, scale: f32, size: Size) -> Rect {
    chrome::terminal_rect(
        config.appearance.tab_bar_position,
        size,
        tab_strip_height_of(config, metrics),
        tab_strip_width_of(config, scale, size),
        status_bar_height_of(config, metrics),
    )
}

/// Grid size for a pane rectangle of a window at `scale`.
fn grid_size_of(config: &Config, metrics: CellMetrics, scale: f32, rect: Rect) -> (u16, u16) {
    let padding = padding_of(config, scale);
    let inner = Rect {
        x: rect.x,
        y: rect.y,
        width: rect.width.saturating_sub(2 * padding.x),
        height: rect.height.saturating_sub(2 * padding.y),
    };
    metrics.grid_size(inner)
}

/// Grid a window of `size` paints, computed before its [`WindowUi`] exists.
///
/// Sessions are created before the window's UI state, and the winsize a session
/// starts with has to be the grid the first frame paints (S5). This repeats the
/// geometry `WindowUi::terminal_rect` and `WindowUi::grid_size` use, so the initial
/// winsize and the first `sync_sizes` cannot disagree.
///
/// `None` when the compositor has not reported a usable size yet: callers then fall
/// back to their historical default and the retry in `sync_sizes` corrects it once a
/// real size arrives.
pub fn initial_grid(
    config: &Config,
    metrics: CellMetrics,
    scale: f32,
    size: Size,
) -> Option<(u16, u16)> {
    if size.width == 0 || size.height == 0 {
        return None;
    }
    let rect = terminal_rect_of(config, metrics, scale, size);
    Some(grid_size_of(config, metrics, scale, rect))
}

/// One window and everything it draws.
pub struct WindowUi {
    pub id: WindowId,
    pub window: Arc<Window>,
    pub renderer: Renderer,
    pub workspace: WindowState,
    pub panes: BTreeMap<PaneId, Pane>,
    pub theme: Theme,
    pub config: Config,
    pub layout: LayoutConfig,
    pub metrics: CellMetrics,
    pub size: Size,
    pub scale: f32,
    pub overlay: Overlay,
    pub drag: DragState,
    pub last_click: Option<ClickInfo>,
    pub cursor_phase: bool,
    pub status: Option<(String, Instant)>,
    /// Text the last key press wrote, so an input-method commit that repeats it is
    /// dropped instead of inserting the character twice.
    pub last_key_text: Option<(String, Instant)>,
    /// Session ids bound to panes, so the window can be torn down cleanly.
    pub session_bindings: BTreeMap<PaneId, astraterm_core::SessionId>,
    /// The floating terminal, if one was opened in this window.
    pub floating: Option<Floating>,
    /// Last rectangle handed to the platform for the input method's candidate window.
    ///
    /// `None` means nothing was sent yet. Kept so the same caret is not reported again
    /// on every keystroke and every frame of output: `set_ime_cursor_area` is a platform
    /// call the compositor cannot make anything new of.
    last_ime_area: Option<Rect>,
    /// The composition an input method is in the middle of (S4).
    ///
    /// It is painted at the cursor and nowhere else; the grid the pane is drawn from
    /// never sees it, so a composition can grow, shrink and vanish without writing a
    /// cell or marking one damaged.
    pub preedit: Option<ime::Preedit>,
    /// When the bell's border flash ends, while one is running (S6).
    pub bell_flash: Option<Instant>,
}

impl WindowUi {
    /// Creates the UI state for a window.
    pub fn new(
        window: Arc<Window>,
        renderer: Renderer,
        workspace: WindowState,
        panes: BTreeMap<PaneId, Pane>,
        theme: Theme,
        config: Config,
        metrics: CellMetrics,
    ) -> Self {
        let scale = renderer.scale();
        let size = renderer.size();
        let layout = layout_config(&config);
        Self {
            id: workspace.id,
            window,
            renderer,
            workspace,
            panes,
            theme,
            config,
            layout,
            metrics,
            size,
            scale,
            overlay: Overlay::None,
            drag: DragState::None,
            last_click: None,
            cursor_phase: true,
            status: None,
            last_key_text: None,
            session_bindings: BTreeMap::new(),
            floating: None,
            last_ime_area: None,
            preedit: None,
            bell_flash: None,
        }
    }

    /// Applies configuration and theme changes without recreating the window.
    pub fn apply_config(&mut self, config: &Config, theme: Theme) {
        if config.font != self.config.font {
            self.renderer.set_font(&config.font);
            self.metrics = self.renderer.cell_metrics();
        }
        self.scale = self.renderer.scale();
        self.layout = layout_config(config);
        self.config = config.clone();
        self.theme = theme;
        self.renderer.invalidate_text_cache();
        self.window.request_redraw();
    }

    /// Updates the window size and PTY geometry inputs.
    pub fn resize(&mut self, size: winit::dpi::PhysicalSize<u32>) {
        self.renderer.resize(size);
        self.size = Size::new(size.width.max(1), size.height.max(1));
        self.metrics = self.renderer.cell_metrics();
        self.window.request_redraw();
    }

    /// Padding in device pixels.
    pub fn padding(&self) -> astraterm_core::Padding {
        padding_of(&self.config, self.scale)
    }

    /// Height of the tab strip, zero when hidden.
    pub fn tab_strip_height(&self) -> u32 {
        tab_strip_height_of(&self.config, self.metrics)
    }

    pub fn status_bar_height(&self) -> u32 {
        status_bar_height_of(&self.config, self.metrics)
    }

    /// Width of the vertical tab strip, zero for the other positions.
    pub fn tab_strip_width(&self) -> u32 {
        tab_strip_width_of(&self.config, self.scale, self.size)
    }

    /// Rectangle available to the terminal panes.
    pub fn terminal_rect(&self) -> Rect {
        terminal_rect_of(&self.config, self.metrics, self.scale, self.size)
    }

    /// Rectangle of the tab strip.
    pub fn tab_strip_rect(&self) -> Rect {
        chrome::strip_rect(
            self.config.appearance.tab_bar_position,
            self.size,
            self.tab_strip_height(),
            self.tab_strip_width(),
            self.status_bar_height(),
        )
    }

    /// Height of one tab in the vertical strip.
    pub fn tab_row_height(&self) -> u32 {
        self.metrics.height + CHROME_PADDING
    }

    /// Cuts a label so it fits in `max_width` device pixels, with an ellipsis.
    pub fn fit_label(&self, label: &str, max_width: u32) -> String {
        chrome::fit_label(label, max_width, self.metrics.width)
    }

    /// Rectangle of the status bar.
    pub fn status_rect(&self) -> Rect {
        chrome::status_rect(
            self.config.appearance.show_status_bar,
            self.size,
            self.status_bar_height(),
        )
    }

    /// Pane rectangles for the active tab.
    pub fn pane_rects(&self) -> Vec<(PaneId, Rect)> {
        let Some(tab) = self.workspace.active_tab() else {
            return Vec::new();
        };
        tab.layout(self.terminal_rect(), self.layout)
            .into_iter()
            .map(|placement| (placement.pane, placement.rect))
            .collect()
    }

    /// Grid size for a pane rectangle.
    pub fn grid_size(&self, rect: Rect) -> (u16, u16) {
        grid_size_of(&self.config, self.metrics, self.scale, rect)
    }

    /// Pane rectangles of the floating terminal, inside its own box.
    ///
    /// Empty while there is no floating terminal or it is hidden: a hidden float
    /// has no on-screen geometry, so nothing is painted, hit-tested or resized
    /// through it.
    pub fn floating_pane_rects(&self) -> Vec<(PaneId, Rect)> {
        let Some(floating) = self.floating.as_ref() else {
            return Vec::new();
        };
        let Some(rect) = self.floating_rect() else {
            return Vec::new();
        };
        floating
            .tab
            .layout(rect, self.layout)
            .into_iter()
            .map(|placement| (placement.pane, placement.rect))
            .collect()
    }

    /// Dividers of the floating terminal, in window coordinates.
    pub fn floating_dividers(&self) -> Vec<(SplitId, SplitAxis, Rect)> {
        let Some(floating) = self.floating.as_ref() else {
            return Vec::new();
        };
        let Some(rect) = self.floating_rect() else {
            return Vec::new();
        };
        floating.tab.dividers(rect, self.layout)
    }

    /// Pane under a pointer position.
    pub fn pane_at(&self, x: f64, y: f64) -> Option<PaneId> {
        // The floating terminal is drawn over the mosaic, so it answers first and a
        // point inside its box never resolves to the pane behind it.
        if let Some(rect) = self.floating_rect()
            && rect.contains(x, y)
        {
            return pane_at_point(&self.floating_pane_rects(), x, y);
        }
        pane_at_point(&self.pane_rects(), x, y)
    }

    /// Pane under a pointer position, focusing it when it is a floating pane.
    ///
    /// This is what a click resolves through, so a point inside the floating
    /// terminal makes the pane under the pointer the float's active pane instead of
    /// only reporting the hit: without it the float's focus would stay on whichever
    /// pane a split or a close last touched, and the original pane of a split could
    /// never be reached again (S7). A mosaic pane is only resolved — a click there
    /// selects text and never moves the mosaic's focus, which is how it behaved
    /// before the float could hold more than one pane.
    pub fn click_pane(&mut self, x: f64, y: f64) -> Option<PaneId> {
        if !self.floating_rect().is_some_and(|rect| rect.contains(x, y)) {
            return self.pane_at(x, y);
        }
        let rects = self.floating_pane_rects();
        self.floating
            .as_mut()
            .and_then(|floating| floating.focus_at(&rects, x, y))
    }

    /// Makes `pane` the focused pane of the layout that holds it.
    ///
    /// A floating pane focuses inside the float, so this can never hand the float's
    /// focus to a mosaic pane or the mosaic's focus to a float; the keyboard keeps
    /// reaching the pane that was asked for.
    pub fn focus_pane(&mut self, pane: PaneId) -> bool {
        if let Some(floating) = self.floating.as_mut()
            && floating.contains(pane)
        {
            return floating.focus(pane);
        }
        self.workspace
            .active_tab_mut()
            .is_some_and(|tab| tab.focus(pane))
    }

    /// Moves the focus one pane in `direction` inside the layout the keys drive.
    ///
    /// The floating terminal drives the keys while it is visible, so the focus moves
    /// between its panes and stops at the float's edge instead of jumping into the
    /// mosaic behind it.
    pub fn focus_direction(&mut self, direction: FocusDirection) -> bool {
        let layout = self.layout;
        if let Some(rect) = self.floating_rect()
            && let Some(floating) = self.floating.as_mut()
        {
            return floating.focus_direction(direction, rect, layout);
        }
        let rect = self.terminal_rect();
        self.workspace
            .active_tab_mut()
            .is_some_and(|tab| tab.focus_direction(direction, rect, layout))
    }

    /// Rectangle of a pane on screen, mosaic or floating.
    pub fn pane_rect(&self, pane: PaneId) -> Option<Rect> {
        if self
            .floating
            .as_ref()
            .is_some_and(|floating| floating.contains(pane))
        {
            return self
                .floating_pane_rects()
                .into_iter()
                .find(|(id, _)| *id == pane)
                .map(|(_, rect)| rect);
        }
        self.pane_rects()
            .into_iter()
            .find(|(id, _)| *id == pane)
            .map(|(_, rect)| rect)
    }

    /// The pane's rectangle without its one-pixel focus border.
    ///
    /// `paint::paint_pane` shrinks a framed pane before it places a single cell, and a
    /// pane is framed when the mosaic is split and always when it is the floating
    /// terminal. This is that same inner rectangle, shared by everything that has to
    /// land on the grid instead of on the frame.
    pub fn pane_inner_rect(&self, pane: PaneId) -> Option<Rect> {
        let rect = self.pane_rect(pane)?;
        let bordered = match self.floating.as_ref() {
            Some(floating) if floating.contains(pane) => {
                pane_shows_border(true, floating.pane_count())
            }
            _ => self
                .workspace
                .active_tab()
                .is_some_and(|tab| pane_shows_border(false, tab.pane_count())),
        };
        Some(if bordered { rect.shrunk(1) } else { rect })
    }

    /// Rectangle the platform should anchor the input method's candidate window to.
    ///
    /// `cursor` is the cursor cell of [`WindowUi::focused_pane`]'s session. With no
    /// cursor on screen the answer is a zero-size area inside the same pane (see
    /// [`crate::ime::no_cursor_area`]): there is no cell to follow, so the platform
    /// places the candidate list itself, but it is still told where the terminal is.
    pub fn ime_area(&self, cursor: Option<CursorCell>) -> Rect {
        let padding = self.padding();
        let Some(inner) = self
            .focused_pane()
            .and_then(|pane| self.pane_inner_rect(pane))
        else {
            return ime::no_cursor_area(self.terminal_rect(), padding);
        };
        ime::cursor_area(inner, padding, self.metrics, cursor)
            .unwrap_or_else(|| ime::no_cursor_area(inner, padding))
    }

    /// Cursor cell the renderer draws for the pane that owns the keyboard.
    ///
    /// Read through the engine's cheap accessors instead of a snapshot: a snapshot
    /// walks and allocates the whole visible grid, and this runs on every batch of
    /// output as well as on every key. [`ime::cursor_cell`] owns the rule, and a test
    /// pins it against the snapshot the renderer draws from.
    pub fn cursor_cell(&self, sessions: &SessionManager) -> Option<CursorCell> {
        let engine = &sessions.for_pane(self.focused_pane()?)?.engine;
        let point = engine.cursor_point();
        let (cols, rows) = engine.size();
        ime::cursor_cell(
            point.line.0,
            point.column.0,
            engine.display_offset(),
            cols,
            rows,
            engine.modes().show_cursor,
        )
    }

    /// Tells the platform where the candidate window should follow, when it moved.
    ///
    /// `force` sends the rectangle even when it did not change, for the two moments the
    /// platform has forgotten it: a newly enabled input method and every preedit
    /// update, which re-anchors the popup at the caret. The cost is the same pane
    /// rectangle the frame being drawn already needs — no snapshot, no redraw request —
    /// and the platform is only called when the rectangle actually changed.
    pub fn sync_ime_area(&mut self, sessions: &SessionManager, force: bool) {
        let area = self.ime_area(self.cursor_cell(sessions));
        if !force && !ime::needs_update(self.last_ime_area, area) {
            return;
        }
        self.last_ime_area = Some(area);
        self.window.set_ime_cursor_area(
            PhysicalPosition::new(area.x, area.y),
            PhysicalSize::new(area.width, area.height),
        );
    }

    /// Cell coordinates inside a pane for a pointer position.
    pub fn cell_at(&self, pane: PaneId, x: f64, y: f64) -> Option<(u16, u16)> {
        let rect = self.pane_rect(pane)?;
        let padding = self.padding();
        let origin = (rect.x + padding.x as i32, rect.y + padding.y as i32);
        if x < origin.0 as f64 || y < origin.1 as f64 {
            return None;
        }
        let (col, row) =
            Rect::new(origin.0, origin.1, rect.width, rect.height).to_cell(x, y, self.metrics);
        if col < 0 || row < 0 {
            return None;
        }
        Some((col as u16, row as u16))
    }

    /// Tab index at a pointer position, if the tab strip was hit.
    pub fn tab_at(&self, x: f64, y: f64) -> Option<usize> {
        let strip = self.tab_strip_rect();
        if !strip.contains(x, y) {
            return None;
        }
        if self.config.appearance.tab_bar_position == TabBarPosition::Right {
            return chrome::vertical_tab_hit(
                strip,
                self.tab_row_height(),
                self.workspace.tabs.len(),
                y,
            );
        }
        let widths: Vec<u32> = self
            .workspace
            .tabs
            .iter()
            .map(|tab| self.tab_width(tab))
            .collect();
        chrome::horizontal_tab_hit(strip, &widths, x)
    }

    /// Width of one tab label.
    pub fn tab_width(&self, tab: &Tab) -> u32 {
        let label = self.tab_label(tab);
        let chars = label.chars().count() as f32;
        let extra = if self.config.appearance.compact_tabs {
            16.0
        } else {
            28.0
        };
        ((chars * self.metrics.width) + extra * self.scale).max(48.0 * self.scale) as u32
    }

    /// Label shown in the tab strip.
    pub fn tab_label(&self, tab: &Tab) -> String {
        if !tab.title.is_empty() {
            return tab.title.clone();
        }
        let pane = self.active_pane_of(tab);
        let pane_meta = pane.and_then(|pane| self.panes.get(&pane));
        match pane_meta {
            Some(meta) => {
                let shell = &self.config.terminal.shell;
                let title = meta.display_title(shell);
                if tab.zoomed.is_some() {
                    format!("{title} [z]")
                } else {
                    title
                }
            }
            None => "shell".to_owned(),
        }
    }

    fn active_pane_of(&self, tab: &Tab) -> Option<PaneId> {
        Some(tab.zoomed.unwrap_or(tab.active_pane))
    }

    /// Currently focused pane of the active tab, ignoring the floating terminal.
    pub fn mosaic_pane(&self) -> Option<PaneId> {
        tab_parent(&self.workspace)
    }

    /// Layout the keyboard drives: the floating terminal's while it is visible,
    /// the active mosaic tab otherwise.
    pub fn driven_tab(&self) -> Option<&Tab> {
        match self.floating.as_ref() {
            Some(floating) if floating.visible => Some(&floating.tab),
            _ => self.workspace.active_tab(),
        }
    }

    /// Mutable form of [`WindowUi::driven_tab`].
    pub fn driven_tab_mut(&mut self) -> Option<&mut Tab> {
        match self.floating.as_mut() {
            Some(floating) if floating.visible => Some(&mut floating.tab),
            _ => self.workspace.active_tab_mut(),
        }
    }

    /// Adjusts the ratio of the divider named by its raw identifier.
    ///
    /// A split identifier is unique in the process, so the floating terminal's tree
    /// and the mosaic's are searched in turn and only the owner is resized: dragging
    /// a floating divider never moves a mosaic pane, and the other way round (S7).
    pub fn adjust_split_ratio(&mut self, split: u64, delta: f32) -> bool {
        let layout = self.layout;
        if let Some(rect) = self.floating_rect()
            && let Some(floating) = self.floating.as_mut()
            && let Some(id) = split_with_raw(&floating.tab, rect, layout, split)
        {
            return floating.tab.root.adjust_ratio(id, delta);
        }
        let rect = self.terminal_rect();
        let Some(tab) = self.workspace.active_tab_mut() else {
            return false;
        };
        let Some(id) = split_with_raw(tab, rect, layout, split) else {
            return false;
        };
        tab.root.adjust_ratio(id, delta)
    }

    /// Pane that owns the keyboard right now.
    ///
    /// A visible floating terminal always has the focus; hiding it returns the
    /// keyboard to the mosaic pane the tab had selected all along.
    pub fn focused_pane(&self) -> Option<PaneId> {
        keyboard_pane(self.floating.as_ref(), self.mosaic_pane())
    }

    /// Rectangle of the floating terminal, or `None` when there is none or it is
    /// hidden. It is the single source of truth for drawing, hit testing and the
    /// PTY size of the floating pane.
    pub fn floating_rect(&self) -> Option<Rect> {
        let floating = self.floating.as_ref()?;
        floating
            .visible
            .then(|| chrome::float_rect(self.terminal_rect()))
    }

    /// Sets a transient status message.
    pub fn set_status(&mut self, message: impl Into<String>) {
        self.status = Some((message.into(), Instant::now()));
        self.window.request_redraw();
    }

    /// Composes the frame.
    ///
    /// The mosaic is composed first: tab strip, panes, status bar. Everything that
    /// must cover the terminal — the floating terminal and the modal overlays — is
    /// composed into the second layer, which the renderer draws after the mosaic's
    /// glyphs; that is what stops the mosaic's text from painting over the floating
    /// terminal.
    pub fn compose(
        &mut self,
        sessions: &mut SessionManager,
        system: &SystemSnapshot,
        images: &BTreeMap<SessionId, SessionImages>,
    ) -> Frame {
        let opacity = self.config.window.opacity.clamp(0.1, 1.0);
        let mut scene = Scene::with_clear(self.theme.palette.background, opacity);

        self.compose_tab_strip(&mut scene);

        let focused = self.focused_pane();
        let rects = self.pane_rects();
        // A mosaic tab frames its panes only once it is split; the images follow the
        // same decision as the glyphs so an image stays on its cells.
        let show_border = pane_shows_border(false, rects.len());
        for (index, (pane, rect)) in rects.iter().enumerate() {
            let live = sessions.for_pane_mut(*pane);
            let Some(session) = live else {
                // A pane without a session (startup failure) still shows its border.
                scene.push_quad(*rect, self.theme.palette.pane_border, 1.0);
                continue;
            };
            let snapshot = session.snapshot();
            let session_id = session.id;
            let options = PaintOptions {
                padding: self.padding(),
                draw_cursor: focused == Some(*pane) && self.cursor_phase,
                // The translucent terminal background is painted once by the scene
                // clear; painting the pane background again would compound alpha and
                // make the window look opaque.
                background_alpha: 0.0,
                focused: focused == Some(*pane),
                show_border,
                // Shades are painted as flat blends only when the user asked for
                // foot's substitution; the default keeps the font's dither.
                solid_shades: self.config.font.solid_shades,
            };
            paint::paint_pane(
                &mut scene,
                (index as u64 + 1) * 0x1000,
                *rect,
                &snapshot,
                &self.theme.palette,
                self.metrics,
                options,
                &mut self.renderer.measure(),
            );

            // Images are anchored to buffer lines, so they are drawn from the
            // pane's session and placed on the same grid the glyphs use.
            if let Some(anchored) = images.get(&session_id) {
                self.compose_images(&mut scene, *rect, show_border, &snapshot, anchored);
            }

            if let Overlay::Search(search) = &self.overlay {
                self.paint_search_highlights(&mut scene, *rect, &snapshot, search);
            }

            // Both are drawn over the pane's own cells and both belong to the pane that
            // owns the keyboard: the bell's flash marks it, and the composition can only
            // be typed into it.
            if focused == Some(*pane) {
                self.compose_bell_flash(&mut scene, *rect, show_border);
                self.compose_preedit(&mut scene, *rect, show_border, &snapshot);
            }
        }

        self.compose_status_bar(&mut scene, sessions, system);
        let overlay = self.compose_upper_layer(sessions, images);
        Frame { scene, overlay }
    }

    /// Composes the layer drawn after the mosaic: the floating terminal, and the
    /// modal overlays above it.
    ///
    /// Both share one layer because a layer is drawn quads-then-glyphs: the modal
    /// chrome is pushed after the floating terminal's, so a dialog still covers it
    /// exactly as it did while both lived in the mosaic scene, and its own labels
    /// still come last. Nothing covers the floating terminal. The layer is scissored
    /// to its own rectangle (see [`upper_layer_clip`]), so no row of the floating
    /// terminal can paint outside its box.
    fn compose_upper_layer(
        &mut self,
        sessions: &mut SessionManager,
        images: &BTreeMap<SessionId, SessionImages>,
    ) -> Option<Scene> {
        let modal = !matches!(self.overlay, Overlay::None);
        let clip = upper_layer_clip(self.floating_rect(), modal, self.size)?;
        let mut scene = Scene {
            clip: Some(clip),
            ..Scene::default()
        };
        self.compose_floating(&mut scene, sessions, images);
        self.compose_overlay(&mut scene, sessions);
        Some(scene)
    }

    /// Paints the floating terminal into the layer above the mosaic.
    ///
    /// Every floating pane covers its own box with an opaque background, so the
    /// mosaic never shows through its cells, and its rows are clipped to that box by
    /// `paint_pane`'s per-row text bounds. Each pane takes its own scene item id base
    /// (see [`floating_id_base`]), so the shaping cache keeps the floating panes
    /// apart from each other and from the mosaic.
    fn compose_floating(
        &mut self,
        scene: &mut Scene,
        sessions: &mut SessionManager,
        images: &BTreeMap<SessionId, SessionImages>,
    ) {
        let Some(floating) = self.floating.as_ref() else {
            return;
        };
        let focused = floating.active_pane();
        // A floating pane is always framed, split or not: the float is a box drawn
        // over the mosaic, so its border is what separates it from the text behind
        // it. The single-pane float painted one before the float could be split, and
        // its images were inset by it, so the same decision feeds both `paint_pane`
        // and `compose_images` (R3-001).
        let show_border = pane_shows_border(true, floating.pane_count());
        let panes: Vec<(PaneId, SessionId, Rect)> = self
            .floating_pane_rects()
            .into_iter()
            .filter_map(|(pane, rect)| {
                floating
                    .session_of(pane)
                    .map(|session| (pane, session, rect))
            })
            .collect();
        for (index, (pane, session_id, rect)) in panes.into_iter().enumerate() {
            let Some(session) = sessions.get_mut(session_id) else {
                continue;
            };
            let snapshot = session.snapshot();
            let options = PaintOptions {
                padding: self.padding(),
                // A visible floating terminal always has the focus, so the cursor of
                // its focused pane follows the same blink phase as any other pane.
                draw_cursor: focused == pane && self.cursor_phase,
                // Opaque: the floating terminal covers the mosaic, and the mosaic must
                // not show through its cells.
                background_alpha: 1.0,
                focused: focused == pane,
                show_border,
                solid_shades: self.config.font.solid_shades,
            };
            paint::paint_pane(
                scene,
                floating_id_base(index),
                rect,
                &snapshot,
                &self.theme.palette,
                self.metrics,
                options,
                &mut self.renderer.measure(),
            );
            if let Some(anchored) = images.get(&session_id) {
                self.compose_images(scene, rect, show_border, &snapshot, anchored);
            }
            if let Overlay::Search(search) = &self.overlay {
                self.paint_search_highlights(scene, rect, &snapshot, search);
            }
            if focused == pane {
                self.compose_bell_flash(scene, rect, show_border);
                self.compose_preedit(scene, rect, show_border, &snapshot);
            }
        }
    }

    /// Whether the bell's border flash is on screen right now.
    pub fn bell_flash_active(&self) -> bool {
        self.bell_flash.is_some_and(|until| Instant::now() < until)
    }

    /// Paints the bell's border flash on the focused pane (S6).
    ///
    /// It is an accent-coloured frame just inside the pane's own rectangle, drawn after
    /// the pane's glyphs so nothing can cover it, and confined to that rectangle so a
    /// flash never lights a neighbouring pane or a divider.
    fn compose_bell_flash(&self, scene: &mut Scene, rect: Rect, show_border: bool) {
        if !self.bell_flash_active() {
            return;
        }
        let inner = if show_border { rect.shrunk(1) } else { rect };
        if inner.is_empty() {
            return;
        }
        let thickness = BELL_FLASH_THICKNESS.min(inner.width).min(inner.height);
        let color = self.theme.palette.accent;
        scene.push_quad(
            Rect::new(inner.x, inner.y, inner.width, thickness),
            color,
            1.0,
        );
        scene.push_quad(
            Rect::new(
                inner.x,
                inner.bottom() - thickness as i32,
                inner.width,
                thickness,
            ),
            color,
            1.0,
        );
        scene.push_quad(
            Rect::new(inner.x, inner.y, thickness, inner.height),
            color,
            1.0,
        );
        scene.push_quad(
            Rect::new(
                inner.right() - thickness as i32,
                inner.y,
                thickness,
                inner.height,
            ),
            color,
            1.0,
        );
    }

    /// Paints the input method's composition at the cursor (S4).
    ///
    /// The items are built from the composition's own state and the snapshot's cursor
    /// cell, and they land in the band of the row the cursor is on — which is exactly
    /// how the damage plan learns that this row changed. Nothing is drawn when there is
    /// no composition, so a session that never composes a character looks exactly as it
    /// did before.
    fn compose_preedit(
        &self,
        scene: &mut Scene,
        rect: Rect,
        show_border: bool,
        snapshot: &Snapshot,
    ) {
        let Some(preedit) = self.preedit.as_ref() else {
            return;
        };
        // A hidden cursor is no caret to compose at: `DECTCEM` off means the program
        // asked for no cursor, and the candidate window is already placed by the
        // platform.
        let Some(cursor) = snapshot.cursor.as_ref() else {
            return;
        };
        let inner = if show_border { rect.shrunk(1) } else { rect };
        let cell = CursorCell {
            column: cursor.column,
            row: cursor.row,
        };
        let Some(area) =
            ime::preedit_area(inner, self.padding(), self.metrics, cell, &preedit.text)
        else {
            return;
        };
        push_preedit(
            scene,
            area,
            &preedit.text,
            self.theme.palette.background,
            self.theme.palette.cursor,
            self.theme.palette.foreground,
        );
    }

    /// Queues the images anchored to one pane into the scene.
    ///
    /// The origin, padding and border match `paint::paint_pane`, so an image lands
    /// on the cells it was anchored to instead of a pixel-shifted copy of them.
    fn compose_images(
        &self,
        scene: &mut Scene,
        rect: Rect,
        show_border: bool,
        snapshot: &Snapshot,
        images: &SessionImages,
    ) {
        // Belt and braces around the intake gate: with the option off nothing is
        // decoded, and a frame composed before the next pump must not draw what a
        // previous one left behind either.
        if !self.config.terminal.images || images.is_empty() {
            return;
        }
        let padding = self.padding();
        let inner = if show_border { rect.shrunk(1) } else { rect };
        let grid = ImageGrid {
            origin: (inner.x + padding.x as i32, inner.y + padding.y as i32),
            metrics: self.metrics,
            rows: snapshot.rows,
            cols: snapshot.cols,
            display_offset: snapshot.display_offset,
        };
        for (placement, id, pixels) in images.items() {
            if let Some(item) = image_item(placement, id, pixels, grid) {
                scene.push_image(item);
            }
        }
    }

    fn compose_tab_strip(&self, scene: &mut Scene) {
        let strip = self.tab_strip_rect();
        if strip.is_empty() {
            return;
        }
        scene.push_quad(strip, self.theme.palette.tab_bar_background, 1.0);

        let vertical = self.config.appearance.tab_bar_position == TabBarPosition::Right;
        let mut offset = if vertical { strip.y } else { strip.x + 8 };
        for tab in &self.workspace.tabs {
            let active = tab.id == self.workspace.active_tab;
            let (tab_rect, label_rect, label_pos) = if vertical {
                let height = self.tab_row_height() as i32;
                let rect = Rect::new(strip.x, offset, strip.width, height as u32);
                let label = Rect::new(
                    strip.x + TAB_INDICATOR_WIDTH as i32 + 6,
                    0,
                    strip.width.saturating_sub(TAB_INDICATOR_WIDTH + 12),
                    strip.height,
                );
                (
                    rect,
                    label,
                    (
                        rect.x + TAB_INDICATOR_WIDTH as i32 + 6,
                        offset + CHROME_PADDING as i32 / 2,
                    ),
                )
            } else {
                let width = self.tab_width(tab);
                let rect = Rect::new(offset, strip.y, width, strip.height);
                let label = Rect::new(offset, strip.y, width, strip.height);
                (
                    rect,
                    label,
                    (offset + 8, strip.y + CHROME_PADDING as i32 / 2),
                )
            };

            if active {
                scene.push_quad(tab_rect, self.theme.palette.tab_active_background, 1.0);
                // The active marker follows the strip: a bar under a horizontal tab
                // and a bar on the left edge of a vertical one.
                if vertical {
                    scene.push_quad(
                        Rect::new(tab_rect.x, tab_rect.y, TAB_INDICATOR_WIDTH, tab_rect.height),
                        self.theme.palette.accent,
                        1.0,
                    );
                } else {
                    scene.push_quad(
                        Rect::new(tab_rect.x, tab_rect.bottom() - 2, tab_rect.width, 2),
                        self.theme.palette.accent,
                        1.0,
                    );
                }
            }

            let label_text =
                self.fit_label(&self.tab_label(tab), label_rect.width.saturating_sub(4));
            scene.push_text(TextItem::plain(
                1_000_000 + tab.id.raw(),
                (label_pos.0 as f32, label_pos.1 as f32),
                label_text,
                paint::rgba(
                    if active {
                        self.theme.palette.tab_active_foreground
                    } else {
                        self.theme.palette.tab_inactive_foreground
                    },
                    1.0,
                ),
                label_rect,
            ));

            if vertical {
                offset += tab_rect.height as i32;
                // A subtle separator between stacked tabs.
                if !active {
                    scene.push_quad(
                        Rect::new(strip.x + 8, offset, strip.width.saturating_sub(16), 1),
                        self.theme.palette.pane_border,
                        1.0,
                    );
                }
            } else {
                offset += tab_rect.width as i32;
            }
        }
    }

    fn compose_status_bar(
        &self,
        scene: &mut Scene,
        sessions: &SessionManager,
        system: &SystemSnapshot,
    ) {
        let rect = self.status_rect();
        if rect.is_empty() {
            return;
        }
        scene.push_quad(rect, self.theme.palette.tab_bar_background, 1.0);
        let baseline = (rect.y + CHROME_PADDING as i32 / 2) as f32;

        // Left: what the focused session is doing.
        let mut parts: Vec<String> = Vec::new();
        if let Some(pane) = self.focused_pane()
            && let Some(session) = sessions.for_pane(pane)
        {
            // S1: the shell's own `OSC 133` marks say whether a command is running and
            // what the last one returned. No marks, no segment: a program that never
            // sends the sequence sees exactly today's status bar.
            if let Some(state) = prompt::prompt_state(session.engine.semantic_marks()) {
                parts.push(state.label());
            }
            if let Some(title) = session.title.clone().or_else(|| session.label.clone()) {
                parts.push(title);
            }
            if let Some(cwd) = &session.cwd {
                parts.push(cwd.display().to_string());
            }
            let (cols, rows) = session.engine.size();
            parts.push(format!("{cols}x{rows}"));
            if session.engine.is_alternate_screen() {
                parts.push("ALT".to_owned());
            }
            if session.engine.has_selection() {
                parts.push("SEL".to_owned());
            }
            if session.search.is_active() {
                parts.push(session.search.indicator());
            }
        }
        if let Some((message, at)) = &self.status
            && at.elapsed() < STATUS_LIFETIME
        {
            parts.push(message.clone());
        }

        let right = self.status_bar_segments(system);
        let left_text = parts.join("  ·  ");
        // The left section never runs under the right one.
        let right_width = right.chars().count() as f32 * self.metrics.width;
        let left_available = (rect.width as f32 - right_width - 24.0).max(0.0) as u32;
        let left_text = self.fit_label(&left_text, left_available);

        scene.push_text(TextItem::plain(
            1_100_000,
            ((rect.x + 8) as f32, baseline),
            left_text,
            paint::rgba(self.theme.palette.tab_inactive_foreground, 1.0),
            rect,
        ));

        if !right.is_empty() {
            let text_width = right.chars().count() as f32 * self.metrics.width;
            let x = (rect.right() as f32 - text_width - 10.0).max(rect.x as f32);
            scene.push_text(TextItem::plain(
                1_100_001,
                (x, baseline),
                right,
                paint::rgba(self.theme.palette.foreground, 1.0),
                rect,
            ));
        }
    }

    /// Right-hand status segments: the hidden floating terminal, CPU, RAM and clock.
    fn status_bar_segments(&self, system: &SystemSnapshot) -> String {
        chrome::status_segments(
            self.floating.as_ref().is_some_and(Floating::is_hidden),
            self.config.appearance.status_bar_cpu,
            self.config.appearance.status_bar_ram,
            self.config.appearance.status_bar_clock,
            &self.config.appearance.clock_format,
            system,
        )
    }

    fn paint_search_highlights(
        &self,
        scene: &mut Scene,
        rect: Rect,
        snapshot: &Snapshot,
        search: &SearchOverlay,
    ) {
        let padding = self.padding();
        let origin_x = rect.x + padding.x as i32;
        let origin_y = rect.y + padding.y as i32;
        for (index, (row, start, length)) in search.highlights.iter().enumerate() {
            let y = origin_y + *row as i32 * self.metrics.height as i32;
            let x = origin_x + (*start as f32 * self.metrics.width) as i32;
            let width = ((*length as f32) * self.metrics.width).ceil().max(1.0) as u32;
            let color = if index == search.current {
                self.theme.palette.accent
            } else {
                self.theme.palette.selection_background
            };
            scene.push_quad(Rect::new(x, y, width, self.metrics.height), color, 0.65);
        }
        let _ = snapshot;
    }

    fn compose_overlay(&self, scene: &mut Scene, sessions: &SessionManager) {
        match &self.overlay {
            Overlay::None => {}
            Overlay::Search(search) => {
                let height = self.metrics.height + CHROME_PADDING;
                let rect = Rect::new(
                    0,
                    self.size
                        .height
                        .saturating_sub(height + self.status_bar_height())
                        as i32,
                    self.size.width,
                    height,
                );
                scene.push_quad(rect, self.theme.palette.tab_bar_background, 0.95);
                let text = format!("/{}", search.query);
                scene.push_text(TextItem::plain(
                    1_200_000,
                    (
                        (rect.x + 8) as f32,
                        (rect.y + CHROME_PADDING as i32 / 2) as f32,
                    ),
                    text,
                    paint::rgba(self.theme.palette.tab_active_foreground, 1.0),
                    rect,
                ));
                if !search.query.is_empty() {
                    let indicator = if search.highlights.is_empty() {
                        "no match".to_owned()
                    } else {
                        format!("{}/{}", search.current + 1, search.highlights.len())
                    };
                    scene.push_text(TextItem::plain(
                        1_200_001,
                        (
                            rect.right() as f32
                                - (indicator.chars().count() as f32 * self.metrics.width)
                                - 8.0,
                            (rect.y + CHROME_PADDING as i32 / 2) as f32,
                        ),
                        indicator,
                        paint::rgba(self.theme.palette.tab_inactive_foreground, 1.0),
                        rect,
                    ));
                }
            }
            Overlay::Rename(rename) => {
                let height = self.metrics.height + CHROME_PADDING;
                let rect = Rect::new(
                    0,
                    self.size
                        .height
                        .saturating_sub(height + self.status_bar_height())
                        as i32,
                    self.size.width,
                    height,
                );
                scene.push_quad(rect, self.theme.palette.tab_bar_background, 0.95);
                scene.push_text(TextItem::plain(
                    1_210_000,
                    (
                        (rect.x + 8) as f32,
                        (rect.y + CHROME_PADDING as i32 / 2) as f32,
                    ),
                    format!("rename tab: {}▏", rename.buffer),
                    paint::rgba(self.theme.palette.tab_active_foreground, 1.0),
                    rect,
                ));
                scene.push_text(TextItem::plain(
                    1_210_001,
                    (
                        rect.right() as f32 - (28.0 * self.metrics.width) - 8.0,
                        (rect.y + CHROME_PADDING as i32 / 2) as f32,
                    ),
                    "enter save · esc cancel".to_owned(),
                    paint::rgba(self.theme.palette.tab_inactive_foreground, 1.0),
                    rect,
                ));
            }
            Overlay::Confirm(confirm) => {
                let width = (self.size.width as f32 * 0.5).clamp(320.0, 720.0) as u32;
                let line_height = self.metrics.height + CHROME_PADDING;
                let height = line_height * 4;
                let rect = Rect::new(
                    (self.size.width.saturating_sub(width) / 2) as i32,
                    (self.size.height.saturating_sub(height) / 2) as i32,
                    width,
                    height,
                );
                scene.push_quad(rect, self.theme.palette.tab_active_background, 0.98);
                scene.push_quad(
                    Rect::new(rect.x, rect.y, rect.width, 2),
                    self.theme.palette.accent,
                    1.0,
                );
                scene.push_text(TextItem::plain(
                    1_210_000,
                    (
                        (rect.x + 12) as f32,
                        (rect.y + CHROME_PADDING as i32) as f32,
                    ),
                    confirm.message.clone(),
                    paint::rgba(self.theme.palette.foreground, 1.0),
                    rect,
                ));
                scene.push_text(TextItem::plain(
                    1_210_001,
                    (
                        (rect.x + 12) as f32,
                        (rect.y + line_height as i32 * 2) as f32,
                    ),
                    "enter/y confirm   esc/n cancel".to_owned(),
                    paint::rgba(self.theme.palette.tab_inactive_foreground, 1.0),
                    rect,
                ));
            }
            Overlay::Settings(panel) => {
                self.compose_settings(scene, panel, sessions);
            }
        }
    }

    fn compose_settings(
        &self,
        scene: &mut Scene,
        panel: &SettingsPanel,
        _sessions: &SessionManager,
    ) {
        // Dim the workspace behind the panel but keep it visible for context.
        scene.push_quad(
            Rect::new(0, 0, self.size.width, self.size.height),
            self.theme.palette.background,
            0.86,
        );

        let width = (self.size.width as f32 * 0.80).clamp(560.0, 1240.0) as u32;
        let height = (self.size.height as f32 * 0.86).clamp(380.0, 1000.0) as u32;
        let rect = Rect::new(
            (self.size.width.saturating_sub(width) / 2) as i32,
            (self.size.height.saturating_sub(height) / 2) as i32,
            width,
            height,
        );
        scene.push_quad(rect, self.theme.palette.tab_bar_background, 0.99);
        scene.push_quad(
            Rect::new(rect.x, rect.y, rect.width, 2),
            self.theme.palette.accent,
            1.0,
        );

        // A slightly loose line height reads better for a list of settings.
        let line_height = (self.metrics.height as f32 * 1.30).max(1.0) as i32;
        let left = rect.x + 16;
        let value_column = left + ((rect.width as f32 * 0.34) as i32).max(180);
        let usable_lines = (rect.height.saturating_sub(24) / line_height as u32) as usize;
        let lines = panel.lines();
        let overflow = lines.len().saturating_sub(usable_lines);

        for (index, line) in lines.iter().enumerate() {
            if index < overflow {
                continue;
            }
            let y = rect.y + 14 + (index - overflow) as i32 * line_height;
            let focused = matches!(line.style, PanelStyle::Focused);
            if focused {
                scene.push_quad(
                    Rect::new(
                        rect.x + 6,
                        y - 2,
                        rect.width.saturating_sub(12),
                        line_height as u32,
                    ),
                    self.theme.palette.tab_active_background,
                    1.0,
                );
            }
            let color = match line.style {
                PanelStyle::Tabs => self.theme.palette.accent,
                PanelStyle::Row => self.theme.palette.foreground,
                PanelStyle::Focused => self.theme.palette.tab_active_foreground,
                PanelStyle::Value => self.theme.palette.tab_inactive_foreground,
                PanelStyle::FocusedValue => self.theme.palette.accent,
                PanelStyle::Hint => self.theme.palette.tab_inactive_foreground,
                PanelStyle::Error => astraterm_config::Rgb::new(0xe0, 0x6c, 0x75),
                PanelStyle::Warning => astraterm_config::Rgb::new(0xe5, 0xc0, 0x7b),
                PanelStyle::Editing => self.theme.palette.cursor,
            };
            scene.push_text(TextItem::plain(
                1_300_000 + index as u64,
                (left as f32, y as f32),
                line.text.clone(),
                paint::rgba(color, 1.0),
                rect,
            ));
            if let Some(value) = &line.value {
                let value_color = if focused {
                    self.theme.palette.accent
                } else {
                    self.theme.palette.tab_inactive_foreground
                };
                scene.push_text(TextItem::plain(
                    1_400_000 + index as u64,
                    (value_column as f32, y as f32),
                    value.clone(),
                    paint::rgba(value_color, 1.0),
                    rect,
                ));
            }
        }

        self.compose_settings_feedback(scene);
    }

    /// Repaints the transient status message above the modal dim.
    ///
    /// The settings panel dims every pixel behind it, including the status bar,
    /// so a save or reload notice written there would be unreadable while the
    /// panel is open. Painting it again last keeps the confirmation legible in
    /// the place where the user already looks for status.
    fn compose_settings_feedback(&self, scene: &mut Scene) {
        let Some((message, at)) = &self.status else {
            return;
        };
        if at.elapsed() >= STATUS_LIFETIME {
            return;
        }
        let height = self.status_bar_height();
        let rect = Rect::new(
            0,
            self.size.height.saturating_sub(height) as i32,
            self.size.width,
            height,
        );
        scene.push_quad(rect, self.theme.palette.tab_bar_background, 1.0);
        scene.push_text(TextItem::plain(
            1_500_000,
            (
                (rect.x + 8) as f32,
                (rect.y + CHROME_PADDING as i32 / 2) as f32,
            ),
            message.clone(),
            paint::rgba(self.theme.palette.tab_inactive_foreground, 1.0),
            rect,
        ));
    }

    /// Whether the cursor should blink right now (used to schedule wakeups).
    pub fn should_blink(&self) -> bool {
        self.config.appearance.cursor_blink
            && !self.config.performance.low_power_mode
            && matches!(self.overlay, Overlay::None)
    }

    /// Advances the blink phase.
    pub fn toggle_cursor_phase(&mut self) {
        self.cursor_phase = !self.cursor_phase;
        self.window.request_redraw();
    }
}

/// Pushes the instructions that paint one composition at the cursor (S4).
///
/// The composition covers the cells it stands on with the terminal background, marks
/// its end with the cursor colour and writes its text on top, all confined to `area`.
/// It is a free function so the exact instructions — and therefore the bands the damage
/// plan rebuilds — are assertable without a window: the whole item set is clipped to the
/// one row the composition occupies.
pub fn push_preedit(
    scene: &mut Scene,
    area: Rect,
    text: &str,
    background: astraterm_config::Rgb,
    marker: astraterm_config::Rgb,
    foreground: astraterm_config::Rgb,
) {
    if area.is_empty() {
        return;
    }
    scene.push_quad(area, background, 1.0);
    scene.push_quad(
        Rect::new(area.x, area.bottom() - 1, area.width, 1),
        marker,
        1.0,
    );
    scene.push_text(TextItem::plain(
        PREEDIT_ID,
        (area.x as f32, area.y as f32),
        text.to_owned(),
        paint::rgba(foreground, 1.0),
        area,
    ));
}

/// Finds all occurrences of `query` in the visible rows.
///
/// Matching is plain substring matching with optional case folding, mirroring the
/// scrollback search semantics.
pub fn find_in_snapshot(
    snapshot: &Snapshot,
    query: &str,
    case_sensitive: bool,
) -> Vec<(usize, usize, usize)> {
    if query.is_empty() {
        return Vec::new();
    }
    let needle: Vec<char> = query.chars().collect();
    let mut hits = Vec::new();
    for (row, line) in snapshot.lines.iter().enumerate() {
        let text = line.text();
        let chars: Vec<char> = text.chars().collect();
        if needle.len() > chars.len() {
            continue;
        }
        for index in 0..=(chars.len() - needle.len()) {
            let matches = needle.iter().enumerate().all(|(offset, expected)| {
                let actual = chars[index + offset];
                if case_sensitive {
                    actual == *expected
                } else {
                    actual.to_lowercase().eq(expected.to_lowercase())
                }
            });
            if matches {
                hits.push((row, index, needle.len()));
            }
        }
    }
    hits
}

/// Layout configuration derived from appearance settings.
pub fn layout_config(config: &Config) -> LayoutConfig {
    LayoutConfig {
        divider: config.appearance.divider_thickness.max(1),
        gap: config.appearance.pane_gap,
        min_pane_width: 80,
        min_pane_height: 40,
    }
}

/// Split identifier of `tab`'s divider whose raw value is `raw`.
///
/// The identifier itself is the workspace crate's, which keeps it private behind
/// `raw`; the divider list is how the UI gets a real one back from [`SplitIdRef`].
fn split_with_raw(tab: &Tab, rect: Rect, config: LayoutConfig, raw: u64) -> Option<SplitId> {
    tab.dividers(rect, config)
        .into_iter()
        .find(|(split, _, _)| split.raw() == raw)
        .map(|(split, _, _)| split)
}

/// What the application should do with a window when a session's child exits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitCleanup {
    /// Another pane survives in the same tab: close just this pane.
    ClosePane(PaneId),
    /// This was the tab's only pane but another tab exists: close the tab.
    CloseTab(TabId),
    /// This is the last pane of the last tab: keep it visible, close nothing.
    KeepOpen,
}

/// Decides the cleanup for an exited session.
///
/// Keeping the metadata of a pane that cannot be closed is what prevents a zombie
/// pane: a pane with no session and no metadata neither renders nor closes.
pub fn plan_exit_cleanup(workspace: &WindowState, pane: PaneId) -> ExitCleanup {
    let Some(tab) = workspace
        .tabs
        .iter()
        .find(|tab| tab.panes().contains(&pane))
    else {
        return ExitCleanup::KeepOpen;
    };
    if tab.pane_count() > 1 {
        return ExitCleanup::ClosePane(pane);
    }
    if workspace.tabs.len() > 1 {
        return ExitCleanup::CloseTab(tab.id);
    }
    ExitCleanup::KeepOpen
}

// --- Terminal images ---------------------------------------------------------

/// Generation carried by every image item.
///
/// A decoded image never changes its bytes, so its generation never changes
/// either and a redraw reuses the texture the cache already holds. A different
/// image arrives as a different `id`, which is what forces an upload; deriving
/// the generation from the frame counter would re-upload every image on every
/// frame.
pub const IMAGE_GENERATION: u64 = 1;

/// Cells an image covers, from its pixel size and the pane's cell metrics.
///
/// The anchoring model stores sizes in cells, so an image is rounded up to whole
/// cells (a 100x50 image in 8x16 cells is 13 columns by 4 rows) and never
/// occupies zero cells.
pub fn image_cells(pixels: (u32, u32), metrics: CellMetrics) -> (u16, u16) {
    let cell_width = metrics.width.max(1.0);
    let cell_height = metrics.height.max(1) as f32;
    let columns = (pixels.0 as f32 / cell_width).ceil().max(1.0);
    let rows = (pixels.1 as f32 / cell_height).ceil().max(1.0);
    (clamp_cells(columns), clamp_cells(rows))
}

fn clamp_cells(value: f32) -> u16 {
    value.min(u16::MAX as f32) as u16
}

/// The cell grid an image is anchored to, in device pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ImageGrid {
    /// Top-left corner of the first cell of the pane.
    pub origin: (i32, i32),
    /// Cell metrics of the pane.
    pub metrics: CellMetrics,
    /// Rows the pane shows.
    pub rows: u16,
    /// Columns the pane shows.
    pub cols: u16,
    /// Lines scrolled back from the live bottom, so an image follows the view.
    pub display_offset: usize,
}

/// Destination rectangle and source sub-rectangle of one drawn image.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ImageRect {
    /// Where it lands on the canvas, in device pixels, clipped to the grid.
    pub rect: Rect,
    /// Source sub-rectangle as `u0, v0, u1, v1`, for the part that is visible.
    pub uv: [f32; 4],
}

/// Places one anchored image on the grid, clipping the part that is off-screen.
///
/// Returns `None` when no part of it is on the grid: scrolled past the top, below
/// the last row or past the last column. Culling here is what keeps a stale anchor
/// from drawing over the chrome or a neighbouring pane, because the image pipeline
/// has no clip rectangle of its own.
pub fn image_rect(placement: &ImagePlacement, grid: ImageGrid) -> Option<ImageRect> {
    let total_rows = placement.rows.max(1) as i32;
    let total_columns = placement.cols.max(1) as i32;
    let screen_row = placement.anchor_line + grid.display_offset as i32;
    let column = placement.column as i32;

    let top = screen_row.max(0);
    let bottom = (screen_row + total_rows).min(grid.rows as i32);
    let left = column.max(0);
    let right = (column + total_columns).min(grid.cols as i32);
    if bottom <= top || right <= left {
        return None;
    }

    let cell_width = grid.metrics.width.max(1.0);
    let cell_height = grid.metrics.height.max(1) as i32;
    // Column edges are truncated and the width is rounded up, exactly like
    // `paint::paint_cell_backgrounds` and the glyph positions: a fractional cell
    // width must not shift an image a pixel away from the text it sits behind.
    let x = grid.origin.0 + (left as f32 * cell_width) as i32;
    let y = grid.origin.1 + top * cell_height;
    let width = (((right - left) as f32) * cell_width).ceil().max(1.0) as u32;
    let height = (bottom - top) as u32 * cell_height as u32;

    Some(ImageRect {
        rect: Rect::new(x, y, width, height),
        uv: [
            (left - column) as f32 / total_columns as f32,
            (top - screen_row) as f32 / total_rows as f32,
            (right - column) as f32 / total_columns as f32,
            (bottom - screen_row) as f32 / total_rows as f32,
        ],
    })
}

/// Scene item for one anchored image, or `None` when it is not on screen.
pub fn image_item(
    placement: &ImagePlacement,
    id: u64,
    pixels: &Arc<ImagePixels>,
    grid: ImageGrid,
) -> Option<ImageItem> {
    let placed = image_rect(placement, grid)?;
    Some(ImageItem::new(id, placed.rect, Arc::clone(pixels), IMAGE_GENERATION).with_uv(placed.uv))
}

/// Whether the anchors still name lines on the screen that is now displayed.
///
/// An image is anchored to buffer lines of the grid it arrived on, and the primary
/// and alternate grids have independent lines: entering the alternate screen
/// discards the primary anchors, and leaving it discards the alternate ones.
/// Nothing is re-anchored, because the model can only drop.
pub fn images_survive_alt_switch(was_alternate: bool, is_alternate: bool) -> bool {
    was_alternate == is_alternate
}

/// One anchored image and the texture it draws with.
struct AnchoredImage {
    /// Texture identifier, assigned once so the cache never re-uploads it.
    id: u64,
    pixels: Arc<ImagePixels>,
}

/// The decoded images of one session: anchors, pixels and a byte budget.
///
/// [`ImageStore`] holds the anchoring model (budget, eviction, drop rules) and this
/// type holds the pixels for each placement *index for index*, so an image is only
/// ever drawn with the bytes that produced it.
pub struct SessionImages {
    store: ImageStore,
    /// Pixels of `store.placements()`, in the same order and with the same length.
    images: Vec<AnchoredImage>,
    /// Reassembly state for kitty chunked transfers (`m=1`), which are several
    /// sequences for one image. It belongs to the session: when the session closes
    /// the decoder is dropped, so a buffer left in flight cannot outlive it.
    decoder: ImageDecoder,
    /// Last texture identifier handed out; never reused, so an evicted id cannot
    /// come back with different pixels.
    next_id: u64,
    /// `line_range().0` when the anchors were moved last.
    top_line: i32,
    /// Alternate-screen flag observed at the last intake.
    alternate_screen: bool,
    /// Anchors dropped because the buffer line they named no longer exists, by any
    /// of the rules that can lose a line: a resize the reflow did not carry them
    /// through, truncated history, or the alternate screen replacing the grid.
    /// Clearing because the option is off is not counted: that is a policy, not a
    /// line that went away. This is a diagnostic counter, never shown in the UI.
    dropped: usize,
}

impl SessionImages {
    /// An empty store for `session`, with [`ImageStore`]'s default budget.
    pub fn new(session: SessionId, top_line: i32) -> Self {
        Self::with_budget(session, top_line, ImageStore::default().budget())
    }

    /// Same, with an explicit byte budget for the decoded pixels.
    pub fn with_budget(session: SessionId, top_line: i32, budget: usize) -> Self {
        Self {
            store: ImageStore::new(budget),
            images: Vec::new(),
            decoder: ImageDecoder::new(),
            // Session ids are unique in the process, so the high half of the
            // identifier keeps texture keys unique across panes without a second
            // allocator.
            next_id: session.raw() << 32,
            top_line,
            alternate_screen: false,
            dropped: 0,
        }
    }

    /// Anchors dropped so far because the buffer line they named stopped existing.
    pub fn dropped_anchors(&self) -> usize {
        self.dropped
    }

    /// Anchors a decoded image and keeps its pixels for drawing.
    ///
    /// The byte total comes from the pixels, never from the caller, so the budget
    /// cannot be lied about. Returns `false` when the image alone exceeds the
    /// budget: the store then holds nothing and no pixels are kept.
    pub fn insert(&mut self, mut placement: ImagePlacement, pixels: Arc<ImagePixels>) -> bool {
        placement.bytes = pixels.byte_len();
        if !self.store.insert(placement) {
            return false;
        }
        // The store evicts the oldest placements, always from the front of its
        // list, so trimming the front here keeps both lists aligned.
        while self.images.len() >= self.store.len() {
            self.images.remove(0);
        }
        self.next_id += 1;
        self.images.push(AnchoredImage {
            id: self.next_id,
            pixels,
        });
        true
    }

    /// Placements, oldest first, with the texture that draws each one.
    pub fn items(&self) -> impl Iterator<Item = (&ImagePlacement, u64, &Arc<ImagePixels>)> + '_ {
        self.store
            .placements()
            .iter()
            .zip(self.images.iter())
            .map(|(placement, image)| (placement, image.id, &image.pixels))
    }

    pub fn is_empty(&self) -> bool {
        self.store.is_empty()
    }

    /// Drops every anchor and its pixels.
    pub fn clear(&mut self) {
        self.store.clear();
        self.images.clear();
    }

    /// Drains the image sequences the session captured and anchors what decodes.
    ///
    /// Returns how many images were anchored. `enabled` is `terminal.images`: when
    /// it is off the payloads are drained and dropped **without decoding**, which
    /// is the point of the option — the bytes come from whatever program is
    /// running, including a remote host over SSH.
    pub fn intake(
        &mut self,
        engine: &mut TerminalEngine,
        metrics: CellMetrics,
        enabled: bool,
    ) -> usize {
        let messages = engine.take_images();
        self.follow_alt_screen(engine.is_alternate_screen());
        self.follow_buffer(engine.line_range().0);
        if !enabled {
            self.clear();
            return 0;
        }
        let cursor = engine.cursor_point();
        let limits = DecodeLimits::default();
        let mut stored = 0;
        for message in messages {
            // A refused payload is a dropped image: the decoder reports the reason
            // and nothing else uses it, so a malformed or oversized sequence can
            // never panic and never draws half an image (S5.2, S5.3). `Ok(None)` is
            // a kitty `m=1` chunk: accepted, still in flight, no image yet (S1.1).
            let Ok(Some(decoded)) = self.decoder.decode(&message, &limits) else {
                continue;
            };
            let Some(pixels) = ImagePixels::new(decoded.width, decoded.height, decoded.rgba) else {
                continue;
            };
            let (cols, rows) = image_cells((decoded.width, decoded.height), metrics);
            let placement = ImagePlacement {
                protocol: message.protocol,
                anchor_line: cursor.line.0,
                column: cursor.column.0.min(u16::MAX as usize) as u16,
                cols,
                rows,
                bytes: 0,
            };
            if self.insert(placement, Arc::new(pixels)) {
                stored += 1;
            }
        }
        stored
    }

    /// Drops the anchors when the screen underneath them changed.
    ///
    /// Returns whether anything was dropped.
    pub fn follow_alt_screen(&mut self, is_alternate: bool) -> bool {
        let survived = images_survive_alt_switch(self.alternate_screen, is_alternate);
        self.alternate_screen = is_alternate;
        if survived {
            return false;
        }
        // The grid the anchors were numbered against is gone, so every anchor now
        // names a line that does not exist on the screen being shown.
        self.dropped += self.store.len();
        self.clear();
        true
    }

    /// Follows the buffer as lines enter or leave its history.
    ///
    /// `top_line` is the oldest line the buffer still holds
    /// ([`TerminalEngine::line_range`]'s first value, negative while there is
    /// scrollback). When new history appears above the screen every existing line
    /// number decreases by the same amount, so the anchors move with it. When
    /// history is *discarded* the remaining lines stay where they are, so nothing
    /// is moved; the anchors that named discarded lines are dropped instead.
    pub fn follow_buffer(&mut self, top_line: i32) {
        let delta = top_line - self.top_line;
        self.top_line = top_line;
        if delta < 0 && !self.images.is_empty() {
            self.shift(delta);
        }
        self.remove_before(top_line);
    }

    /// Drops the anchors above `line`.
    ///
    /// The same rule as [`ImageStore::remove_before`], applied here because the
    /// store cannot report which placements it kept and the pixels have to follow
    /// their placement.
    pub fn remove_before(&mut self, line: i32) {
        self.retain(|placement| placement.anchor_line >= line);
    }

    /// Drops the anchors whose buffer line a resize removed, and returns how many.
    ///
    /// Called after a resize with the buffer range the reflow left behind
    /// ([`TerminalEngine::line_range`]). Nothing is re-anchored: recovering where a
    /// line moved would need an old-to-new map that `alacritty_terminal` does not
    /// expose, so an image the reflow shifted keeps pointing at its old line number
    /// (S5.3). What the resize must not do is throw away an anchor that still has a
    /// line: an anchor in the scrollback keeps its image, and drawing it is already
    /// culled by `image_rect` until the view scrolls back to it (S5.1).
    pub fn drop_evicted(&mut self, range: (i32, i32)) -> usize {
        let (top, bottom) = range;
        self.retain(|placement| (top..=bottom).contains(&placement.anchor_line))
    }

    /// Moves every anchor by `delta` lines, keeping placements and pixels paired.
    fn shift(&mut self, delta: i32) {
        let placements: Vec<ImagePlacement> = self
            .store
            .placements()
            .iter()
            .map(|placement| ImagePlacement {
                anchor_line: placement.anchor_line + delta,
                ..*placement
            })
            .collect();
        let images = std::mem::take(&mut self.images);
        self.rebuild(placements, images);
    }

    /// Keeps only the placements the predicate accepts, with their pixels, and
    /// counts the ones it dropped.
    fn retain(&mut self, mut keep: impl FnMut(&ImagePlacement) -> bool) -> usize {
        let mut placements = Vec::with_capacity(self.store.len());
        let mut images = Vec::with_capacity(self.images.len());
        let taken = std::mem::take(&mut self.images);
        let mut dropped = 0;
        for (placement, image) in self.store.placements().iter().zip(taken) {
            if keep(placement) {
                placements.push(*placement);
                images.push(image);
            } else {
                dropped += 1;
            }
        }
        self.dropped += dropped;
        self.rebuild(placements, images);
        dropped
    }

    /// Rebuilds the store from paired placements and pixels.
    ///
    /// Insertion order is preserved and the byte total only ever shrinks here, so
    /// nothing is evicted by the rebuild.
    fn rebuild(&mut self, placements: Vec<ImagePlacement>, images: Vec<AnchoredImage>) {
        self.store.clear();
        for placement in &placements {
            self.store.insert(*placement);
        }
        self.images = images;
        debug_assert_eq!(self.store.len(), self.images.len());
    }
}

/// Detects an http(s) URL on the cell under `column`.
///
/// The word containing the column is extracted and accepted only when it starts
/// with a web scheme, so a click never launches anything the text did not show.
pub fn url_at(text: &str, column: usize) -> Option<String> {
    let chars: Vec<char> = text.chars().collect();
    if column >= chars.len() || chars[column].is_whitespace() {
        return None;
    }
    let mut start = column;
    while start > 0 && !chars[start - 1].is_whitespace() {
        start -= 1;
    }
    let mut end = column;
    while end + 1 < chars.len() && !chars[end + 1].is_whitespace() {
        end += 1;
    }
    let word: String = chars[start..=end].iter().collect();
    let trimmed = word.trim_end_matches(['.', ',', ')', ']', ';', '\'', '"']);
    let lowered = trimmed.to_ascii_lowercase();
    if lowered.starts_with("http://") || lowered.starts_with("https://") {
        Some(trimmed.to_owned())
    } else {
        None
    }
}

/// What a click resolved to, and what the platform opener answered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClickOpen {
    /// The destination the click resolved to.
    pub url: String,
    /// The opener's verdict, reported in the status bar by the caller.
    pub result: Result<(), String>,
}

/// Resolves what a click on the cell under `column` opens.
///
/// A cell that carries an `OSC 8` hyperlink contributes that destination, and
/// only that destination: it wins over the visible characters so a label can
/// differ from its target. The destination comes from the program, so it is
/// screened with the same allow-list the opener enforces, and a refused scheme
/// resolves to nothing rather than falling back to text the program only
/// displayed. A cell with no hyperlink keeps the text-detection behaviour of
/// [`url_at`].
pub fn link_target(
    cell: Option<&CellSnapshot>,
    line: &RowSnapshot,
    column: usize,
) -> Option<String> {
    match cell.and_then(CellSnapshot::hyperlink) {
        Some(uri) => linux::is_safe_url(uri).then(|| uri.to_owned()),
        None => url_at(&line.text(), column),
    }
}

/// Resolves the click and hands the result to `open` exactly once.
///
/// `open` is the platform opener in the application and a recorder in the tests,
/// so the click path needs no GUI to be exercised. It is called only for a URL
/// [`link_target`] accepted, and never with an empty or refused destination.
pub fn open_clicked_url<F>(
    cell: Option<&CellSnapshot>,
    line: &RowSnapshot,
    column: usize,
    open: F,
) -> Option<ClickOpen>
where
    F: FnOnce(&str) -> Result<(), String>,
{
    let url = link_target(cell, line, column)?;
    let result = open(&url);
    Some(ClickOpen { url, result })
}

#[cfg(test)]
mod tests {
    use super::*;
    use astraterm_terminal::TerminalEngine;
    use astraterm_terminal::engine::EngineOptions;
    use astraterm_terminal::image::ImageProtocol;

    fn snapshot(text: &str) -> Snapshot {
        let mut engine = TerminalEngine::new(EngineOptions {
            cols: 40,
            rows: 4,
            ..Default::default()
        });
        engine.process(text.as_bytes());
        engine.snapshot()
    }

    #[test]
    fn finds_occurrences_in_visible_rows() {
        let snapshot = snapshot("alpha beta alpha\r\nsecond alpha");
        let hits = find_in_snapshot(&snapshot, "alpha", false);
        assert_eq!(hits.len(), 3);
        assert_eq!(hits[0], (0, 0, 5));
        assert_eq!(hits[1], (0, 11, 5));
        assert_eq!(hits[2], (1, 7, 5));
    }

    #[test]
    fn search_is_case_insensitive_by_default() {
        let snapshot = snapshot("Alpha");
        assert_eq!(find_in_snapshot(&snapshot, "alpha", false).len(), 1);
        assert!(find_in_snapshot(&snapshot, "alpha", true).is_empty());
    }

    #[test]
    fn empty_query_matches_nothing() {
        let snapshot = snapshot("anything");
        assert!(find_in_snapshot(&snapshot, "", false).is_empty());
    }

    #[test]
    fn url_detection_respects_the_clicked_column() {
        let text = "see https://example.com/docs for details";
        assert_eq!(url_at(text, 6).as_deref(), Some("https://example.com/docs"));
        assert_eq!(url_at(text, 0), None, "`see` is not a URL");
        assert_eq!(url_at(text, 30), None, "a plain word is not a URL");
        assert_eq!(url_at(text, 3), None, "whitespace selects nothing");
        assert_eq!(url_at(text, 999), None);
    }

    #[test]
    fn url_detection_strips_trailing_punctuation() {
        let text = "(see https://example.com/a).";
        assert_eq!(url_at(text, 20).as_deref(), Some("https://example.com/a"));
    }

    #[test]
    fn url_detection_ignores_non_web_words() {
        let text = "ssh user@host and mailto:x@y";
        assert_eq!(url_at(text, 2), None);
    }

    /// Runs the click path for the cell under `column`, recording every URL the
    /// opener is handed. Nothing is launched: the opener is the seam.
    fn click(snapshot: &Snapshot, column: u16) -> (Option<super::ClickOpen>, Vec<String>) {
        let mut opened = Vec::new();
        let outcome = super::open_clicked_url(
            snapshot.cell(column, 0),
            &snapshot.lines[0],
            column as usize,
            |url| {
                opened.push(url.to_owned());
                Ok(())
            },
        );
        (outcome, opened)
    }

    #[test]
    fn a_click_on_a_linked_cell_opens_the_hyperlink() {
        // B6: the visible text and the destination disagree on purpose, so a
        // path that still scans the text opens the wrong URL.
        let snapshot = snapshot(
            "\x1b]8;;https://link.example/target\x1b\\https://text.example/page\x1b]8;;\x1b\\",
        );
        let (outcome, opened) = click(&snapshot, 0);
        assert_eq!(
            opened,
            vec!["https://link.example/target".to_owned()],
            "the opener is asked once, for the hyperlink and not for the text"
        );
        let outcome = outcome.expect("a linked click resolves to a URL");
        assert_eq!(outcome.url, "https://link.example/target");
        assert!(outcome.result.is_ok());
    }

    #[test]
    fn a_click_on_an_unlinked_cell_still_opens_the_visible_url() {
        // Pins the behaviour the hyperlink path must not disturb.
        let snapshot = snapshot("see https://example.com/docs for details");
        let (outcome, opened) = click(&snapshot, 6);
        assert_eq!(opened, vec!["https://example.com/docs".to_owned()]);
        assert!(outcome.is_some_and(|outcome| outcome.result.is_ok()));
    }

    #[test]
    fn an_unlinked_non_url_cell_opens_nothing() {
        let snapshot = snapshot("see https://example.com/docs for details");
        let (outcome, opened) = click(&snapshot, 0);
        assert!(outcome.is_none());
        assert!(opened.is_empty(), "`see` is not a URL");
    }

    #[test]
    fn a_refused_scheme_never_reaches_the_opener() {
        // The destination is program-controlled. Only the schemes the opener
        // already accepts may pass, and a refused link does not fall back to the
        // visible text either: the link wins even when it is refused.
        for uri in ["file:///etc/passwd", "javascript:alert(1)", "ssh://host/x"] {
            let text = format!("\x1b]8;;{uri}\x1b\\https://text.example/page\x1b]8;;\x1b\\");
            let snapshot = snapshot(&text);
            let (outcome, opened) = click(&snapshot, 0);
            assert!(outcome.is_none(), "{uri} must be refused");
            assert!(
                opened.is_empty(),
                "{uri} must never reach the opener, not even as the text URL"
            );
        }
    }

    #[test]
    fn the_window_geometry_is_computed_before_the_ui_exists() {
        let config = Config::default();
        // Cell 10x20 at scale 1: the top strip is 20 + 6 = 26 tall, the default
        // padding is 12x10 and the status bar is off, so a 800x600 window has
        // 776x554 pixels of cells: 77 columns and 27 rows.
        let metrics = CellMetrics {
            width: 10.0,
            height: 20,
            baseline: 14.0,
        };
        let grid = initial_grid(&config, metrics, 1.0, Size::new(800, 600));
        assert_eq!(grid, Some((77, 27)));
        assert_ne!(
            grid,
            Some((80, 24)),
            "the historical default is not the grid the window paints"
        );
    }

    #[test]
    fn a_window_without_a_reported_size_has_no_initial_grid() {
        let config = Config::default();
        let metrics = CellMetrics {
            width: 10.0,
            height: 20,
            baseline: 14.0,
        };
        assert_eq!(initial_grid(&config, metrics, 1.0, Size::new(0, 0)), None);
        assert_eq!(
            initial_grid(&config, metrics, 1.0, Size::new(800, 0)),
            None,
            "a zero height is no more usable than a zero window"
        );
    }

    #[test]
    fn blink_gate_fires_twice_per_period() {
        let start = Instant::now();
        let (toggle, next) = blink_due_with(start, None, CURSOR_BLINK_PERIOD);
        assert!(toggle, "the first check arms the timer");
        assert_eq!(next, start + CURSOR_BLINK_PERIOD);

        let (toggle, next_same) = blink_due_with(
            start + Duration::from_millis(100),
            Some(next),
            CURSOR_BLINK_PERIOD,
        );
        assert!(!toggle, "no toggle before the deadline");
        assert_eq!(next_same, next);

        let (toggle, next_after) = blink_due_with(next, Some(next), CURSOR_BLINK_PERIOD);
        assert!(toggle, "toggles exactly at the deadline");
        assert_eq!(next_after, next + CURSOR_BLINK_PERIOD);
    }

    #[test]
    fn a_composition_damages_exactly_the_row_it_is_painted_on() {
        use astraterm_config::Rgb;
        use astraterm_renderer::damage::{Bands, Geometry, plan};

        // Three text rows of 16 px on an 8 px grid: the bands are the rows.
        let geometry = Geometry::new((160, 48), 16, [0.0, 0.0, 0.0, 1.0]);
        let mut before = Scene::default();
        for row in 0..3u32 {
            before.push_text(TextItem::plain(
                u64::from(row) + 1,
                (0.0, (row * 16) as f32),
                "aaaaaaaaaaaaaaaaaaaa",
                paint::rgba(Rgb::new(0xff, 0xff, 0xff), 1.0),
                Rect::new(0, (row * 16) as i32, 160, 16),
            ));
        }
        let mut after = before.clone();
        // A composition of three cells at column 2 of row 1: exactly one row's worth
        // of pixels, which is what makes it one band.
        push_preedit(
            &mut after,
            Rect::new(16, 16, 24, 16),
            "あいう",
            Rgb::new(0x10, 0x10, 0x10),
            Rgb::new(0xff, 0x88, 0x00),
            Rgb::new(0xff, 0xff, 0xff),
        );

        let previous = Bands::record(&before, None, geometry);
        let current = Bands::record(&after, None, geometry);
        let damage = plan(Some(&previous), &current);
        assert_eq!(
            damage.bands(),
            &[1],
            "the composition is a change of the cursor row, and of nothing else"
        );
    }

    #[test]
    fn blink_gate_does_not_burst_after_a_long_stall() {
        let start = Instant::now();
        let deadline = start + CURSOR_BLINK_PERIOD;
        let late = start + CURSOR_BLINK_PERIOD * 10;
        let (toggle, next) = blink_due_with(late, Some(deadline), CURSOR_BLINK_PERIOD);
        assert!(toggle);
        assert!(
            next > late,
            "the next deadline is in the future, so no burst"
        );
    }

    #[test]
    fn rename_buffer_is_trimmed_and_bounded() {
        let mut rename = RenameOverlay::new(TabId::from_raw(1), "work");
        rename.push(' ');
        rename.push('x');
        assert_eq!(rename.committed_title(), "work x");
        for _ in 0..200 {
            rename.push('y');
        }
        assert!(rename.buffer.chars().count() <= 120);
        rename.pop();
        assert!(rename.buffer.ends_with('y'));
    }

    #[test]
    fn empty_rename_clears_the_title() {
        let rename = RenameOverlay::new(TabId::from_raw(1), "   ");
        assert_eq!(rename.committed_title(), "");
    }

    #[test]
    fn exit_cleanup_closes_a_pane_when_siblings_remain() {
        let first = PaneId::from_raw(1);
        let second = PaneId::from_raw(2);
        let mut tab = Tab::new("t", first);
        tab.split_focused(SplitAxis::Vertical, second);
        let workspace = WindowState::new(WindowId::from_raw(1), tab);
        assert_eq!(
            plan_exit_cleanup(&workspace, second),
            ExitCleanup::ClosePane(second)
        );
    }

    #[test]
    fn exit_cleanup_closes_the_tab_when_it_is_the_only_pane_but_not_the_only_tab() {
        let first = PaneId::from_raw(1);
        let other = PaneId::from_raw(2);
        let tab = Tab::new("one", first);
        let mut workspace = WindowState::new(WindowId::from_raw(1), tab);
        workspace.add_tab(Tab::new("two", other));
        let tab_id = workspace.tabs[0].id;
        assert_eq!(
            plan_exit_cleanup(&workspace, first),
            ExitCleanup::CloseTab(tab_id)
        );
    }

    #[test]
    fn exit_cleanup_keeps_the_last_pane_of_the_last_tab() {
        let only = PaneId::from_raw(1);
        let workspace = WindowState::new(WindowId::from_raw(1), Tab::new("only", only));
        assert_eq!(
            plan_exit_cleanup(&workspace, only),
            ExitCleanup::KeepOpen,
            "the last pane must stay rendered instead of becoming a zombie"
        );
    }

    #[test]
    fn exit_cleanup_ignores_unknown_panes() {
        let workspace =
            WindowState::new(WindowId::from_raw(1), Tab::new("only", PaneId::from_raw(1)));
        assert_eq!(
            plan_exit_cleanup(&workspace, PaneId::from_raw(99)),
            ExitCleanup::KeepOpen
        );
    }

    #[test]
    fn layout_config_maps_appearance_settings() {
        let mut config = Config::default();
        config.appearance.divider_thickness = 3;
        config.appearance.pane_gap = 4;
        let layout = layout_config(&config);
        assert_eq!(layout.divider, 3);
        assert_eq!(layout.gap, 4);
    }

    // --- Terminal images ---------------------------------------------------

    /// A 2x2 raw RGBA kitty image (`f=32`, `s=2`, `v=2`).
    ///
    /// Kitty's raw format needs no PNG encoder in the test binary: the base64
    /// body of sixteen zero bytes is a constant, and every pixel is transparent.
    const KITTY_IMAGE: &[u8] = b"\x1b_Ga=T,f=32,s=2,v=2;AAAAAAAAAAAAAAAAAAAAAA==\x1b\\";

    /// The same 2x2 raw RGBA image as [`KITTY_IMAGE`], split over three APCs.
    ///
    /// The base64 body of sixteen zero bytes (`AAAAAAAAAAAAAAAAAAAAAA==`) is cut in
    /// 8, 6 and 8 symbols: the first two chunks carry `m=1` and the last closes the
    /// transfer with `m=0`. `s=`, `v=` and `f=` appear only in the first one.
    const KITTY_CHUNKED: [(&[u8], bool); 3] = [
        (b"\x1b_Ga=T,f=32,s=2,v=2,i=7,m=1;AAAAAAAA\x1b\\", true),
        (b"\x1b_Gi=7,m=1;AAAAAAAA\x1b\\", true),
        (b"\x1b_Gi=7,m=0;AAAAAA==\x1b\\", false),
    ];

    fn image_metrics() -> CellMetrics {
        CellMetrics {
            width: 8.0,
            height: 16,
            baseline: 12.0,
        }
    }

    fn anchored(anchor_line: i32, column: u16, cols: u16, rows: u16) -> ImagePlacement {
        ImagePlacement {
            protocol: ImageProtocol::Kitty,
            anchor_line,
            column,
            cols,
            rows,
            bytes: 0,
        }
    }

    fn image_grid() -> ImageGrid {
        ImageGrid {
            origin: (10, 20),
            metrics: image_metrics(),
            rows: 4,
            cols: 10,
            display_offset: 0,
        }
    }

    fn pixels(width: u32, height: u32, red: u8) -> Arc<ImagePixels> {
        let rgba = vec![red; (width * height * 4) as usize];
        Arc::new(ImagePixels::new(width, height, rgba).expect("matching RGBA buffer"))
    }

    fn image_engine() -> TerminalEngine {
        TerminalEngine::new(EngineOptions {
            cols: 20,
            rows: 5,
            ..Default::default()
        })
    }

    fn assert_close(actual: f32, expected: f32) {
        assert!(
            (actual - expected).abs() < 1e-6,
            "expected {expected}, got {actual}"
        );
    }

    #[test]
    fn an_image_covers_whole_cells() {
        assert_eq!(image_cells((100, 50), image_metrics()), (13, 4));
        assert_eq!(
            image_cells((16, 32), image_metrics()),
            (2, 2),
            "exact multiples keep their cell count"
        );
        assert_eq!(
            image_cells((1, 1), image_metrics()),
            (1, 1),
            "a sub-cell image still occupies one cell"
        );
        assert_eq!(image_cells((0, 0), image_metrics()), (1, 1));
    }

    #[test]
    fn a_visible_image_covers_its_cells() {
        let placed = image_rect(&anchored(1, 2, 3, 2), image_grid()).expect("inside the grid");
        assert_eq!(placed.rect, Rect::new(26, 36, 24, 32));
        assert_eq!(placed.uv, [0.0, 0.0, 1.0, 1.0]);
    }

    #[test]
    fn scrolling_back_moves_the_image_down_with_the_text() {
        let grid = ImageGrid {
            display_offset: 2,
            ..image_grid()
        };
        let placed = image_rect(&anchored(1, 0, 1, 1), grid).expect("still on screen");
        assert_eq!(
            placed.rect,
            Rect::new(10, 68, 8, 16),
            "row 1 plus two lines of scrollback"
        );
    }

    #[test]
    fn an_image_scrolled_past_the_top_is_culled() {
        assert_eq!(image_rect(&anchored(-2, 0, 1, 2), image_grid()), None);
        assert_eq!(
            image_rect(&anchored(4, 0, 1, 1), image_grid()),
            None,
            "the row below the last one is outside the grid"
        );
        assert_eq!(
            image_rect(&anchored(0, 10, 2, 1), image_grid()),
            None,
            "the column past the last one is outside the grid"
        );
    }

    #[test]
    fn a_partially_visible_image_is_clipped_instead_of_overflowing() {
        let top = image_rect(&anchored(-1, 0, 2, 3), image_grid()).expect("two rows visible");
        assert_eq!(top.rect, Rect::new(10, 20, 16, 32));
        assert_close(top.uv[1], 1.0 / 3.0);
        assert_close(top.uv[3], 1.0);

        let bottom = image_rect(&anchored(3, 0, 1, 3), image_grid()).expect("one row visible");
        assert_eq!(bottom.rect, Rect::new(10, 68, 8, 16));
        assert_close(bottom.uv[1], 0.0);
        assert_close(bottom.uv[3], 1.0 / 3.0);
    }

    #[test]
    fn an_image_wider_than_the_grid_is_clipped() {
        let placed = image_rect(&anchored(0, 8, 4, 1), image_grid()).expect("two columns visible");
        assert_eq!(placed.rect, Rect::new(74, 20, 16, 16));
        assert_close(placed.uv[2], 0.5);
    }

    #[test]
    fn an_item_keeps_one_upload_generation_per_image() {
        let image = pixels(2, 2, 1);
        let first = image_item(&anchored(0, 0, 1, 1), 7, &image, image_grid()).expect("visible");
        let second = image_item(&anchored(0, 0, 1, 1), 7, &image, image_grid()).expect("visible");
        assert_eq!(first.id, 7);
        assert_eq!(
            first.generation, second.generation,
            "the generation must not change with the frame"
        );
        assert_eq!(first.rect, second.rect);
        assert!(Arc::ptr_eq(&first.pixels, &second.pixels));
    }

    #[test]
    fn the_alternate_screen_invalidates_every_anchor() {
        assert!(images_survive_alt_switch(false, false));
        assert!(images_survive_alt_switch(true, true));
        assert!(
            !images_survive_alt_switch(false, true),
            "entering the alternate screen drops the primary anchors"
        );
        assert!(
            !images_survive_alt_switch(true, false),
            "leaving it drops the alternate anchors"
        );
    }

    #[test]
    fn a_decoded_image_keeps_one_identity_across_frames() {
        let mut images = SessionImages::new(SessionId::from_raw(4), 0);
        assert!(images.insert(anchored(2, 0, 2, 1), pixels(16, 16, 3)));
        let first: Vec<(u64, usize)> = images
            .items()
            .map(|(_, id, pixels)| (id, pixels.rgba.len()))
            .collect();
        let second: Vec<(u64, usize)> = images
            .items()
            .map(|(_, id, pixels)| (id, pixels.rgba.len()))
            .collect();
        assert_eq!(first, second, "identifiers are assigned once, at insert");
        assert_eq!(first.len(), 1);
        assert!(first[0].0 > 0);
    }

    #[test]
    fn the_budget_evicts_the_oldest_image_first() {
        let mut images = SessionImages::with_budget(SessionId::from_raw(5), 0, 64);
        assert!(images.insert(anchored(1, 0, 1, 1), pixels(4, 4, 10)));
        assert!(images.insert(anchored(2, 0, 1, 1), pixels(4, 4, 20)));

        let kept: Vec<u8> = images
            .items()
            .map(|(_, _, pixels)| pixels.rgba[0])
            .collect();
        assert_eq!(kept, vec![20], "64 bytes of budget hold one 4x4 image");
        let bytes: usize = images.items().map(|(_, _, pixels)| pixels.byte_len()).sum();
        assert_eq!(bytes, 64, "the budget accounts for the decoded pixels");
        let anchor = images.items().next().expect("one image").0.anchor_line;
        assert_eq!(anchor, 2);
    }

    #[test]
    fn an_image_larger_than_the_budget_is_refused() {
        let mut images = SessionImages::with_budget(SessionId::from_raw(6), 0, 32);
        assert!(!images.insert(anchored(1, 0, 1, 1), pixels(4, 4, 1)));
        assert!(images.is_empty());
        assert_eq!(images.items().count(), 0);
    }

    #[test]
    fn lines_that_scroll_into_history_move_the_anchors() {
        let mut images = SessionImages::new(SessionId::from_raw(7), 0);
        assert!(images.insert(anchored(4, 0, 1, 1), pixels(2, 2, 9)));

        images.follow_buffer(-3);

        let anchor = images.items().next().expect("one image").0.anchor_line;
        assert_eq!(
            anchor, 1,
            "the line the image sits on moved up with the content"
        );
        assert_eq!(images.items().count(), 1, "the texture is kept");
    }

    #[test]
    fn discarding_history_drops_the_anchors_that_left_the_buffer() {
        let mut images = SessionImages::new(SessionId::from_raw(8), -4);
        assert!(images.insert(anchored(-2, 0, 1, 1), pixels(2, 2, 1)));
        assert!(images.insert(anchored(1, 0, 1, 1), pixels(2, 2, 2)));

        // Clearing the scrollback moves the top back to the live screen; anchors
        // that named discarded lines must not survive it.
        images.follow_buffer(0);

        let kept: Vec<u8> = images
            .items()
            .map(|(_, _, pixels)| pixels.rgba[0])
            .collect();
        assert_eq!(kept, vec![2]);
        let anchor = images.items().next().expect("one image").0.anchor_line;
        assert_eq!(anchor, 1);
    }

    #[test]
    fn entering_and_leaving_the_alternate_screen_drops_the_anchors() {
        let mut images = SessionImages::new(SessionId::from_raw(9), 0);
        assert!(images.insert(anchored(0, 0, 1, 1), pixels(2, 2, 1)));

        images.follow_alt_screen(true);
        assert!(images.is_empty(), "the primary anchors are dropped");

        assert!(images.insert(anchored(0, 0, 1, 1), pixels(2, 2, 2)));
        images.follow_alt_screen(true);
        assert_eq!(
            images.items().count(),
            1,
            "staying on the same screen keeps them"
        );

        images.follow_alt_screen(false);
        assert!(images.is_empty(), "the alternate anchors are dropped");
    }

    #[test]
    fn an_anchor_the_resize_pushed_off_screen_keeps_its_image() {
        let mut images = SessionImages::new(SessionId::from_raw(20), 0);
        // A 24-row screen over a buffer whose oldest line is -30: line 30 has no
        // cell on screen but is a real buffer line, and -6 is in the scrollback.
        assert!(images.insert(anchored(-6, 0, 1, 1), pixels(2, 2, 1)));
        assert!(images.insert(anchored(30, 0, 1, 1), pixels(2, 2, 2)));

        let dropped = images.drop_evicted((-30, 60));

        let kept: Vec<u8> = images
            .items()
            .map(|(_, _, pixels)| pixels.rgba[0])
            .collect();
        assert_eq!(
            kept,
            vec![1, 2],
            "an anchor keeps its image while its line exists in the buffer"
        );
        assert_eq!(dropped, 0);
        assert_eq!(images.dropped_anchors(), 0);
    }

    #[test]
    fn a_resize_drops_and_counts_the_anchors_beyond_the_buffer() {
        let mut images = SessionImages::new(SessionId::from_raw(21), 0);
        assert!(images.insert(anchored(-2, 0, 1, 1), pixels(2, 2, 1)));
        assert!(images.insert(anchored(50, 0, 1, 1), pixels(2, 2, 2)));

        let dropped = images.drop_evicted((-20, 40));

        let kept: Vec<u8> = images
            .items()
            .map(|(_, _, pixels)| pixels.rgba[0])
            .collect();
        assert_eq!(kept, vec![1], "line 50 is past the last buffer line");
        assert_eq!(dropped, 1);
        assert_eq!(
            images.dropped_anchors(),
            1,
            "the drop is counted instead of happening silently"
        );
    }

    #[test]
    fn a_resize_without_images_drops_and_counts_nothing() {
        let mut images = SessionImages::new(SessionId::from_raw(22), 0);

        assert_eq!(images.drop_evicted((-20, 40)), 0);

        assert!(images.is_empty());
        assert_eq!(images.items().count(), 0);
        assert_eq!(images.dropped_anchors(), 0);
    }

    #[test]
    fn a_captured_kitty_image_is_anchored_and_drawn() {
        let mut engine = image_engine();
        let mut images = SessionImages::new(SessionId::from_raw(11), engine.line_range().0);

        engine.process(KITTY_IMAGE);
        assert_eq!(images.intake(&mut engine, image_metrics(), true), 1);

        let (placement, id, pixels) = images.items().next().expect("one anchored image");
        assert_eq!(placement.protocol, ImageProtocol::Kitty);
        assert_eq!((placement.cols, placement.rows), (1, 1));
        assert_eq!(placement.anchor_line, engine.cursor_point().line.0);
        assert_eq!(pixels.byte_len(), 2 * 2 * 4);

        let grid = ImageGrid {
            origin: (0, 0),
            metrics: image_metrics(),
            rows: 5,
            cols: 20,
            display_offset: 0,
        };
        let item = image_item(placement, id, pixels, grid).expect("on screen");
        assert_eq!(item.rect, Rect::new(0, 0, 8, 16));
    }

    #[test]
    fn images_are_discarded_without_decoding_when_the_option_is_off() {
        let mut engine = image_engine();
        let mut images = SessionImages::new(SessionId::from_raw(12), engine.line_range().0);

        engine.process(KITTY_IMAGE);
        assert_eq!(images.intake(&mut engine, image_metrics(), false), 0);
        assert!(images.is_empty());
        assert!(
            engine.take_images().is_empty(),
            "the payloads are drained, not left to accumulate"
        );
    }

    #[test]
    fn a_refused_payload_leaves_the_session_usable() {
        let mut engine = image_engine();
        let mut images = SessionImages::new(SessionId::from_raw(13), engine.line_range().0);

        // An unfinished chunked transfer produces no image on its own, and a
        // truncated iTerm2 payload is simply broken: both are dropped silently.
        engine.process(b"\x1b_Ga=T,f=32,s=2,v=2,m=1,i=9;AAAAAAAAAAAAAAAAAAAAAA==\x1b\\");
        engine.process(b"\x1b]1337;File=inline=1:not-base64!\x07");
        assert_eq!(images.intake(&mut engine, image_metrics(), true), 0);
        assert!(images.is_empty());

        engine.process(b"still fine");
        let snapshot = engine.snapshot();
        assert_eq!(snapshot.lines[0].text().trim_end(), "still fine");
    }

    #[test]
    fn a_kitty_transfer_split_over_sequences_is_one_image() {
        let mut engine = image_engine();
        let mut images = SessionImages::new(SessionId::from_raw(14), engine.line_range().0);

        // One 2x2 RGBA image sent as three APCs: the first two carry `m=1`, the last
        // closes the transfer. Only the last one can anchor a placement.
        for (sequence, more) in KITTY_CHUNKED {
            engine.process(sequence);
            let anchored = images.intake(&mut engine, image_metrics(), true);
            if more {
                assert_eq!(anchored, 0, "an m=1 chunk is not an image yet");
            } else {
                assert_eq!(anchored, 1, "the final chunk completes the image");
            }
        }
        let (placement, _, pixels) = images.items().next().expect("one anchored image");
        assert_eq!(placement.protocol, ImageProtocol::Kitty);
        assert_eq!((placement.cols, placement.rows), (1, 1));
        assert_eq!(pixels.byte_len(), 2 * 2 * 4);
    }

    #[test]
    fn an_image_sequence_does_not_reach_the_grid_as_text() {
        let mut engine = image_engine();
        engine.process(KITTY_IMAGE);
        let snapshot = engine.snapshot();
        assert_eq!(
            snapshot.lines[0].text().trim_end(),
            "",
            "the payload is captured, not printed"
        );
    }
}

#[cfg(test)]
mod floating_tests {
    use super::*;

    /// A floating terminal with a fixed session, visible unless asked otherwise.
    fn floating(visible: bool) -> Floating {
        let mut floating = Floating::new(PaneId::from_raw(7), SessionId::from_raw(21));
        floating.set_visible(visible);
        floating
    }

    /// Layout with no gap, so a float divider sits exactly on a pane edge.
    fn config() -> LayoutConfig {
        LayoutConfig {
            divider: 2,
            gap: 0,
            min_pane_width: 100,
            min_pane_height: 60,
        }
    }

    /// The rectangle the mosaic and the floating terminal are laid out in.
    fn content() -> Rect {
        Rect::new(0, 24, 1200, 800)
    }

    /// A floating terminal with two panes, split vertically.
    fn split_floating() -> (Floating, PaneId, PaneId) {
        let mut floating = floating(true);
        let first = PaneId::from_raw(7);
        let second = PaneId::from_raw(8);
        assert!(floating.split_active(SplitAxis::Vertical, second, SessionId::from_raw(22)));
        (floating, first, second)
    }

    #[test]
    fn the_first_press_creates_and_the_next_ones_hide_and_show_the_same_session() {
        assert_eq!(plan_floating_press(None), FloatingPress::Create);

        let mut open = floating(true);
        assert_eq!(plan_floating_press(Some(&open)), FloatingPress::Hide);
        open.set_visible(false);
        assert!(open.is_hidden());
        assert_eq!(
            open.session_of(PaneId::from_raw(7)),
            Some(SessionId::from_raw(21)),
            "hiding never replaces the session, so the shell keeps its state"
        );

        assert_eq!(plan_floating_press(Some(&open)), FloatingPress::Show);
        open.set_visible(true);
        assert!(open.visible);
        assert_eq!(
            open.session_of(PaneId::from_raw(7)),
            Some(SessionId::from_raw(21)),
            "showing reuses the session that stayed alive"
        );
    }

    #[test]
    fn hiding_hands_the_keyboard_back_to_the_mosaic() {
        let mosaic = Some(PaneId::from_raw(3));
        assert_eq!(
            keyboard_pane(Some(&floating(true)), mosaic),
            Some(PaneId::from_raw(7)),
            "a visible floating terminal always owns the focus"
        );
        assert_eq!(
            keyboard_pane(Some(&floating(false)), mosaic),
            mosaic,
            "hiding it returns the focus to the mosaic pane that kept it"
        );
        assert_eq!(keyboard_pane(None, mosaic), mosaic);
    }

    #[test]
    fn the_floating_terminal_owns_every_pane_it_holds() {
        let (open, first, second) = split_floating();
        assert!(closes_floating(Some(&open), first));
        assert!(
            closes_floating(Some(&open), second),
            "every pane of the float's own layout is a floating pane"
        );
        assert!(
            !closes_floating(Some(&open), PaneId::from_raw(3)),
            "a mosaic pane is never a floating one"
        );
        assert!(!closes_floating(None, PaneId::from_raw(7)));
        // Once destroyed there is no floating left, so focus resolves to the mosaic.
        assert_eq!(
            keyboard_pane(None, Some(PaneId::from_raw(3))),
            Some(PaneId::from_raw(3))
        );
    }

    /// T5/S7: a split while the float owns the keys joins the float's layout and
    /// leaves the mosaic's pane count alone; with the float hidden the mosaic is
    /// the layout the keyboard drives.
    #[test]
    fn a_split_while_the_float_has_the_keyboard_adds_a_pane_to_the_float() {
        let mosaic_pane = PaneId::from_raw(3);
        let mut workspace =
            WindowState::new(WindowId::from_raw(1), Tab::new("mosaic", mosaic_pane));

        let mut float = floating(true);
        assert!(split_lands_in_floating(Some(&float)));
        assert_eq!(
            keyboard_pane(Some(&float), tab_parent(&workspace)),
            Some(PaneId::from_raw(7))
        );

        let second = PaneId::from_raw(8);
        assert!(float.split_active(SplitAxis::Vertical, second, SessionId::from_raw(22)));

        assert_eq!(float.pane_count(), 2);
        assert_eq!(float.panes(), vec![PaneId::from_raw(7), second]);
        assert_eq!(float.session_of(second), Some(SessionId::from_raw(22)));
        assert_eq!(
            workspace.pane_count(),
            1,
            "the mosaic keeps its single pane: the split never reached it (S5)"
        );

        // The reverse: with the float hidden the mosaic is what the keys drive, and
        // the split lands there.
        let float = floating(false);
        assert!(!split_lands_in_floating(Some(&float)));
        assert!(!split_lands_in_floating(None));
        let tab = workspace.active_tab_mut().expect("an active tab");
        assert!(
            tab.split_focused(SplitAxis::Horizontal, PaneId::from_raw(9))
                .is_some()
        );
        assert_eq!(workspace.pane_count(), 2);
        assert_eq!(float.pane_count(), 1, "the float stayed as it was");
    }

    /// T5/S7: closing one floating pane keeps the others, and the last pane is the
    /// caller's cue to destroy the float and its sessions.
    #[test]
    fn closing_one_floating_pane_keeps_the_float_and_the_last_one_ends_it() {
        let (mut float, first, second) = split_floating();

        assert!(float.close_pane(second));
        assert_eq!(float.pane_count(), 1);
        assert_eq!(float.panes(), vec![first]);
        assert_eq!(
            float.session_ids().collect::<Vec<_>>(),
            vec![SessionId::from_raw(21)],
            "the closed pane's session leaves with it"
        );
        assert_eq!(float.active_pane(), first, "the survivor takes the focus");
        assert_eq!(float.session_of(second), None);

        assert!(
            !float.close_pane(first),
            "the last pane is refused, so the caller destroys the float"
        );
        assert_eq!(float.pane_count(), 1);
        assert_eq!(float.session_of(first), Some(SessionId::from_raw(21)));
    }

    #[test]
    fn closing_an_unknown_floating_pane_keeps_every_pane() {
        let (mut float, _, second) = split_floating();
        assert!(!float.close_pane(PaneId::from_raw(99)));
        assert_eq!(float.pane_count(), 2);
        assert_eq!(float.session_of(second), Some(SessionId::from_raw(22)));
    }

    /// The rectangles the floating panes are drawn at, inside the float box.
    fn float_rects(float: &Floating) -> Vec<(PaneId, Rect)> {
        let rect = chrome::float_rect(content());
        float
            .tab
            .layout(rect, config())
            .into_iter()
            .map(|placement| (placement.pane, placement.rect))
            .collect()
    }

    /// Centre of one floating pane, which is where a click lands on it.
    fn float_centre(rects: &[(PaneId, Rect)], pane: PaneId) -> (f64, f64) {
        let (_, rect) = rects
            .iter()
            .find(|(id, _)| *id == pane)
            .expect("the pane has a rectangle inside the float");
        (
            (rect.x + rect.width as i32 / 2) as f64,
            (rect.y + rect.height as i32 / 2) as f64,
        )
    }

    /// T5/S7 + R3-002: clicking a floating pane makes it the float's active pane —
    /// the pointer resolves the pane *and* focuses it — so `keyboard_pane` reports
    /// the pane that was clicked instead of the one the split left focused.
    #[test]
    fn a_click_inside_a_floating_pane_makes_it_the_active_floating_pane() {
        let (mut float, first, second) = split_floating();
        let mosaic = Some(PaneId::from_raw(3));
        assert_eq!(
            float.active_pane(),
            second,
            "the split focused the new pane"
        );

        for pane in [first, second] {
            let rects = float_rects(&float);
            let (x, y) = float_centre(&rects, pane);
            assert_eq!(float.focus_at(&rects, x, y), Some(pane));
            assert_eq!(float.active_pane(), pane, "{pane:?} is the click target");
            assert_eq!(
                keyboard_pane(Some(&float), mosaic),
                Some(pane),
                "the keys follow the pane the user clicked"
            );
        }

        // A point that hits no floating pane must not move the focus: a miss is not
        // a silent focus change.
        let rects = float_rects(&float);
        assert_eq!(float.focus_at(&rects, -5.0, -5.0), None);
        assert_eq!(float.active_pane(), second);
    }

    /// T5/S7 + R3-002: the focus commands move between floating panes while the
    /// float owns the keys, and they stop at its edges instead of reaching a mosaic
    /// pane behind it.
    #[test]
    fn the_focus_commands_move_between_floating_panes_and_stop_at_the_edges() {
        let (mut float, first, second) = split_floating();
        let rect = chrome::float_rect(content());
        let mosaic = Some(PaneId::from_raw(3));
        assert_eq!(float.active_pane(), second);

        assert!(
            float.focus_direction(FocusDirection::Left, rect, config()),
            "there is a floating pane to the left"
        );
        assert_eq!(float.active_pane(), first);
        assert_eq!(keyboard_pane(Some(&float), mosaic), Some(first));

        assert!(
            !float.focus_direction(FocusDirection::Left, rect, config()),
            "nothing lies left of the first floating pane"
        );
        assert_eq!(
            float.active_pane(),
            first,
            "the focus stays put at the edge"
        );

        assert!(float.focus_direction(FocusDirection::Right, rect, config()));
        assert_eq!(float.active_pane(), second);
        assert!(!float.focus_direction(FocusDirection::Right, rect, config()));
        assert_eq!(float.active_pane(), second);

        // A vertical split has nothing above or below, so up and down stop at the
        // float's edge instead of resolving a mosaic pane.
        assert!(!float.focus_direction(FocusDirection::Up, rect, config()));
        assert!(!float.focus_direction(FocusDirection::Down, rect, config()));
        assert_eq!(float.active_pane(), second);
        assert_eq!(keyboard_pane(Some(&float), mosaic), Some(second));
    }

    /// T5/S7 + R3-002: a freshly split float has a well-defined active pane, the
    /// pane that was active before the split can be clicked back, and hiding and
    /// showing the float keeps that focus.
    #[test]
    fn a_freshly_split_float_has_an_active_pane_and_the_old_one_can_be_clicked_back() {
        let mosaic = Some(PaneId::from_raw(3));
        for axis in [SplitAxis::Vertical, SplitAxis::Horizontal] {
            let mut float = floating(true);
            let first = float.active_pane();
            let second = PaneId::from_raw(8);
            assert!(float.split_active(axis, second, SessionId::from_raw(22)));
            assert_eq!(
                float.active_pane(),
                second,
                "{axis:?}: the new pane of a split is the one the keys reach"
            );
            assert_eq!(float.panes(), vec![first, second]);

            let rects = float_rects(&float);
            let (x, y) = float_centre(&rects, first);
            assert_eq!(float.focus_at(&rects, x, y), Some(first));
            assert_eq!(
                float.active_pane(),
                first,
                "{axis:?}: clicking the pane the split pushed out of focus works"
            );
            assert_eq!(keyboard_pane(Some(&float), mosaic), Some(first));

            float.set_visible(false);
            assert_eq!(keyboard_pane(Some(&float), mosaic), mosaic);
            float.set_visible(true);
            assert_eq!(
                keyboard_pane(Some(&float), mosaic),
                Some(first),
                "{axis:?}: showing the float restores the pane that was active"
            );
        }
    }

    /// R3-001: a one-pane float paints a border and insets its images by it, exactly
    /// as the single-pane float always did, so splitting the float never moves the
    /// images of the pane that was already there.
    #[test]
    fn a_single_pane_float_still_insets_its_images_by_its_border() {
        let one = floating(true);
        let (two, _, _) = split_floating();
        assert_eq!(one.pane_count(), 1);
        assert_eq!(two.pane_count(), 2);

        assert!(
            pane_shows_border(true, one.pane_count()),
            "the float keeps its border with a single pane, so its images are inset"
        );
        assert_eq!(
            pane_shows_border(true, one.pane_count()),
            pane_shows_border(true, two.pane_count()),
            "the image origin must not move when a split appears"
        );
        // The mosaic keeps its own rule: a lone pane fills its area, a split is framed.
        assert!(!pane_shows_border(false, 1));
        assert!(pane_shows_border(false, 2));
    }

    /// T5/S7: the floating panes tile the float box, and each one is resolved on its
    /// own rectangle and divider instead of the whole float being one pane.
    #[test]
    fn floating_panes_tile_the_float_box_and_are_hit_tested_on_their_own_rects() {
        let (float, _, _) = split_floating();
        let rect = chrome::float_rect(content());
        let rects: Vec<(PaneId, Rect)> = float
            .tab
            .layout(rect, config())
            .into_iter()
            .map(|placement| (placement.pane, placement.rect))
            .collect();
        assert_eq!(rects.len(), 2);

        for (pane, pane_rect) in &rects {
            let centre_x = (pane_rect.x + pane_rect.width as i32 / 2) as f64;
            let centre_y = (pane_rect.y + pane_rect.height as i32 / 2) as f64;
            assert!(
                rect.contains(centre_x, centre_y),
                "{pane:?} must stay inside the float box"
            );
            assert_eq!(pane_at_point(&rects, centre_x, centre_y), Some(*pane));
            assert_eq!(
                float.tab.pane_rect(*pane, rect, config()),
                Some(*pane_rect),
                "each pane has its own rectangle, not the whole float"
            );
            assert_ne!(
                *pane_rect, rect,
                "a split float never gives a pane the whole box"
            );
        }

        let dividers = float.tab.dividers(rect, config());
        assert_eq!(dividers.len(), 1, "one divider for two floating panes");
        let (split, axis, divider) = dividers[0];
        assert_eq!(axis, SplitAxis::Vertical);
        let drag = divider_drag_at(
            &dividers,
            rect,
            2.0,
            divider.x as f64,
            divider.y as f64 + 1.0,
        )
        .expect("the floor of the divider is grabbable");
        match drag {
            DragState::Divider {
                split: SplitIdRef(raw),
                axis,
                extent,
                ..
            } => {
                assert_eq!(raw, split.raw(), "the drag names this very split");
                assert_eq!(axis, SplitAxis::Vertical);
                assert_eq!(
                    extent, rect.width as f64,
                    "a float drag is normalised by the float box, not the window"
                );
            }
            other => panic!("expected a divider drag, got {other:?}"),
        }
    }

    /// T5/S7: every floating pane paints under its own scene item id base, wide
    /// enough apart that the shaping cache cannot confuse them.
    #[test]
    fn every_floating_pane_gets_its_own_scene_id_base() {
        let bases: Vec<u64> = (0..4).map(floating_id_base).collect();
        assert_eq!(
            bases[0], FLOATING_ID_BASE,
            "the first pane keeps the base a single-pane float always used"
        );
        let mut sorted = bases.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            bases.len(),
            "the bases are distinct: {bases:?}"
        );
        // `paint_pane` puts the cursor at `base + 0x10_000`, so the stride has to be
        // wider than that offset plus the tallest pane.
        const { assert!(FLOATING_PANE_STRIDE > 0x10_000) };
        assert!(
            bases[0] > 1_500_000,
            "the float range starts above every fixed chrome range"
        );
    }

    /// T5/S7: a floating divider belongs to the float's tree alone, so dragging it
    /// never resizes a mosaic pane even when both are laid out in the same rectangle.
    #[test]
    fn a_divider_is_resolved_by_exactly_one_split_tree() {
        let rect = chrome::float_rect(content());
        let (float, _, _) = split_floating();
        let float_split = float.tab.dividers(rect, config())[0].0;
        assert_eq!(
            split_with_raw(&float.tab, rect, config(), float_split.raw()),
            Some(float_split)
        );

        let mut mosaic = Tab::new("mosaic", PaneId::from_raw(3));
        assert!(
            mosaic
                .split_focused(SplitAxis::Vertical, PaneId::from_raw(4))
                .is_some()
        );
        assert_eq!(
            split_with_raw(&mosaic, rect, config(), float_split.raw()),
            None,
            "the mosaic does not own the float's divider"
        );
    }

    /// T4/S6: a new tab inherits the active tab, never the floating pane.
    #[test]
    fn a_new_tab_names_the_active_tab_as_its_parent() {
        let mosaic_pane = PaneId::from_raw(3);
        let workspace = WindowState::new(WindowId::from_raw(1), Tab::new("mosaic", mosaic_pane));
        let float = floating(true);

        // The premise of the bug: the float owns the keyboard, so the focus is its
        // pane and not the pane of the active tab.
        assert_eq!(
            keyboard_pane(Some(&float), tab_parent(&workspace)),
            Some(PaneId::from_raw(7))
        );
        assert_eq!(tab_parent(&workspace), Some(mosaic_pane));
        assert_ne!(
            tab_parent(&workspace),
            Some(float.active_pane()),
            "the floating pane's directory must not leak into a mosaic tab"
        );
    }

    /// S6: the parent is the *active* tab's pane even when another tab exists.
    #[test]
    fn the_tab_parent_follows_the_active_tab_and_its_focused_pane() {
        let first = PaneId::from_raw(3);
        let other = PaneId::from_raw(5);
        let mut workspace = WindowState::new(WindowId::from_raw(1), Tab::new("one", first));
        workspace.add_tab(Tab::new("two", other));
        assert_eq!(tab_parent(&workspace), Some(other), "the new tab is active");

        workspace.select_index(0);
        assert_eq!(tab_parent(&workspace), Some(first));

        // A zoomed pane is what the user sees as focused, so it is the parent even
        // when another pane holds `active_pane` behind the zoom.
        let tab = workspace.active_tab_mut().expect("an active tab");
        let split = PaneId::from_raw(6);
        assert!(tab.split_focused(SplitAxis::Vertical, split).is_some());
        tab.focus(first);
        tab.toggle_zoom();
        assert!(tab.focus(split), "focus moves behind the zoom");
        assert_eq!(tab.active_pane, split);
        assert_eq!(tab.zoomed, Some(first));
        assert_eq!(tab_parent(&workspace), Some(first));
    }

    /// PRESERVE: with no floating terminal the mosaic still answers for the pane.
    #[test]
    fn without_a_float_the_mosaic_pane_is_the_tab_parent() {
        let mosaic_pane = PaneId::from_raw(3);
        let workspace = WindowState::new(WindowId::from_raw(1), Tab::new("mosaic", mosaic_pane));
        assert_eq!(
            keyboard_pane(None, tab_parent(&workspace)),
            Some(mosaic_pane)
        );
        assert!(!split_lands_in_floating(None));
        assert!(pane_at_point(&[], 1.0, 1.0).is_none(), "no rects, no hit");
    }
}

#[cfg(test)]
mod upper_layer_tests {
    use super::*;

    #[test]
    fn nothing_is_drawn_above_the_mosaic_without_a_float_or_a_modal() {
        assert_eq!(upper_layer_clip(None, false, Size::new(800, 600)), None);
    }

    #[test]
    fn a_visible_float_confines_the_layer_to_its_own_rect() {
        let float = Rect::new(120, 90, 640, 420);
        assert_eq!(
            upper_layer_clip(Some(float), false, Size::new(800, 600)),
            Some(float)
        );
    }

    #[test]
    fn the_float_layer_stays_inside_the_window_content() {
        let size = Size::new(1280, 800);
        let content = Rect::new(0, 30, 1280, 740);
        let float = chrome::float_rect(content);
        let clip = upper_layer_clip(Some(float), false, size).expect("a visible float has a layer");
        assert!(clip.x >= 0 && clip.y >= 0, "{clip:?}");
        assert!(clip.right() <= size.width as i32, "{clip:?}");
        assert!(clip.bottom() <= size.height as i32, "{clip:?}");
    }

    #[test]
    fn a_modal_overlay_keeps_the_whole_window() {
        let size = Size::new(800, 600);
        let window = Rect::new(0, 0, 800, 600);
        assert_eq!(
            upper_layer_clip(Some(Rect::new(10, 10, 100, 100)), true, size),
            Some(window),
            "a dialog dims the whole window, so it must not be scissored to the float"
        );
        assert_eq!(upper_layer_clip(None, true, size), Some(window));
    }
}
