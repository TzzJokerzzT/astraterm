//! `OSC 133` shell-integration marks, driven through the real VT parser.
//!
//! S1 (engine half): the engine records the prompt and command boundaries a
//! shell reports and exposes them in the snapshot, against the buffer row they
//! arrived on, so the application can jump between prompts later.

use astraterm_terminal::snapshot::{SemanticMarkKind, Snapshot};
use astraterm_terminal::{EngineOptions, MAX_SEMANTIC_MARKS, TerminalEngine};

fn engine(cols: u16, rows: u16, scrollback: usize) -> TerminalEngine {
    TerminalEngine::new(EngineOptions {
        cols,
        rows,
        scrollback,
        ..Default::default()
    })
}

fn feed(engine: &mut TerminalEngine, text: &str) {
    engine.process(text.as_bytes());
}

/// `(kind, buffer line)` of every mark the snapshot reports.
fn marks(snapshot: &Snapshot) -> Vec<(SemanticMarkKind, i32)> {
    snapshot
        .marks
        .iter()
        .map(|mark| (mark.kind, mark.line))
        .collect()
}

#[test]
fn every_mark_kind_reaches_the_snapshot_on_its_row() {
    // A shell writes `A` (prompt start) before its prompt, `B` (prompt end) after
    // it, `C` (command start) when the command runs and `D` (finished) with the
    // exit code when it is done.
    let mut engine = engine(20, 5, 100);
    feed(&mut engine, "\x1b]133;A\x07$ ");
    feed(&mut engine, "\x1b]133;B\x07echo hi\r\n");
    feed(&mut engine, "\x1b]133;C\x07hi\r\n");
    feed(&mut engine, "\x1b]133;D;0\x07");

    let snapshot = engine.snapshot();
    assert_eq!(
        marks(&snapshot),
        vec![
            (SemanticMarkKind::PromptStart, 0),
            (SemanticMarkKind::PromptEnd, 0),
            (SemanticMarkKind::CommandStart, 1),
            (SemanticMarkKind::CommandFinished, 2),
        ],
        "every mark kind is recorded against the row it arrived on"
    );
    assert_eq!(
        snapshot
            .marks
            .iter()
            .map(|mark| mark.exit_code)
            .collect::<Vec<_>>(),
        vec![None, None, None, Some(0)],
        "only `D` carries the exit code"
    );
    assert_eq!(snapshot.lines[0].text(), "$ echo hi");
    assert_eq!(snapshot.lines[1].text(), "hi");
}

#[test]
fn marks_keep_their_row_when_one_read_carries_several_prompts() {
    // A read can carry more than one prompt (a fast shell, a background job). The
    // marks must be recorded where they arrived in the stream, not at the cursor
    // position reached by the end of the chunk.
    let mut engine = engine(20, 6, 100);
    feed(&mut engine, "\x1b]133;A\x07one\r\n");
    feed(&mut engine, "\x1b]133;D;0\x07\x1b]133;A\x07two\r\n");

    let snapshot = engine.snapshot();
    assert_eq!(
        marks(&snapshot),
        vec![
            (SemanticMarkKind::PromptStart, 0),
            (SemanticMarkKind::CommandFinished, 1),
            (SemanticMarkKind::PromptStart, 1),
        ],
        "the marks of the second read belong to the row they arrived on, not to the row the cursor reached"
    );
}

#[test]
fn a_mark_split_across_reads_is_recorded_once() {
    // A read boundary is meaningless: the mark arrives whole even when the PTY
    // cut it in the middle of the introducer and in the middle of the body.
    let mut engine = engine(20, 3, 100);
    engine.process(b"\x1b");
    engine.process(b"]");
    engine.process(b"13");
    engine.process(b"3;A");
    engine.process(b"\x07$ ");

    let snapshot = engine.snapshot();
    assert_eq!(
        marks(&snapshot),
        vec![(SemanticMarkKind::PromptStart, 0)],
        "one mark, not one per read"
    );
    assert_eq!(
        snapshot.lines[0].text(),
        "$",
        "no byte of the mark may reach the grid"
    );
}

#[test]
fn the_string_terminator_ends_a_mark_like_bel() {
    let mut engine = engine(20, 3, 100);
    feed(&mut engine, "\x1b]133;A\x1b\\$ \x1b]133;B\x1b\\");
    let snapshot = engine.snapshot();
    assert_eq!(
        marks(&snapshot),
        vec![
            (SemanticMarkKind::PromptStart, 0),
            (SemanticMarkKind::PromptEnd, 0),
        ]
    );
    assert_eq!(snapshot.lines[0].text(), "$");
}

#[test]
fn an_unknown_mark_letter_records_nothing() {
    let mut engine = engine(20, 3, 100);
    feed(&mut engine, "\x1b]133;Z\x07\x1b]133;\x07\x1b]133;A\x07");
    let snapshot = engine.snapshot();
    assert_eq!(
        marks(&snapshot),
        vec![(SemanticMarkKind::PromptStart, 0)],
        "only the mark the specification defines is recorded"
    );
    assert_eq!(snapshot.lines[0].text(), "");
}

#[test]
fn a_finished_command_without_a_readable_code_has_no_exit_code() {
    for (payload, expected) in [
        ("\x1b]133;D\x07", None),
        ("\x1b]133;D;\x07", None),
        ("\x1b]133;D;7\x07", Some(7)),
        ("\x1b]133;D;1\x07", Some(1)),
        ("\x1b]133;D;99999999999999\x07", None),
        ("\x1b]133;D;nonsense\x07", None),
    ] {
        let mut engine = engine(20, 3, 100);
        feed(&mut engine, payload);
        let snapshot = engine.snapshot();
        assert_eq!(
            snapshot.marks.first().map(|mark| mark.exit_code),
            Some(expected),
            "{payload:?}"
        );
    }
}

#[test]
fn marks_stay_on_their_buffer_lines_when_the_view_scrolls() {
    let mut engine = engine(20, 3, 20);
    for index in 0..6 {
        feed(&mut engine, &format!("\x1b]133;A\x07line {index}\r\n"));
    }
    let before = marks(&engine.snapshot());

    engine.scroll_display(2);
    let snapshot = engine.snapshot();
    assert_eq!(
        snapshot.display_offset, 2,
        "the view moved into the history"
    );
    assert_eq!(
        marks(&snapshot),
        before,
        "a mark is a buffer coordinate, like a selection: the view moves, the marks do not"
    );
}

#[test]
fn a_mark_keeps_naming_its_own_prompt_while_the_buffer_scrolls() {
    // The decision about scrolling: a mark follows its content. Each prompt is
    // written on its own row, so the row a surviving mark names must still hold the
    // prompt it was recorded for.
    use alacritty_terminal::index::Line;

    let mut engine = engine(24, 4, 40);
    for index in 0..12 {
        feed(&mut engine, &format!("\x1b]133;A\x07prompt {index}\r\n"));
    }
    let snapshot = engine.snapshot();
    assert_eq!(
        snapshot.marks.len(),
        12,
        "every prompt is still in the buffer"
    );
    for mark in &snapshot.marks {
        let text = engine.line_text(Line(mark.line));
        assert!(
            text.starts_with("prompt "),
            "the mark at line {} names {text:?}",
            mark.line
        );
    }
    assert_eq!(
        snapshot.marks.first().map(|mark| mark.line),
        Some(engine.line_range().0),
        "the oldest mark sits on the oldest retained row"
    );
    assert_eq!(
        engine.line_text(Line(snapshot.marks[0].line)),
        "prompt 0",
        "and names the prompt it was recorded for"
    );
}

#[test]
fn the_configured_scrollback_still_holds_that_many_lines() {
    // The marks have to be measurable as the buffer scrolls, which the engine does
    // by keeping a small allowance above the configured scrollback and trimming it
    // back. That must not change how much history the user configured.
    use alacritty_terminal::index::Line;

    let mut engine = engine(20, 3, 4);
    for index in 0..20 {
        feed(&mut engine, &format!("line {index}\r\n"));
    }
    let (topmost, bottommost) = engine.line_range();
    assert_eq!(topmost, -4, "exactly the configured scrollback is retained");
    assert_eq!(bottommost, 2);
    assert_eq!(engine.line_text(Line(topmost)), "line 14");

    engine.scroll_to(false);
    let snapshot = engine.snapshot();
    assert_eq!(snapshot.display_offset, 4, "the history is still reachable");
    assert_eq!(snapshot.lines[0].text(), "line 14");
    assert_eq!(snapshot.lines[1].text(), "line 15");
}

#[test]
fn marks_whose_rows_were_trimmed_away_are_dropped() {
    // The scrollback is short, so writing more lines than it holds trims the
    // oldest rows; a mark for a row that no longer exists would point at unrelated
    // content, so it goes with the row.
    let mut engine = engine(20, 3, 2);
    for index in 0..20 {
        feed(&mut engine, &format!("\x1b]133;A\x07line {index}\r\n"));
    }
    let snapshot = engine.snapshot();
    let (topmost, _) = engine.line_range();

    assert!(!snapshot.marks.is_empty(), "the newest prompts are kept");
    assert!(
        snapshot.marks.iter().all(|mark| mark.line >= topmost),
        "every surviving mark points at a live row: {:?} (topmost {topmost})",
        marks(&snapshot)
    );
    assert!(
        snapshot.marks.len() < 20,
        "the trimmed marks are gone: {} survived",
        snapshot.marks.len()
    );
    let last = snapshot.marks.last().expect("the newest prompt is kept");
    assert_eq!(
        engine.line_text(alacritty_terminal::index::Line(last.line)),
        "line 19",
        "the newest mark still names the newest prompt"
    );
    assert_eq!(
        snapshot.marks.first().map(|mark| mark.line),
        Some(topmost),
        "the oldest surviving mark is on the oldest retained row"
    );
}

#[test]
fn a_scroll_too_large_to_measure_drops_the_marks_instead_of_moving_them_wrong() {
    // The engine re-anchors the marks by the growth it can measure. A single
    // sequence that scrolls further than the measurement allowance cannot be
    // measured, and a row that names the wrong content is worse than no row, so the
    // marks go.
    let mut engine = engine(20, 4, 20);
    feed(&mut engine, "\x1b]133;A\x07one\r\n\x1b]133;A\x07two\r\n");
    assert_eq!(engine.snapshot().marks.len(), 2);

    // `CSI 9 S` scrolls nine lines, thirty times, all without a line feed.
    feed(&mut engine, &"\x1b[9S".repeat(30));
    assert!(
        engine.snapshot().marks.is_empty(),
        "an unmeasurable scroll drops the marks"
    );
}

#[test]
fn a_flood_of_marks_is_capped() {
    // A program that never truncates the marks would otherwise grow the engine's
    // memory for the lifetime of the session.
    let mut engine = engine(20, 3, 100);
    let mut payload = String::new();
    for _ in 0..MAX_SEMANTIC_MARKS + 8 {
        payload.push_str("\x1b]133;D;0\x07");
    }
    feed(&mut engine, &payload);

    let snapshot = engine.snapshot();
    assert_eq!(
        snapshot.marks.len(),
        MAX_SEMANTIC_MARKS,
        "the cap bounds the retained marks"
    );
    assert!(
        snapshot
            .marks
            .iter()
            .all(|mark| mark.kind == SemanticMarkKind::CommandFinished),
        "the surviving marks are the newest ones"
    );
}

#[test]
fn splitting_the_stream_never_changes_the_result() {
    // The parser now receives the text line by line, so a read boundary must stay
    // meaningless: every possible cut of the same stream has to produce the same
    // screen, cursor, damage and marks.
    fn run(chunks: &[&[u8]]) -> Snapshot {
        let mut engine = engine(24, 5, 20);
        for chunk in chunks {
            engine.process(chunk);
        }
        engine.snapshot()
    }

    let stream = concat!(
        "\x1b[31mred\x1b[0m \x1b[1mbold\x1b[0m\r\n",
        "$ \x1b]133;B\x07echo hi\r\n",
        "\x1b]133;C\x07hi\r\n",
        "\x1b]133;D;0\x07",
        "\x1b]8;;https://example.com\x07link\x1b]8;;\x07\r\n",
        "\x1b]0;title\x07done",
    )
    .as_bytes()
    .to_vec();

    let whole = run(&[&stream]);
    assert!(!whole.marks.is_empty(), "the stream carries marks");
    for cut in 1..stream.len() {
        assert_eq!(
            run(&[&stream[..cut], &stream[cut..]]),
            whole,
            "a read boundary at {cut} changed the result"
        );
    }
}

#[test]
fn a_terminal_without_osc_133_produces_the_snapshot_it_always_did() {
    // S1: nothing changes for a program that never sends the sequence. This pins
    // the whole snapshot, so a new field that leaks into it is caught.
    use astraterm_terminal::snapshot::{
        CellSnapshot, CursorSnapshot, CursorVisual, ModeFlags, RowSnapshot, Snapshot,
    };

    let cell = |ch: char| CellSnapshot {
        ch,
        ..Default::default()
    };
    let mut engine = engine(4, 2, 100);
    feed(&mut engine, "hi");
    assert_eq!(
        engine.snapshot(),
        Snapshot {
            cols: 4,
            rows: 2,
            display_offset: 0,
            lines: vec![
                RowSnapshot {
                    cells: vec![
                        cell('h'),
                        cell('i'),
                        CellSnapshot::default(),
                        CellSnapshot::default()
                    ],
                },
                RowSnapshot {
                    cells: vec![
                        CellSnapshot::default(),
                        CellSnapshot::default(),
                        CellSnapshot::default(),
                        CellSnapshot::default(),
                    ],
                },
            ],
            cursor: Some(CursorSnapshot {
                column: 2,
                row: 0,
                shape: CursorVisual::Block,
            }),
            modes: ModeFlags {
                show_cursor: true,
                ..Default::default()
            },
            damaged_rows: vec![0, 1],
            selection: None,
            marks: Vec::new(),
        }
    );
}

#[test]
fn adding_marks_changes_nothing_else_in_the_snapshot() {
    // The differential half of the pin above: the same content with and without
    // the marks produces the same screen, cursor and damage.
    let mut plain = engine(20, 4, 100);
    feed(&mut plain, "$ echo hi\r\nhi\r\n");

    let mut marked = engine(20, 4, 100);
    feed(
        &mut marked,
        "\x1b]133;A\x07$ \x1b]133;B\x07echo hi\r\n\x1b]133;C\x07hi\r\n\x1b]133;D;0\x07",
    );

    let plain = plain.snapshot();
    let marked = marked.snapshot();
    assert!(!marked.marks.is_empty(), "the marks are there");
    assert_eq!(
        Snapshot {
            marks: Vec::new(),
            ..marked
        },
        plain,
        "removing the marks must leave today's snapshot behind"
    );
}
