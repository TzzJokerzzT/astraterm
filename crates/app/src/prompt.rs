//! What the shell's `OSC 133` marks say about the command in the status bar.
//!
//! The engine records one [`SemanticMark`] per boundary a shell emits (`A` prompt
//! start, `B` prompt end, `C` command start, `D` finished with an optional exit
//! code) and exposes them through [`astraterm_terminal::TerminalEngine::semantic_marks`].
//! Everything here is a pure decision on that list, so the two questions the UI asks
//! — "is a command running?" and "what did the last one return?" — are answered
//! without a window, a grid or a clock.
//!
//! A program that never sends the sequence leaves the list empty, and every function
//! here then answers "nothing to say": the status bar and the prompt shortcuts behave
//! exactly as they did before shell integration existed.

use astraterm_terminal::snapshot::{SemanticMark, SemanticMarkKind};

/// What the status bar reports about the shell's last command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptState {
    /// A command started and has not reported finishing.
    Running,
    /// The last command finished; the shell's exit code, when it sent a readable one.
    Finished(Option<i32>),
}

impl PromptState {
    /// The status-bar segment, or `None` when there is nothing worth showing.
    pub fn label(self) -> String {
        match self {
            PromptState::Running => "running".to_owned(),
            PromptState::Finished(Some(0)) => "exit 0".to_owned(),
            PromptState::Finished(Some(code)) => format!("exit {code}"),
            // `D` without a code still says the command ended, just not how.
            PromptState::Finished(None) => "exit ?".to_owned(),
        }
    }
}

/// Whether a command is running, or what the last one returned.
///
/// The question is answered by the newest mark that reports a command
/// (`CommandStart` or `CommandFinished`), not by the newest mark overall: a shell
/// emits `A`/`B` for the *next* prompt as soon as the user gets it back, and that
/// must not erase the exit code of the command that just ended.
pub fn prompt_state(marks: &[SemanticMark]) -> Option<PromptState> {
    for mark in marks.iter().rev() {
        match mark.kind {
            SemanticMarkKind::CommandStart => return Some(PromptState::Running),
            SemanticMarkKind::CommandFinished => {
                return Some(PromptState::Finished(mark.exit_code));
            }
            // Prompt boundaries do not report the state of a command.
            SemanticMarkKind::PromptStart | SemanticMarkKind::PromptEnd => {}
        }
    }
    None
}

/// Scrollback offset that brings the previous or the next prompt into view.
///
/// `offset` is the engine's `display_offset`: rows scrolled back from the live screen,
/// so `0` is the bottom of the buffer. A mark's visible row is `line + offset`, the same
/// arithmetic the snapshot uses for its rows, which makes the viewport's buffer lines
/// `-offset` (the top row) to `rows - 1 - offset` (the bottom one).
///
/// * Forward finds the first prompt *below* the viewport and lands it on the bottom row,
///   so the offset only ever shrinks — the move is always towards the live screen. At
///   `offset == 0` there is nothing below the viewport, so it answers `None`.
/// * Backward finds the last prompt *above* the viewport and lands it on the top row.
///
/// A prompt already on screen is not a target of either direction: a jump means "the
/// next one out of view", which is what makes repeated presses walk the prompts instead
/// of stopping at the one already visible.
///
/// `None` means "no marks, or none in that direction", and the caller then does nothing
/// and says nothing. Only the prompt *start* (`A`) is a target: `B`, `C` and `D` sit on
/// rows the prompt or the command already occupies.
///
/// The lines are read as numbers and never used to index a grid, so a mark whose row a
/// reflow or a trim removed cannot be dereferenced here: the engine drops those marks
/// with the rows they named, and anything it keeps still has its row.
pub fn jump_offset(
    marks: &[SemanticMark],
    offset: usize,
    rows: u16,
    forward: bool,
) -> Option<usize> {
    if rows == 0 {
        return None;
    }
    let rows = i32::from(rows);
    let offset = i32::try_from(offset).unwrap_or(i32::MAX);
    let top = -offset;
    let bottom = rows - 1 - offset;
    let prompts = || {
        marks
            .iter()
            .filter(|mark| mark.kind == SemanticMarkKind::PromptStart)
            .map(|mark| mark.line)
    };
    let line = if forward {
        prompts().filter(|line| *line > bottom).min()?
    } else {
        prompts().filter(|line| *line < top).max()?
    };
    Some(target_offset(line, rows, offset, forward))
}

/// Offset that puts `line` on the bottom (forward) or the top (backward) row.
fn target_offset(line: i32, rows: i32, offset: i32, forward: bool) -> usize {
    let target = if forward {
        // Towards the live screen: the new offset can only be smaller.
        (rows - 1 - line).clamp(0, offset)
    } else {
        // `line < -offset` guarantees a non-negative result.
        -line
    };
    target.max(0) as usize
}

#[cfg(test)]
mod tests {
    use super::*;
    use astraterm_terminal::snapshot::{SemanticMark, SemanticMarkKind};

    fn mark(kind: SemanticMarkKind, line: i32) -> SemanticMark {
        SemanticMark {
            kind,
            line,
            exit_code: None,
        }
    }

    fn finished(line: i32, exit_code: Option<i32>) -> SemanticMark {
        SemanticMark {
            kind: SemanticMarkKind::CommandFinished,
            line,
            exit_code,
        }
    }

    fn prompt(line: i32) -> SemanticMark {
        mark(SemanticMarkKind::PromptStart, line)
    }

    fn prompt_end(line: i32) -> SemanticMark {
        mark(SemanticMarkKind::PromptEnd, line)
    }

    fn started(line: i32) -> SemanticMark {
        mark(SemanticMarkKind::CommandStart, line)
    }

    /// The grid the jump tests use, high enough that a prompt fits above and below it.
    const ROWS: u16 = 10;

    fn jump(marks: &[SemanticMark], offset: usize, forward: bool) -> Option<usize> {
        jump_offset(marks, offset, ROWS, forward)
    }

    /// Visible row a mark lands on at `offset`, the arithmetic the snapshot uses.
    fn visible_row(line: i32, offset: usize) -> i32 {
        line + offset as i32
    }

    #[test]
    fn a_jump_with_no_marks_does_nothing() {
        assert_eq!(jump(&[], 0, false), None);
        assert_eq!(jump(&[], 0, true), None);
        assert_eq!(jump(&[], 40, false), None);
        assert_eq!(jump(&[], 40, true), None);
    }

    #[test]
    fn a_grid_with_no_rows_has_nowhere_to_scroll() {
        let marks = [prompt(-5)];
        assert_eq!(jump_offset(&marks, 0, 0, false), None);
        assert_eq!(jump_offset(&marks, 0, 0, true), None);
    }

    /// Both directions, walked over the same four prompts: each press lands the next
    /// prompt out of view, and repeated presses reach every prompt exactly once.
    #[test]
    fn the_two_directions_walk_the_prompts_and_land_them_inside_the_viewport() {
        let marks = [prompt(-30), prompt(-20), prompt(-5), prompt(-1)];

        // Backwards, from the live screen into the scrollback.
        assert_eq!(jump(&marks, 0, false), Some(1), "the nearest prompt above");
        assert_eq!(jump(&marks, 1, false), Some(5));
        assert_eq!(jump(&marks, 5, false), Some(20));
        assert_eq!(jump(&marks, 20, false), Some(30));
        assert_eq!(
            jump(&marks, 30, false),
            None,
            "nothing above the oldest prompt"
        );

        // Every landing put the prompt on the top row, so it is on screen.
        for (offset, line) in [(1, -1), (5, -5), (20, -20), (30, -30)] {
            assert_eq!(
                visible_row(line, offset),
                0,
                "the prompt lands on the top row"
            );
        }

        // Forwards again, towards the live screen.
        assert_eq!(jump(&marks, 30, true), Some(29));
        assert_eq!(visible_row(-20, 29), ROWS as i32 - 1);
        assert_eq!(jump(&marks, 29, true), Some(14));
        assert_eq!(visible_row(-5, 14), ROWS as i32 - 1);
        assert_eq!(jump(&marks, 14, true), Some(10));
        assert_eq!(visible_row(-1, 10), ROWS as i32 - 1);
        assert_eq!(
            jump(&marks, 10, true),
            None,
            "the last prompt is on the bottom row already"
        );
    }

    #[test]
    fn a_prompt_already_on_screen_is_not_a_jump_target() {
        // Offset 1 puts the prompt at -1 exactly on the top row, and the prompt at -5 is
        // still above the viewport: a jump means the next one *out of* view.
        let marks = [prompt(-5), prompt(-1)];
        assert_eq!(jump(&marks, 1, false), Some(5));
        // Offset 10 puts the prompt at -1 on the bottom row: forward has nothing to do.
        assert_eq!(jump(&marks, 10, true), None);
    }

    #[test]
    fn jumping_forward_from_the_live_screen_does_nothing() {
        // At offset 0 nothing is below the viewport: the live screen is the bottom of the
        // buffer, so there is no prompt to move towards.
        let marks = [prompt(-5), prompt(-1), prompt(0)];
        assert_eq!(jump(&marks, 0, true), None);
        assert_eq!(jump(&marks, 0, false), Some(1), "but backwards still works");
    }

    #[test]
    fn only_the_prompt_start_is_a_jump_target() {
        // The other three marks name rows the prompt or the command already occupy; a
        // jump that stopped on them would leave the prompt itself off screen.
        let marks = [
            started(-30),
            prompt_end(-25),
            finished(-20, Some(0)),
            prompt(-5),
        ];
        assert_eq!(
            jump(&marks, 0, false),
            Some(5),
            "the only prompt start wins"
        );
        assert_eq!(
            jump(&marks, 6, false),
            None,
            "the other marks above the viewport are not jump targets"
        );
    }

    #[test]
    fn a_mark_below_the_grid_is_clamped_instead_of_underflowing() {
        // A stale row (a resize moved the content, a shell kept drawing) must not produce
        // an offset outside the scrollback: the forward jump can only move towards the
        // live screen, so the worst case is "already there".
        let marks = [prompt(99)];
        assert_eq!(jump(&marks, 0, true), Some(0), "already at the live screen");
        assert_eq!(
            jump(&marks, 12, true),
            Some(0),
            "a row past the grid pins the viewport to the live screen instead of underflowing"
        );
    }

    #[test]
    fn a_forward_jump_never_scrolls_past_the_live_screen() {
        // Every prompt is below the viewport here, including one on the very last row.
        let marks = [prompt(0), prompt(9)];
        for offset in 0..40 {
            let Some(target) = jump(&marks, offset, true) else {
                continue;
            };
            assert!(
                target <= offset,
                "forward can only shrink the offset ({offset} -> {target})"
            );
        }
    }

    #[test]
    fn a_backward_jump_never_scrolls_the_wrong_way() {
        let marks = [prompt(-30), prompt(-20), prompt(-5)];
        for offset in 0..40 {
            let Some(target) = jump(&marks, offset, false) else {
                continue;
            };
            assert!(
                target >= offset,
                "backward can only grow the offset ({offset} -> {target})"
            );
        }
    }

    #[test]
    fn a_program_that_never_sends_marks_has_nothing_to_say() {
        assert_eq!(prompt_state(&[]), None);
    }

    #[test]
    fn a_prompt_without_a_command_reports_nothing() {
        assert_eq!(prompt_state(&[prompt(-3), prompt_end(-3)]), None);
    }

    #[test]
    fn a_command_that_started_and_did_not_finish_is_running() {
        let marks = [prompt(-2), prompt_end(-2), started(-1)];
        assert_eq!(prompt_state(&marks), Some(PromptState::Running));
        assert_eq!(PromptState::Running.label(), "running");
    }

    #[test]
    fn a_finished_command_reports_the_exit_code_the_shell_sent() {
        assert_eq!(
            prompt_state(&[
                prompt(-2),
                prompt_end(-2),
                started(-1),
                finished(-1, Some(0))
            ]),
            Some(PromptState::Finished(Some(0)))
        );
        assert_eq!(
            prompt_state(&[started(-1), finished(-1, Some(127))]),
            Some(PromptState::Finished(Some(127)))
        );
        assert_eq!(
            PromptState::Finished(Some(0)).label(),
            "exit 0",
            "success is worth showing, it is the common case"
        );
        assert_eq!(PromptState::Finished(Some(127)).label(), "exit 127");
    }

    #[test]
    fn a_finished_command_without_a_readable_code_still_reports_ending() {
        assert_eq!(
            prompt_state(&[started(-1), finished(-1, None)]),
            Some(PromptState::Finished(None))
        );
        assert_eq!(PromptState::Finished(None).label(), "exit ?");
    }

    #[test]
    fn the_next_prompt_does_not_erase_the_last_exit_code() {
        // The shell repaints `A`/`B` for the new prompt as soon as the command ends;
        // the exit code of the command that just finished is still the last thing
        // the user wants to know.
        let marks = [
            prompt(-2),
            prompt_end(-2),
            started(-1),
            finished(-1, Some(2)),
            prompt(0),
            prompt_end(0),
        ];
        assert_eq!(
            prompt_state(&marks),
            Some(PromptState::Finished(Some(2))),
            "the newest command mark decides, not the newest mark"
        );
    }

    #[test]
    fn a_command_mark_after_a_finish_is_running_again() {
        let marks = [
            started(-2),
            finished(-2, Some(0)),
            prompt(-1),
            prompt_end(-1),
            started(0),
        ];
        assert_eq!(prompt_state(&marks), Some(PromptState::Running));
    }

    #[test]
    fn the_newest_command_mark_wins_over_the_older_ones() {
        let marks = [
            started(-4),
            finished(-4, Some(1)),
            started(-3),
            finished(-3, Some(0)),
            started(-2),
            finished(-2, Some(9)),
        ];
        assert_eq!(prompt_state(&marks), Some(PromptState::Finished(Some(9))));
    }
}
