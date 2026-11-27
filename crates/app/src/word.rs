//! The word a double click selects, from the configured `terminal.word_delimiters`.
//!
//! The emulator core has its own, fixed escape set; the configuration exposes the
//! delimiter string the user expects to control (S2), so the range is resolved here
//! from the row the click landed on and handed to the engine as a plain selection.
//! The rule is the one every terminal user knows: a click inside a word selects the
//! run of characters up to the nearest delimiter on either side, and a click on a
//! delimiter selects that one cell.

use astraterm_terminal::snapshot::CellSnapshot;

/// Column range of the word at `column`, both ends included.
///
/// `cells` is one row of the snapshot: one entry per column, so an index is a
/// column. A click on the trailing half of a wide glyph selects the glyph, and a
/// range never splits one: the spacer is always kept with the cell that draws it.
///
/// `None` when `column` is outside the row, which is a stale click and not a word.
pub fn word_range(cells: &[CellSnapshot], column: u16, delimiters: &str) -> Option<(u16, u16)> {
    let mut at = column as usize;
    if at >= cells.len() {
        return None;
    }
    // The trailing half of a wide glyph draws nothing of its own: a click there
    // belongs to the glyph on its left.
    if cells[at].wide_spacer && at > 0 {
        at -= 1;
    }
    let ends_word = |index: usize| ends_a_word(&cells[index], delimiters);
    if ends_word(at) {
        return Some(cell_span(cells, at, at));
    }
    let mut start = at;
    while start > 0 && !ends_word(start - 1) {
        start -= 1;
    }
    let mut end = at;
    while end + 1 < cells.len() && !ends_word(end + 1) {
        end += 1;
    }
    Some(cell_span(cells, start, end))
}

/// Whether a delimiter, whitespace or a control character ends a word there.
///
/// The configured string is the user's list; whitespace and control characters end a
/// word whatever it says, because no configuration makes a tab part of an identifier.
fn ends_a_word(cell: &CellSnapshot, delimiters: &str) -> bool {
    cell.ch.is_whitespace() || cell.ch.is_control() || delimiters.contains(cell.ch)
}

/// Widens a column range so it never splits a wide glyph.
///
/// The glyph sorts before its spacer, so only the right end can land on a spacer: the
/// spacer is kept with the cell that draws it, which is also the half the selection
/// rectangle has to cover for the glyph to be selected at all.
fn cell_span(cells: &[CellSnapshot], start: usize, end: usize) -> (u16, u16) {
    let end = if cells.get(end + 1).is_some_and(|cell| cell.wide_spacer) {
        end + 1
    } else {
        end
    };
    (start as u16, end as u16)
}

#[cfg(test)]
mod tests {
    use super::*;
    use astraterm_terminal::snapshot::CellSnapshot;

    const DEFAULT_DELIMITERS: &str = " /\\()\"'-.,:;<>~!@#$%^&*|+=[]{}~?";

    /// One cell per column, the way the snapshot builds a row.
    fn row(text: &str) -> Vec<CellSnapshot> {
        let mut cells: Vec<CellSnapshot> = text
            .chars()
            .map(|ch| CellSnapshot {
                ch,
                ..CellSnapshot::default()
            })
            .collect();
        if cells.is_empty() {
            cells.push(CellSnapshot::default());
        }
        cells
    }

    /// A row holding one double-width glyph: the glyph and its spacer cell.
    fn wide_row(fill: &str) -> Vec<CellSnapshot> {
        let mut cells = vec![
            CellSnapshot {
                ch: '漢',
                wide: true,
                ..CellSnapshot::default()
            },
            CellSnapshot {
                wide_spacer: true,
                ..CellSnapshot::default()
            },
        ];
        cells.extend(row(fill));
        cells
    }

    #[test]
    fn a_double_click_selects_the_word_under_the_pointer() {
        let cells = row("foo bar baz");
        assert_eq!(word_range(&cells, 0, DEFAULT_DELIMITERS), Some((0, 2)));
        assert_eq!(word_range(&cells, 2, DEFAULT_DELIMITERS), Some((0, 2)));
        assert_eq!(word_range(&cells, 5, DEFAULT_DELIMITERS), Some((4, 6)));
        assert_eq!(word_range(&cells, 10, DEFAULT_DELIMITERS), Some((8, 10)));
    }

    #[test]
    fn a_click_on_a_delimiter_selects_that_cell() {
        let cells = row("foo bar   ");
        assert_eq!(
            word_range(&cells, 3, DEFAULT_DELIMITERS),
            Some((3, 3)),
            "punctuation is not a word, and a double click there selects the mark itself"
        );
        assert_eq!(
            word_range(&cells, 7, DEFAULT_DELIMITERS),
            Some((7, 7)),
            "the blank part of a padded row is blank like any other"
        );
    }

    #[test]
    fn the_configured_delimiters_decide_the_boundaries() {
        let cells = row("foo-bar/baz");
        // With the shipped default `-` and `/` both end a word.
        assert_eq!(word_range(&cells, 1, DEFAULT_DELIMITERS), Some((0, 2)));
        assert_eq!(word_range(&cells, 5, DEFAULT_DELIMITERS), Some((4, 6)));
        // A user whose list has no `-` makes it a word character, while `/` still
        // ends the word: only the configured delimiters count.
        assert_eq!(
            word_range(&cells, 5, " /\\()\"'.,:;<>~!@#$%^&*|+=[]{}~?"),
            Some((0, 6))
        );
        // An empty list leaves only the whitespace and control rules.
        assert_eq!(word_range(&cells, 5, ""), Some((0, 10)));
    }

    #[test]
    fn whitespace_always_ends_a_word() {
        let cells = row("one\ttwo three");
        assert_eq!(
            word_range(&cells, 1, DEFAULT_DELIMITERS),
            Some((0, 2)),
            "a tab ends the word even though it is not in the configured list"
        );
        assert_eq!(
            word_range(&cells, 4, ""),
            Some((4, 6)),
            "a space ends the word even with an empty delimiter list"
        );
    }

    #[test]
    fn a_wide_glyph_is_never_cut_in_half() {
        let cells = wide_row("ab");
        assert_eq!(
            word_range(&cells, 0, DEFAULT_DELIMITERS),
            Some((0, 1)),
            "the glyph and its spacer are one character"
        );
        assert_eq!(
            word_range(&cells, 1, DEFAULT_DELIMITERS),
            Some((0, 1)),
            "a click on the trailing half selects the glyph it belongs to"
        );
        assert_eq!(word_range(&cells, 2, DEFAULT_DELIMITERS), Some((2, 3)));
    }

    #[test]
    fn a_word_at_the_edge_of_the_row_stops_there() {
        let cells = row("abc");
        assert_eq!(word_range(&cells, 0, DEFAULT_DELIMITERS), Some((0, 2)));
        assert_eq!(word_range(&cells, 2, DEFAULT_DELIMITERS), Some((0, 2)));
    }

    /// The visible row holding `needle`, so the test does not depend on where the
    /// emulator put the text.
    fn row_of(
        engine: &mut astraterm_terminal::TerminalEngine,
        needle: &str,
    ) -> (u16, Vec<CellSnapshot>) {
        let snapshot = engine.snapshot();
        let row = snapshot
            .lines
            .iter()
            .position(|line| line.text().starts_with(needle))
            .expect("the fixture text is on screen");
        (row as u16, snapshot.lines[row].cells.clone())
    }

    fn fixture() -> astraterm_terminal::TerminalEngine {
        use astraterm_terminal::TerminalEngine;
        use astraterm_terminal::engine::EngineOptions;

        let mut engine = TerminalEngine::new(EngineOptions {
            cols: 40,
            rows: 6,
            scrollback: 10,
            ..Default::default()
        });
        engine.process(b"foo bar baz\r\nsegu nda linea\r\n");
        engine
    }

    /// The two calls `Application::begin_selection` makes for a double click have to
    /// copy the word and nothing else: the engine keeps its own, fixed delimiter set,
    /// so the range resolved here is the whole mechanism.
    #[test]
    fn the_range_handed_to_the_engine_copies_exactly_the_word() {
        use astraterm_terminal::{SelectionKind, Side};

        let mut engine = fixture();
        let (row, cells) = row_of(&mut engine, "foo bar baz");
        let (start, end) =
            word_range(&cells, 5, DEFAULT_DELIMITERS).expect("`bar` is a word on that row");
        engine.update_selection(start, row, SelectionKind::Simple, Side::Left);
        engine.update_selection(end, row, SelectionKind::SimpleOrExisting, Side::Right);
        assert_eq!(engine.selection_text().as_deref(), Some("bar"));
    }

    /// The rectangle the Alt+drag press creates keeps its shape while the drag
    /// extends it, which is the contract `SelectionKind::Block` has with the motion
    /// path.
    #[test]
    fn a_block_selection_keeps_its_rectangle_while_the_drag_grows() {
        use astraterm_terminal::{SelectionKind, Side};

        let mut engine = fixture();
        let (row, _) = row_of(&mut engine, "foo bar baz");
        engine.update_selection(1, row, SelectionKind::Block, Side::Left);
        engine.update_selection(3, row + 1, SelectionKind::SimpleOrExisting, Side::Right);
        assert_eq!(
            engine.selection_text().as_deref(),
            Some("oo \negu"),
            "a rectangle copies one equally wide slice of every row it covers"
        );
    }

    #[test]
    fn a_column_outside_the_row_selects_nothing() {
        let cells = row("abc");
        assert_eq!(word_range(&cells, 3, DEFAULT_DELIMITERS), None);
        assert_eq!(word_range(&cells, 900, DEFAULT_DELIMITERS), None);
    }
}
