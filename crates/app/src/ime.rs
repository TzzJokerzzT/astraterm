//! Where the platform puts the input method's candidate window.
//!
//! The window layer tells the platform which rectangle the candidate list should
//! follow ([`winit::window::Window::set_ime_cursor_area`]). That rectangle is the
//! terminal's cursor cell in physical pixels, so it is derived from the same geometry
//! the renderer paints the cursor with — the pane's rectangle without its focus
//! border, the terminal padding and the cell metrics — never from an origin of its
//! own.
//!
//! Everything here is a pure decision on purpose: the winit call cannot be exercised
//! without a window and a compositor, so what the platform is told is decided by
//! functions that take numbers and return a rectangle.

use astraterm_core::{CellMetrics, Padding, Rect};
use winit::event::Ime;

/// One cell of a pane's grid: the cursor's column and row, zero-based.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CursorCell {
    pub column: u16,
    pub row: u16,
}

/// Physical rectangle of the cursor cell inside `pane`.
///
/// `pane` is the pane rectangle the renderer paints
/// ([`crate::ui::WindowUi::pane_rect`]) **without** its one-pixel focus border, which
/// `paint::paint_pane` shrinks away before it places a single cell; `padding` is the
/// terminal padding in device pixels. `cursor` is the cell the renderer would draw the
/// cursor in, or `None` when the emulation shows no cursor.
///
/// The area is one painted cell, never the glyph: its origin is the pane corner plus
/// the padding plus `(column * cell width, row * cell height)` and its size is the cell
/// advance rounded up, which is exactly the arithmetic `paint_pane` uses for the
/// cursor quad.
///
/// It is then clamped into the grid area of the pane. That only bites when the cursor
/// cell is outside what the painted rectangle holds — the frames between a window
/// resize and the session's own resize, or a cursor the emulation left past the last
/// column — and it keeps a candidate window from hanging over the pane's border, a
/// neighbouring pane or the window frame.
///
/// `None` when there is no cursor to follow; the caller then sends
/// [`no_cursor_area`].
pub fn cursor_area(
    pane: Rect,
    padding: Padding,
    metrics: CellMetrics,
    cursor: Option<CursorCell>,
) -> Option<Rect> {
    let cursor = cursor?;
    let cell_width = metrics.width.max(1.0);
    let cell_height = metrics.height.max(1);
    let grid = grid_area(pane, padding);
    let origin_x = grid.x;
    let origin_y = grid.y;
    let area = Rect::new(
        origin_x + (cursor.column as f32 * cell_width) as i32,
        origin_y + cursor.row as i32 * cell_height as i32,
        cell_width.ceil() as u32,
        cell_height,
    );
    Some(clamp_into(area, grid))
}

/// The area to send while no cursor is on screen.
///
/// A hidden cursor has no cell to anchor to and winit has no "clear the area" call, so
/// the platform is handed a zero-size rectangle: that is how an input method is told
/// there is no caret to follow, and it is what makes the candidate window fall back to
/// the platform's own placement instead of to a stale cell from the last composition.
/// Its origin is the pane's inner corner, so the fallback stays inside the terminal
/// rather than at the window's top-left over the tab strip.
pub fn no_cursor_area(pane: Rect, padding: Padding) -> Rect {
    let grid = grid_area(pane, padding);
    Rect::new(grid.x, grid.y, 0, 0)
}

/// Cursor cell the renderer draws, from the engine's cheap accessors.
///
/// [`astraterm_terminal::TerminalEngine::snapshot`] is the authority on where the
/// cursor is, but it walks and allocates the whole visible grid, and the IME area is
/// recomputed on every batch of output and on every keystroke. This is the snapshot's
/// cursor rule in one place: the cell exists only while the emulation shows the cursor
/// and its row is on the visible screen, the row follows the display offset, and the
/// column is clamped to the grid. `the_cursor_cell_agrees_with_the_engine_snapshot`
/// pins the two together.
///
/// It is not the renderer's cell in one transient position: when the cursor sits on the
/// trailing half of a wide glyph, the snapshot moves it one cell left to the glyph's
/// own cell and this does not, because the cell flags are only reachable through a full
/// snapshot. The candidate window is then one cell to the right of the painted cursor
/// until the cursor moves off that cell; a cheap accessor in the terminal crate
/// (`TerminalEngine::cursor_cell`) would close it.
pub fn cursor_cell(
    line: i32,
    column: usize,
    display_offset: usize,
    cols: u16,
    rows: u16,
    show_cursor: bool,
) -> Option<CursorCell> {
    if !show_cursor || cols == 0 || rows == 0 {
        return None;
    }
    let row = line + display_offset as i32;
    if row < 0 || row >= rows as i32 {
        return None;
    }
    Some(CursorCell {
        column: column.min(cols as usize - 1) as u16,
        row: row as u16,
    })
}

/// The composition an input method is in the middle of (S4).
///
/// It lives beside the terminal's own text, never in it: the grid is drawn from the
/// snapshot and is not touched, so the composition can grow, shrink and disappear
/// without writing a single cell.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Preedit {
    /// Text being composed, exactly as the input method last reported it.
    pub text: String,
}

/// The composition to paint after one input-method event.
///
/// `current` is what the last frame painted. The input method reports the *whole*
/// composition every time, so a replacement is kept and a deleted character is
/// simply absent from the new text; an empty preedit, a commit and a disabled input
/// method all end the composition, which is what clears it from the screen.
/// `Enabled` starts a session without touching a composition in progress, because
/// some platforms send the current text again right after it.
pub fn apply_preedit(current: Option<Preedit>, event: &Ime) -> Option<Preedit> {
    match event {
        Ime::Preedit(text, _) if text.is_empty() => None,
        Ime::Preedit(text, _) => Some(Preedit { text: text.clone() }),
        // The text reached the PTY, so the composition it stood for is gone.
        Ime::Commit(_) | Ime::Disabled => None,
        Ime::Enabled => current,
    }
}

/// Rectangle the composition occupies, starting at the cursor cell.
///
/// It is the cursor cell widened by one cell per character, clamped to the grid the
/// pane shows, so a long composition can never paint over a neighbouring pane or a
/// border. `None` when there is nothing to paint or no cursor to anchor it to.
pub fn preedit_area(
    pane: Rect,
    padding: Padding,
    metrics: CellMetrics,
    cursor: CursorCell,
    text: &str,
) -> Option<Rect> {
    let cells = text.chars().count() as f32;
    if cells < 1.0 {
        return None;
    }
    let cell = cursor_area(pane, padding, metrics, Some(cursor))?;
    let grid = grid_area(pane, padding);
    let width = (cells * metrics.width.max(1.0)).ceil() as u32;
    let available = grid.right().saturating_sub(cell.x).max(0) as u32;
    Some(Rect::new(
        cell.x,
        cell.y,
        width.min(available).max(1),
        cell.height,
    ))
}

/// Whether `area` has to be pushed to the platform.
///
/// Sending the same rectangle again on every keystroke and every frame of output is a
/// protocol message the platform cannot use, so only a change goes out. A zero-size
/// (hidden-cursor) area is a change like any other, so a cursor that appears or
/// disappears is always reported.
pub fn needs_update(last: Option<Rect>, area: Rect) -> bool {
    last != Some(area)
}

/// Rectangle the grid occupies inside `pane`: the pane minus the padding.
fn grid_area(pane: Rect, padding: Padding) -> Rect {
    Rect::new(
        pane.x + padding.x as i32,
        pane.y + padding.y as i32,
        pane.width.saturating_sub(2 * padding.x),
        pane.height.saturating_sub(2 * padding.y),
    )
}

/// Moves `area` inside `bounds`, shrinking it when it does not fit at all.
fn clamp_into(area: Rect, bounds: Rect) -> Rect {
    let width = area.width.min(bounds.width);
    let height = area.height.min(bounds.height);
    Rect::new(
        area.x.clamp(bounds.x, bounds.right() - width as i32),
        area.y.clamp(bounds.y, bounds.bottom() - height as i32),
        width,
        height,
    )
}

#[cfg(test)]
mod tests {
    use astraterm_core::{CellMetrics, Padding, Rect};

    use super::{
        CursorCell, Preedit, apply_preedit, cursor_area, cursor_cell, needs_update, no_cursor_area,
        preedit_area,
    };
    use winit::event::Ime;

    /// An 8x16 cell, the shape a default font produces.
    const CELL: CellMetrics = CellMetrics {
        width: 8.0,
        height: 16,
        baseline: 12.0,
    };

    #[test]
    fn a_mosaic_pane_anchors_the_area_to_the_cursor_cell() {
        let pane = Rect::new(0, 0, 800, 600);
        let area = cursor_area(
            pane,
            Padding::default(),
            CELL,
            Some(CursorCell { column: 5, row: 3 }),
        )
        .expect("the cursor is on screen");
        assert_eq!(area, Rect::new(40, 48, 8, 16));
    }

    #[test]
    fn the_floating_pane_uses_its_own_rectangle() {
        // The float's pane rectangle with its one-pixel border already shrunk away,
        // somewhere inside the window: the area follows the float, not the mosaic
        // behind it.
        let pane = Rect::new(141, 101, 398, 298);
        let area = cursor_area(
            pane,
            Padding::default(),
            CELL,
            Some(CursorCell { column: 2, row: 1 }),
        )
        .expect("the cursor is on screen");
        assert_eq!(area, Rect::new(141 + 16, 101 + 16, 8, 16));
    }

    #[test]
    fn nonzero_padding_insets_the_origin() {
        let pane = Rect::new(10, 20, 400, 300);
        let padding = Padding { x: 12, y: 10 };
        let area = cursor_area(pane, padding, CELL, Some(CursorCell { column: 4, row: 2 }))
            .expect("the cursor is on screen");
        assert_eq!(area, Rect::new(10 + 12 + 32, 20 + 10 + 32, 8, 16));
    }

    #[test]
    fn a_fractional_cell_width_tracks_the_advance_and_rounds_the_cell_up() {
        // An advance of 8.6 px cannot land on the pixel grid: the renderer truncates
        // the accumulated origin and paints the cell one whole pixel wider, and the
        // area has to match it or the candidate window drifts by a pixel per column.
        let cell = CellMetrics {
            width: 8.6,
            height: 18,
            baseline: 14.0,
        };
        let pane = Rect::new(0, 0, 860, 360);
        let area = cursor_area(
            pane,
            Padding::default(),
            cell,
            Some(CursorCell { column: 3, row: 4 }),
        )
        .expect("the cursor is on screen");
        assert_eq!(area.x, 25);
        assert_eq!(area.y, 72);
        // The area is one painted cell, not the glyph: the width never depends on
        // the text under the cursor.
        assert_eq!(area.width as f32, cell.width.ceil());
        assert_eq!(area.height, cell.height);
    }

    #[test]
    fn the_last_cell_of_the_grid_keeps_its_origin() {
        // 800x600 holds 100 columns and 37 whole rows of an 8x16 cell, so the last
        // cell ends inside the pane and clamping leaves it exactly where the grid
        // puts it.
        let area = cursor_area(
            Rect::new(0, 0, 800, 600),
            Padding::default(),
            CELL,
            Some(CursorCell {
                column: 99,
                row: 36,
            }),
        )
        .expect("the cursor is on screen");
        assert_eq!(area, Rect::new(792, 576, 8, 16));
    }

    #[test]
    fn a_cursor_past_the_last_cell_is_clamped_inside_the_pane() {
        // 85 px fit ten columns of 8 px. A cursor on column 12 is the grid the
        // session still has from before a resize, so the cell would reach past the
        // pane's edge; the area is clamped inside the pane instead of hanging over
        // its border or into the neighbouring pane, and it keeps one cell's size.
        let pane = Rect::new(0, 0, 85, 100);
        let area = cursor_area(
            pane,
            Padding::default(),
            CELL,
            Some(CursorCell {
                column: 12,
                row: 40,
            }),
        )
        .expect("the cursor is on screen");
        assert_eq!(area, Rect::new(77, 84, 8, 16));
        assert_eq!(area.right(), pane.right());
        assert_eq!(area.bottom(), pane.bottom());
    }

    #[test]
    fn a_hidden_cursor_has_no_cell_and_lands_on_a_zero_size_area() {
        let pane = Rect::new(0, 0, 800, 600);
        assert_eq!(cursor_area(pane, Padding::default(), CELL, None), None);
        assert_eq!(
            no_cursor_area(pane, Padding { x: 12, y: 10 }),
            Rect::new(12, 10, 0, 0)
        );
        assert!(no_cursor_area(pane, Padding::default()).is_empty());
    }

    #[test]
    fn the_cursor_cell_is_clamped_and_gone_when_the_emulation_hides_it() {
        assert_eq!(
            cursor_cell(3, 7, 0, 80, 24, true),
            Some(CursorCell { column: 7, row: 3 })
        );
        // Scrolled back two lines: the cursor row moves down with the viewport.
        assert_eq!(
            cursor_cell(-20, 0, 20, 80, 24, true),
            Some(CursorCell { column: 0, row: 0 })
        );
        // Scrolled far enough that the cursor is off the visible screen.
        assert_eq!(cursor_cell(-30, 0, 20, 80, 24, true), None);
        // `DECTCEM` off: the program asked for no cursor at all.
        assert_eq!(cursor_cell(3, 7, 0, 80, 24, false), None);
        // A column the grid no longer has (a stale resize) is clamped to the last.
        assert_eq!(
            cursor_cell(0, 100, 0, 80, 24, true),
            Some(CursorCell { column: 79, row: 0 })
        );
    }

    /// The cursor cell the engine itself reports, so the cheap path cannot drift from
    /// the snapshot the renderer draws from.
    fn assert_matches_snapshot(engine: &mut astraterm_terminal::TerminalEngine) {
        let point = engine.cursor_point();
        let (cols, rows) = engine.size();
        let cell = cursor_cell(
            point.line.0,
            point.column.0,
            engine.display_offset(),
            cols,
            rows,
            engine.modes().show_cursor,
        );
        let expected = engine.snapshot().cursor.map(|cursor| CursorCell {
            column: cursor.column,
            row: cursor.row,
        });
        assert_eq!(cell, expected);
    }

    #[test]
    fn the_cursor_cell_agrees_with_the_engine_snapshot() {
        use astraterm_terminal::TerminalEngine;
        use astraterm_terminal::engine::EngineOptions;

        let mut engine = TerminalEngine::new(EngineOptions {
            cols: 20,
            rows: 5,
            scrollback: 10,
            ..Default::default()
        });
        engine.process(b"hola\r\nmundo");
        assert_matches_snapshot(&mut engine);
        engine.process(b"\x1b[5;10H");
        assert_matches_snapshot(&mut engine);
        engine.process(b"\x1b[?25l");
        assert_matches_snapshot(&mut engine);
        engine.process(b"\x1b[?25h");
        assert_matches_snapshot(&mut engine);
        engine.process(b"linea\r\nlinea\r\nlinea\r\n");
        engine.scroll_display(2);
        assert_matches_snapshot(&mut engine);
        engine.scroll_display(40);
        assert_matches_snapshot(&mut engine);
    }

    #[test]
    fn an_unchanged_area_is_not_sent_again() {
        let area = Rect::new(8, 16, 8, 16);
        assert!(needs_update(None, area), "the first area always goes out");
        assert!(!needs_update(Some(area), area));
        assert!(needs_update(Some(area), Rect::new(16, 16, 8, 16)));
        // A hidden cursor is a different area, so going from a cell to "no caret"
        // and back is sent both times.
        assert!(needs_update(Some(area), Rect::new(0, 0, 0, 0)));
        assert!(needs_update(Some(Rect::new(0, 0, 0, 0)), area));
    }

    fn preedit(text: &str) -> Option<Preedit> {
        Some(Preedit {
            text: text.to_owned(),
        })
    }

    #[test]
    fn no_preedit_paints_nothing() {
        let pane = Rect::new(0, 0, 800, 600);
        assert_eq!(
            preedit_area(
                pane,
                Padding::default(),
                CELL,
                CursorCell { column: 0, row: 0 },
                ""
            ),
            None
        );
        assert_eq!(
            apply_preedit(None, &Ime::Preedit(String::new(), None)),
            None
        );
        assert_eq!(apply_preedit(None, &Ime::Commit("a".to_owned())), None);
        assert_eq!(apply_preedit(None, &Ime::Disabled), None);
    }

    #[test]
    fn a_composition_is_painted_on_the_cursor_cell() {
        let pane = Rect::new(0, 0, 200, 80);
        let padding = Padding { x: 4, y: 4 };
        // 8x16 cells starting at (4, 4): column 2 and row 3 are 20 px across and 52
        // px down, and three characters are three cells wide.
        let area = preedit_area(
            pane,
            padding,
            CELL,
            CursorCell { column: 2, row: 3 },
            "にほん",
        )
        .expect("there is a composition to paint");
        assert_eq!(area, Rect::new(20, 52, 24, 16));
    }

    #[test]
    fn the_composition_never_paints_outside_the_grid() {
        // Ten columns of 8 px fit beside the padding, so a composition of twenty
        // characters from column 6 is cut at the pane's inner edge.
        let pane = Rect::new(0, 0, 92, 100);
        let area = preedit_area(
            pane,
            Padding::default(),
            CELL,
            CursorCell { column: 6, row: 0 },
            &"x".repeat(20),
        )
        .expect("there is a composition to paint");
        assert_eq!(area.x, 48);
        assert_eq!(area.right(), 92);
        assert_eq!(area.height, 16);
    }

    #[test]
    fn a_composition_replaces_and_deletes_as_the_input_method_updates_it() {
        let mut current = apply_preedit(None, &Ime::Preedit("に".to_owned(), None));
        assert_eq!(current, preedit("に"));
        // A replacement: the input method sends the whole new composition.
        current = apply_preedit(current, &Ime::Preedit("にほ".to_owned(), None));
        assert_eq!(current, preedit("にほ"));
        // A deletion: the character is simply not in the new text.
        current = apply_preedit(current, &Ime::Preedit("に".to_owned(), None));
        assert_eq!(current, preedit("に"));
        // The platform reports the whole text replaced rather than edited.
        current = apply_preedit(current, &Ime::Preedit("kanji".to_owned(), None));
        assert_eq!(current, preedit("kanji"));
    }

    #[test]
    fn a_commit_or_a_disabled_input_method_clears_the_composition() {
        let composing = preedit("かん");
        assert_eq!(
            apply_preedit(composing.clone(), &Ime::Commit("漢".to_owned())),
            None,
            "the committed text went to the PTY; nothing is left to paint"
        );
        assert_eq!(apply_preedit(composing.clone(), &Ime::Disabled), None);
        assert_eq!(
            apply_preedit(composing.clone(), &Ime::Preedit(String::new(), None)),
            None,
            "an empty preedit is how a cancelled composition arrives"
        );
        assert_eq!(
            apply_preedit(composing.clone(), &Ime::Enabled),
            composing,
            "a new session does not throw away text the platform is still composing"
        );
    }
}
