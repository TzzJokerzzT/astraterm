//! Terminal emulation adapter over `alacritty_terminal`.
//!
//! The rest of AstraTerm never touches upstream types: everything goes through
//! this facade. That keeps a future replacement of the emulation core possible and
//! keeps the renderer independent from the VT implementation.
//!
//! The parser is a real VT/xterm implementation — this crate never fakes terminal
//! behaviour with a text widget.

pub mod engine;
pub mod image;
pub mod image_decode;
pub mod input;
pub mod search;
pub mod shell;
pub mod snapshot;

pub use alacritty_terminal::index::Side;
pub use engine::{
    CursorShape, EngineOptions, MAX_SEMANTIC_MARKS, SelectionKind, TerminalEngine, TerminalEvent,
};
pub use image::{
    ImageMessage, ImagePlacement, ImageProtocol, ImageScanner, ImageStore, MAX_IMAGE_BYTES,
    MAX_PENDING_BYTES, ScanOutput, TerminalNotification,
};
pub use input::{
    Direction, KeyPress, MouseEvent, MouseKind, TermKey, encode_key, encode_mouse, encode_paste,
};
pub use search::{SearchDirection, SearchHit, SearchState};
pub use shell::{ShellMark, ShellMarkScanner, ShellPiece};
pub use snapshot::{
    CellFlags, CellSnapshot, CursorSnapshot, CursorVisual, ModeFlags, RowSnapshot, SelectionSpan,
    SemanticMark, SemanticMarkKind, Snapshot, SnapshotColor,
};
