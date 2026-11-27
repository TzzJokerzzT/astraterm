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
pub use alacritty_terminal::vte::ansi::CursorShape;
use alacritty_terminal::vte::ansi::{
    Color as AnsiColor, CursorStyle, NamedColor, Processor,
};

use crate::image::{ImageMessage, ImageScanner};
use crate::snapshot::{
    CellFlags, CellSnapshot, CursorSnapshot, CursorVisual, KittyFlags, ModeFlags, RowSnapshot,
    Snapshot, SnapshotColor,
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
    /// The running program asked for a desktop notification (OSC 9 or
    /// `OSC 777;notify;`). Both fields are raw terminal input; the application
    /// sanitizes them before use.
    Notification { title: Option<String>, body: String },
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
    /// Default cursor shape, used until an application asks for another one.
    ///
    /// `DECSCUSR` (`CSI Ps SP q`) outranks this: the shape a program requests is
    /// the shape painted, and `CSI 0 SP q` restores this default. Every shape the
    /// emulator knows is carried, not just a block-or-hollow choice.
    pub cursor_shape: CursorShape,
    /// Blink the cursor by default, until an application overrides the style.
    pub cursor_blink: bool,
    /// Allow applications to read and write the system clipboard with OSC 52.
    ///
    /// Off by default: OSC 52 is untrusted terminal input and writing the user's
    /// clipboard from a remote session is a real risk.
    pub allow_osc52: bool,
    /// Advertise the Kitty keyboard protocol.
    ///
    /// On by default: applications probe with `CSI ? u` and fall back to the xterm
    /// encodings when nobody answers. Turning it off keeps the historical
    /// xterm-only behaviour for an application that misbehaves with the protocol.
    pub kitty_keyboard: bool,
}

impl Default for EngineOptions {
    fn default() -> Self {
        Self {
            cols: 80,
            rows: 24,
            scrollback: 10_000,
            cursor_shape: CursorShape::Block,
            cursor_blink: true,
            allow_osc52: false,
            kitty_keyboard: true,
        }
    }
}

/// VT state plus grid for one session.
pub struct TerminalEngine {
    term: Term<Listener>,
    parser: Processor,
    listener: Listener,
    /// Pulls kitty graphics, iTerm2 inline images and sixel out of the stream
    /// before the VT parser sees them; the core parses none of them.
    images: ImageScanner,
    /// Image sequences captured since the last [`Self::take_images`].
    image_messages: Vec<ImageMessage>,
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
            // The configured shape is only a default: `Term::cursor_style` falls
            // back to it while no program has set one, so what a program asks for
            // with `DECSCUSR` always wins.
            default_cursor_style: CursorStyle {
                shape: options.cursor_shape,
                blinking: options.cursor_blink,
            },
            // Applications query this with `CSI ? u`; answering is what makes them
            // speak the protocol, and a test guards the xterm fallback.
            kitty_keyboard: options.kitty_keyboard,
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
            images: ImageScanner::new(),
            image_messages: Vec::new(),
            base: Point::new(Line(0), Column(0)),
            cols,
            rows,
            options,
            dirty: true,
        };
        engine.base = engine.term.grid().cursor.point;
        engine
    }

    /// Feeds raw PTY output through the sequence scanner and then the VT parser.
    ///
    /// A chunk boundary is meaningless here: the scanner keeps partial image and
    /// notification sequences across calls, and for any stream without them the
    /// bytes reach the parser unchanged.
    pub fn process(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let output = self.images.scan(bytes);
        if !output.messages.is_empty() {
            self.image_messages.extend(output.messages);
        }
        if !output.passthrough.is_empty() {
            self.parser.advance(&mut self.term, &output.passthrough);
        }
        // Notifications go straight to the event sink: unlike images they carry no
        // payload for another subsystem, so `drain_events` is enough. Pushing them
        // after the parser keeps the events of one chunk in stream order.
        for notification in output.notifications {
            self.listener.push(TerminalEvent::Notification {
                title: notification.title,
                body: notification.body,
            });
        }
        self.dirty = true;
    }

    /// Drains the image sequences captured since the last call.
    ///
    /// Notifications do not come through here: they are pushed as
    /// [`TerminalEvent::Notification`] and drained by [`Self::drain_events`].
    ///
    /// Nothing is decoded or drawn yet (S5.1): the renderer slice turns these
    /// payloads into textures, and the anchoring model in [`crate::image`] places
    /// them on the grid.
    pub fn take_images(&mut self) -> Vec<ImageMessage> {
        std::mem::take(&mut self.image_messages)
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
            kitty: KittyFlags {
                disambiguate: mode.contains(TermMode::DISAMBIGUATE_ESC_CODES),
                report_event_types: mode.contains(TermMode::REPORT_EVENT_TYPES),
                report_alternate_keys: mode.contains(TermMode::REPORT_ALTERNATE_KEYS),
                report_all_keys_as_esc: mode.contains(TermMode::REPORT_ALL_KEYS_AS_ESC),
                report_associated_text: mode.contains(TermMode::REPORT_ASSOCIATED_TEXT),
            },
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
    // B6: the `OSC 8` destination is not part of the visible text, so it has to
    // travel beside it or a click can only guess from the characters. The model
    // clears its cursor template on an empty URI, so a closed span yields `None`.
    let hyperlink = cell
        .hyperlink()
        .map(|link| link.uri().to_owned().into_boxed_str())
        .filter(|uri| !uri.is_empty());
    CellSnapshot {
        ch: cell.c,
        fg: convert_color(cell.fg),
        bg: convert_color(cell.bg),
        flags: CellFlags {
            bold: flags.contains(Flags::BOLD),
            italic: flags.contains(Flags::ITALIC),
            // A linked cell the program did not style is underlined anyway: the
            // painter already honours this flag, so the link is visible without
            // a renderer change. A proper link style belongs in the renderer.
            underline: flags.intersects(Flags::ALL_UNDERLINES) || hyperlink.is_some(),
            dim: flags.contains(Flags::DIM),
            inverse: flags.contains(Flags::INVERSE),
            hidden: flags.contains(Flags::HIDDEN),
            strikethrough: flags.contains(Flags::STRIKEOUT),
        },
        wide: flags.contains(Flags::WIDE_CHAR),
        wide_spacer: flags.contains(Flags::WIDE_CHAR_SPACER)
            || flags.contains(Flags::LEADING_WIDE_CHAR_SPACER),
        // S9: everything the model stored as zero-width data of this cell —
        // variation selectors, joiners, keycaps, tag characters and combining
        // marks — travels with the base character, so the shaper can see the
        // cluster whole.
        zerowidth: cell
            .zerowidth()
            .map(|codepoints| codepoints.to_vec().into_boxed_slice()),
        hyperlink,
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
mod kitty_keyboard_tests {
    use super::{EngineOptions, TerminalEngine, TerminalEvent};

    fn engine(kitty_keyboard: bool) -> TerminalEngine {
        TerminalEngine::new(EngineOptions {
            cols: 20,
            rows: 4,
            kitty_keyboard,
            ..Default::default()
        })
    }

    fn replies(engine: &mut TerminalEngine) -> Vec<Vec<u8>> {
        engine
            .drain_events()
            .into_iter()
            .filter_map(|event| match event {
                TerminalEvent::PtyReply(bytes) => Some(bytes),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn keyboard_mode_query_is_answered_when_the_protocol_is_enabled() {
        let mut engine = engine(true);
        engine.process(b"\x1b[?u");
        assert_eq!(replies(&mut engine), vec![b"\x1b[?0u".to_vec()]);
    }

    #[test]
    fn keyboard_mode_query_is_silent_when_the_protocol_is_disabled() {
        let mut engine = engine(false);
        engine.process(b"\x1b[?u");
        assert!(
            replies(&mut engine).is_empty(),
            "a disabled protocol must not answer `CSI ? u`"
        );
    }

    #[test]
    fn push_set_and_pop_update_the_reported_flags() {
        let mut engine = engine(true);

        engine.process(b"\x1b[>2u");
        assert!(engine.modes().kitty.report_event_types);
        engine.process(b"\x1b[?u");
        assert_eq!(replies(&mut engine), vec![b"\x1b[?2u".to_vec()]);

        engine.process(b"\x1b[=3u");
        let kitty = engine.modes().kitty;
        assert!(kitty.disambiguate && kitty.report_event_types);
        assert!(!kitty.report_all_keys_as_esc);

        engine.process(b"\x1b[<u");
        assert!(
            !engine.modes().kitty.enabled(),
            "pop restores the empty stack"
        );
    }
}

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

#[cfg(test)]
mod notification_event_tests {
    use super::{EngineOptions, TerminalEngine, TerminalEvent};

    fn notifications(engine: &mut TerminalEngine) -> Vec<TerminalEvent> {
        engine
            .drain_events()
            .into_iter()
            .filter(|event| matches!(event, TerminalEvent::Notification { .. }))
            .collect()
    }

    #[test]
    fn osc_9_and_osc_777_reach_the_application_as_events() {
        let mut engine = TerminalEngine::new(EngineOptions {
            cols: 40,
            rows: 4,
            ..Default::default()
        });
        engine.process(b"\x1b]9;first notification\x07");
        engine.process(b"\x1b]777;notify;Build;done in 4s\x1b\\");

        assert_eq!(
            notifications(&mut engine),
            vec![
                TerminalEvent::Notification {
                    title: None,
                    body: "first notification".to_owned(),
                },
                TerminalEvent::Notification {
                    title: Some("Build".to_owned()),
                    body: "done in 4s".to_owned(),
                },
            ],
            "both notification protocols must surface as events"
        );
        assert!(
            notifications(&mut engine).is_empty(),
            "drain_events must take the notifications with it"
        );
    }

    #[test]
    fn a_foreign_osc_raises_no_notification_event() {
        let mut engine = TerminalEngine::new(EngineOptions {
            cols: 40,
            rows: 4,
            ..Default::default()
        });
        engine.process(b"\x1b]777;other;payload\x07\x1b]0;title\x07");
        assert!(notifications(&mut engine).is_empty());
        let snapshot = engine.snapshot();
        assert_eq!(
            snapshot.lines[0].text(),
            "",
            "the foreign OSC must not print anything either"
        );
    }
}

#[cfg(test)]
mod zerowidth_snapshot_tests {
    use super::{EngineOptions, TerminalEngine};

    /// The cell at `column` of the first row, as the renderer receives it.
    fn cell_text(engine: &mut TerminalEngine, column: usize) -> (char, Vec<char>) {
        let snapshot = engine.snapshot();
        let cell = &snapshot.lines[0].cells[column];
        (cell.ch, cell.zerowidth().to_vec())
    }

    #[test]
    fn zero_width_codepoints_reach_the_snapshot() {
        // S9: `convert_cell` used to drop `Cell::zerowidth()`, so the renderer
        // shaped the base character alone: `❤️` arrived as `❤`, `1️⃣` as `1`.
        let cases: [(&str, char, &[char]); 4] = [
            // Emoji presentation selector.
            ("\u{2764}\u{FE0F}", '\u{2764}', &['\u{FE0F}']),
            // Keycap: the presentation selector and the enclosing keycap.
            ("\u{0031}\u{FE0F}\u{20E3}", '1', &['\u{FE0F}', '\u{20E3}']),
            // Text presentation selector.
            ("\u{2764}\u{FE0E}", '\u{2764}', &['\u{FE0E}']),
            // Combining acute accent.
            ("e\u{0301}", 'e', &['\u{0301}']),
        ];
        for (input, base, zerowidth) in cases {
            let mut engine = TerminalEngine::new(EngineOptions {
                cols: 10,
                rows: 2,
                ..Default::default()
            });
            engine.process(input.as_bytes());
            assert_eq!(
                cell_text(&mut engine, 0),
                (base, zerowidth.to_vec()),
                "the zero-width data of {input:?} must survive into the snapshot"
            );
        }
    }

    #[test]
    fn a_zwj_sequence_reaches_the_snapshot_whole() {
        // S9: the model stores a ZWJ sequence across the wide cells of its members
        // and keeps each joiner as zero-width data of the cell before it, so the
        // family emoji arrives as three wide cells joined by the data between them.
        let mut engine = TerminalEngine::new(EngineOptions {
            cols: 10,
            rows: 2,
            ..Default::default()
        });
        engine.process("\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}".as_bytes());
        assert_eq!(cell_text(&mut engine, 0), ('\u{1F468}', vec!['\u{200D}']));
        assert_eq!(cell_text(&mut engine, 2), ('\u{1F469}', vec!['\u{200D}']));
        assert_eq!(cell_text(&mut engine, 4), ('\u{1F467}', Vec::new()));
    }
}

#[cfg(test)]
mod hyperlink_snapshot_tests {
    use super::{EngineOptions, TerminalEngine};

    /// A one-line engine wide enough for the whole link and the text after it.
    fn engine() -> TerminalEngine {
        TerminalEngine::new(EngineOptions {
            cols: 40,
            rows: 2,
            ..Default::default()
        })
    }

    /// The hyperlink the snapshot reports for `column` of the first row.
    fn hyperlink_at(engine: &mut TerminalEngine, column: u16) -> Option<String> {
        let snapshot = engine.snapshot();
        snapshot
            .cell(column, 0)
            .and_then(|cell| cell.hyperlink().map(str::to_owned))
    }

    #[test]
    fn an_osc8_hyperlink_reaches_the_snapshot() {
        // B6: `convert_cell` dropped `Cell::hyperlink()`, so the destination the
        // program attached to the text never reached the UI and a click could
        // only guess from the visible characters.
        let mut engine = engine();
        engine.process(b"\x1b]8;;https://example.com/target\x1b\\click me");
        for column in 0..8 {
            assert_eq!(
                hyperlink_at(&mut engine, column).as_deref(),
                Some("https://example.com/target"),
                "column {column} belongs to the link span"
            );
        }
        assert_eq!(
            hyperlink_at(&mut engine, 8),
            None,
            "the cell after the span carries no hyperlink"
        );
        let snapshot = engine.snapshot();
        assert_eq!(
            snapshot.lines[0].text(),
            "click me",
            "the destination is carried beside the text, never printed"
        );
    }

    #[test]
    fn a_cell_without_a_hyperlink_carries_none() {
        let mut engine = engine();
        engine.process(b"plain text");
        assert_eq!(hyperlink_at(&mut engine, 0), None);
    }

    #[test]
    fn an_empty_uri_closes_the_link() {
        // `OSC 8 ; ; ST` with an empty URI ends the span: the model clears the
        // cursor template, so everything written afterwards is unlinked.
        let mut engine = engine();
        engine.process(
            b"\x1b]8;;https://example.com/target\x1b\\link\x1b]8;;\x1b\\plain",
        );
        assert_eq!(
            hyperlink_at(&mut engine, 0).as_deref(),
            Some("https://example.com/target"),
            "the span covers the text before the closing sequence"
        );
        assert_eq!(
            hyperlink_at(&mut engine, 4),
            None,
            "the text after the closing sequence is not linked"
        );
    }

    #[test]
    fn an_opened_hyperlink_with_no_visible_text_links_no_cell() {
        let mut engine = engine();
        engine.process(b"\x1b]8;;https://example.com/target\x1b\\");
        let snapshot = engine.snapshot();
        assert!(
            snapshot
                .lines
                .iter()
                .flat_map(|row| &row.cells)
                .all(|cell| cell.hyperlink().is_none()),
            "a sequence that prints nothing links nothing"
        );
    }

    #[test]
    fn a_linked_cell_is_underlined_even_when_the_program_asked_for_no_style() {
        // The underline is the only indication that needs no renderer change:
        // the painter already honours `CellFlags::underline`.
        let mut engine = engine();
        engine.process(b"\x1b]8;;https://example.com/target\x1b\\link");
        let snapshot = engine.snapshot();
        assert!(
            snapshot.lines[0].cells[0].flags.underline,
            "a link the program did not style is still marked"
        );
    }
}

#[cfg(test)]
mod cursor_precedence_tests {
    use super::{CursorShape, EngineOptions, TerminalEngine};
    use crate::snapshot::CursorVisual;
    use alacritty_terminal::vte::ansi::CursorStyle;

    fn engine(cursor_shape: CursorShape, cursor_blink: bool) -> TerminalEngine {
        TerminalEngine::new(EngineOptions {
            cols: 8,
            rows: 2,
            cursor_shape,
            cursor_blink,
            ..Default::default()
        })
    }

    /// Shape the renderer would paint, as the snapshot reports it.
    fn painted_shape(engine: &mut TerminalEngine) -> Option<CursorVisual> {
        engine.snapshot().cursor.map(|cursor| cursor.shape)
    }

    #[test]
    fn the_configured_shape_is_the_default_for_every_engine_shape() {
        // S2/S5: with nothing written yet, the configured shape is the default, and
        // every shape the engine knows survives the mapping.
        for (configured, expected) in [
            (CursorShape::Block, CursorVisual::Block),
            (CursorShape::Beam, CursorVisual::Beam),
            (CursorShape::Underline, CursorVisual::Underline),
            (CursorShape::HollowBlock, CursorVisual::HollowBlock),
            (CursorShape::Hidden, CursorVisual::Hidden),
        ] {
            let mut engine = engine(configured, true);
            assert_eq!(
                painted_shape(&mut engine),
                Some(expected),
                "configured {configured:?} with no DECSCUSR"
            );
        }
    }

    #[test]
    fn the_program_shape_wins_and_the_default_comes_back() {
        // S1: `DECSCUSR` is recorded and outranks the configured default in both
        // directions; `CSI 0 SP q` drops the program's choice and restores it.
        let mut engine = engine(CursorShape::HollowBlock, true);

        engine.process(b"\x1b[5 q");
        assert_eq!(painted_shape(&mut engine), Some(CursorVisual::Beam));

        engine.process(b"\x1b[2 q");
        assert_eq!(painted_shape(&mut engine), Some(CursorVisual::Block));

        engine.process(b"\x1b[0 q");
        assert_eq!(
            painted_shape(&mut engine),
            Some(CursorVisual::HollowBlock),
            "DECSCUSR 0 asks for the default"
        );
    }

    #[test]
    fn hiding_the_cursor_suppresses_it_and_showing_restores_its_shape() {
        // S3: `DECTCEM` still suppresses the cursor, and showing it again paints the
        // shape the program asked for.
        let mut engine = engine(CursorShape::Underline, true);
        engine.process(b"\x1b[5 q");

        engine.process(b"\x1b[?25l");
        assert!(engine.snapshot().cursor.is_none());

        engine.process(b"\x1b[?25h");
        assert_eq!(painted_shape(&mut engine), Some(CursorVisual::Beam));
    }

    #[test]
    fn the_configured_blink_flag_seeds_the_default_style() {
        // The configured blink flag is the engine's default until a program sets its
        // own style.
        assert_eq!(
            engine(CursorShape::Beam, false).term.cursor_style(),
            CursorStyle {
                shape: CursorShape::Beam,
                blinking: false,
            }
        );
        assert_eq!(
            engine(CursorShape::Beam, true).term.cursor_style(),
            CursorStyle {
                shape: CursorShape::Beam,
                blinking: true,
            }
        );
    }

    #[test]
    fn a_program_setting_its_own_style_replaces_the_default_blink_flag() {
        // A program that asks for a steady style outranks the configured flag.
        let mut engine = engine(CursorShape::Beam, true);
        engine.process(b"\x1b[2 q");
        assert_eq!(
            engine.term.cursor_style(),
            CursorStyle {
                shape: CursorShape::Block,
                blinking: false,
            }
        );
        engine.process(b"\x1b[0 q");
        assert_eq!(
            engine.term.cursor_style(),
            CursorStyle {
                shape: CursorShape::Beam,
                blinking: true,
            }
        );
    }
}
