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
use astraterm_core::{CellMetrics, PaneId, Rect, Size, SplitAxis, TabId, WindowId};
use astraterm_renderer::Renderer;
use astraterm_renderer::paint::{self, PaintOptions, Scene, TextItem};
use astraterm_session::SessionManager;
use astraterm_settings_ui::panel::{PanelStyle, SettingsPanel};
use astraterm_terminal::snapshot::{CursorVisual, Snapshot};
use astraterm_workspace::{LayoutConfig, Pane, Tab, WindowState};
use winit::window::Window;

/// Height of the tab strip in cell rows.
const TAB_STRIP_ROWS: f32 = 1.0;
/// Base padding added to the tab strip and status bar, in device pixels.
const CHROME_PADDING: u32 = 6;

/// Blink period of the text cursor.
pub const CURSOR_BLINK_PERIOD: Duration = Duration::from_millis(530);

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
    /// Session ids bound to panes, so the window can be torn down cleanly.
    pub session_bindings: BTreeMap<PaneId, astraterm_core::SessionId>,
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
            session_bindings: BTreeMap::new(),
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

    /// Rectangle available to the terminal panes.
    pub fn terminal_rect(&self) -> Rect {
        let tab = self.tab_strip_height();
        let status = self.status_bar_height();
        let height = self.size.height.saturating_sub(tab + status);
        let y = match self.config.appearance.tab_bar_position {
            TabBarPosition::Bottom => 0,
            _ => tab as i32,
        };
        Rect::new(0, y, self.size.width, height)
    }

    /// Rectangle of the tab strip.
    pub fn tab_strip_rect(&self) -> Rect {
        match self.config.appearance.tab_bar_position {
            TabBarPosition::Hidden => Rect::new(0, 0, 0, 0),
            TabBarPosition::Bottom => Rect::new(
                0,
                self.size.height.saturating_sub(self.tab_strip_height()) as i32,
                self.size.width,
                self.tab_strip_height(),
            ),
            TabBarPosition::Top => Rect::new(0, 0, self.size.width, self.tab_strip_height()),
        }
    }

    /// Rectangle of the status bar.
    pub fn status_rect(&self) -> Rect {
        if !self.config.appearance.show_status_bar {
            return Rect::new(0, 0, 0, 0);
        }
        let height = self.status_bar_height();
        Rect::new(
            0,
            self.size.height.saturating_sub(height) as i32,
            self.size.width,
            height,
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
        self.pane_rects()
            .into_iter()
            .find(|(_, rect)| rect.contains(x, y))
            .map(|(pane, _)| pane)
    }

    /// Cell coordinates inside a pane for a pointer position.
    pub fn cell_at(&self, pane: PaneId, x: f64, y: f64) -> Option<(u16, u16)> {
        let rect = self.pane_rects().into_iter().find(|(id, _)| *id == pane)?.1;
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
        let mut offset = strip.x as f64 + 8.0;
        for (index, tab) in self.workspace.tabs.iter().enumerate() {
            let width = self.tab_width(tab) as f64;
            if x >= offset && x < offset + width {
                return Some(index);
            }
            offset += width;
        }
        None
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

    /// Currently focused pane of the active tab.
    pub fn focused_pane(&self) -> Option<PaneId> {
        self.workspace
            .active_tab()
            .map(|tab| tab.zoomed.unwrap_or(tab.active_pane))
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
    pub fn compose(&mut self, sessions: &mut SessionManager) -> Scene {
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
            let options = PaintOptions {
                padding: self.padding(),
                draw_cursor: focused == Some(*pane) && self.cursor_phase,
                cursor_override: Some(self.cursor_style()),
                background_alpha: 1.0,
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

            if let Overlay::Search(search) = &self.overlay {
                self.paint_search_highlights(&mut scene, *rect, &snapshot, search);
            }
        }

        self.compose_status_bar(&mut scene, sessions);
        self.compose_overlay(&mut scene, sessions);
        scene
    }

    fn compose_tab_strip(&self, scene: &mut Scene) {
        let strip = self.tab_strip_rect();
        if strip.is_empty() {
            return;
        }
        scene.push_quad(strip, self.theme.palette.tab_bar_background, 1.0);

        let mut offset = strip.x + 8;
        for tab in &self.workspace.tabs {
            let width = self.tab_width(tab);
            let active = tab.id == self.workspace.active_tab;
            if active {
                scene.push_quad(
                    Rect::new(offset, strip.y, width, strip.height),
                    self.theme.palette.tab_active_background,
                    1.0,
                );
                scene.push_quad(
                    Rect::new(offset, strip.bottom() - 2, width, 2),
                    self.theme.palette.accent,
                    1.0,
                );
            }
            scene.push_text(TextItem::plain(
                1_000_000 + tab.id.raw(),
                (
                    (offset + 8) as f32,
                    (strip.y + CHROME_PADDING as i32 / 2) as f32,
                ),
                self.tab_label(tab),
                paint::rgba(
                    if active {
                        self.theme.palette.tab_active_foreground
                    } else {
                        self.theme.palette.tab_inactive_foreground
                    },
                    1.0,
                ),
                strip,
            ));
            offset += width as i32;
        }
    }

    fn compose_status_bar(&self, scene: &mut Scene, sessions: &SessionManager) {
        let rect = self.status_rect();
        if rect.is_empty() {
            return;
        }
        scene.push_quad(rect, self.theme.palette.tab_bar_background, 1.0);

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

        scene.push_text(TextItem::plain(
            1_100_000,
            (
                (rect.x + 8) as f32,
                (rect.y + CHROME_PADDING as i32 / 2) as f32,
            ),
            parts.join("  ·  "),
            paint::rgba(self.theme.palette.tab_inactive_foreground, 1.0),
            rect,
        ));
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
    fn layout_config_maps_appearance_settings() {
        let mut config = Config::default();
        config.appearance.divider_thickness = 3;
        config.appearance.pane_gap = 4;
        let layout = layout_config(&config);
        assert_eq!(layout.divider, 3);
        assert_eq!(layout.gap, 4);
    }
}
