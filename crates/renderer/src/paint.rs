//! Rasterisation-independent painting: turns terminal snapshots and UI state into
//! a flat list of coloured quads, image items and text runs.
//!
//! Keeping this pure means colour resolution, selection highlighting, cursor shapes
//! and cell layout are unit-tested without a window, a GPU or a font.

use std::sync::Arc;

use astraterm_config::palette::{Palette, Rgb};
use astraterm_core::{CellMetrics, Padding, Rect};
use astraterm_terminal::snapshot::{
    CellFlags, CellSnapshot, CursorVisual, RowSnapshot, Snapshot, SnapshotColor,
};

use crate::text::ClusterMeasurer;

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
///
/// The lists are drawn in a fixed order that ignores the order they were pushed
/// in: `quads` (backgrounds, selection, cursor, chrome), then `images`, then
/// `texts`. Terminal images are therefore always behind the glyphs that sit on
/// top of them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Scene {
    /// Window clear colour (the terminal background of the active pane).
    pub clear: [f32; 4],
    /// Rectangle this scene may paint into when it is drawn as an overlay layer.
    ///
    /// The mosaic scene ignores it: it owns the whole target. A layer composed for
    /// something that only covers part of the window (the floating terminal) names
    /// its own box here, so the renderer can scissor it and nothing it draws leaks
    /// outside — see [`crate::Renderer::render`].
    pub clip: Option<Rect>,
    pub quads: Vec<Quad>,
    pub images: Vec<ImageItem>,
    pub texts: Vec<TextItem>,
}

/// Decoded RGBA pixels of one image, shared with whoever produced them.
///
/// The buffer is allocated once by the producer and never copied per frame: the
/// renderer uploads it when the generation changes and the scene only holds an
/// `Arc` to it. Cloning the scene (linearisation, transparent-surface conversion)
/// clones the pointer, not the pixels.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImagePixels {
    pub width: u32,
    pub height: u32,
    /// Tightly packed RGBA, 4 bytes per pixel, row-major.
    pub rgba: Vec<u8>,
}

impl ImagePixels {
    /// Wraps decoded pixels, refusing a buffer that does not match the size.
    pub fn new(width: u32, height: u32, rgba: Vec<u8>) -> Option<Self> {
        if width == 0 || height == 0 || rgba.len() != width as usize * height as usize * 4 {
            return None;
        }
        Some(Self {
            width,
            height,
            rgba,
        })
    }

    /// Bytes this image occupies, on the CPU and on the GPU.
    pub fn byte_len(&self) -> usize {
        self.rgba.len()
    }
}

/// A decoded image placed on the canvas.
#[derive(Clone, Debug, PartialEq)]
pub struct ImageItem {
    /// Stable identifier: the texture cache is keyed by it, so an item that keeps
    /// its identifier is never uploaded twice for the same generation.
    pub id: u64,
    /// Destination rectangle in device pixels.
    pub rect: Rect,
    pub pixels: Arc<ImagePixels>,
    /// Changes whenever `pixels` holds new content, forcing a re-upload.
    pub generation: u64,
    /// Source sub-rectangle in normalised UV coordinates (`u0, v0, u1, v1`).
    pub uv: [f32; 4],
}

impl ImageItem {
    pub fn new(id: u64, rect: Rect, pixels: Arc<ImagePixels>, generation: u64) -> Self {
        Self {
            id,
            rect,
            pixels,
            generation,
            uv: [0.0, 0.0, 1.0, 1.0],
        }
    }

    /// Draws only a sub-rectangle of the texture, for an image that is clipped.
    pub fn with_uv(mut self, uv: [f32; 4]) -> Self {
        self.uv = uv;
        self
    }
}

impl Scene {
    /// Premultiplies the clear colour.
    ///
    /// A premultiplied surface (what Wayland compositors prefer) needs `rgb * a` in
    /// the clear value, because clearing writes the value without blending. The
    /// translucent background is painted exactly once, by the clear: the pane
    /// background is left transparent by the caller, otherwise overlapping layers
    /// would compound alpha and the window would look opaque again.
    pub fn premultiply_clear(&mut self) {
        let alpha = self.clear[3];
        self.clear[0] *= alpha;
        self.clear[1] *= alpha;
        self.clear[2] *= alpha;
    }

    /// Converts every colour in the scene to linear light, for sRGB targets.
    ///
    /// Image pixels are untouched: their bytes stay sRGB and the texture is
    /// uploaded as an sRGB view, so the GPU converts while sampling.
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
            ..Default::default()
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

    /// Queues an image. It is drawn above every quad and below every glyph,
    /// whatever order the caller pushes items in.
    pub fn push_image(&mut self, item: ImageItem) {
        if item.rect.is_empty() || item.pixels.rgba.is_empty() {
            return;
        }
        self.images.push(item);
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
/// cached glyph buffer. `measure` supplies the shaped advance of a grapheme cluster,
/// which is what decides whether that cluster needs its own grid-anchored segment
/// (S8); it is handed in rather than reached for because painting owns no font.
#[allow(clippy::too_many_arguments)]
pub fn paint_pane(
    scene: &mut Scene,
    id_base: u64,
    rect: Rect,
    snapshot: &Snapshot,
    theme: &Palette,
    metrics: CellMetrics,
    options: PaintOptions,
    measure: &mut ClusterMeasurer<'_>,
) {
    if rect.is_empty() {
        return;
    }

    // Pane background first, so text and selection sit on top.
    //
    // The border is chrome and always opaque; only the pane *background* follows
    // `background_alpha`, which the application sets to 0 so a translucent window is
    // painted once by the scene clear instead of compositing alpha twice.
    //
    // A border is drawn as four thin edges, never as a filled rectangle: filling the
    // whole pane and covering it with the background only works while the background
    // is opaque.
    if options.show_border {
        let border = if options.focused {
            theme.pane_border_focused
        } else {
            theme.pane_border
        };
        push_border(scene, rect, border);
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
        for item in row_text_item(
            id_base + row as u64,
            origin_x,
            y,
            line,
            theme,
            clip,
            cell_w,
            measure,
        ) {
            scene.push_text(item);
        }
    }

    if options.draw_cursor
        && let Some(cursor) = snapshot.cursor
    {
        paint_cursor(
            scene, id_base, inner, origin_x, origin_y, cursor, snapshot, theme, metrics, options,
        );
    }
}

/// Draws a one-pixel border along the four edges of `rect`.
pub fn push_border(scene: &mut Scene, rect: Rect, color: Rgb) {
    if rect.width == 0 || rect.height == 0 {
        return;
    }
    let thickness = 1u32.min(rect.width.min(rect.height));
    scene.push_quad(Rect::new(rect.x, rect.y, rect.width, thickness), color, 1.0);
    scene.push_quad(
        Rect::new(
            rect.x,
            rect.bottom() - thickness as i32,
            rect.width,
            thickness,
        ),
        color,
        1.0,
    );
    scene.push_quad(
        Rect::new(rect.x, rect.y, thickness, rect.height),
        color,
        1.0,
    );
    scene.push_quad(
        Rect::new(
            rect.right() - thickness as i32,
            rect.y,
            thickness,
            rect.height,
        ),
        color,
        1.0,
    );
}

/// The advance of a cluster may differ from the columns the model reserved by this
/// much before the cluster gets its own grid-anchored segment.
///
/// Half a pixel absorbs the rounding a monospace font and the cell metrics disagree
/// by, without letting a real drift through: the gaps this feature exists for are far
/// larger, measured with the shipped default font as `क` at 1.280 cells where the grid
/// reserves one, and emoji fallbacks at 2.075 cells where it reserves two.
const ANCHOR_TOLERANCE: f32 = 0.5;

/// Whether `c` continues the grapheme cluster of the character before it.
///
/// This list only recognises a continuation; it never decides that a cluster is
/// anchored, which is what measurement does (S8). It matters because a segment
/// boundary inside a cluster would shape the pieces separately: `👍` and `🏽` are
/// two codepoints of one glyph, and painting them as two items paints two glyphs.
///
/// The snapshot carries the codepoints the model stored as zero-width data of a
/// cell ([`CellSnapshot::zerowidth`], S9), so these ranges fire for the selectors,
/// keycaps, tags and combining marks that share a cell with their base — `❤️` and
/// `e`+U+0301 arrive whole — and for a skin-tone modifier in a wide cell of its
/// own. A joiner at the end of a cell also joins the *next* cell, whatever that
/// cell holds, which is what keeps a ZWJ sequence such as `👨‍👩‍👧` whole; see
/// [`cluster_at`].
fn cluster_continuation(c: char) -> bool {
    matches!(c,
        // Variation selectors (text and emoji presentation).
        '\u{FE00}'..='\u{FE0F}'
            // Zero-width joiner and emoji keycap.
            | '\u{200D}'
            | '\u{20E3}'
            // Skin-tone modifiers.
            | '\u{1F3FB}'..='\u{1F3FF}'
            // Tag characters (subdivision flags).
            | '\u{E0020}'..='\u{E007F}'
            // Combining marks, which are the general mark categories spelled out by
            // block: diacritics, Hebrew points, Arabic harakat, Syriac, Devanagari,
            // the extended and symbol supplements, and the half marks.
            | '\u{0300}'..='\u{036F}'
            | '\u{0483}'..='\u{0489}'
            | '\u{0591}'..='\u{05BD}'
            | '\u{05BF}'
            | '\u{05C1}'..='\u{05C7}'
            | '\u{0610}'..='\u{061A}'
            | '\u{064B}'..='\u{065F}'
            | '\u{0670}'
            | '\u{06D6}'..='\u{06ED}'
            | '\u{0711}'
            | '\u{0730}'..='\u{074A}'
            | '\u{07A6}'..='\u{07B0}'
            | '\u{0900}'..='\u{0903}'
            | '\u{093A}'..='\u{094F}'
            | '\u{0955}'..='\u{0957}'
            | '\u{0962}'..='\u{0963}'
            | '\u{1AB0}'..='\u{1AFF}'
            | '\u{1DC0}'..='\u{1DFF}'
            | '\u{20D0}'..='\u{20F0}'
            | '\u{FE20}'..='\u{FE2F}'
    )
}

/// Whether `c` is a regional indicator, half of a flag emoji.
fn regional_indicator(c: char) -> bool {
    matches!(c, '\u{1F1E6}'..='\u{1F1FF}')
}

/// One grapheme cluster of a row.
struct Cluster<'a> {
    /// Grid column of the first cell.
    column: usize,
    /// Columns the cluster owns, including the spacers of its wide cells.
    columns: usize,
    /// First cell, whose style paints the whole cluster.
    base: &'a CellSnapshot,
    text: String,
    /// Whether the cluster needs its own grid-anchored segment (S8).
    anchored: bool,
}

/// Reads the cluster that starts at `column` and decides whether it is anchored.
///
/// The walk absorbs every following cell that continues the base — a wide cell and
/// its spacer, a skin-tone modifier in its own cell, the second half of a flag, and
/// the member of a ZWJ sequence that follows a joiner — and stops at the first cell
/// that starts a new cluster. A hidden cell never continues anything: it paints a
/// space and hands its own style to nothing.
///
/// The cluster reports exactly the columns it consumed, so the walk of the row can
/// advance by what it reports without skipping a cell. A wide base whose spacer is
/// missing therefore owns one column, not two (S10).
///
/// Anchoring is then measured, not listed: a wide base always anchors (the model
/// already gave it two columns), and a cluster whose shaped advance disagrees with
/// the columns reserved for it anchors by more than [`ANCHOR_TOLERANCE`].
fn cluster_at<'a>(
    row: &'a RowSnapshot,
    column: usize,
    cell_width: f32,
    measure: &mut ClusterMeasurer<'_>,
) -> Cluster<'a> {
    let base = &row.cells[column];
    let mut text = String::new();
    push_cell_text(&mut text, base);
    // A cluster holds at most one regional-indicator pair, so the third indicator
    // starts the next cluster instead of joining this one. A hidden cell is not a
    // half of anything: it paints a space, so it must not seed the count either.
    let mut indicators = usize::from(!base.flags.hidden && regional_indicator(base.ch));
    let mut end = column + 1;
    while let Some(cell) = row.cells.get(end) {
        let continues = if cell.wide_spacer {
            // A spacer belongs to the wide cell before it, as it always did.
            true
        } else if cell.flags.hidden {
            // A hidden cell neither continues a cluster nor contributes an
            // indicator: absorbing it would append its space to the cluster, and
            // absorbing the visible cell after it would paint that cell in the
            // hidden cell's style (S10).
            false
        } else if text.ends_with('\u{200D}') {
            // The cell before ended with a zero-width joiner, so this cell is the
            // next member of the same sequence whatever its codepoint is: a ZWJ
            // emoji is stored as one wide cell per member, with the joiner kept as
            // zero-width data of the cell before it.
            true
        } else if regional_indicator(cell.ch) {
            indicators == 1
        } else {
            cluster_continuation(cell.ch)
        };
        if !continues {
            break;
        }
        if !cell.wide_spacer {
            push_cell_text(&mut text, cell);
            indicators += usize::from(regional_indicator(cell.ch));
        }
        end += 1;
    }
    let columns = end - column;
    let reserved = columns as f32 * cell_width;
    let measured = measure.advance(&text);
    Cluster {
        column,
        columns,
        base,
        text,
        anchored: base.wide || (measured - reserved).abs() > ANCHOR_TOLERANCE,
    }
}

/// Appends one cell's text to a cluster, hiding a hidden cell behind a space.
///
/// The zero-width continuations the model kept for the cell follow its base
/// character, so the shaper receives the cluster whole instead of separate cells
/// that would paint as separate glyphs (S9).
fn push_cell_text(text: &mut String, cell: &CellSnapshot) {
    if cell.flags.hidden {
        text.push(' ');
        return;
    }
    text.push(cell.ch);
    text.extend(cell.zerowidth().iter().copied());
}

/// Stable identifier of the text segment that starts at `column` of `row_id`.
///
/// [`TextItem::id`] keys the shaped-buffer cache in [`crate::text`], so every
/// segment needs an identifier that is deterministic for a given pair and
/// distinct from every other row and column. A segment starting at column zero
/// keeps the whole-row identifier the cache used before rows were split, so the
/// common single-segment row is unchanged; the remaining columns set the high
/// bit, which no practical row identifier uses, and pack the column below the
/// row.
fn segment_id(row_id: u64, column: usize) -> u64 {
    if column == 0 {
        return row_id;
    }
    (1u64 << 63) | ((row_id & 0x7fff_ffff) << 32) | (column as u64 & 0xffff_ffff)
}

/// Builds the text items for one row, splitting it into grid-anchored segments.
///
/// The row is walked one grapheme cluster at a time (S7): a cluster never straddles
/// two items, so the shaper always sees it whole. Clusters whose shaped advance
/// matches the columns the model reserved for them are merged into one item per style
/// run, which keeps the common row cheap; a wide cluster, or one whose measured
/// advance disagrees with its columns (S8), becomes its own item, positioned at
/// `origin_x + column * cell_width` and clipped to the columns it owns plus one cell
/// of slack, so its painted advance never shifts the rest of the row.
#[allow(clippy::too_many_arguments)]
fn row_text_item(
    row_id: u64,
    origin_x: i32,
    y: i32,
    row: &RowSnapshot,
    theme: &Palette,
    clip: Rect,
    cell_width: f32,
    measure: &mut ClusterMeasurer<'_>,
) -> Vec<TextItem> {
    let mut items: Vec<TextItem> = Vec::new();
    let mut spans: Vec<Span> = Vec::new();
    let mut current = String::new();
    let mut current_style: Option<(Rgb, bool, bool)> = None;
    let mut run_start: Option<usize> = None;

    let mut column = 0;
    while column < row.cells.len() {
        // An orphan spacer owns no segment of its own: the wide cell it belongs to
        // was never written, so there is nothing to paint.
        if row.cells[column].wide_spacer {
            column += 1;
            continue;
        }
        let cluster = cluster_at(row, column, cell_width, measure);
        column = cluster.column + cluster.columns;
        if cluster.anchored {
            flush_clean_run(
                &mut items,
                &mut spans,
                &mut current,
                &mut current_style,
                &mut run_start,
                row_id,
                origin_x,
                y,
                clip,
                cell_width,
            );
            items.push(anchored_item(
                row_id, origin_x, y, &cluster, theme, clip, cell_width,
            ));
            continue;
        }
        run_start.get_or_insert(cluster.column);
        let (foreground, bold, italic) = cell_style(cluster.base, theme);
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
        current.push_str(&cluster.text);
    }
    flush_clean_run(
        &mut items,
        &mut spans,
        &mut current,
        &mut current_style,
        &mut run_start,
        row_id,
        origin_x,
        y,
        clip,
        cell_width,
    );
    items
}

/// Closes the clean run being accumulated into one item positioned at its first
/// column, if the run has any text.
#[allow(clippy::too_many_arguments)]
fn flush_clean_run(
    items: &mut Vec<TextItem>,
    spans: &mut Vec<Span>,
    text: &mut String,
    style: &mut Option<(Rgb, bool, bool)>,
    start: &mut Option<usize>,
    row_id: u64,
    origin_x: i32,
    y: i32,
    clip: Rect,
    cell_width: f32,
) {
    if let Some((color, bold, italic)) = style.take()
        && !text.is_empty()
    {
        spans.push(Span {
            text: std::mem::take(text),
            color: rgba(color, 1.0),
            bold,
            italic,
        });
    }
    if let Some(column) = start.take()
        && !spans.is_empty()
    {
        items.push(TextItem {
            id: segment_id(row_id, column),
            position: (origin_x as f32 + column as f32 * cell_width, y as f32),
            spans: std::mem::take(spans),
            clip,
        });
    }
}

/// Builds the grid-anchored item for one cluster.
///
/// The text is the whole cluster and nothing else: the item's own columns already
/// reserve the space a wide glyph needs, so no filler is appended.
fn anchored_item(
    row_id: u64,
    origin_x: i32,
    y: i32,
    cluster: &Cluster<'_>,
    theme: &Palette,
    clip: Rect,
    cell_width: f32,
) -> TextItem {
    let (color, bold, italic) = cell_style(cluster.base, theme);
    let background = cell_column_clip(
        origin_x,
        y,
        cluster.column,
        cluster.columns,
        cell_width,
        clip,
    );
    TextItem {
        id: segment_id(row_id, cluster.column),
        position: (
            origin_x as f32 + cluster.column as f32 * cell_width,
            y as f32,
        ),
        spans: vec![Span {
            text: cluster.text.clone(),
            color: rgba(color, 1.0),
            bold,
            italic,
        }],
        clip: background,
    }
}

/// Rectangle covering `columns` grid columns from `column`, plus one cell of
/// slack, intersected with the row clip.
///
/// The slack keeps a glyph the model under-reserved from being cut in half: a
/// fallback emoji can advance up to two cells in a slot the grid gave one, and
/// scissoring it to that slot alone would show half a glyph. One spare cell
/// bounds the bleed without letting an over-wide glyph reach the next visible
/// character, whose own segment is positioned independently of this clip.
fn cell_column_clip(
    origin_x: i32,
    y: i32,
    column: usize,
    columns: usize,
    cell_width: f32,
    clip: Rect,
) -> Rect {
    let x = origin_x + (column as f32 * cell_width) as i32;
    let width = ((columns + 1) as f32 * cell_width).ceil().max(1.0) as u32;
    let rect = Rect::new(x, y, width, clip.height);
    let left = rect.x.max(clip.x);
    let top = rect.y.max(clip.y);
    let right = rect.right().min(clip.right());
    let bottom = rect.bottom().min(clip.bottom());
    Rect::new(
        left,
        top,
        (right - left).max(0) as u32,
        (bottom - top).max(0) as u32,
    )
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
    use astraterm_config::schema::FontConfig;
    use astraterm_terminal::TerminalEngine;
    use astraterm_terminal::engine::EngineOptions;
    use cosmic_text::FontSystem;

    use crate::text::{ClusterMeasurer, ClusterWidths, TextStyle};

    /// The font the tests measure with, plus its advance cache.
    ///
    /// Anchoring comes from measurement (S8), so the grid these tests assert against
    /// has to be the measured one: a hardcoded cell width that disagreed with the
    /// font would anchor every ASCII cluster and make the split assertions mean
    /// nothing.
    struct TestFont {
        system: FontSystem,
        style: TextStyle,
        widths: ClusterWidths,
    }

    impl TestFont {
        /// The shipped default font configuration.
        fn new() -> Self {
            let mut system = FontSystem::new();
            let style = TextStyle::new(&FontConfig::default(), 1.0, &mut system);
            Self {
                system,
                style,
                widths: ClusterWidths::new(),
            }
        }

        /// Cell metrics and a measurer for painting one row or pane.
        fn painter(&mut self) -> (CellMetrics, ClusterMeasurer<'_>) {
            let cell = self.style.cell;
            (
                cell,
                ClusterMeasurer::new(&mut self.system, &self.style, &mut self.widths),
            )
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
        let mut font = TestFont::new();
        let (metrics, mut measure) = font.painter();
        paint_pane(
            &mut scene,
            0,
            Rect::new(0, 0, 200, 100),
            &snapshot,
            &theme(),
            metrics,
            PaintOptions {
                focused: true,
                ..Default::default()
            },
            &mut measure,
        );
        assert!(
            scene.quads.len() >= 5,
            "four border edges plus the pane background"
        );
        assert!(
            scene
                .quads
                .iter()
                .any(|quad| quad.color == rgba(theme().pane_border_focused, 1.0)),
            "the focused border is painted"
        );
        assert!(
            scene
                .quads
                .iter()
                .any(|quad| quad.color == rgba(theme().background, 1.0)),
            "the pane background is painted"
        );
        // One text item per visible row, plus one for the character under the cursor.
        assert_eq!(scene.texts.len(), 3, "row items plus the cursor cell");
        assert_eq!(scene.texts[0].id, 0);
        assert_eq!(scene.texts[1].id, 1);
        assert!(scene.texts[0].spans[0].text.starts_with("hi"));
    }

    #[test]
    fn the_border_stays_opaque_when_the_pane_background_is_transparent() {
        let mut scene = Scene::default();
        let snapshot = snapshot("x", 5, 1);
        let mut font = TestFont::new();
        let (metrics, mut measure) = font.painter();
        paint_pane(
            &mut scene,
            0,
            Rect::new(0, 0, 100, 40),
            &snapshot,
            &theme(),
            metrics,
            PaintOptions {
                background_alpha: 0.0,
                focused: true,
                // The cursor shares the accent colour in this palette.
                draw_cursor: false,
                ..Default::default()
            },
            &mut measure,
        );
        let edges: Vec<&Quad> = scene
            .quads
            .iter()
            .filter(|quad| quad.color == rgba(theme().pane_border_focused, 1.0))
            .collect();
        assert_eq!(edges.len(), 4, "the border is four edges, not a fill");
        assert!(
            edges.iter().all(|edge| edge.color[3] == 1.0),
            "edges stay opaque"
        );
        assert!(
            edges
                .iter()
                .all(|edge| edge.rect.width.min(edge.rect.height) <= 1),
            "each edge is thin"
        );
    }

    #[test]
    fn focused_and_unfocused_borders_differ() {
        let mut focused = Scene::default();
        let mut unfocused = Scene::default();
        let snapshot = snapshot("x", 5, 1);
        let mut font = TestFont::new();
        let (metrics, mut measure) = font.painter();
        paint_pane(
            &mut focused,
            0,
            Rect::new(0, 0, 50, 20),
            &snapshot,
            &theme(),
            metrics,
            PaintOptions {
                focused: true,
                ..Default::default()
            },
            &mut measure,
        );
        paint_pane(
            &mut unfocused,
            0,
            Rect::new(0, 0, 50, 20),
            &snapshot,
            &theme(),
            metrics,
            PaintOptions {
                focused: false,
                ..Default::default()
            },
            &mut measure,
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
        let cell = &snapshot.lines[0].cells[0];
        assert!(cell.flags.inverse);
        let (foreground, _, _) = cell_style(cell, &theme());
        assert_eq!(foreground, theme().background);
        assert_eq!(cell_background(cell, &theme()), Some(theme().foreground));
    }

    #[test]
    fn dim_text_loses_contrast() {
        let snapshot = snapshot("\x1b[2mD\x1b[0m", 5, 1);
        let cell = &snapshot.lines[0].cells[0];
        let (dim, _, _) = cell_style(cell, &theme());
        assert_ne!(dim, theme().foreground);
        assert!(dim.mix(theme().background, 0.0) == dim);
    }

    #[test]
    fn hidden_cells_render_as_spaces() {
        let snapshot = snapshot("\x1b[8msecret\x1b[0m", 10, 1);
        let mut font = TestFont::new();
        let (metrics, mut measure) = font.painter();
        let items = row_text_item(
            0,
            0,
            0,
            &snapshot.lines[0],
            &theme(),
            Rect::new(0, 0, 100, 20),
            metrics.width,
            &mut measure,
        );
        assert_eq!(items.len(), 1, "hidden one-column cells stay one run");
        let text: String = items[0]
            .spans
            .iter()
            .map(|span| span.text.clone())
            .collect();
        assert_eq!(text.trim_end(), "");
        assert!(text.starts_with("      "));
    }

    #[test]
    fn adjacent_cells_with_the_same_style_share_one_span() {
        let snapshot = snapshot("abcdef", 10, 1);
        let mut font = TestFont::new();
        let (metrics, mut measure) = font.painter();
        let items = row_text_item(
            0,
            0,
            0,
            &snapshot.lines[0],
            &theme(),
            Rect::new(0, 0, 100, 20),
            metrics.width,
            &mut measure,
        );
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].spans.len(), 1);
        assert_eq!(items[0].spans[0].text.trim_end(), "abcdef");
    }

    #[test]
    fn style_changes_split_spans() {
        let snapshot = snapshot("\x1b[31mred\x1b[32mgreen\x1b[0m", 20, 1);
        let mut font = TestFont::new();
        let (metrics, mut measure) = font.painter();
        let items = row_text_item(
            0,
            0,
            0,
            &snapshot.lines[0],
            &theme(),
            Rect::new(0, 0, 200, 20),
            metrics.width,
            &mut measure,
        );
        let item = &items[0];
        assert!(item.spans.len() >= 2, "each colour becomes its own span");
        assert_eq!(item.spans[0].color, rgba(theme().ansi[1], 1.0));
        assert_eq!(item.spans[1].color, rgba(theme().ansi[2], 1.0));
        assert!(item.spans[0].text.starts_with("red"));
        assert!(item.spans[1].text.starts_with("green"));
    }

    /// Flattens a segment's spans into its complete text.
    fn segment_text(item: &TextItem) -> String {
        item.spans.iter().map(|span| span.text.clone()).collect()
    }

    #[test]
    fn grid_anchored_glyph_starts_at_its_own_column() {
        // `ab` is a clean run at column 0, `⏱` is a grid-anchored segment at
        // column 2 (it reserves one model column but its emoji fallback paints
        // wider), and `cd` is the next clean run, starting at column 3, so `d`
        // lands on column 4 without any filler.
        let snapshot = snapshot("ab⏱cd", 10, 1);
        let mut font = TestFont::new();
        let (metrics, mut measure) = font.painter();
        let cell_width = metrics.width;
        let origin_x = 4;
        let items = row_text_item(
            3,
            origin_x,
            0,
            &snapshot.lines[0],
            &theme(),
            Rect::new(0, 0, 100, 20),
            cell_width,
            &mut measure,
        );
        let starts: Vec<usize> = items
            .iter()
            .map(|item| ((item.position.0 - origin_x as f32) / cell_width) as usize)
            .collect();
        assert_eq!(
            starts,
            vec![0, 2, 3],
            "segments start on columns 0, 2 and 3"
        );
        assert_eq!(segment_text(&items[0]), "ab");
        assert_eq!(
            segment_text(&items[1]),
            "⏱",
            "no filler space after the glyph"
        );
        assert!(
            segment_text(&items[2]).starts_with("cd"),
            "the next clean run resumes at column 3, so `d` is painted on column 4: {:?}",
            segment_text(&items[2])
        );
        for item in &items {
            assert!(!segment_text(item).contains("⏱ "), "filler spaces are gone");
        }
        // The anchored segment is clipped to its own column plus one cell of
        // slack, so a fallback glyph wider than the reserved column is not cut.
        let anchored = &items[1];
        assert_eq!(
            anchored.clip,
            Rect::new(
                origin_x + (2.0 * cell_width) as i32,
                0,
                (2.0 * cell_width).ceil() as u32,
                20
            )
        );
    }

    #[test]
    fn wide_glyphs_own_their_segments_without_filler_spaces() {
        let snapshot = snapshot("你好AB", 10, 1);
        let mut font = TestFont::new();
        let (metrics, mut measure) = font.painter();
        let cell_width = metrics.width;
        let items = row_text_item(
            0,
            0,
            0,
            &snapshot.lines[0],
            &theme(),
            Rect::new(0, 0, 100, 20),
            cell_width,
            &mut measure,
        );
        assert_eq!(items.len(), 3, "two wide segments plus the ASCII run");
        assert_eq!(items[0].position.0, 0.0);
        assert_eq!(segment_text(&items[0]), "你");
        assert_eq!(items[1].position.0, 2.0 * cell_width);
        assert_eq!(segment_text(&items[1]), "好");
        assert_eq!(items[2].position.0, 4.0 * cell_width);
        assert!(segment_text(&items[2]).starts_with("AB"));
        // Each wide glyph is clipped to the two columns it owns plus the one
        // spare cell.
        let clip_of = |column: usize| {
            Rect::new(
                (column as f32 * cell_width) as i32,
                0,
                (3.0 * cell_width).ceil() as u32,
                20,
            )
        };
        assert_eq!(items[0].clip, clip_of(0));
        assert_eq!(items[1].clip, clip_of(2));
        for item in &items {
            let text = segment_text(item);
            assert!(
                !text.contains("你 "),
                "no filler after a wide glyph: {text:?}"
            );
            assert!(
                !text.contains("好 "),
                "no filler after a wide glyph: {text:?}"
            );
        }
    }

    #[test]
    fn plain_ascii_rows_keep_one_item_with_one_span() {
        // S6: the common row must behave exactly as before the split, including
        // keeping the whole-row identifier the cache was keyed by.
        let snapshot = snapshot("plain", 10, 1);
        let mut font = TestFont::new();
        let (metrics, mut measure) = font.painter();
        let clip = Rect::new(0, 0, 100, 20);
        let items = row_text_item(
            7,
            4,
            0,
            &snapshot.lines[0],
            &theme(),
            clip,
            metrics.width,
            &mut measure,
        );
        assert_eq!(items.len(), 1);
        let item = &items[0];
        assert_eq!(item.spans.len(), 1);
        assert_eq!(item.id, 7);
        assert_eq!(item.position, (4.0, 0.0));
        assert_eq!(item.clip, clip);
        assert_eq!(item.spans[0].text.trim_end(), "plain");
    }

    #[test]
    fn segment_ids_are_stable_and_distinct() {
        assert_eq!(segment_id(3, 4), segment_id(3, 4), "deterministic");
        assert_ne!(segment_id(3, 4), segment_id(3, 5), "different columns");
        assert_ne!(segment_id(3, 4), segment_id(4, 4), "different rows");
        assert_ne!(
            segment_id(3, 0),
            segment_id(4, 0),
            "different rows at column 0"
        );
        // A row whose first segment starts at column 0 keeps the identifier the
        // cache already used for the whole row.
        assert_eq!(segment_id(0, 0), 0);
        assert_eq!(segment_id(9, 0), 9);
    }

    #[test]
    fn the_codepoint_list_only_recognises_continuations() {
        // S7: the list decides whether the next cell still belongs to the cluster
        // being painted. Anchoring is measured (S8), so no test here asks a
        // codepoint whether it anchors: `grid_anchor_required` no longer exists.
        for c in [
            '\u{FE0E}',  // text presentation selector
            '\u{FE0F}',  // emoji presentation selector
            '\u{200D}',  // zero-width joiner
            '\u{20E3}',  // keycap
            '\u{1F3FD}', // skin-tone modifier
            '\u{E0067}', // tag character
            '\u{0301}',  // combining acute
            '\u{094D}',  // devanagari virama
            '\u{20D0}',  // combining mark for symbols
        ] {
            assert!(cluster_continuation(c), "U+{:04X} continues a cluster", c as u32);
        }
        for c in ['a', ' ', 'Z', '0', '你', 'क', '❤', '\u{1F44D}', '│', '─'] {
            assert!(
                !cluster_continuation(c),
                "U+{:04X} starts a cluster",
                c as u32
            );
        }
        // A regional indicator is a base: the pair rule only absorbs the second
        // half of a flag, never a third indicator.
        assert!(regional_indicator('\u{1F1EA}'));
        assert!(!cluster_continuation('\u{1F1EA}'));
    }

    #[test]
    fn a_skin_tone_modifier_stays_in_its_base_segment() {
        // S7: the model stores the modifier in a wide cell of its own, so deciding
        // per cell shaped `👍` and `🏽` as two glyphs. One item must carry both.
        let snapshot = snapshot("\u{1F44D}\u{1F3FD}", 4, 1);
        let mut font = TestFont::new();
        let (metrics, mut measure) = font.painter();
        let cell_width = metrics.width;
        let items = row_text_item(
            0,
            0,
            0,
            &snapshot.lines[0],
            &theme(),
            Rect::new(0, 0, 200, 20),
            cell_width,
            &mut measure,
        );
        assert_eq!(items.len(), 1, "base and modifier are one segment");
        let item = &items[0];
        assert_eq!(segment_text(item), "\u{1F44D}\u{1F3FD}");
        assert_eq!(item.position, (0.0, 0.0));
        assert_eq!(item.spans.len(), 1);
        // The cluster owns the four columns the model reserved (base, its spacer,
        // the modifier in its own wide cell, its spacer) plus one cell of slack.
        assert_eq!(item.clip.width, (5.0 * cell_width).ceil() as u32);
    }

    #[test]
    fn a_regional_indicator_pair_stays_one_segment() {
        // S7: the two halves are one-column cells, so a per-cell split drew a pair
        // of indicator letters instead of the flag.
        let snapshot = snapshot("\u{1F1EA}\u{1F1F8}", 2, 1);
        let mut font = TestFont::new();
        let (metrics, mut measure) = font.painter();
        let cell_width = metrics.width;
        let items = row_text_item(
            0,
            0,
            0,
            &snapshot.lines[0],
            &theme(),
            Rect::new(0, 0, 200, 20),
            cell_width,
            &mut measure,
        );
        assert_eq!(items.len(), 1, "the flag is one segment");
        let item = &items[0];
        assert_eq!(segment_text(item), "\u{1F1EA}\u{1F1F8}");
        assert_eq!(item.position, (0.0, 0.0));
        // Two reserved columns plus the one cell of slack.
        assert_eq!(item.clip.width, (3.0 * cell_width).ceil() as u32);
    }

    #[test]
    fn a_third_regional_indicator_starts_a_new_cluster() {
        // A cluster holds one flag: the next pair must not be swept into the first.
        let snapshot = snapshot("\u{1F1EA}\u{1F1F8}\u{1F1EB}\u{1F1F7}", 4, 1);
        let mut font = TestFont::new();
        let (metrics, mut measure) = font.painter();
        let cell_width = metrics.width;
        let items = row_text_item(
            0,
            0,
            0,
            &snapshot.lines[0],
            &theme(),
            Rect::new(0, 0, 200, 20),
            cell_width,
            &mut measure,
        );
        assert_eq!(items.len(), 2, "two flags, two segments");
        assert_eq!(segment_text(&items[0]), "\u{1F1EA}\u{1F1F8}");
        assert_eq!(items[0].position, (0.0, 0.0));
        assert_eq!(segment_text(&items[1]), "\u{1F1EB}\u{1F1F7}");
        assert_eq!(items[1].position, (2.0 * cell_width, 0.0));
    }

    #[test]
    fn zero_width_continuations_shape_inside_one_cluster() {
        // S9: with the snapshot carrying the zero-width data, the shaper receives the
        // whole grapheme cluster instead of its base character alone, and no segment
        // boundary falls inside it.
        let cases: [(&str, &str, usize); 5] = [
            // Emoji presentation selector.
            ("\u{2764}\u{FE0F}", "\u{2764}\u{FE0F}", 1),
            // Keycap: selector plus enclosing keycap.
            ("\u{0031}\u{FE0F}\u{20E3}", "1\u{FE0F}\u{20E3}", 1),
            // Text presentation selector.
            ("\u{2764}\u{FE0E}", "\u{2764}\u{FE0E}", 1),
            // Combining acute accent.
            ("e\u{0301}", "e\u{0301}", 1),
            // ZWJ family emoji: three wide cells joined by the joiners kept in the
            // zero-width data of the cell before each one.
            (
                "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}",
                "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}",
                6,
            ),
        ];
        for (input, expected, columns) in cases {
            let snapshot = snapshot(input, 10, 1);
            let mut font = TestFont::new();
            let (metrics, mut measure) = font.painter();
            let cell_width = metrics.width;
            let cluster = cluster_at(&snapshot.lines[0], 0, cell_width, &mut measure);
            assert_eq!(cluster.text, expected, "{input:?} is one cluster");
            assert_eq!(cluster.columns, columns, "{input:?} owns its columns");
            let items = row_text_item(
                0,
                0,
                0,
                &snapshot.lines[0],
                &theme(),
                Rect::new(0, 0, 200, 20),
                cell_width,
                &mut measure,
            );
            assert!(
                items
                    .iter()
                    .any(|item| segment_text(item).contains(expected)),
                "{input:?} must reach the shaper in one segment instead of split: {:?}",
                items.iter().map(segment_text).collect::<Vec<_>>()
            );
        }
    }

    /// A row of explicitly built cells: a shape the emulator rarely emits but a
    /// snapshot can still carry.
    fn row_of(cells: Vec<CellSnapshot>) -> RowSnapshot {
        RowSnapshot { cells }
    }

    #[test]
    fn a_wide_base_without_a_spacer_does_not_skip_the_next_cell() {
        // S10: a wide cell normally owns two columns, the second being its spacer.
        // When the spacer is genuinely missing the cluster must still consume exactly
        // the columns it reports, or the cell after it is skipped and never painted.
        let row = row_of(vec![
            CellSnapshot {
                ch: '你',
                wide: true,
                ..Default::default()
            },
            CellSnapshot {
                ch: 'x',
                ..Default::default()
            },
        ]);
        let mut font = TestFont::new();
        let (metrics, mut measure) = font.painter();
        let cell_width = metrics.width;
        let items = row_text_item(
            0,
            0,
            0,
            &row,
            &theme(),
            Rect::new(0, 0, 200, 20),
            cell_width,
            &mut measure,
        );
        assert_eq!(items.len(), 2, "the wide glyph and the cell after it");
        assert_eq!(segment_text(&items[0]), "你");
        assert_eq!(
            items[1].position.0, cell_width,
            "the next cell keeps its own column"
        );
        assert!(
            segment_text(&items[1]).starts_with('x'),
            "the cell after a wide base is still painted"
        );
    }

    #[test]
    fn a_hidden_indicator_does_not_absorb_the_visible_one() {
        // S10: a hidden cell contributes no regional indicator, so the visible half
        // after it starts its own cluster instead of inheriting the hidden style.
        let row = row_of(vec![
            CellSnapshot {
                ch: '\u{1F1EA}',
                fg: SnapshotColor::Indexed(1),
                flags: CellFlags {
                    hidden: true,
                    ..Default::default()
                },
                ..Default::default()
            },
            CellSnapshot {
                ch: '\u{1F1F8}',
                fg: SnapshotColor::Indexed(2),
                ..Default::default()
            },
        ]);
        let mut font = TestFont::new();
        let (metrics, mut measure) = font.painter();
        let cell_width = metrics.width;
        let hidden = cluster_at(&row, 0, cell_width, &mut measure);
        assert_eq!(hidden.text, " ", "a hidden cell paints as a space");
        assert_eq!(hidden.columns, 1, "a hidden cell owns no continuation");
        let visible = cluster_at(&row, 1, cell_width, &mut measure);
        assert_eq!(visible.text, "\u{1F1F8}");
        assert_eq!(visible.columns, 1);
        let items = row_text_item(
            0,
            0,
            0,
            &row,
            &theme(),
            Rect::new(0, 0, 200, 20),
            cell_width,
            &mut measure,
        );
        let span = items
            .iter()
            .flat_map(|item| &item.spans)
            .find(|span| span.text.contains('\u{1F1F8}'))
            .expect("the visible indicator is painted");
        assert_eq!(span.text, "\u{1F1F8}", "the hidden cell does not join it");
        assert_eq!(
            span.color,
            rgba(theme().ansi[2], 1.0),
            "the visible half keeps its own style"
        );
    }

    #[test]
    fn a_hidden_continuation_does_not_join_the_visible_indicator() {
        // S10 the other way round: a hidden cell neither continues a cluster nor
        // lends its hidden state to the visible half it follows.
        let row = row_of(vec![
            CellSnapshot {
                ch: '\u{1F1EA}',
                fg: SnapshotColor::Indexed(2),
                ..Default::default()
            },
            CellSnapshot {
                ch: '\u{1F1F8}',
                fg: SnapshotColor::Indexed(1),
                flags: CellFlags {
                    hidden: true,
                    ..Default::default()
                },
                ..Default::default()
            },
        ]);
        let mut font = TestFont::new();
        let (metrics, mut measure) = font.painter();
        let cell_width = metrics.width;
        let cluster = cluster_at(&row, 0, cell_width, &mut measure);
        assert_eq!(cluster.text, "\u{1F1EA}", "the hidden half stays out");
        assert_eq!(cluster.columns, 1);
        let items = row_text_item(
            0,
            0,
            0,
            &row,
            &theme(),
            Rect::new(0, 0, 200, 20),
            cell_width,
            &mut measure,
        );
        let span = items
            .iter()
            .flat_map(|item| &item.spans)
            .find(|span| span.text.contains('\u{1F1EA}'))
            .expect("the visible indicator is painted");
        assert_eq!(span.text, "\u{1F1EA}", "no hidden space is appended");
        assert_eq!(span.color, rgba(theme().ansi[2], 1.0));
    }

    #[test]
    fn a_fallback_script_anchors_because_its_advance_is_measured() {
        // S8: `क` is in no emoji or symbol block, so no codepoint list anchors it.
        // Its shaped advance is 1.280 cells while the model reserves one, so it
        // drifts exactly the way an emoji fallback does — and the measurement, not
        // a list, is what splits the row here. ASCII in the same row, whose advance
        // matches its column, stays in one run.
        let snapshot = snapshot("\u{0915}\u{0915}\u{0915}ab", 10, 1);
        let mut font = TestFont::new();
        let (metrics, mut measure) = font.painter();
        let cell_width = metrics.width;
        let clip = Rect::new(0, 0, 200, 20);
        assert!(
            measure.advance("\u{0915}") - cell_width > ANCHOR_TOLERANCE,
            "the fixture only proves anything if devanagari actually drifts"
        );
        assert!(
            (measure.advance("a") - cell_width).abs() <= ANCHOR_TOLERANCE,
            "ASCII advances exactly one cell, so it must not anchor"
        );
        let items = row_text_item(
            0,
            0,
            0,
            &snapshot.lines[0],
            &theme(),
            clip,
            cell_width,
            &mut measure,
        );
        assert_eq!(items.len(), 4, "three anchored clusters plus the ASCII run");
        for (column, item) in items.iter().take(3).enumerate() {
            assert_eq!(segment_text(item), "\u{0915}");
            assert_eq!(item.position, (column as f32 * cell_width, 0.0));
            assert!(
                item.clip != clip,
                "an anchored cluster is clipped to its own column"
            );
        }
        // The ASCII run keeps the row clip and one span: it was never split, which
        // is what proves the split came from measurement rather than a list.
        let ascii = &items[3];
        assert_eq!(ascii.position, (3.0 * cell_width, 0.0));
        assert_eq!(ascii.clip, clip);
        assert_eq!(ascii.spans.len(), 1);
        assert!(segment_text(ascii).starts_with("ab"));
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
        let mut font = TestFont::new();
        let (metrics, mut measure) = font.painter();
        paint_pane(
            &mut scene,
            0,
            Rect::new(0, 0, 200, 40),
            &snapshot,
            &theme(),
            metrics,
            PaintOptions::default(),
            &mut measure,
        );
        let highlight = scene
            .quads
            .iter()
            .find(|quad| quad.color == rgba(theme().selection_background, 1.0))
            .expect("selection highlight");
        assert_eq!(highlight.rect.width, (5.0 * metrics.width).ceil() as u32);
        assert_eq!(
            highlight.rect.x, 1,
            "the pane border insets the grid by one pixel"
        );
    }

    #[test]
    fn cursor_is_painted_in_its_configured_shape() {
        let snapshot = snapshot("x", 5, 1);
        let mut font = TestFont::new();
        let (metrics, mut measure) = font.painter();
        let mut block = Scene::default();
        paint_pane(
            &mut block,
            0,
            Rect::new(0, 0, 100, 40),
            &snapshot,
            &theme(),
            metrics,
            PaintOptions::default(),
            &mut measure,
        );
        // The focused pane border shares the accent colour, so compare cell-sized
        // quads only.
        let cell_width = metrics.width.ceil().max(1.0) as u32;
        let cell_quads = |scene: &Scene| -> Vec<Quad> {
            scene
                .quads
                .iter()
                .copied()
                .filter(|quad| {
                    quad.color == rgba(theme().cursor, 1.0)
                        && quad.rect.width <= cell_width
                        && quad.rect.height <= metrics.height
                })
                .collect()
        };
        let block_quads = cell_quads(&block);
        assert_eq!(
            block_quads.len(),
            1,
            "block cursor draws exactly one filled cell quad"
        );
        assert_eq!(block_quads[0].rect.width, cell_width);
        assert_eq!(block_quads[0].rect.height, metrics.height);

        let mut beam = Scene::default();
        paint_pane(
            &mut beam,
            0,
            Rect::new(0, 0, 100, 40),
            &snapshot,
            &theme(),
            metrics,
            PaintOptions {
                cursor_override: Some(CursorVisual::Beam),
                ..Default::default()
            },
            &mut measure,
        );
        let beam_quads = cell_quads(&beam);
        assert_eq!(beam_quads.len(), 1);
        assert!(beam_quads[0].rect.width <= 2, "beam cursor is a thin bar");
    }

    #[test]
    fn cursor_is_skipped_when_disabled() {
        let snapshot = snapshot("x", 5, 1);
        let mut font = TestFont::new();
        let (metrics, mut measure) = font.painter();
        let mut scene = Scene::default();
        paint_pane(
            &mut scene,
            0,
            Rect::new(0, 0, 100, 40),
            &snapshot,
            &theme(),
            metrics,
            PaintOptions {
                draw_cursor: false,
                ..Default::default()
            },
            &mut measure,
        );
        // Only the (same-coloured) focus border remains; no cell-sized cursor quad.
        let cell_width = metrics.width.ceil().max(1.0) as u32;
        assert!(
            !scene
                .quads
                .iter()
                .any(|quad| quad.color == rgba(theme().cursor, 1.0)
                    && quad.rect.width <= cell_width
                    && quad.rect.height <= metrics.height),
            "no cursor cell is painted when the cursor is disabled"
        );
    }

    #[test]
    fn cell_backgrounds_are_batched_into_runs() {
        let snapshot = snapshot("\x1b[41mred bg\x1b[0m", 20, 1);
        let mut font = TestFont::new();
        let (metrics, _) = font.painter();
        let mut scene = Scene::default();
        paint_cell_backgrounds(&mut scene, (0, 0), &snapshot, &theme(), metrics, 1);
        assert_eq!(scene.quads.len(), 1);
        assert_eq!(scene.quads[0].color, rgba(theme().ansi[1], 1.0));
        assert_eq!(
            scene.quads[0].rect.width,
            (6.0 * metrics.width).ceil().max(1.0) as u32
        );
    }

    #[test]
    fn underline_draws_a_quad() {
        let snapshot = snapshot("\x1b[4munder\x1b[0m", 10, 1);
        let mut font = TestFont::new();
        let (metrics, _) = font.painter();
        let mut scene = Scene::default();
        paint_cell_backgrounds(&mut scene, (0, 0), &snapshot, &theme(), metrics, 1);
        assert!(!scene.quads.is_empty());
    }

    #[test]
    fn a_scene_that_names_no_clip_covers_the_whole_target() {
        // The mosaic scene is drawn without a scissor: only a layer composed for
        // part of the window (the floating terminal) names a clip.
        let mosaic = Scene::with_clear(Rgb::new(0x1c, 0x1f, 0x26), 1.0);
        assert_eq!(mosaic.clip, None);
        let layer = Scene {
            clip: Some(Rect::new(4, 5, 60, 40)),
            ..Scene::default()
        };
        assert_eq!(layer.clip, Some(Rect::new(4, 5, 60, 40)));
    }

    #[test]
    fn premultiplying_the_clear_scales_rgb_by_alpha() {
        let mut scene = Scene::with_clear(Rgb::new(0x80, 0x40, 0x20), 0.5);
        scene.premultiply_clear();
        let expected = 0x80 as f32 / 255.0 * 0.5;
        assert!((scene.clear[0] - expected).abs() < 1e-5);
        assert!((scene.clear[3] - 0.5).abs() < 1e-5, "alpha is preserved");
        // An opaque background is unchanged.
        let mut opaque = Scene::with_clear(Rgb::new(0x80, 0x40, 0x20), 1.0);
        let before = opaque.clear;
        opaque.premultiply_clear();
        assert_eq!(opaque.clear, before);
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

    fn pixels() -> Arc<ImagePixels> {
        Arc::new(ImagePixels::new(2, 1, vec![0xff; 8]).expect("two RGBA pixels"))
    }

    #[test]
    fn images_are_queued_apart_from_quads_and_text() {
        let pixels = pixels();
        let mut scene = Scene::default();
        // Pushed in the wrong order on purpose: the lists, not the push order,
        // decide that quads are painted first, then images, then glyphs.
        scene.push_text(TextItem::plain(
            0,
            (0.0, 0.0),
            "text",
            [1.0; 4],
            Rect::new(0, 0, 10, 10),
        ));
        scene.push_image(ImageItem::new(
            7,
            Rect::new(4, 5, 20, 10),
            Arc::clone(&pixels),
            3,
        ));
        scene.push_quad(Rect::new(0, 0, 10, 10), theme().background, 1.0);

        assert_eq!(scene.quads.len(), 1);
        assert_eq!(scene.images.len(), 1);
        assert_eq!(scene.texts.len(), 1);
        let image = &scene.images[0];
        assert_eq!(image.id, 7);
        assert_eq!(image.rect, Rect::new(4, 5, 20, 10));
        assert_eq!(image.generation, 3);
        assert_eq!(image.uv, [0.0, 0.0, 1.0, 1.0]);
        assert!(
            Arc::ptr_eq(&image.pixels, &pixels),
            "the pixels are shared with the producer, never copied into the scene"
        );
    }

    #[test]
    fn linearizing_a_scene_leaves_image_pixels_untouched() {
        let pixels = Arc::new(ImagePixels::new(1, 1, vec![10, 20, 30, 255]).expect("one pixel"));
        let mut scene = Scene::with_clear(Rgb::new(0x80, 0x40, 0x20), 1.0);
        scene.push_image(ImageItem::new(1, Rect::new(0, 0, 1, 1), pixels, 1));
        scene.linearize();
        assert_eq!(
            scene.images[0].pixels.rgba,
            vec![10, 20, 30, 255],
            "the sRGB texture view converts while sampling"
        );
    }

    #[test]
    fn an_image_that_cannot_be_drawn_is_not_queued() {
        assert!(ImagePixels::new(0, 4, Vec::new()).is_none());
        assert!(
            ImagePixels::new(2, 2, vec![0; 4]).is_none(),
            "the buffer must match width * height * 4"
        );
        let pixels = pixels();
        let mut scene = Scene::default();
        scene.push_image(ImageItem::new(
            1,
            Rect::new(0, 0, 0, 10),
            Arc::clone(&pixels),
            1,
        ));
        assert!(
            scene.images.is_empty(),
            "an empty destination rectangle draws nothing"
        );
    }
}
