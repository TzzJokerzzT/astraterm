//! Renderer-facing snapshot of the terminal grid.
//!
//! Snapshots are taken only when a session is dirty (after output arrived), never
//! once per frame, and they carry the set of damaged rows so the renderer can
//! rebuild just the text buffers that changed.

/// Cell colour, resolved by the renderer against the active theme.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnapshotColor {
    /// Use the theme foreground (or background, when used as a background).
    Default,
    /// One of the 16 ANSI palette slots.
    Indexed(u8),
    Rgb(u8, u8, u8),
}

/// Visual attributes of a cell.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CellFlags {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub dim: bool,
    pub inverse: bool,
    pub hidden: bool,
    pub strikethrough: bool,
}

/// One grid cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellSnapshot {
    pub ch: char,
    pub fg: SnapshotColor,
    pub bg: SnapshotColor,
    pub flags: CellFlags,
    /// First half of a double-width character.
    pub wide: bool,
    /// Second half of a double-width character: drawn as part of the previous cell.
    pub wide_spacer: bool,
}

impl Default for CellSnapshot {
    fn default() -> Self {
        Self {
            ch: ' ',
            fg: SnapshotColor::Default,
            bg: SnapshotColor::Default,
            flags: CellFlags::default(),
            wide: false,
            wide_spacer: false,
        }
    }
}

impl CellSnapshot {
    /// Plain text content, treating spacers as empty.
    pub fn text(self) -> char {
        if self.wide_spacer { ' ' } else { self.ch }
    }
}

/// A screen row.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RowSnapshot {
    /// Cells, one per column.
    pub cells: Vec<CellSnapshot>,
}

impl RowSnapshot {
    /// Row text with trailing blanks trimmed, used for search and for copying.
    pub fn text(&self) -> String {
        let mut text: String = self.cells.iter().map(|cell| cell.text()).collect();
        while text.ends_with(' ') {
            text.pop();
        }
        text
    }

    /// Whether the row contains anything other than blank default cells.
    pub fn is_blank(&self) -> bool {
        self.cells.iter().all(|cell| {
            cell.ch == ' '
                && cell.bg == SnapshotColor::Default
                && cell.flags == CellFlags::default()
        })
    }
}

/// Cursor shape as reported by the emulator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorVisual {
    Block,
    Beam,
    Underline,
    HollowBlock,
    Hidden,
}

/// Cursor position and shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CursorSnapshot {
    /// Column inside the grid.
    pub column: u16,
    /// Row inside the visible screen.
    pub row: u16,
    pub shape: CursorVisual,
}

/// A selection in buffer coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SelectionSpan {
    /// `(line, column)` of the first cell.
    pub start: (i32, usize),
    /// `(line, column)` of the last cell.
    pub end: (i32, usize),
    /// Rectangular selection.
    pub is_block: bool,
}

/// Terminal modes the UI and input encoder care about.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ModeFlags {
    pub app_cursor: bool,
    pub app_keypad: bool,
    pub bracketed_paste: bool,
    pub mouse_click: bool,
    pub mouse_drag: bool,
    pub mouse_motion: bool,
    pub sgr_mouse: bool,
    pub utf8_mouse: bool,
    pub alt_screen: bool,
    pub show_cursor: bool,
    pub focus_reporting: bool,
    pub insert: bool,
}

impl ModeFlags {
    /// Whether any mouse reporting mode is enabled.
    pub fn mouse_reporting(&self) -> bool {
        self.mouse_click || self.mouse_drag || self.mouse_motion
    }
}

/// A complete view of the visible screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub cols: u16,
    pub rows: u16,
    /// Rows scrolled back from the bottom of the scrollback.
    pub display_offset: usize,
    pub lines: Vec<RowSnapshot>,
    pub cursor: Option<CursorSnapshot>,
    pub modes: ModeFlags,
    /// Rows whose content changed since the previous snapshot.
    pub damaged_rows: Vec<usize>,
    /// Current selection, in buffer coordinates.
    pub selection: Option<SelectionSpan>,
}

impl Snapshot {
    /// Text of the whole visible screen, one line per row.
    pub fn text(&self) -> String {
        let mut out = String::new();
        for row in &self.lines {
            out.push_str(&row.text());
            out.push('\n');
        }
        out
    }

    /// Resolves a cell, if the coordinates are inside the grid.
    pub fn cell(&self, column: u16, row: u16) -> Option<&CellSnapshot> {
        self.lines.get(row as usize)?.cells.get(column as usize)
    }
}
