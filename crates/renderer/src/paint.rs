//! Rasterisation-independent painting: turns terminal snapshots and UI state into
//! a flat list of coloured quads and text runs.
//!
//! Keeping this pure means colour resolution, selection highlighting, cursor shapes
//! and cell layout are unit-tested without a window, a GPU or a font.

use astraterm_config::palette::{Palette, Rgb};
use astraterm_core::{CellMetrics, Padding, Rect};
use astraterm_terminal::snapshot::{
    CellFlags, CellSnapshot, CursorVisual, RowSnapshot, Snapshot, SnapshotColor,
};

/// A solid rectangle in device pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Quad {
    pub rect: Rect,
    /// Straight-alpha RGBA in 0.0..=1.0.
    pub color: [f32; 4],
}

/// A run of text sharing one style.
#[derive(Clone, Debug, PartialEq)]
pub struct Span {
    pub text: String,
    pub color: [f32; 4],
    pub bold: bool,
    pub italic: bool,
}

/// A positioned text run, clipped to a rectangle.
#[derive(Clone, Debug, PartialEq)]
pub struct TextItem {
    /// Stable identifier: reused buffer caches are keyed by it, so an item that
    /// keeps its identifier and content never gets reshaped.
    pub id: u64,
    /// Top-left position in device pixels.
    pub position: (f32, f32),
    pub spans: Vec<Span>,
    pub clip: Rect,
}

impl TextItem {
    /// Single-style text item.
    pub fn plain(
        id: u64,
        position: (f32, f32),
        text: impl Into<String>,
        color: [f32; 4],
        clip: Rect,
    ) -> Self {
        Self {
            id,
            position,
            spans: vec![Span {
                text: text.into(),
                color,
                bold: false,
                italic: false,
            }],
            clip,
        }
    }

    /// Combined text content, used for cache hashing.
    pub fn content_hash(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.spans.len().hash(&mut hasher);
        for span in &self.spans {
            span.text.hash(&mut hasher);
            span.bold.hash(&mut hasher);
            span.italic.hash(&mut hasher);
            for channel in span.color {
                channel.to_bits().hash(&mut hasher);
            }
        }
        hasher.finish()
    }
}

/// One frame's worth of drawing instructions.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Scene {
    /// Window clear colour (the terminal background of the active pane).
    pub clear: [f32; 4],
    pub quads: Vec<Quad>,
    pub texts: Vec<TextItem>,
}

impl Scene {
    /// Converts every colour in the scene to linear light, for sRGB targets.
    pub fn linearize(&mut self) {
        self.clear = linear_rgba(self.clear);
        for quad in &mut self.quads {
            quad.color = linear_rgba(quad.color);
        }
        for text in &mut self.texts {
            for span in &mut text.spans {
                span.color = linear_rgba(span.color);
            }
        }
    }

    pub fn with_clear(color: Rgb, opacity: f32) -> Self {
        Self {
            clear: rgba(color, opacity),
            quads: Vec::new(),
            texts: Vec::new(),
        }
    }

    pub fn push_quad(&mut self, rect: Rect, color: Rgb, alpha: f32) {
        if rect.is_empty() {
            return;
        }
        self.quads.push(Quad {
            rect,
            color: rgba(color, alpha),
        });
    }

    pub fn push_quad_rgba(&mut self, rect: Rect, color: [f32; 4]) {
        if rect.is_empty() {
            return;
        }
        self.quads.push(Quad { rect, color });
    }

    pub fn push_text(&mut self, item: TextItem) {
        if item.spans.iter().all(|span| span.text.is_empty()) {
            return;
        }
        self.texts.push(item);
    }
}

/// Resolves a snapshot colour against the active palette.
pub fn resolve(theme: &Palette, color: SnapshotColor, default: Rgb) -> Rgb {
    match color {
        SnapshotColor::Default => default,
        SnapshotColor::Indexed(index) => theme.ansi_color(index),
        SnapshotColor::Rgb(r, g, b) => Rgb::new(r, g, b),
    }
}

/// Converts an sRGB channel (0.0..=1.0) to linear light.
///
/// An sRGB render target expects linear values in the shader; writing sRGB values
/// directly washes the whole UI out.
pub fn srgb_to_linear(channel: f32) -> f32 {
    let channel = channel.clamp(0.0, 1.0);
    if channel <= 0.040_45 {
        channel / 12.92
    } else {
        ((channel + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_rgba(color: [f32; 4]) -> [f32; 4] {
    [
        srgb_to_linear(color[0]),
        srgb_to_linear(color[1]),
        srgb_to_linear(color[2]),
        color[3],
    ]
}

/// Converts a colour to premultiplication-friendly straight RGBA.
pub fn rgba(color: Rgb, alpha: f32) -> [f32; 4] {
    [
        color.r as f32 / 255.0,
        color.g as f32 / 255.0,
        color.b as f32 / 255.0,
        alpha.clamp(0.0, 1.0),
    ]
}

/// Per-pane painting options.
#[derive(Clone, Copy, Debug)]
pub struct PaintOptions {
    /// Terminal padding inside the pane rectangle.
    pub padding: Padding,
    /// Draw the cursor (hidden when the pane is not focused, or while blinking off).
    pub draw_cursor: bool,
    /// Cursor style override from configuration (`None` follows the application).
    pub cursor_override: Option<CursorVisual>,
    /// Alpha multiplier for the pane background, so a translucent window still
    /// paints opaque text.
    pub background_alpha: f32,
    /// Draw a one-pixel focus border.
    pub focused: bool,
    /// Divider colour, also used for the focus border when unfocused.
    pub show_border: bool,
}

impl Default for PaintOptions {
    fn default() -> Self {
        Self {
            padding: Padding { x: 0, y: 0 },
            draw_cursor: true,
            cursor_override: None,
            background_alpha: 1.0,
            focused: true,
            show_border: true,
        }
    }
}

/// Paints one pane's grid into `scene`.
///
/// `id_base` namespaces the text item identifiers so two panes never share a
/// cached glyph buffer.
pub fn paint_pane(
    scene: &mut Scene,
    id_base: u64,
    rect: Rect,
    snapshot: &Snapshot,
    theme: &Palette,
    metrics: CellMetrics,
    options: PaintOptions,
) {
    if rect.is_empty() {
        return;
    }

    // Pane background first, so text and selection sit on top.
    if options.show_border {
        let border = if options.focused {
            theme.pane_border_focused
        } else {
            theme.pane_border
        };
        scene.push_quad(rect, border, options.background_alpha);
    }
    let inner = if options.show_border {
        rect.shrunk(1)
    } else {
        rect
    };
    scene.push_quad(inner, theme.background, options.background_alpha);

    let origin_x = inner.x + options.padding.x as i32;
    let origin_y = inner.y + options.padding.y as i32;
    let cell_w = metrics.width.max(1.0);
    let lines = (inner.height.saturating_sub(2 * options.padding.y) as f32
        / metrics.height.max(1) as f32)
        .floor() as usize;

    // Selection highlight is painted beneath the glyphs.
    if let Some(selection) = snapshot.selection {
        for (row, line) in snapshot.lines.iter().enumerate().take(lines) {
            let buffer_line = row as i32 - snapshot.display_offset as i32;
            if buffer_line < selection.start.0 || buffer_line > selection.end.0 {
                continue;
            }
            let (from, to) = selection_columns(selection, buffer_line, line.cells.len());
            if from >= to {
                continue;
            }
            let x = origin_x + (from as f32 * cell_w) as i32;
            let width = ((to - from) as f32 * cell_w).ceil().max(1.0) as u32;
            let y = origin_y + row as i32 * metrics.height as i32;
            scene.push_quad_rgba(
                Rect::new(x, y, width, metrics.height),
                rgba(theme.selection_background, 1.0),
            );
        }
    }

    // Reverse video and explicit cell backgrounds sit above the pane background but
    // below the glyphs.
    paint_cell_backgrounds(scene, (origin_x, origin_y), snapshot, theme, metrics, lines);

    for (row, line) in snapshot.lines.iter().enumerate().take(lines) {
        let y = origin_y + row as i32 * metrics.height as i32;
        let clip = Rect::new(inner.x, y, inner.width, metrics.height);
        let item = row_text_item(id_base + row as u64, origin_x, y, line, theme, clip);
        scene.push_text(item);
    }

    if options.draw_cursor
        && let Some(cursor) = snapshot.cursor
    {
        paint_cursor(
            scene, id_base, inner, origin_x, origin_y, cursor, snapshot, theme, metrics, options,
        );
    }
}

/// Builds the text item for one row, merging adjacent cells with identical style
/// into a single span so shaping stays cheap.
fn row_text_item(
    id: u64,
    origin_x: i32,
    y: i32,
    row: &RowSnapshot,
    theme: &Palette,
    clip: Rect,
) -> TextItem {
    let mut spans: Vec<Span> = Vec::new();
    let mut current = String::new();
    let mut current_style: Option<(Rgb, bool, bool)> = None;

    for cell in &row.cells {
        if cell.wide_spacer {
            continue;
        }
        let (foreground, bold, italic) = cell_style(cell, theme);
        let style = (foreground, bold, italic);
        if current_style != Some(style) {
            if let Some((color, bold, italic)) = current_style
                && !current.is_empty()
            {
                spans.push(Span {
                    text: std::mem::take(&mut current),
                    color: rgba(color, 1.0),
                    bold,
                    italic,
                });
            }
            current_style = Some(style);
        }
        if cell.flags.hidden {
            current.push(' ');
        } else {
            current.push(cell.ch);
            if cell.wide {
                // Compensate the missing second column so following glyphs keep
                // their exact grid position.
                current.push(' ');
            }
        }
    }
    if let Some((color, bold, italic)) = current_style
        && !current.is_empty()
    {
        spans.push(Span {
            text: current,
            color: rgba(color, 1.0),
            bold,
            italic,
        });
    }

    TextItem {
        id,
        position: (origin_x as f32, y as f32),
        spans,
        clip,
    }
}

/// Resolves the foreground colour and attributes of a cell.
pub fn cell_style(cell: &CellSnapshot, theme: &Palette) -> (Rgb, bool, bool) {
    let mut foreground = resolve(theme, cell.fg, theme.foreground);
    let mut background = resolve(theme, cell.bg, theme.background);
    if cell.flags.inverse {
        std::mem::swap(&mut foreground, &mut background);
    }
    if cell.flags.dim {
        // Dim text keeps its hue but loses contrast, like a real terminal.
        foreground = foreground.mix(background, 0.45);
    }
    (foreground, cell.flags.bold, cell.flags.italic)
}

/// Background colour of a cell, honouring the inverse attribute.
pub fn cell_background(cell: &CellSnapshot, theme: &Palette) -> Option<Rgb> {
    if cell.bg == SnapshotColor::Default && !cell.flags.inverse {
        return None;
    }
    let mut foreground = resolve(theme, cell.fg, theme.foreground);
    let mut background = resolve(theme, cell.bg, theme.background);
    if cell.flags.inverse {
        std::mem::swap(&mut foreground, &mut background);
    }
    Some(background)
}

/// Whether a cell's background needs an explicit quad.
pub fn cell_needs_background(cell: &CellSnapshot) -> bool {
    !matches!(cell.bg, SnapshotColor::Default) || cell.flags.inverse
}

/// Whether a cell needs an underline quad.
pub fn cell_underlined(cell: &CellSnapshot) -> bool {
    cell.flags.underline && !cell.flags.hidden
}

/// Column range (exclusive end) of the selection within one line.
fn selection_columns(
    selection: astraterm_terminal::snapshot::SelectionSpan,
    buffer_line: i32,
    columns: usize,
) -> (usize, usize) {
    let (start_line, start_col) = selection.start;
    let (end_line, end_col) = selection.end;
    let from = if buffer_line == start_line && !selection.is_block {
        start_col
    } else {
        0
    };
    let to = if buffer_line == end_line && !selection.is_block {
        end_col + 1
    } else {
        columns
    };
    if selection.is_block {
        (start_col.min(columns), (end_col + 1).min(columns))
    } else {
        (from.min(columns), to.min(columns))
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_cursor(
    scene: &mut Scene,
    id_base: u64,
    inner: Rect,
    origin_x: i32,
    origin_y: i32,
    cursor: astraterm_terminal::snapshot::CursorSnapshot,
    snapshot: &Snapshot,
    theme: &Palette,
    metrics: CellMetrics,
    options: PaintOptions,
) {
    let style = options.cursor_override.unwrap_or(cursor.shape);
    if style == CursorVisual::Hidden {
        return;
    }
    let x = origin_x + (cursor.column as f32 * metrics.width) as i32;
    let y = origin_y + cursor.row as i32 * metrics.height as i32;
    if !inner.contains(x as f64, y as f64) {
        return;
    }
    let height = metrics.height;
    let width = metrics.width.ceil().max(1.0) as u32;

    match style {
        CursorVisual::Block => {
            scene.push_quad(Rect::new(x, y, width, height), theme.cursor, 1.0);
            // Repaint the character under the cursor in the contrast colour.
            if let Some(cell) = snapshot.cell(cursor.column, cursor.row) {
                let (_, bold, italic) = cell_style(cell, theme);
                scene.push_text(TextItem {
                    id: id_base + 0x10_000 + cursor.row as u64,
                    position: (x as f32, y as f32),
                    spans: vec![Span {
                        text: cell.text().to_string(),
                        color: rgba(theme.cursor_text, 1.0),
                        bold,
                        italic,
                    }],
                    clip: Rect::new(x, y, width, height),
                });
            }
        }
        CursorVisual::HollowBlock => {
            let thickness = 1.max(width / 12);
            scene.push_quad(Rect::new(x, y, width, thickness), theme.cursor, 1.0);
            scene.push_quad(
                Rect::new(x, y + height as i32 - thickness as i32, width, thickness),
                theme.cursor,
                1.0,
            );
            scene.push_quad(Rect::new(x, y, thickness, height), theme.cursor, 1.0);
            scene.push_quad(
                Rect::new(x + width as i32 - thickness as i32, y, thickness, height),
                theme.cursor,
                1.0,
            );
        }
        CursorVisual::Beam => {
            let thickness = 2.min(width);
            scene.push_quad(Rect::new(x, y, thickness, height), theme.cursor, 1.0);
        }
        CursorVisual::Underline => {
            let thickness = 2.min(height);
            scene.push_quad(
                Rect::new(x, y + height as i32 - thickness as i32, width, thickness),
                theme.cursor,
                1.0,
            );
        }
        CursorVisual::Hidden => {}
    }
}

/// Paints cell backgrounds (reverse video, explicit colours) on top of the pane
/// background but below the glyphs.
pub fn paint_cell_backgrounds(
    scene: &mut Scene,
    origin: (i32, i32),
    snapshot: &Snapshot,
    theme: &Palette,
    metrics: CellMetrics,
    max_rows: usize,
) {
    for (row, line) in snapshot.lines.iter().enumerate().take(max_rows) {
        let y = origin.1 + row as i32 * metrics.height as i32;
        let mut run_start: Option<(usize, Rgb)> = None;
        for (column, cell) in line.cells.iter().enumerate() {
            let background = if cell_needs_background(cell) {
                cell_background(cell, theme)
            } else {
                None
            };
            match (run_start, background) {
                (Some((start, color)), Some(next)) if color == next => {
                    let _ = (start, next);
                }
                (Some((start, color)), _) => {
                    let x = origin.0 + (start as f32 * metrics.width) as i32;
                    let width = (((column - start) as f32) * metrics.width).ceil().max(1.0) as u32;
                    scene.push_quad(Rect::new(x, y, width, metrics.height), color, 1.0);
                    run_start = background.map(|next| (column, next));
                }
                (None, Some(color)) => run_start = Some((column, color)),
                (None, None) => {}
            }
        }
        if let Some((start, color)) = run_start {
            let x = origin.0 + (start as f32 * metrics.width) as i32;
            let width = (((line.cells.len() - start) as f32) * metrics.width)
                .ceil()
                .max(1.0) as u32;
            scene.push_quad(Rect::new(x, y, width, metrics.height), color, 1.0);
        }

        // Underlines are drawn as quads so every underline style looks consistent.
        for (column, cell) in line.cells.iter().enumerate() {
            if !cell_underlined(cell) {
                continue;
            }
            let (color, _, _) = cell_style(cell, theme);
            let x = origin.0 + (column as f32 * metrics.width) as i32;
            let thickness = 1.max(metrics.height / 14);
            scene.push_quad(
                Rect::new(
                    x,
                    y + metrics.height as i32 - thickness as i32 - 1,
                    metrics.width.ceil().max(1.0) as u32,
                    thickness,
                ),
                color,
                1.0,
            );
        }
    }
}

/// Convenience: the style flags that affect shaping only.
pub fn shaping_flags(cell: &CellSnapshot) -> (bool, bool) {
    (cell.flags.bold, cell.flags.italic)
}

/// Kept for symmetry with [`CellFlags`] so callers do not need the upstream type.
pub fn flags_of(cell: &CellSnapshot) -> CellFlags {
    cell.flags
}

#[cfg(test)]
mod tests {
    use super::*;
    use astraterm_terminal::TerminalEngine;
    use astraterm_terminal::engine::EngineOptions;

    fn metrics() -> CellMetrics {
        CellMetrics {
            width: 10.0,
            height: 20,
            baseline: 15.0,
        }
    }

    fn theme() -> Palette {
        Palette::midnight()
    }

    fn snapshot(text: &str, cols: u16, rows: u16) -> Snapshot {
        let mut engine = TerminalEngine::new(EngineOptions {
            cols,
            rows,
            ..Default::default()
        });
        engine.process(text.as_bytes());
        engine.snapshot()
    }

    #[test]
    fn pane_background_and_border_are_painted() {
        let mut scene = Scene::default();
        let snapshot = snapshot("hi", 10, 2);
        paint_pane(
            &mut scene,
            0,
            Rect::new(0, 0, 200, 100),
            &snapshot,
            &theme(),
            metrics(),
            PaintOptions {
                focused: true,
                ..Default::default()
            },
        );
        assert!(scene.quads.len() >= 2);
        assert_eq!(scene.quads[0].color, rgba(theme().pane_border_focused, 1.0));
        assert_eq!(scene.quads[1].color, rgba(theme().background, 1.0));
        // One text item per visible row, plus one for the character under the cursor.
        assert_eq!(scene.texts.len(), 3, "row items plus the cursor cell");
        assert_eq!(scene.texts[0].id, 0);
        assert_eq!(scene.texts[1].id, 1);
        assert!(scene.texts[0].spans[0].text.starts_with("hi"));
    }

    #[test]
    fn focused_and_unfocused_borders_differ() {
        let mut focused = Scene::default();
        let mut unfocused = Scene::default();
        let snapshot = snapshot("x", 5, 1);
        paint_pane(
            &mut focused,
            0,
            Rect::new(0, 0, 50, 20),
            &snapshot,
            &theme(),
            metrics(),
            PaintOptions {
                focused: true,
                ..Default::default()
            },
        );
        paint_pane(
            &mut unfocused,
            0,
            Rect::new(0, 0, 50, 20),
            &snapshot,
            &theme(),
            metrics(),
            PaintOptions {
                focused: false,
                ..Default::default()
            },
        );
        assert_ne!(focused.quads[0].color, unfocused.quads[0].color);
    }

    #[test]
    fn true_colour_and_palette_cells_resolve() {
        let snapshot = snapshot("\x1b[38;2;1;2;3mT\x1b[0m\x1b[31mR\x1b[0m", 5, 1);
        let (true_color, _, _) = cell_style(&snapshot.lines[0].cells[0], &theme());
        assert_eq!(true_color, Rgb::new(1, 2, 3));
        let (palette, _, _) = cell_style(&snapshot.lines[0].cells[1], &theme());
        assert_eq!(palette, theme().ansi[1]);
    }

    #[test]
    fn inverse_swaps_foreground_and_background() {
        let snapshot = snapshot("\x1b[7mI\x1b[0m", 5, 1);
        let cell = snapshot.lines[0].cells[0];
        assert!(cell.flags.inverse);
        let (foreground, _, _) = cell_style(&cell, &theme());
        assert_eq!(foreground, theme().background);
        assert_eq!(cell_background(&cell, &theme()), Some(theme().foreground));
    }

    #[test]
    fn dim_text_loses_contrast() {
        let snapshot = snapshot("\x1b[2mD\x1b[0m", 5, 1);
        let cell = snapshot.lines[0].cells[0];
        let (dim, _, _) = cell_style(&cell, &theme());
        assert_ne!(dim, theme().foreground);
        assert!(dim.mix(theme().background, 0.0) == dim);
    }

    #[test]
    fn hidden_cells_render_as_spaces() {
        let snapshot = snapshot("\x1b[8msecret\x1b[0m", 10, 1);
        let item = row_text_item(
            0,
            0,
            0,
            &snapshot.lines[0],
            &theme(),
            Rect::new(0, 0, 100, 20),
        );
        let text: String = item.spans.iter().map(|span| span.text.clone()).collect();
        assert_eq!(text.trim_end(), "");
        assert!(text.starts_with("      "));
    }

    #[test]
    fn adjacent_cells_with_the_same_style_share_one_span() {
        let snapshot = snapshot("abcdef", 10, 1);
        let item = row_text_item(
            0,
            0,
            0,
            &snapshot.lines[0],
            &theme(),
            Rect::new(0, 0, 100, 20),
        );
        assert_eq!(item.spans.len(), 1);
        assert_eq!(item.spans[0].text.trim_end(), "abcdef");
    }

    #[test]
    fn style_changes_split_spans() {
        let snapshot = snapshot("\x1b[31mred\x1b[32mgreen\x1b[0m", 20, 1);
        let item = row_text_item(
            0,
            0,
            0,
            &snapshot.lines[0],
            &theme(),
            Rect::new(0, 0, 200, 20),
        );
        assert!(item.spans.len() >= 2, "each colour becomes its own span");
        assert_eq!(item.spans[0].color, rgba(theme().ansi[1], 1.0));
        assert_eq!(item.spans[1].color, rgba(theme().ansi[2], 1.0));
        assert!(item.spans[0].text.starts_with("red"));
        assert!(item.spans[1].text.starts_with("green"));
    }

    #[test]
    fn wide_characters_keep_column_alignment() {
        let snapshot = snapshot("你a", 10, 1);
        let item = row_text_item(
            0,
            0,
            0,
            &snapshot.lines[0],
            &theme(),
            Rect::new(0, 0, 100, 20),
        );
        let text: String = item.spans[0].text.clone();
        // Wide glyph + filler so the next glyph starts two cells later.
        assert!(text.starts_with("你 a"), "{text:?}");
    }

    #[test]
    fn selection_paints_a_highlight_rectangle() {
        let mut engine = TerminalEngine::new(EngineOptions {
            cols: 20,
            rows: 2,
            ..Default::default()
        });
        engine.process(b"hello world");
        engine.update_selection(
            0,
            0,
            astraterm_terminal::SelectionKind::Simple,
            alacritty_terminal::index::Side::Left,
        );
        engine.update_selection(
            4,
            0,
            astraterm_terminal::SelectionKind::SimpleOrExisting,
            alacritty_terminal::index::Side::Right,
        );
        let snapshot = engine.snapshot();
        let mut scene = Scene::default();
        paint_pane(
            &mut scene,
            0,
            Rect::new(0, 0, 200, 40),
            &snapshot,
            &theme(),
            metrics(),
            PaintOptions::default(),
        );
        let highlight = scene
            .quads
            .iter()
            .find(|quad| quad.color == rgba(theme().selection_background, 1.0))
            .expect("selection highlight");
        assert_eq!(highlight.rect.width, 50);
        assert_eq!(
            highlight.rect.x, 1,
            "the pane border insets the grid by one pixel"
        );
    }

    #[test]
    fn cursor_is_painted_in_its_configured_shape() {
        let snapshot = snapshot("x", 5, 1);
        let mut block = Scene::default();
        paint_pane(
            &mut block,
            0,
            Rect::new(0, 0, 100, 40),
            &snapshot,
            &theme(),
            metrics(),
            PaintOptions::default(),
        );
        // The focused pane border shares the accent colour, so compare cell-sized
        // quads only.
        let cell_quads = |scene: &Scene| -> Vec<Quad> {
            scene
                .quads
                .iter()
                .copied()
                .filter(|quad| quad.color == rgba(theme().cursor, 1.0) && quad.rect.width <= 10)
                .collect()
        };
        let block_quads = cell_quads(&block);
        assert_eq!(
            block_quads.len(),
            1,
            "block cursor draws exactly one filled cell quad"
        );
        assert_eq!(block_quads[0].rect.width, 10);
        assert_eq!(block_quads[0].rect.height, 20);

        let mut beam = Scene::default();
        paint_pane(
            &mut beam,
            0,
            Rect::new(0, 0, 100, 40),
            &snapshot,
            &theme(),
            metrics(),
            PaintOptions {
                cursor_override: Some(CursorVisual::Beam),
                ..Default::default()
            },
        );
        let beam_quads = cell_quads(&beam);
        assert_eq!(beam_quads.len(), 1);
        assert!(beam_quads[0].rect.width <= 2, "beam cursor is a thin bar");
    }

    #[test]
    fn cursor_is_skipped_when_disabled() {
        let snapshot = snapshot("x", 5, 1);
        let mut scene = Scene::default();
        paint_pane(
            &mut scene,
            0,
            Rect::new(0, 0, 100, 40),
            &snapshot,
            &theme(),
            metrics(),
            PaintOptions {
                draw_cursor: false,
                ..Default::default()
            },
        );
        // Only the (same-coloured) focus border remains; no cell-sized cursor quad.
        assert!(
            !scene
                .quads
                .iter()
                .any(|quad| quad.color == rgba(theme().cursor, 1.0) && quad.rect.width <= 10),
            "no cursor cell is painted when the cursor is disabled"
        );
    }

    #[test]
    fn cell_backgrounds_are_batched_into_runs() {
        let snapshot = snapshot("\x1b[41mred bg\x1b[0m", 20, 1);
        let mut scene = Scene::default();
        paint_cell_backgrounds(&mut scene, (0, 0), &snapshot, &theme(), metrics(), 1);
        assert_eq!(scene.quads.len(), 1);
        assert_eq!(scene.quads[0].color, rgba(theme().ansi[1], 1.0));
        assert_eq!(scene.quads[0].rect.width, 60);
    }

    #[test]
    fn underline_draws_a_quad() {
        let snapshot = snapshot("\x1b[4munder\x1b[0m", 10, 1);
        let mut scene = Scene::default();
        paint_cell_backgrounds(&mut scene, (0, 0), &snapshot, &theme(), metrics(), 1);
        assert!(!scene.quads.is_empty());
    }

    #[test]
    fn srgb_conversion_is_monotonic_and_bounded() {
        assert!(srgb_to_linear(0.0).abs() < f32::EPSILON);
        assert!((srgb_to_linear(1.0) - 1.0).abs() < f32::EPSILON);
        let mid = srgb_to_linear(0.5);
        assert!(
            mid > 0.2 && mid < 0.25,
            "0.5 sRGB is about 0.214 linear, got {mid}"
        );
        let charcoal = srgb_to_linear(0x1c as f32 / 255.0);
        assert!(charcoal < 0.02, "charcoal must stay dark, got {charcoal}");
    }

    #[test]
    fn linearizing_a_scene_converts_every_colour() {
        let mut scene = Scene::with_clear(Rgb::new(0x1c, 0x1f, 0x26), 1.0);
        scene.push_quad(Rect::new(0, 0, 10, 10), Rgb::new(0xff, 0xff, 0xff), 1.0);
        scene.push_text(TextItem::plain(
            1,
            (0.0, 0.0),
            "x",
            [0.5; 4],
            Rect::new(0, 0, 10, 10),
        ));
        scene.linearize();
        assert!(scene.clear[0] < 0.02);
        assert!((scene.quads[0].color[0] - 1.0).abs() < 1e-5);
        assert!((scene.texts[0].spans[0].color[0] - 0.214).abs() < 0.01);
        assert_eq!(scene.texts[0].spans[0].color[3], 0.5, "alpha is untouched");
    }

    #[test]
    fn content_hash_changes_with_colour_and_text() {
        let clip = Rect::new(0, 0, 10, 10);
        let a = TextItem::plain(1, (0.0, 0.0), "same", [1.0, 1.0, 1.0, 1.0], clip);
        let b = TextItem::plain(1, (0.0, 0.0), "same", [1.0, 1.0, 1.0, 1.0], clip);
        let c = TextItem::plain(1, (0.0, 0.0), "same", [0.5, 1.0, 1.0, 1.0], clip);
        let d = TextItem::plain(1, (0.0, 0.0), "other", [1.0, 1.0, 1.0, 1.0], clip);
        assert_eq!(a.content_hash(), b.content_hash());
        assert_ne!(a.content_hash(), c.content_hash());
        assert_ne!(a.content_hash(), d.content_hash());
    }
}
