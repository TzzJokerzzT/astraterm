//! Per-window UI state and scene composition.
//!
//! `WindowUi` owns one window: its renderer, workspace (tabs and split tree), pane
//! metadata, chrome state (tab strip, status bar, overlays) and pointer
//! interaction state. It never owns PTY sessions; those live in the application so
//! that several windows can share one manager.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use astraterm_config::schema::{Config, CursorStyle, TabBarPosition};
use astraterm_config::themes::Theme;
use astraterm_core::{CellMetrics, PaneId, Rect, SessionId, Size, SplitAxis, TabId, WindowId};
use astraterm_platform::system::SystemSnapshot;
use astraterm_renderer::Renderer;
use astraterm_renderer::paint::{self, ImageItem, ImagePixels, PaintOptions, Scene, TextItem};
use astraterm_session::SessionManager;
use astraterm_settings_ui::panel::{PanelStyle, SettingsPanel};
use astraterm_terminal::TerminalEngine;
use astraterm_terminal::image::{ImagePlacement, ImageStore};
use astraterm_terminal::image_decode::{DecodeLimits, decode};
use astraterm_terminal::snapshot::{CursorVisual, Snapshot};
use astraterm_workspace::{LayoutConfig, Pane, Tab, WindowState};

use crate::chrome;
use winit::window::Window;

/// Height of the tab strip in cell rows.
const TAB_STRIP_ROWS: f32 = 1.0;
/// Base padding added to the tab strip and status bar, in device pixels.
const CHROME_PADDING: u32 = 6;
/// Width of the active-tab indicator, in device pixels.
const TAB_INDICATOR_WIDTH: u32 = 3;

/// Blink period of the text cursor.
pub const CURSOR_BLINK_PERIOD: Duration = Duration::from_millis(530);

/// Scene item id base of the floating terminal.
///
/// Chrome uses the fixed ranges from 1_000_000 up and terminal rows use
/// `(pane_index + 1) * 0x1000`, so this range is unique in the scene and its shaped
/// rows stay cached across frames.
const FLOATING_ID_BASE: u64 = 0x20_0000;

/// Decides whether the cursor blink should toggle now, and when to check again.
///
/// A blink that re-armed itself on every frame would spin the event loop at frame
/// rate; this gate keeps it at exactly two toggles per period.
pub fn blink_due(now: Instant, deadline: Option<Instant>) -> (bool, Instant) {
    match deadline {
        Some(deadline) if now < deadline => (false, deadline),
        Some(deadline) => {
            // Catch up without ever firing more than once per call.
            let next = deadline + CURSOR_BLINK_PERIOD;
            (
                true,
                if next <= now {
                    now + CURSOR_BLINK_PERIOD
                } else {
                    next
                },
            )
        }
        None => (true, now + CURSOR_BLINK_PERIOD),
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
/// It is deliberately *outside* the split tree: the mosaic layout and the
/// reflow never see it, so splits, zooms and divider drags ignore it. A window
/// owns at most one, and it keeps its own session, which survives being hidden:
/// hiding only flips `visible` and never touches the PTY.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Floating {
    /// Pane the session is registered under, which is how the keyboard, paste and
    /// the mouse reach it. It is not part of the workspace.
    pub pane: PaneId,
    /// Session of the floating shell; a hide/show cycle never replaces it.
    pub session: SessionId,
    /// Whether it is currently drawn and owns the keyboard.
    pub visible: bool,
}

impl Floating {
    /// A floating terminal showing `session`, created by the first toggle press.
    pub fn new(pane: PaneId, session: SessionId) -> Self {
        Self {
            pane,
            session,
            visible: true,
        }
    }

    /// Shows or hides it without touching the session.
    pub fn set_visible(&mut self, visible: bool) {
        self.visible = visible;
    }

    /// Whether it exists but is not on screen: the status bar reports this state.
    pub fn is_hidden(&self) -> bool {
        !self.visible
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
        Some(floating) if floating.visible => Some(floating.pane),
        _ => mosaic,
    }
}

/// Whether `ClosePane` aims at the floating terminal instead of a mosaic pane.
pub fn closes_floating(floating: Option<&Floating>, pane: PaneId) -> bool {
    floating.is_some_and(|floating| floating.pane == pane)
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

/// Repeated-click tracking for double/triple click selection.
#[derive(Clone, Copy, Debug)]
pub struct ClickInfo {
    pub at: Instant,
    pub column: u16,
    pub row: u16,
    pub count: u8,
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
        astraterm_core::Padding {
            x: (self.config.appearance.padding_x as f32 * self.scale) as u32,
            y: (self.config.appearance.padding_y as f32 * self.scale) as u32,
        }
    }

    /// Height of the tab strip, zero when hidden.
    pub fn tab_strip_height(&self) -> u32 {
        match self.config.appearance.tab_bar_position {
            TabBarPosition::Hidden => 0,
            _ => (self.metrics.height as f32 * TAB_STRIP_ROWS) as u32 + CHROME_PADDING,
        }
    }

    pub fn status_bar_height(&self) -> u32 {
        if self.config.appearance.show_status_bar {
            self.metrics.height + CHROME_PADDING
        } else {
            0
        }
    }

    /// Width of the vertical tab strip, zero for the other positions.
    pub fn tab_strip_width(&self) -> u32 {
        match self.config.appearance.tab_bar_position {
            TabBarPosition::Right => {
                let requested = (self.config.appearance.tab_strip_width as f32 * self.scale) as u32;
                requested.min(self.size.width.saturating_sub(160))
            }
            _ => 0,
        }
    }

    /// Rectangle available to the terminal panes.
    pub fn terminal_rect(&self) -> Rect {
        chrome::terminal_rect(
            self.config.appearance.tab_bar_position,
            self.size,
            self.tab_strip_height(),
            self.tab_strip_width(),
            self.status_bar_height(),
        )
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
        let padding = self.padding();
        let inner = Rect {
            x: rect.x,
            y: rect.y,
            width: rect.width.saturating_sub(2 * padding.x),
            height: rect.height.saturating_sub(2 * padding.y),
        };
        self.metrics.grid_size(inner)
    }

    /// Pane under a pointer position.
    pub fn pane_at(&self, x: f64, y: f64) -> Option<PaneId> {
        // The floating terminal is drawn over the mosaic, so it answers first.
        if let Some(floating) = self.floating.as_ref()
            && let Some(rect) = self.floating_rect()
            && rect.contains(x, y)
        {
            return Some(floating.pane);
        }
        self.pane_rects()
            .into_iter()
            .find(|(_, rect)| rect.contains(x, y))
            .map(|(pane, _)| pane)
    }

    /// Rectangle of a pane on screen, mosaic or floating.
    pub fn pane_rect(&self, pane: PaneId) -> Option<Rect> {
        if let Some(rect) = self.floating_rect()
            && closes_floating(self.floating.as_ref(), pane)
        {
            return Some(rect);
        }
        self.pane_rects()
            .into_iter()
            .find(|(id, _)| *id == pane)
            .map(|(_, rect)| rect)
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
        self.workspace
            .active_tab()
            .map(|tab| tab.zoomed.unwrap_or(tab.active_pane))
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

    /// Cursor visual style from configuration.
    pub fn cursor_style(&self) -> CursorVisual {
        match self.config.appearance.cursor {
            CursorStyle::Block => CursorVisual::Block,
            CursorStyle::Beam => CursorVisual::Beam,
            CursorStyle::Underline => CursorVisual::Underline,
            CursorStyle::Hollow => CursorVisual::HollowBlock,
        }
    }

    /// Composes the frame.
    pub fn compose(
        &mut self,
        sessions: &mut SessionManager,
        system: &SystemSnapshot,
        images: &BTreeMap<SessionId, SessionImages>,
    ) -> Scene {
        let opacity = self.config.window.opacity.clamp(0.1, 1.0);
        let mut scene = Scene::with_clear(self.theme.palette.background, opacity);

        self.compose_tab_strip(&mut scene);

        let focused = self.focused_pane();
        let rects = self.pane_rects();
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
                cursor_override: Some(self.cursor_style()),
                // The translucent terminal background is painted once by the scene
                // clear; painting the pane background again would compound alpha and
                // make the window look opaque.
                background_alpha: 0.0,
                focused: focused == Some(*pane),
                show_border: rects.len() > 1,
            };
            paint::paint_pane(
                &mut scene,
                (index as u64 + 1) * 0x1000,
                *rect,
                &snapshot,
                &self.theme.palette,
                self.metrics,
                options,
            );

            // Images are anchored to buffer lines, so they are drawn from the
            // pane's session and placed on the same grid the glyphs use.
            if let Some(anchored) = images.get(&session_id) {
                self.compose_images(&mut scene, *rect, rects.len() > 1, &snapshot, anchored);
            }

            if let Overlay::Search(search) = &self.overlay {
                self.paint_search_highlights(&mut scene, *rect, &snapshot, search);
            }
        }

        self.compose_floating(&mut scene, sessions, images);
        self.compose_status_bar(&mut scene, sessions, system);
        self.compose_overlay(&mut scene, sessions);
        scene
    }

    /// Paints the floating terminal over the mosaic, below the modal overlays.
    ///
    /// Composition order is the whole z-order: the mosaic is painted first, the
    /// floating terminal covers its centre with an opaque background, and the
    /// status bar and overlays are composed afterwards, so a dialog still wins.
    fn compose_floating(
        &self,
        scene: &mut Scene,
        sessions: &mut SessionManager,
        images: &BTreeMap<SessionId, SessionImages>,
    ) {
        let Some(floating) = self.floating.as_ref() else {
            return;
        };
        let Some(rect) = self.floating_rect() else {
            return;
        };
        let Some(session) = sessions.get_mut(floating.session) else {
            return;
        };
        let snapshot = session.snapshot();
        let session_id = session.id;
        let options = PaintOptions {
            padding: self.padding(),
            // A visible floating terminal always has the focus, so its cursor
            // follows the same blink phase as any other focused pane.
            draw_cursor: self.cursor_phase,
            cursor_override: Some(self.cursor_style()),
            // Opaque: the floating terminal covers the mosaic, and the mosaic must
            // not show through its cells.
            background_alpha: 1.0,
            focused: true,
            show_border: true,
        };
        paint::paint_pane(
            scene,
            FLOATING_ID_BASE,
            rect,
            &snapshot,
            &self.theme.palette,
            self.metrics,
            options,
        );
        if let Some(anchored) = images.get(&session_id) {
            self.compose_images(scene, rect, true, &snapshot, anchored);
        }
        if let Overlay::Search(search) = &self.overlay {
            self.paint_search_highlights(scene, rect, &snapshot, search);
        }
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
            && at.elapsed() < Duration::from_secs(4)
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
    /// Last texture identifier handed out; never reused, so an evicted id cannot
    /// come back with different pixels.
    next_id: u64,
    /// `line_range().0` when the anchors were moved last.
    top_line: i32,
    /// Alternate-screen flag observed at the last intake.
    alternate_screen: bool,
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
            // Session ids are unique in the process, so the high half of the
            // identifier keeps texture keys unique across panes without a second
            // allocator.
            next_id: session.raw() << 32,
            top_line,
            alternate_screen: false,
        }
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
            // A refused payload is a dropped image: `decode` reports the reason and
            // nothing else uses it, so a malformed or oversized sequence can never
            // panic and never draws half an image (S5.2, S5.3).
            let Ok(decoded) = decode(&message, &limits) else {
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

    /// Drops the anchors a grid of `rows` x `cols` cells can never show.
    ///
    /// Called after a resize. Nothing is re-anchored — the anchoring model can only
    /// drop — so an image the reflow moved to another line keeps pointing at its old
    /// line number.
    pub fn drop_outside(&mut self, rows: u16, cols: u16) {
        self.retain(|placement| placement.anchor_line < rows as i32 && placement.column < cols);
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

    /// Keeps only the placements the predicate accepts, with their pixels.
    fn retain(&mut self, mut keep: impl FnMut(&ImagePlacement) -> bool) {
        let mut placements = Vec::with_capacity(self.store.len());
        let mut images = Vec::with_capacity(self.images.len());
        let taken = std::mem::take(&mut self.images);
        for (placement, image) in self.store.placements().iter().zip(taken) {
            if keep(placement) {
                placements.push(*placement);
                images.push(image);
            }
        }
        self.rebuild(placements, images);
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

    #[test]
    fn blink_gate_fires_twice_per_period() {
        let start = Instant::now();
        let (toggle, next) = blink_due(start, None);
        assert!(toggle, "the first check arms the timer");
        assert_eq!(next, start + CURSOR_BLINK_PERIOD);

        let (toggle, next_same) = blink_due(start + Duration::from_millis(100), Some(next));
        assert!(!toggle, "no toggle before the deadline");
        assert_eq!(next_same, next);

        let (toggle, next_after) = blink_due(next, Some(next));
        assert!(toggle, "toggles exactly at the deadline");
        assert_eq!(next_after, next + CURSOR_BLINK_PERIOD);
    }

    #[test]
    fn blink_gate_does_not_burst_after_a_long_stall() {
        let start = Instant::now();
        let deadline = start + CURSOR_BLINK_PERIOD;
        let late = start + CURSOR_BLINK_PERIOD * 10;
        let (toggle, next) = blink_due(late, Some(deadline));
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
    fn resize_discards_the_anchors_the_new_grid_cannot_show() {
        let mut images = SessionImages::new(SessionId::from_raw(10), 0);
        assert!(images.insert(anchored(30, 0, 1, 1), pixels(2, 2, 1)));
        assert!(images.insert(anchored(2, 0, 1, 1), pixels(2, 2, 2)));
        assert!(images.insert(anchored(0, 100, 1, 1), pixels(2, 2, 3)));

        images.drop_outside(24, 80);

        let kept: Vec<u8> = images
            .items()
            .map(|(_, _, pixels)| pixels.rgba[0])
            .collect();
        assert_eq!(kept, vec![2]);
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

        // A chunked kitty transfer is legal on the wire but not supported here, and
        // a truncated iTerm2 payload is simply broken: both are dropped silently.
        engine.process(b"\x1b_Ga=T,f=32,s=2,v=2,m=1;AAAAAAAAAAAAAAAAAAAAAA==\x1b\\");
        engine.process(b"\x1b]1337;File=inline=1:not-base64!\x07");
        assert_eq!(images.intake(&mut engine, image_metrics(), true), 0);
        assert!(images.is_empty());

        engine.process(b"still fine");
        let snapshot = engine.snapshot();
        assert_eq!(snapshot.lines[0].text().trim_end(), "still fine");
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

    #[test]
    fn the_first_press_creates_and_the_next_ones_hide_and_show_the_same_session() {
        assert_eq!(plan_floating_press(None), FloatingPress::Create);

        let mut open = floating(true);
        assert_eq!(plan_floating_press(Some(&open)), FloatingPress::Hide);
        open.set_visible(false);
        assert!(open.is_hidden());
        assert_eq!(
            open.session,
            SessionId::from_raw(21),
            "hiding never replaces the session, so the shell keeps its state"
        );

        assert_eq!(plan_floating_press(Some(&open)), FloatingPress::Show);
        open.set_visible(true);
        assert!(open.visible);
        assert_eq!(
            open.session,
            SessionId::from_raw(21),
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
    fn closing_the_floating_terminal_destroys_it_and_restores_the_mosaic_focus() {
        let open = floating(true);
        assert!(closes_floating(Some(&open), PaneId::from_raw(7)));
        assert!(
            !closes_floating(Some(&open), PaneId::from_raw(3)),
            "a mosaic pane is never the floating one"
        );
        assert!(!closes_floating(None, PaneId::from_raw(7)));
        // Once destroyed there is no floating left, so focus resolves to the mosaic.
        assert_eq!(
            keyboard_pane(None, Some(PaneId::from_raw(3))),
            Some(PaneId::from_raw(3))
        );
    }
}
