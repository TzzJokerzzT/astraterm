//! `OSC 133` shell-integration marks.
//!
//! A shell with shell integration enabled brackets the parts of its own output:
//! `133;A` marks the start of the prompt, `133;B` its end (and the start of the
//! user's input), `133;C` the moment the command starts running and
//! `133;D[;<code>]` its completion, optionally with the exit code. The terminal
//! uses them to jump between prompts and to tell whether a command is running.
//!
//! `alacritty_terminal` 0.26 has no hook for them: an unknown OSC body is logged
//! and dropped, with no way for the embedder to see it. Like the image and
//! notification sequences, the marks are therefore captured in front of the VT
//! parser and the parser never sees them — which changes nothing on screen,
//! because it would have ignored them anyway.
//!
//! What the capture needs beyond [`crate::image::ImageScanner`] is *ordering*: a
//! mark belongs to the row the cursor was on when it arrived, and one PTY read
//! routinely carries several prompts. Recording the marks of a chunk after the
//! whole chunk has been parsed would put them all on the row the chunk ended on,
//! so [`ShellMarkScanner::split`] cuts the byte stream into text and marks in
//! stream order and the engine feeds one piece at a time.
//!
//! Nothing is buffered across a read boundary except a candidate `ESC ] 133;`
//! introducer and a mark body, both small and bounded, so a
//! stream without `OSC 133` costs one scan for `ESC` — the same scan the image
//! scanner already does — and no allocation.

use std::borrow::Cow;

use crate::snapshot::SemanticMarkKind;

/// Longest mark body the scanner will remember.
///
/// A real body is a few bytes (`D;130`). A body that grows past this is not a
/// mark, so it is dropped and the rest of the sequence is consumed without being
/// remembered, which keeps a malformed sequence from growing the scanner.
const MAX_MARK_BODY: usize = 256;

/// The full introducer of a mark: `ESC ] 133 ;`.
const INTRODUCER: &[u8] = b"\x1b]133;";

const ESC: u8 = 0x1b;
const BEL: u8 = 0x07;
const CAN: u8 = 0x18;
const SUB: u8 = 0x1a;

/// Maps the letter of a mark body to the boundary it reports.
fn mark_kind(letter: u8) -> Option<SemanticMarkKind> {
    Some(match letter {
        b'A' => SemanticMarkKind::PromptStart,
        b'B' => SemanticMarkKind::PromptEnd,
        b'C' => SemanticMarkKind::CommandStart,
        b'D' => SemanticMarkKind::CommandFinished,
        _ => return None,
    })
}

/// One mark, without a position: the caller records where it arrived.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShellMark {
    pub kind: SemanticMarkKind,
    /// Exit code the shell sent with `D`, when it sent a readable one.
    pub exit_code: Option<i32>,
}

/// One piece of a split read: text for the VT parser, or a mark between them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShellPiece<'a> {
    /// Bytes the VT parser still has to see, in order.
    Text(Cow<'a, [u8]>),
    /// A mark that arrived after the preceding text and before the following one.
    Mark(ShellMark),
}

/// State of the incremental scan, persisted between reads.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum State {
    /// Between sequences; only `ESC` is interesting.
    #[default]
    Ground,
    /// A candidate `ESC ] 133;` introducer is being matched.
    Introducer,
    /// The introducer matched; the mark body is being collected.
    Body,
    /// The body grew past [`MAX_MARK_BODY`]: consume to the terminator, keep
    /// nothing.
    Discard,
}

/// Incremental `OSC 133` splitter.
///
/// Keep one alive for the lifetime of the stream and hand it the bytes in
/// whatever pieces they arrive: a sequence cut in half by a read boundary is
/// still one mark.
#[derive(Clone, Debug, Default)]
pub struct ShellMarkScanner {
    state: State,
    /// The candidate introducer matched so far, or the mark body being collected.
    buffered: Vec<u8>,
    /// The previous byte was `ESC` inside a mark body, so the next one may close
    /// it with `ESC \`. Kept across reads because a read can end right there.
    esc_pending: bool,
}

impl ShellMarkScanner {
    pub fn new() -> Self {
        Self::default()
    }

    /// Splits one read into text and marks, in stream order.
    ///
    /// Text pieces borrow from `bytes` whenever they are contiguous in it; only a
    /// candidate sequence that straddled a read boundary is copied.
    pub fn split<'a>(&mut self, bytes: &'a [u8]) -> Vec<ShellPiece<'a>> {
        let mut pieces: Vec<ShellPiece<'a>> = Vec::new();
        let mut pos = 0;
        // Start of the text run that has not been handed out yet, and the offset
        // of the `ESC` that interrupted it, when that `ESC` arrived in this read.
        let mut run_start = 0;
        let mut held_start: Option<usize> = None;

        loop {
            match self.state {
                State::Ground => {
                    let Some(offset) = bytes[pos..].iter().position(|&byte| byte == ESC) else {
                        break;
                    };
                    let esc_at = pos + offset;
                    match bytes.get(esc_at + 1) {
                        // The read ends on the `ESC`: hold it, the next read decides
                        // whether it opens a mark. Emitting it now would print the
                        // rest of the sequence when it does. The text before it is
                        // complete, so it goes out immediately.
                        None => {
                            push_text(&mut pieces, bytes, run_start, Some(esc_at));
                            self.buffered.clear();
                            self.buffered.push(ESC);
                            self.state = State::Introducer;
                            break;
                        }
                        Some(b']') => {
                            held_start = Some(esc_at);
                            self.buffered.clear();
                            self.buffered.extend_from_slice(&bytes[esc_at..esc_at + 2]);
                            pos = esc_at + 2;
                            self.state = State::Introducer;
                        }
                        // `ESC` followed by anything else cannot open `ESC ] 133;`,
                        // so it is ordinary text and the run keeps going.
                        Some(_) => pos = esc_at + 1,
                    }
                }
                State::Introducer => {
                    let Some(&byte) = bytes.get(pos) else {
                        break;
                    };
                    pos += 1;
                    self.buffered.push(byte);
                    if INTRODUCER.starts_with(&self.buffered) {
                        if self.buffered.len() < INTRODUCER.len() {
                            continue;
                        }
                        // The introducer matched: everything before it is text.
                        push_text(&mut pieces, bytes, run_start, held_start);
                        self.buffered.clear();
                        self.esc_pending = false;
                        self.state = State::Body;
                        run_start = pos;
                        held_start = None;
                        continue;
                    }
                    // Not a mark. Every held byte is ordinary text and the parser
                    // has not seen any of it yet, so hand it over whole.
                    push_text(&mut pieces, bytes, run_start, held_start);
                    pieces.push(ShellPiece::Text(Cow::Owned(std::mem::take(
                        &mut self.buffered,
                    ))));
                    self.state = State::Ground;
                    run_start = pos;
                    held_start = None;
                }
                State::Body | State::Discard => {
                    let mut terminated = false;
                    while let Some(&byte) = bytes.get(pos) {
                        pos += 1;
                        if self.esc_pending {
                            self.esc_pending = false;
                            if byte == b'\\' {
                                terminated = true;
                                break;
                            }
                            // A stray `ESC` ends the sequence here and belongs to
                            // the stream, not to the mark: rewind so the ground
                            // state hands it to the parser.
                            self.end_mark(&mut pieces);
                            pos -= 1;
                            run_start = pos;
                            break;
                        }
                        match byte {
                            BEL => {
                                terminated = true;
                                break;
                            }
                            CAN | SUB => {
                                // The program cancelled the sequence: no mark.
                                self.reset();
                                run_start = pos;
                                break;
                            }
                            ESC => self.esc_pending = true,
                            _ if self.state == State::Body => self.push_body(byte),
                            _ => {}
                        }
                    }
                    if terminated {
                        self.end_mark(&mut pieces);
                        run_start = pos;
                    }
                    if self.state != State::Ground {
                        // The read ended inside the sequence: nothing else in it
                        // is text.
                        break;
                    }
                }
            }
        }

        if self.state == State::Ground {
            if run_start < bytes.len() {
                pieces.push(ShellPiece::Text(Cow::Borrowed(&bytes[run_start..])));
            }
        } else {
            // A candidate sequence crossed the read boundary. The text before it
            // is complete and can never be part of the sequence, so it is handed
            // over now rather than held with the sequence.
            push_text(&mut pieces, bytes, run_start, held_start);
        }
        pieces
    }

    /// Bytes held for an unterminated candidate, for tests and diagnostics.
    pub fn pending_bytes(&self) -> usize {
        self.buffered.len()
    }

    fn push_body(&mut self, byte: u8) {
        if self.buffered.len() < MAX_MARK_BODY {
            self.buffered.push(byte);
            return;
        }
        // Too long to be a mark: keep consuming to the terminator so the bytes are
        // dropped rather than printed, and stop remembering them.
        self.buffered.clear();
        self.state = State::Discard;
    }

    /// Emits the mark the body describes, if it describes one, and returns to the
    /// ground state.
    fn end_mark<'a>(&mut self, pieces: &mut Vec<ShellPiece<'a>>) {
        // Only a body that finished as a body can be a mark: a body abandoned at
        // the hold bound is in the discard state and carries nothing.
        match parse_mark(&self.buffered) {
            Some(mark) if self.state == State::Body => pieces.push(ShellPiece::Mark(mark)),
            _ => {}
        }
        self.reset();
    }

    fn reset(&mut self) {
        self.buffered.clear();
        self.esc_pending = false;
        self.state = State::Ground;
    }
}

/// Appends `bytes[start..end]` as a text piece, where `end` is the offset of the
/// `ESC` that interrupted the run and is absent when that `ESC` came from an
/// earlier read.
fn push_text<'a>(
    pieces: &mut Vec<ShellPiece<'a>>,
    bytes: &'a [u8],
    start: usize,
    end: Option<usize>,
) {
    let end = end.unwrap_or(start);
    if end > start {
        pieces.push(ShellPiece::Text(Cow::Borrowed(&bytes[start..end])));
    }
}

/// Parses a mark body: a letter, and for `D` an optional `;`-separated code.
fn parse_mark(body: &[u8]) -> Option<ShellMark> {
    let (letter, rest) = body.split_first()?;
    let kind = mark_kind(*letter)?;
    let exit_code = match kind {
        SemanticMarkKind::CommandFinished => rest
            .strip_prefix(b";")
            .and_then(|params| params.split(|&byte| byte == b';').next())
            .and_then(|code| std::str::from_utf8(code).ok())
            .and_then(|code| code.trim().parse::<i32>().ok()),
        _ => None,
    };
    Some(ShellMark { kind, exit_code })
}

#[cfg(test)]
mod tests {
    use super::{ShellMark, ShellMarkScanner, ShellPiece};
    use crate::snapshot::SemanticMarkKind;

    /// Splits `reads` in order and collects the marks, ignoring the text.
    fn marks(reads: &[&[u8]]) -> Vec<ShellMark> {
        let mut scanner = ShellMarkScanner::new();
        reads
            .iter()
            .flat_map(|read| scanner.split(read))
            .filter_map(|piece| match piece {
                ShellPiece::Mark(mark) => Some(mark),
                ShellPiece::Text(_) => None,
            })
            .collect()
    }

    /// Splits `reads` in order and rebuilds the text the parser would have seen.
    fn text(reads: &[&[u8]]) -> Vec<u8> {
        let mut scanner = ShellMarkScanner::new();
        reads
            .iter()
            .flat_map(|read| scanner.split(read))
            .flat_map(|piece| match piece {
                ShellPiece::Text(text) => text.into_owned(),
                ShellPiece::Mark(_) => Vec::new(),
            })
            .collect()
    }

    #[test]
    fn the_four_marks_parse_with_their_codes() {
        assert_eq!(
            marks(&[b"\x1b]133;A\x07\x1b]133;B\x07\x1b]133;C\x07\x1b]133;D;130\x07"]),
            vec![
                ShellMark {
                    kind: SemanticMarkKind::PromptStart,
                    exit_code: None,
                },
                ShellMark {
                    kind: SemanticMarkKind::PromptEnd,
                    exit_code: None,
                },
                ShellMark {
                    kind: SemanticMarkKind::CommandStart,
                    exit_code: None,
                },
                ShellMark {
                    kind: SemanticMarkKind::CommandFinished,
                    exit_code: Some(130),
                },
            ]
        );
    }

    #[test]
    fn text_around_the_marks_passes_through_in_order() {
        // The parser must see the same bytes it always did, with the sequences
        // removed: `$ ` before the prompt end and `hi` after the command start.
        assert_eq!(
            text(&[b"$ \x1b]133;B\x07echo hi\r\n\x1b]133;C\x07hi\r\n"]),
            b"$ echo hi\r\nhi\r\n".to_vec()
        );
    }

    #[test]
    fn a_sequence_split_at_every_boundary_is_still_one_mark() {
        assert_eq!(
            marks(&[b"\x1b", b"]", b"1", b"33", b";", b"A", b"\x1b", b"\\"]),
            vec![ShellMark {
                kind: SemanticMarkKind::PromptStart,
                exit_code: None,
            }]
        );
        assert_eq!(text(&[b"\x1b", b"]1", b"33;A\x07"]), Vec::new());
    }

    #[test]
    fn text_before_a_straddling_introducer_is_not_lost() {
        // The run before a held `ESC` is complete the moment the `ESC` is seen, and
        // it must not wait for the next read: if the sequence turns out to be a
        // mark, the held bytes are dropped and the text would go with them.
        assert_eq!(
            text(&[b"prompt\x1b]133", b";A\x07rest"]),
            b"promptrest".to_vec()
        );
        assert_eq!(text(&[b"abc\x1b", b"[31m"]), b"abc\x1b[31m".to_vec());
    }

    #[test]
    fn a_foreign_osc_passes_through_whole() {
        // `ESC ] 1 3 3 7 ;` shares a prefix with the mark and must not be eaten.
        let bytes = b"\x1b]1337;File=abc\x07";
        assert_eq!(marks(&[bytes]), Vec::new());
        assert_eq!(text(&[bytes]), bytes.to_vec());
    }

    #[test]
    fn an_escape_before_the_mark_still_reaches_the_parser() {
        // `ESC [` cannot open a mark, so both bytes and the `CSI` they start stay
        // in the stream.
        let bytes = b"\x1b[31m\x1b]133;A\x07red";
        assert_eq!(
            marks(&[bytes]),
            vec![ShellMark {
                kind: SemanticMarkKind::PromptStart,
                exit_code: None,
            }]
        );
        assert_eq!(text(&[bytes]), b"\x1b[31mred".to_vec());
    }

    #[test]
    fn a_cancelled_sequence_records_nothing_and_prints_nothing() {
        assert_eq!(marks(&[b"\x1b]133;A\x18rest"]).len(), 0);
        assert_eq!(text(&[b"\x1b]133;A\x18rest"]), b"rest".to_vec());
    }

    #[test]
    fn an_over_long_body_is_dropped_and_the_terminator_is_found() {
        let mut body = b"\x1b]133;A".to_vec();
        body.extend(std::iter::repeat_n(b'x', super::MAX_MARK_BODY + 10));
        body.extend_from_slice(b"\x07after");
        assert!(marks(&[&body]).is_empty());
        assert_eq!(text(&[&body]), b"after".to_vec());
    }
}
