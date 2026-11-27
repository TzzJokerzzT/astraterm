//! Terminal compatibility tests.
//!
//! These drive the real VT parser with escape sequences and assert on the
//! resulting grid, modes and events. They are the executable part of the
//! compatibility matrix described in `docs/TERMINAL_COMPATIBILITY.md`.

use alacritty_terminal::index::Side;
use astraterm_terminal::engine::{EngineOptions, SelectionKind, TerminalEvent};
use astraterm_terminal::input::{KeyPress, TermKey};
use astraterm_terminal::snapshot::SnapshotColor;
use astraterm_terminal::{TerminalEngine, encode_key, encode_paste};

fn engine(cols: u16, rows: u16) -> TerminalEngine {
    TerminalEngine::new(EngineOptions {
        cols,
        rows,
        scrollback: 200,
        ..Default::default()
    })
}

fn feed(engine: &mut TerminalEngine, text: &str) {
    engine.process(text.as_bytes());
}

fn row_text(snapshot: &astraterm_terminal::snapshot::Snapshot, row: usize) -> String {
    snapshot.lines[row].text()
}

#[test]
fn plain_text_lands_in_the_grid() {
    let mut engine = engine(20, 3);
    feed(&mut engine, "hello");
    let snapshot = engine.snapshot();
    assert_eq!(row_text(&snapshot, 0), "hello");
    assert_eq!(snapshot.cursor.unwrap().column, 5);
    assert_eq!(snapshot.cursor.unwrap().row, 0);
}

#[test]
fn newlines_and_carriage_returns_move_the_cursor() {
    let mut engine = engine(20, 4);
    feed(&mut engine, "one\r\ntwo\r\n");
    let snapshot = engine.snapshot();
    assert_eq!(row_text(&snapshot, 0), "one");
    assert_eq!(row_text(&snapshot, 1), "two");
}

#[test]
fn ansi_16_colours_are_resolved_to_palette_slots() {
    let mut engine = engine(20, 2);
    feed(&mut engine, "\x1b[31mred\x1b[92mbright\x1b[0m");
    let snapshot = engine.snapshot();
    assert_eq!(snapshot.lines[0].cells[0].fg, SnapshotColor::Indexed(1));
    assert_eq!(snapshot.lines[0].cells[3].fg, SnapshotColor::Indexed(10));
    assert_eq!(snapshot.lines[0].cells[9].fg, SnapshotColor::Default);
}

#[test]
fn ansi_256_colours_are_preserved() {
    let mut engine = engine(20, 2);
    feed(&mut engine, "\x1b[38;5;208morange\x1b[0m");
    let snapshot = engine.snapshot();
    assert_eq!(snapshot.lines[0].cells[0].fg, SnapshotColor::Indexed(208));
}

#[test]
fn true_colour_is_preserved() {
    let mut engine = engine(20, 2);
    feed(&mut engine, "\x1b[38;2;139;156;255mtruecolor\x1b[0m");
    let snapshot = engine.snapshot();
    assert_eq!(
        snapshot.lines[0].cells[0].fg,
        SnapshotColor::Rgb(139, 156, 255)
    );
}

#[test]
fn background_and_attributes_are_reported() {
    let mut engine = engine(20, 2);
    feed(&mut engine, "\x1b[1;3;4;9;48;2;10;20;30mstyled\x1b[0m");
    let snapshot = engine.snapshot();
    let cell = &snapshot.lines[0].cells[0];
    assert!(cell.flags.bold);
    assert!(cell.flags.italic);
    assert!(cell.flags.underline);
    assert!(cell.flags.strikethrough);
    assert_eq!(cell.bg, SnapshotColor::Rgb(10, 20, 30));
}

#[test]
fn alternate_screen_preserves_the_primary_content() {
    let mut engine = engine(20, 3);
    feed(&mut engine, "primary");
    feed(&mut engine, "\x1b[?1049h");
    assert!(engine.modes().alt_screen);
    feed(&mut engine, "\x1b[H\x1b[2Jalternate");
    let snapshot = engine.snapshot();
    assert_eq!(row_text(&snapshot, 0), "alternate");

    feed(&mut engine, "\x1b[?1049l");
    assert!(!engine.modes().alt_screen);
    let snapshot = engine.snapshot();
    assert_eq!(row_text(&snapshot, 0), "primary");
}

#[test]
fn bracketed_paste_and_application_cursor_modes_are_tracked() {
    let mut engine = engine(20, 3);
    assert!(!engine.modes().bracketed_paste);
    feed(&mut engine, "\x1b[?2004h");
    assert!(engine.modes().bracketed_paste);
    feed(&mut engine, "\x1b[?2004l");
    assert!(!engine.modes().bracketed_paste);

    feed(&mut engine, "\x1b[?1h");
    assert!(engine.modes().app_cursor);
    let bytes = encode_key(
        KeyPress::new(TermKey::Arrow(astraterm_terminal::Direction::Up)),
        engine.modes(),
    );
    assert_eq!(bytes, b"\x1bOA");
}

#[test]
fn mouse_reporting_modes_are_tracked() {
    let mut engine = engine(20, 3);
    assert!(!engine.modes().mouse_reporting());
    feed(&mut engine, "\x1b[?1000h\x1b[?1006h");
    let modes = engine.modes();
    assert!(modes.mouse_click);
    assert!(modes.sgr_mouse);
    feed(&mut engine, "\x1b[?1002h");
    assert!(engine.modes().mouse_drag);
    feed(&mut engine, "\x1b[?1003h");
    assert!(engine.modes().mouse_motion);
}

#[test]
fn cursor_visibility_and_shape_follow_decscusr() {
    let mut engine = engine(20, 3);
    feed(&mut engine, "\x1b[?25l");
    assert!(!engine.modes().show_cursor);
    assert!(engine.snapshot().cursor.is_none());
    feed(&mut engine, "\x1b[?25h");
    assert_eq!(
        engine.snapshot().cursor.unwrap().shape,
        astraterm_terminal::CursorVisual::Block
    );

    feed(&mut engine, "\x1b[4 q");
    assert_eq!(
        engine.snapshot().cursor.unwrap().shape,
        astraterm_terminal::CursorVisual::Underline
    );
    feed(&mut engine, "\x1b[5 q");
    assert_eq!(
        engine.snapshot().cursor.unwrap().shape,
        astraterm_terminal::CursorVisual::Beam
    );
}

#[test]
fn osc_title_is_reported_as_an_event() {
    let mut engine = engine(20, 3);
    feed(&mut engine, "\x1b]0;my title\x07");
    let events = engine.drain_events();
    assert!(
        events
            .iter()
            .any(|event| *event == TerminalEvent::Title("my title".to_owned())),
        "{events:?}"
    );
    feed(&mut engine, "\x1b]2;other title\x1b\\");
    let events = engine.drain_events();
    assert!(
        events
            .iter()
            .any(|event| *event == TerminalEvent::Title("other title".to_owned())),
        "{events:?}"
    );
}

#[test]
fn bell_is_reported() {
    let mut engine = engine(20, 3);
    feed(&mut engine, "\x07");
    assert!(engine.drain_events().contains(&TerminalEvent::Bell));
}

#[test]
fn erase_and_scroll_operations_work() {
    let mut engine = engine(10, 3);
    feed(&mut engine, "aaa\r\nbbb\r\nccc");
    feed(&mut engine, "\x1b[2J");
    let snapshot = engine.snapshot();
    assert!(snapshot.lines.iter().all(|row| row.is_blank()));

    feed(&mut engine, "1\r\n2\r\n3\r\n4");
    let snapshot = engine.snapshot();
    assert_eq!(row_text(&snapshot, 0), "2");
    assert_eq!(row_text(&snapshot, 2), "4");
}

#[test]
fn scrollback_holds_older_lines_and_scrolling_shows_them() {
    let mut engine = engine(10, 3);
    for index in 0..10 {
        feed(&mut engine, &format!("line{index}\r\n"));
    }
    assert_eq!(engine.display_offset(), 0);
    // Positive deltas scroll into the scrollback (older output).
    engine.scroll_display(5);
    assert!(engine.display_offset() > 0);
    engine.scroll_display(-2);
    assert_eq!(engine.display_offset(), 3);
    let snapshot = engine.snapshot();
    assert!(snapshot.display_offset > 0);
    engine.scroll_to(true);
    assert_eq!(engine.display_offset(), 0);
}

#[test]
fn resize_reflows_wrapped_text() {
    let mut engine = engine(10, 6);
    feed(&mut engine, "0123456789abcdefghij");
    let before = engine.snapshot();
    assert_eq!(row_text(&before, 0), "0123456789");
    assert_eq!(row_text(&before, 1), "abcdefghij");

    engine.resize(5, 6);
    let after = engine.snapshot();
    assert_eq!(after.cols, 5);
    // The core reflows wrapped text and keeps the cursor anchored, so some rows can
    // move into the scrollback. Everything must still be reachable, in order.
    engine.scroll_to(false);
    let top = engine.snapshot();
    let joined: Vec<String> = top
        .lines
        .iter()
        .map(|row| row.text())
        .filter(|row| !row.is_empty())
        .collect();
    let position = joined
        .iter()
        .position(|row| row == "01234")
        .unwrap_or_else(|| panic!("reflowed rows: {joined:?}"));
    assert_eq!(joined[position + 1], "56789");
    assert_eq!(joined[position + 2], "abcde");
    assert_eq!(joined[position + 3], "fghij");
    engine.scroll_to(true);
}

#[test]
fn wide_characters_use_two_columns() {
    let mut engine = engine(10, 2);
    feed(&mut engine, "你好");
    let snapshot = engine.snapshot();
    assert_eq!(snapshot.lines[0].cells[0].ch, '你');
    assert!(snapshot.lines[0].cells[0].wide);
    assert!(snapshot.lines[0].cells[1].wide_spacer);
    assert_eq!(snapshot.lines[0].cells[2].ch, '好');
    assert_eq!(snapshot.cursor.unwrap().column, 4);
}

#[test]
fn combining_marks_stay_attached_to_their_base_cell() {
    let mut engine = engine(10, 2);
    feed(&mut engine, "e\u{0301}x");
    let snapshot = engine.snapshot();
    assert_eq!(snapshot.lines[0].cells[0].ch, 'e');
    assert_eq!(snapshot.lines[0].cells[1].ch, 'x');
}

#[test]
fn selection_produces_the_expected_text() {
    let mut engine = engine(20, 3);
    feed(&mut engine, "hello world\r\nsecond line");
    engine.update_selection(0, 0, SelectionKind::Simple, Side::Left);
    engine.update_selection(4, 0, SelectionKind::SimpleOrExisting, Side::Right);
    assert_eq!(engine.selection_text().as_deref(), Some("hello"));
    engine.clear_selection();
    assert!(engine.selection_text().is_none());
}

#[test]
fn line_selection_includes_whole_lines() {
    let mut engine = engine(20, 3);
    feed(&mut engine, "one\r\ntwo\r\nthree");
    engine.update_selection(1, 0, SelectionKind::Lines, Side::Left);
    engine.update_selection(1, 1, SelectionKind::SimpleOrExisting, Side::Right);
    let text = engine.selection_text().unwrap();
    assert!(text.contains("one"), "{text:?}");
    assert!(text.contains("two"), "{text:?}");
}

#[test]
fn a_block_selection_copies_the_rectangle_padded_to_its_width() {
    // S2: the rectangle is the cells between the two corners, one line per row.
    // A row shorter than the rectangle is padded, so the copy keeps its shape
    // instead of collapsing the columns.
    let mut engine = engine(20, 3);
    feed(&mut engine, "ab\r\ncdef\r\n");
    engine.update_selection(0, 0, SelectionKind::Block, Side::Left);
    engine.update_selection(3, 1, SelectionKind::SimpleOrExisting, Side::Right);
    assert_eq!(engine.selection_text().as_deref(), Some("ab  \ncdef"));
}

#[test]
fn a_block_selection_pads_a_blank_row_to_the_rectangle_width() {
    let mut engine = engine(20, 3);
    feed(&mut engine, "abcd\r\n\r\n");
    engine.update_selection(0, 0, SelectionKind::Block, Side::Left);
    engine.update_selection(3, 1, SelectionKind::SimpleOrExisting, Side::Right);
    assert_eq!(engine.selection_text().as_deref(), Some("abcd\n    "));
}

#[test]
fn a_block_selection_of_one_row_is_just_that_rectangle() {
    let mut engine = engine(20, 3);
    feed(&mut engine, "xy\r\nabcdef\r\n");
    engine.update_selection(1, 1, SelectionKind::Block, Side::Left);
    engine.update_selection(3, 1, SelectionKind::SimpleOrExisting, Side::Right);
    assert_eq!(engine.selection_text().as_deref(), Some("bcd"));
}

#[test]
fn a_linear_selection_keeps_its_trailing_blank_trimming() {
    // S2 preserves the linear kind exactly: trailing blanks of the selected text
    // are still trimmed, which is what a copy of one visual line is expected to do.
    let mut engine = engine(20, 3);
    feed(&mut engine, "hello   \r\nworld");
    engine.update_selection(0, 0, SelectionKind::Simple, Side::Left);
    engine.update_selection(7, 0, SelectionKind::SimpleOrExisting, Side::Right);
    assert_eq!(engine.selection_text().as_deref(), Some("hello"));
}

#[test]
fn damage_reports_rows_that_changed() {
    let mut engine = engine(10, 4);
    feed(&mut engine, "first\r\n");
    let snapshot = engine.snapshot();
    assert!(
        snapshot.damaged_rows.contains(&0),
        "{:?}",
        snapshot.damaged_rows
    );
    feed(&mut engine, "second\r\n");
    assert!(engine.is_dirty());
    let snapshot = engine.snapshot();
    assert!(
        snapshot.damaged_rows.contains(&1),
        "{:?}",
        snapshot.damaged_rows
    );
}

#[test]
fn snapshot_text_round_trips_visible_lines() {
    let mut engine = engine(12, 3);
    feed(&mut engine, "alpha\r\nbeta");
    let text = engine.snapshot().text();
    assert!(text.starts_with("alpha\nbeta\n"), "{text:?}");
}

#[test]
fn bracketed_paste_encoding_follows_application_mode() {
    let mut engine = engine(20, 3);
    let plain = encode_paste("pasted", engine.modes());
    assert_eq!(plain, b"pasted");
    feed(&mut engine, "\x1b[?2004h");
    let bracketed = encode_paste("pasted", engine.modes());
    assert_eq!(bracketed, b"\x1b[200~pasted\x1b[201~");
}

#[test]
fn line_text_exposes_buffer_lines_for_search() {
    let mut engine = engine(20, 3);
    feed(&mut engine, "searchable line\r\n");
    let (top, _bottom) = engine.line_range();
    assert_eq!(
        engine.line_text(alacritty_terminal::index::Line(top)),
        "searchable line"
    );
}

#[test]
fn osc52_is_disabled_by_default() {
    let mut engine = engine(20, 3);
    feed(&mut engine, "\x1b]52;c;aGVsbG8=\x07");
    let events = engine.drain_events();
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, TerminalEvent::ClipboardStore(_))),
        "OSC 52 must be ignored unless explicitly enabled: {events:?}"
    );
}

#[test]
fn malformed_escape_sequences_do_not_corrupt_the_grid() {
    let mut engine = engine(20, 3);
    feed(&mut engine, "\x1b[999999999999999999m");
    feed(&mut engine, "\x1b]");
    feed(&mut engine, "\x1b[");
    // The unterminated `CSI` swallows the next byte as its final byte (`s` is a
    // valid CSI final byte for "save cursor"), which is exactly what a real
    // terminal does; what matters is that the grid stays consistent.
    feed(&mut engine, "safe");
    let snapshot = engine.snapshot();
    assert_eq!(row_text(&snapshot, 0), "afe");
}
