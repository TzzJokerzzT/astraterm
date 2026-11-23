//! `TerminalEngine`: the only place that talks to `alacritty_terminal`.
//!
//! The engine owns the VT parser, the grid, the scrollback, the modes and the
//! selection. It is intentionally synchronous and single-threaded: bytes come in
//! from the session layer, and a [`Snapshot`] is produced only when the UI asks
//! for one after being notified that the session is dirty.

use std::sync::{Arc, Mutex};

use alacritty_terminal::event::{Event as AlacrittyEvent, EventListener, WindowSize};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::term::{Config as TermConfig, Term, TermMode};
use alacritty_terminal::vte::ansi::{
    Color as AnsiColor, CursorShape, CursorStyle, NamedColor, Processor,
};

use crate::snapshot::{
    CellFlags, CellSnapshot, CursorSnapshot, CursorVisual, ModeFlags, RowSnapshot, Snapshot,
    SnapshotColor,
};

/// Number of distinct colours the terminal core can report.
const COLOR_COUNT: usize = 269;

/// Grid dimensions handed to the emulation core.
///
/// `alacritty_terminal` only implements `Dimensions` for its own types (and for a
/// tuple behind `cfg(test)`), so the adapter provides its own.
#[derive(Clone, Copy, Debug)]
struct GridSize {
    cols: usize,
    rows: usize,
}

impl Dimensions for GridSize {
    fn total_lines(&self) -> usize {
        self.rows
    }

    fn screen_lines(&self) -> usize {
        self.rows
    }

    fn columns(&self) -> usize {
        self.cols
    }
}

/// Events the emulator raises towards the application.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminalEvent {
    /// OSC 0/2 set the window title.
    Title(String),
    /// The application reset the title.
    ResetTitle,
    /// The application asked to store text in the clipboard (OSC 52).
    ClipboardStore(String),
    /// The application asked to read the clipboard.
    ClipboardLoad,
    /// The bell rang.
    Bell,
    /// Escape sequence with a direct reply for the PTY, already encoded.
    PtyReply(Vec<u8>),
    /// OSC 4/10/11 colour query: the app should resolve and reply.
    ///
    /// The value is the queried index: 0-255 is an ANSI slot, 256 the foreground
    /// and 257 the background.
    ColorQuery(usize),
    /// The terminal wants the application to redraw sooner than usual.
    Wakeup,
    /// The child process asked the terminal to exit.
    Exit,
}

/// Shared event sink. Cloned into the `Term`.
#[derive(Clone, Default)]
struct Listener {
    events: Arc<Mutex<Vec<TerminalEvent>>>,
    replies: Arc<Mutex<Vec<u8>>>,
}

impl Listener {
    fn push(&self, event: TerminalEvent) {
        if let Ok(mut events) = self.events.lock() {
            events.push(event);
        }
    }

    fn reply(&self, bytes: String) {
        if let Ok(mut replies) = self.replies.lock() {
            replies.extend_from_slice(bytes.as_bytes());
        }
    }
}

impl EventListener for Listener {
    fn send_event(&self, event: AlacrittyEvent) {
        match event {
            AlacrittyEvent::Title(title) => self.push(TerminalEvent::Title(title)),
            AlacrittyEvent::ResetTitle => self.push(TerminalEvent::ResetTitle),
            AlacrittyEvent::ClipboardStore(_, text) => {
                self.push(TerminalEvent::ClipboardStore(text));
            }
            AlacrittyEvent::ClipboardLoad(_, _) => self.push(TerminalEvent::ClipboardLoad),
            AlacrittyEvent::Bell => self.push(TerminalEvent::Bell),
            AlacrittyEvent::PtyWrite(text) => {
                self.reply(text);
                let bytes = self.take_replies();
                self.push(TerminalEvent::PtyReply(bytes));
            }
            AlacrittyEvent::ColorRequest(index, formatter) => {
                // The formatting closure needs the actual colour, which the theme
                // owns, so the application resolves it and answers.
                self.push(TerminalEvent::ColorQuery(index.min(COLOR_COUNT - 1)));
                let _ = formatter;
            }
            AlacrittyEvent::TextAreaSizeRequest(_) => {}
            AlacrittyEvent::Wakeup => self.push(TerminalEvent::Wakeup),
            AlacrittyEvent::Exit => self.push(TerminalEvent::Exit),
            AlacrittyEvent::CursorBlinkingChange | AlacrittyEvent::MouseCursorDirty => {}
            AlacrittyEvent::ChildExit(_) => self.push(TerminalEvent::Exit),
        }
    }
}

impl Listener {
    fn take_replies(&self) -> Vec<u8> {
        self.replies
            .lock()
            .map(|mut replies| std::mem::take(&mut *replies))
            .unwrap_or_default()
    }
}

/// Options for a new engine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EngineOptions {
    pub cols: u16,
    pub rows: u16,
    pub scrollback: usize,
    /// Advertise a hollow block cursor instead of a filled block.
    pub hollow_cursor: bool,
    /// Allow applications to read and write the system clipboard with OSC 52.
    ///
    /// Off by default: OSC 52 is untrusted terminal input and writing the user's
    /// clipboard from a remote session is a real risk.
    pub allow_osc52: bool,
}

impl Default for EngineOptions {
    fn default() -> Self {
        Self {
            cols: 80,
            rows: 24,
            scrollback: 10_000,
            hollow_cursor: false,
            allow_osc52: false,
        }
    }
}

/// VT state plus grid for one session.
pub struct TerminalEngine {
    term: Term<Listener>,
    parser: Processor,
    listener: Listener,
    base: Point,
    cols: u16,
    rows: u16,
    options: EngineOptions,
    /// True when something changed since the last snapshot.
    dirty: bool,
}

impl TerminalEngine {
    pub fn new(options: EngineOptions) -> Self {
        let listener = Listener::default();
        let config = TermConfig {
            scrolling_history: options.scrollback,
            default_cursor_style: CursorStyle {
                shape: if options.hollow_cursor {
                    CursorShape::HollowBlock
                } else {
                    CursorShape::Block
                },
                blinking: true,
            },
            // Kitty keyboard protocol is not advertised: it changes what sequences
            // applications expect and is not implemented by this encoder yet.
            kitty_keyboard: false,
            osc52: if options.allow_osc52 {
                alacritty_terminal::term::Osc52::OnlyCopy
            } else {
                alacritty_terminal::term::Osc52::Disabled
            },
            ..TermConfig::default()
        };
        let cols = options.cols.max(1);
        let rows = options.rows.max(1);
        let size = GridSize {
            cols: cols as usize,
            rows: rows as usize,
        };
        let term = Term::new(config, &size, listener.clone());
        let mut engine = Self {
            term,
            parser: Processor::new(),
            listener,
            base: Point::new(Line(0), Column(0)),
            cols,
            rows,
            options,
            dirty: true,
        };
        engine.base = engine.term.grid().cursor.point;
        engine
    }

    /// Feeds raw PTY output through the VT parser.
    pub fn process(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        self.parser.advance(&mut self.term, bytes);
        self.dirty = true;
    }

    /// Resizes the grid, letting the core reflow the scrollback.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        let cols = cols.max(1);
        let rows = rows.max(1);
        if cols == self.cols && rows == self.rows {
            return;
        }
        self.term.resize(GridSize {
            cols: cols as usize,
            rows: rows as usize,
        });
        self.cols = cols;
        self.rows = rows;
        self.dirty = true;
    }

    pub fn size(&self) -> (u16, u16) {
        (self.cols, self.rows)
    }

    /// Scrolls the viewport by `delta` lines.
    ///
    /// Positive values move the viewport *into* the scrollback (older output),
    /// negative values move it back towards the live bottom. A mouse wheel up is a
    /// positive delta.
    pub fn scroll_display(&mut self, delta: i32) {
        if delta == 0 {
            return;
        }
        self.term.scroll_display(Scroll::Delta(delta));
        self.dirty = true;
    }

    /// Jumps to the top or bottom of the scrollback.
    pub fn scroll_to(&mut self, bottom: bool) {
        let scroll = if bottom { Scroll::Bottom } else { Scroll::Top };
        self.term.scroll_display(scroll);
        self.dirty = true;
    }

    pub fn display_offset(&self) -> usize {
        self.term.grid().display_offset()
    }

    /// Whether the application is currently on the alternate screen.
    pub fn is_alternate_screen(&self) -> bool {
        self.term.mode().contains(TermMode::ALT_SCREEN)
    }

    /// Current terminal modes, translated for the UI and the input encoder.
    pub fn modes(&self) -> ModeFlags {
        let mode = *self.term.mode();
        ModeFlags {
            app_cursor: mode.contains(TermMode::APP_CURSOR),
            app_keypad: mode.contains(TermMode::APP_KEYPAD),
            bracketed_paste: mode.contains(TermMode::BRACKETED_PASTE),
            mouse_click: mode.contains(TermMode::MOUSE_REPORT_CLICK),
            mouse_drag: mode.contains(TermMode::MOUSE_DRAG),
            mouse_motion: mode.contains(TermMode::MOUSE_MOTION),
            sgr_mouse: mode.contains(TermMode::SGR_MOUSE),
            utf8_mouse: mode.contains(TermMode::UTF8_MOUSE),
            alt_screen: mode.contains(TermMode::ALT_SCREEN),
            show_cursor: mode.contains(TermMode::SHOW_CURSOR),
            focus_reporting: mode.contains(TermMode::FOCUS_IN_OUT),
            insert: mode.contains(TermMode::INSERT),
        }
    }

    /// Drains emulator events (title changes, clipboard requests, bell, replies).
    pub fn drain_events(&mut self) -> Vec<TerminalEvent> {
        self.listener
            .events
            .lock()
            .map(|mut events| std::mem::take(&mut *events))
            .unwrap_or_default()
    }

    /// Builds a snapshot of the visible screen.
    ///
    /// Call this only when the session is dirty; it walks the visible grid.
    pub fn snapshot(&mut self) -> Snapshot {
        let content = self.term.renderable_content();
        let display_offset = content.display_offset;
        let modes = self.modes();
        let cursor_info = content.cursor;
        let selection = content.selection;

        let mut lines: Vec<RowSnapshot> = Vec::with_capacity(self.rows as usize);
        for _ in 0..self.rows {
            lines.push(RowSnapshot {
                cells: Vec::with_capacity(self.cols as usize),
            });
        }

        for indexed in content.display_iter {
            let point = indexed.point;
            let cell = indexed.cell;
            let row = point.line.0 + display_offset as i32;
            if row < 0 || row >= self.rows as i32 {
                continue;
            }
            let column = point.column.0;
            if column >= self.cols as usize {
                continue;
            }
            let row_entry = &mut lines[row as usize];
            while row_entry.cells.len() < column {
                row_entry.cells.push(CellSnapshot::default());
            }
            row_entry.cells.push(convert_cell(cell));
        }
        for row in &mut lines {
            row.cells
                .resize(self.cols as usize, CellSnapshot::default());
        }

        let damaged_rows = collect_damage(&mut self.term, self.rows as usize);

        let cursor = {
            let cursor_row = cursor_info.point.line.0 + display_offset as i32;
            if cursor_row >= 0 && cursor_row < self.rows as i32 && modes.show_cursor {
                Some(CursorSnapshot {
                    column: cursor_info.point.column.0.min(self.cols as usize - 1) as u16,
                    row: cursor_row as u16,
                    shape: match cursor_info.shape {
                        CursorShape::Block => CursorVisual::Block,
                        CursorShape::Beam => CursorVisual::Beam,
                        CursorShape::Underline => CursorVisual::Underline,
                        CursorShape::HollowBlock => CursorVisual::HollowBlock,
                        CursorShape::Hidden => CursorVisual::Hidden,
                    },
                })
            } else {
                None
            }
        };

        self.dirty = false;

        Snapshot {
            cols: self.cols,
            rows: self.rows,
            display_offset,
            lines,
            cursor,
            modes,
            damaged_rows,
            selection: selection.map(|range| crate::snapshot::SelectionSpan {
                start: (range.start.line.0, range.start.column.0),
                end: (range.end.line.0, range.end.column.0),
                is_block: range.is_block,
            }),
        }
    }

    /// Whether a snapshot is worth taking.
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Marks the session dirty (used after a selection change).
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    // --- Selection -----------------------------------------------------------

    /// Starts or extends a selection at a screen coordinate.
    pub fn update_selection(&mut self, column: u16, row: u16, kind: SelectionKind, side: Side) {
        let point = self.screen_to_buffer_point(column, row);
        match &mut self.term.selection {
            Some(selection) if kind == SelectionKind::SimpleOrExisting => {
                selection.update(point, side);
            }
            _ => {
                let selection_type = match kind {
                    SelectionKind::Simple | SelectionKind::SimpleOrExisting => {
                        SelectionType::Simple
                    }
                    SelectionKind::Semantic => SelectionType::Semantic,
                    SelectionKind::Lines => SelectionType::Lines,
                    SelectionKind::Block => SelectionType::Block,
                };
                self.term.selection = Some(Selection::new(selection_type, point, side));
            }
        }
        self.dirty = true;
    }

    /// Clears the selection.
    pub fn clear_selection(&mut self) {
        if self.term.selection.take().is_some() {
            self.dirty = true;
        }
    }

    pub fn has_selection(&self) -> bool {
        self.term.selection.is_some()
    }

    /// The selected text, if any.
    pub fn selection_text(&self) -> Option<String> {
        self.term.selection_to_string()
    }

    /// Converts a screen position into a buffer position, honouring scrollback.
    pub fn screen_to_buffer_point(&self, column: u16, row: u16) -> Point {
        let offset = self.term.grid().display_offset() as i32;
        Point::new(Line(row as i32 - offset), Column(column as usize))
    }

    // --- Text and answers ----------------------------------------------------

    /// Text of one buffer line, used by search and by "copy line".
    pub fn line_text(&self, line: Line) -> String {
        let grid = self.term.grid();
        let mut text = String::new();
        for column in 0..self.cols as usize {
            let cell = &grid[Point::new(line, Column(column))];
            if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                continue;
            }
            text.push(cell.c);
        }
        while text.ends_with(' ') {
            text.pop();
        }
        text
    }

    /// Range of buffer lines currently held, oldest first.
    pub fn line_range(&self) -> (i32, i32) {
        (self.term.topmost_line().0, self.term.bottommost_line().0)
    }

    /// Encodes the answer to a colour query (OSC 4/10/11) for the PTY.
    pub fn encode_color_reply(index: usize, rgb: (u8, u8, u8)) -> Vec<u8> {
        match index {
            256 => format!(
                "\x1b]10;rgb:{:02x}{:02x}/{:02x}{:02x}/{:02x}{:02x}\x07",
                rgb.0, rgb.0, rgb.1, rgb.1, rgb.2, rgb.2
            )
            .into_bytes(),
            257 => format!(
                "\x1b]11;rgb:{:02x}{:02x}/{:02x}{:02x}/{:02x}{:02x}\x07",
                rgb.0, rgb.0, rgb.1, rgb.1, rgb.2, rgb.2
            )
            .into_bytes(),
            index => format!(
                "\x1b]4;{index};rgb:{:02x}{:02x}/{:02x}{:02x}/{:02x}{:02x}\x07",
                rgb.0, rgb.0, rgb.1, rgb.1, rgb.2, rgb.2
            )
            .into_bytes(),
        }
    }

    /// Options this engine was created with.
    pub fn options(&self) -> &EngineOptions {
        &self.options
    }

    /// Column of the cursor in the buffer, used by the search API.
    pub fn cursor_point(&self) -> Point {
        self.term.grid().cursor.point
    }

    /// Base point captured at construction, kept for tests and diagnostics.
    pub fn origin(&self) -> Point {
        self.base
    }
}

/// Selection granularity requested by the UI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectionKind {
    /// Start a new simple selection.
    Simple,
    /// Extend the current simple selection, starting one if needed.
    SimpleOrExisting,
    /// Word-wise selection (double click).
    Semantic,
    /// Line-wise selection (triple click).
    Lines,
    /// Rectangular selection (Alt+drag).
    Block,
}

fn convert_cell(cell: &alacritty_terminal::term::cell::Cell) -> CellSnapshot {
    let flags = cell.flags;
    CellSnapshot {
        ch: cell.c,
        fg: convert_color(cell.fg),
        bg: convert_color(cell.bg),
        flags: CellFlags {
            bold: flags.contains(Flags::BOLD),
            italic: flags.contains(Flags::ITALIC),
            underline: flags.intersects(Flags::ALL_UNDERLINES),
            dim: flags.contains(Flags::DIM),
            inverse: flags.contains(Flags::INVERSE),
            hidden: flags.contains(Flags::HIDDEN),
            strikethrough: flags.contains(Flags::STRIKEOUT),
        },
        wide: flags.contains(Flags::WIDE_CHAR),
        wide_spacer: flags.contains(Flags::WIDE_CHAR_SPACER)
            || flags.contains(Flags::LEADING_WIDE_CHAR_SPACER),
    }
}

fn convert_color(color: AnsiColor) -> SnapshotColor {
    match color {
        AnsiColor::Named(named) => match named {
            NamedColor::Foreground | NamedColor::Background => SnapshotColor::Default,
            NamedColor::Black => SnapshotColor::Indexed(0),
            NamedColor::Red => SnapshotColor::Indexed(1),
            NamedColor::Green => SnapshotColor::Indexed(2),
            NamedColor::Yellow => SnapshotColor::Indexed(3),
            NamedColor::Blue => SnapshotColor::Indexed(4),
            NamedColor::Magenta => SnapshotColor::Indexed(5),
            NamedColor::Cyan => SnapshotColor::Indexed(6),
            NamedColor::White => SnapshotColor::Indexed(7),
            NamedColor::BrightBlack => SnapshotColor::Indexed(8),
            NamedColor::BrightRed => SnapshotColor::Indexed(9),
            NamedColor::BrightGreen => SnapshotColor::Indexed(10),
            NamedColor::BrightYellow => SnapshotColor::Indexed(11),
            NamedColor::BrightBlue => SnapshotColor::Indexed(12),
            NamedColor::BrightMagenta => SnapshotColor::Indexed(13),
            NamedColor::BrightCyan => SnapshotColor::Indexed(14),
            NamedColor::BrightWhite => SnapshotColor::Indexed(15),
            // Dim variants collapse onto their bright counterpart; the DIM flag
            // already carries the visual intent.
            NamedColor::DimBlack => SnapshotColor::Indexed(0),
            NamedColor::DimRed => SnapshotColor::Indexed(1),
            NamedColor::DimGreen => SnapshotColor::Indexed(2),
            NamedColor::DimYellow => SnapshotColor::Indexed(3),
            NamedColor::DimBlue => SnapshotColor::Indexed(4),
            NamedColor::DimMagenta => SnapshotColor::Indexed(5),
            NamedColor::DimCyan => SnapshotColor::Indexed(6),
            NamedColor::DimWhite => SnapshotColor::Indexed(7),
            NamedColor::Cursor | NamedColor::BrightForeground | NamedColor::DimForeground => {
                SnapshotColor::Default
            }
        },
        AnsiColor::Indexed(index) => SnapshotColor::Indexed(index),
        AnsiColor::Spec(rgb) => SnapshotColor::Rgb(rgb.r, rgb.g, rgb.b),
    }
}

fn collect_damage(term: &mut Term<Listener>, rows: usize) -> Vec<usize> {
    use alacritty_terminal::term::TermDamage;

    let mut damaged: Vec<usize> = match term.damage() {
        // A full-damage report (clear, resize, reset) invalidates every row.
        TermDamage::Full => (0..rows).collect(),
        TermDamage::Partial(lines) => lines
            .filter_map(|bounds| (bounds.line < rows).then_some(bounds.line))
            .collect(),
    };
    if !damaged.is_empty() {
        damaged.sort_unstable();
        damaged.dedup();
        term.reset_damage();
    }
    damaged
}

/// Resolves a named colour to its palette slot, used when answering colour
/// queries from the theme.
pub fn named_color_slot(named: NamedColor) -> Option<u8> {
    match named {
        NamedColor::Foreground | NamedColor::Background => None,
        NamedColor::Cursor | NamedColor::BrightForeground | NamedColor::DimForeground => None,
        NamedColor::Black | NamedColor::DimBlack => Some(0),
        NamedColor::Red | NamedColor::DimRed => Some(1),
        NamedColor::Green | NamedColor::DimGreen => Some(2),
        NamedColor::Yellow | NamedColor::DimYellow => Some(3),
        NamedColor::Blue | NamedColor::DimBlue => Some(4),
        NamedColor::Magenta | NamedColor::DimMagenta => Some(5),
        NamedColor::Cyan | NamedColor::DimCyan => Some(6),
        NamedColor::White | NamedColor::DimWhite => Some(7),
        NamedColor::BrightBlack => Some(8),
        NamedColor::BrightRed => Some(9),
        NamedColor::BrightGreen => Some(10),
        NamedColor::BrightYellow => Some(11),
        NamedColor::BrightBlue => Some(12),
        NamedColor::BrightMagenta => Some(13),
        NamedColor::BrightCyan => Some(14),
        NamedColor::BrightWhite => Some(15),
    }
}

/// `Colors` type re-export guard: keeps the import meaningful for future
/// theme-aware colour resolution.
pub type TermColors = Colors;

/// `WindowSize` is re-exported so the session layer can answer size queries.
pub type TermWindowSize = WindowSize;

#[cfg(test)]
mod color_reply_tests {
    use super::TerminalEngine;

    #[test]
    fn palette_replies_use_osc_4() {
        let bytes = TerminalEngine::encode_color_reply(4, (0x8b, 0x9c, 0xff));
        assert_eq!(
            String::from_utf8(bytes).unwrap(),
            "\x1b]4;4;rgb:8b8b/9c9c/ffff\x07"
        );
    }

    #[test]
    fn foreground_and_background_use_osc_10_and_11() {
        let foreground =
            String::from_utf8(TerminalEngine::encode_color_reply(256, (0xd4, 0xd8, 0xe0))).unwrap();
        assert!(
            foreground.starts_with("\x1b]10;rgb:d4d4/d8d8/e0e0"),
            "{foreground:?}"
        );
        let background =
            String::from_utf8(TerminalEngine::encode_color_reply(257, (0x1c, 0x1f, 0x26))).unwrap();
        assert!(
            background.starts_with("\x1b]11;rgb:1c1c/1f1f/2626"),
            "{background:?}"
        );
    }
}
