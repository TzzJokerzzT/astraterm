//! Sequence capture in front of the VT parser: an incremental scanner for kitty
//! graphics, iTerm2 `OSC 1337;File=`, sixel and the two notification OSCs, plus
//! the pure anchoring model for decoded images.
//!
//! Besides images, the scanner extracts terminal-level sequences the core drops:
//! `OSC 9` and `OSC 777;notify;` ask for a desktop notification, and
//! `alacritty_terminal` dispatches unknown OSC bodies to `unhandled`, which only
//! logs at debug level, with no hook for the embedder. Consuming them here is the
//! only way to see them. The module name still says "image" and is now too narrow:
//! renaming it is known debt, deliberately not paid in the same change as the
//! behaviour.
//!
//! `alacritty_terminal` 0.26 parses no image protocol at all, so a kitty
//! graphics APC, an iTerm2 `File=` payload or a sixel DCS currently reaches the
//! grid as literal junk. This module sits in front of the VT parser
//! ([`crate::engine::TerminalEngine::process`]), pulls those three sequences out
//! of the byte stream and hands everything else through untouched. Nothing here
//! draws anything (S5.1): the scanner produces raw payloads, and [`ImageStore`]
//! is the anchoring model the renderer slice will consume.
//!
//! Two rules drive the design:
//!
//! * **Passthrough is byte-identical** when the stream carries no image
//!   sequence, including when the stream is split across arbitrary chunk
//!   boundaries. PTY reads arrive in 32 KiB blocks (`crates/pty/src/lib.rs`), so
//!   one sequence routinely spans two or three chunks; the scanner remembers the
//!   sequence it is inside and nothing else.
//! * **Nothing is unbounded.** Image bytes come from untrusted processes (S5.2),
//!   so a single payload is capped at [`MAX_IMAGE_BYTES`] and an unterminated
//!   sequence is abandoned once it reaches [`MAX_PENDING_BYTES`], which is what
//!   keeps a malformed sequence from swallowing the text that follows it (S5.3).
//!
//! Only the five sequences above are buffered. Every other escape sequence — CSI,
//! any other OSC, any other APC or DCS — is streamed through byte for byte as it
//! arrives, so a `CSI ? u` or an `OSC 8` hyperlink costs no allocation.

/// Largest image payload the scanner will hand to a decoder.
///
/// S5.2: image bytes come from untrusted processes, so a payload that claims
/// more than this is rejected instead of decoded.
pub const MAX_IMAGE_BYTES: usize = 8 * 1024 * 1024;

/// Largest number of bytes the scanner will hold for one unterminated sequence.
///
/// This is [`MAX_IMAGE_BYTES`] plus a small header allowance (the DCS parameters
/// and the APC/OSC introducer). Once a sequence reaches it the scanner abandons
/// it, counts it in [`ScanOutput::dropped`] and returns to the ground state, so
/// the text that follows is not swallowed.
pub const MAX_PENDING_BYTES: usize = MAX_IMAGE_BYTES + 8 * 1024;

/// Largest DCS header (parameters plus intermediates) the scanner buffers before
/// deciding that a DCS is not a sixel image. Real headers are a handful of bytes.
const MAX_HEADER_BYTES: usize = 64;

const ESC: u8 = 0x1b;
const BEL: u8 = 0x07;
const CAN: u8 = 0x18;
const SUB: u8 = 0x1a;

const APC_INTRO: u8 = b'_';
const OSC_INTRO: u8 = b']';
const DCS_INTRO: u8 = b'P';

/// kitty graphics is the only APC body this scanner owns.
const KITTY_APC_BODY: u8 = b'G';

/// OSC bodies this scanner owns, identified by their introducer prefix.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OscKind {
    /// `1337;File=` — iTerm2 inline image.
    Iterm2,
    /// `9;` — an OSC 9 notification, body only.
    Notify9,
    /// `777;notify;` — an rxvt-style notification, `title;body`.
    Notify777,
}

/// The OSC introducers the scanner buffers. No prefix is a prefix of another, so
/// `9;` can never shadow `1337;File=` or `777;notify;`.
const OSC_INTROS: [(&[u8], OscKind); 3] = [
    (b"1337;File=", OscKind::Iterm2),
    (b"9;", OscKind::Notify9),
    (b"777;notify;", OscKind::Notify777),
];

/// A DCS with the final byte `q` (and no intermediate) is sixel.
const SIXEL_FINAL: u8 = b'q';
/// `ESC \` closes an APC, a DCS and an OSC.
const ST_FINAL: u8 = b'\\';

/// The image protocol a captured sequence belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ImageProtocol {
    /// kitty graphics: `ESC _ G <payload> ESC \`.
    Kitty,
    /// iTerm2 inline image: `ESC ] 1337;File=<payload>` ended by BEL or `ESC \`.
    Iterm2,
    /// sixel: `ESC P <params> q <payload> ESC \`.
    Sixel,
}

impl ImageProtocol {
    /// Stable name, used by diagnostics and by the decoders in the next slice.
    pub fn name(self) -> &'static str {
        match self {
            ImageProtocol::Kitty => "kitty",
            ImageProtocol::Iterm2 => "iterm2",
            ImageProtocol::Sixel => "sixel",
        }
    }
}

/// One image sequence captured from the byte stream.
///
/// `payload` is the protocol body without its introducer and without the
/// terminator: what follows `G` for kitty, what follows `File=` for iTerm2, and
/// the sixel data that follows `q`. It is raw and untrusted; the decoders are a
/// later slice.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageMessage {
    pub protocol: ImageProtocol,
    pub payload: Vec<u8>,
}

impl ImageMessage {
    /// Payload length in bytes.
    pub fn bytes(&self) -> usize {
        self.payload.len()
    }

    pub fn is_empty(&self) -> bool {
        self.payload.is_empty()
    }
}

/// One notification sequence captured from the byte stream.
///
/// Both parts are raw and untrusted: the scanner only strips the introducer and
/// splits the title from the body. The application sanitizes the text before it
/// reaches the status bar or the notification daemon. OSC 9 carries a body only,
/// so `title` is `None` there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalNotification {
    pub title: Option<String>,
    pub body: String,
}

/// Result of one [`ImageScanner::scan`] call.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScanOutput {
    /// Bytes the caller must still hand to the VT parser, in order.
    pub passthrough: Vec<u8>,
    /// Image sequences completed during this call, in stream order.
    pub messages: Vec<ImageMessage>,
    /// Notification sequences completed during this call, in stream order.
    pub notifications: Vec<TerminalNotification>,
    /// Sequences discarded during this call: over [`MAX_IMAGE_BYTES`],
    /// unterminated past [`MAX_PENDING_BYTES`], empty, or cancelled with
    /// `CAN`/`SUB`. Foreign sequences are never counted here, and a notification
    /// whose body is empty is ignored without being counted.
    pub dropped: usize,
}

/// State of the incremental scan, persisted between calls.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum State {
    /// Between sequences; only `ESC` is interesting.
    #[default]
    Ground,
    /// Consumed `ESC`; the next byte decides whether a sequence we own starts.
    Escape,
    /// Consumed `ESC _`; the next byte decides between kitty and a foreign APC.
    Apc,
    /// Consumed `ESC ]`; buffering the `1337;File=` candidate.
    Osc,
    /// Consumed `ESC P`; buffering the DCS header looking for a sixel `q`.
    DcsHeader,
    /// A sequence we do not own, streamed through verbatim until its terminator.
    /// `osc` sequences also end on BEL.
    Foreign { osc: bool },
    /// Consumed `ESC _ G`; collecting a kitty payload.
    Kitty,
    /// Consumed `ESC ] 1337;File=`; collecting an iTerm2 payload.
    Iterm2,
    /// Consumed an OSC introducer this scanner owns; collecting its body.
    Notification(OscKind),
    /// Consumed the sixel `q`; collecting sixel data.
    Sixel,
}

/// Incremental sequence scanner for the image and notification protocols.
///
/// Feed the bytes in whatever pieces they arrive and keep the scanner alive for
/// the lifetime of the stream; it remembers partial sequences across calls.
#[derive(Clone, Debug, Default)]
pub struct ImageScanner {
    state: State,
    /// Payload of the image sequence being collected, bounded by
    /// [`MAX_PENDING_BYTES`].
    payload: Vec<u8>,
    /// Bounded small buffer: the `1337;File=` candidate prefix or the DCS header.
    /// Flushed into the passthrough as soon as the sequence is not an image.
    scratch: Vec<u8>,
    /// The current DCS header carried an intermediate byte, which rules out
    /// sixel (`$q` is DECRQSS, not an image).
    saw_intermediate: bool,
    /// The previous byte was `ESC` inside a sequence, so the next one may close it
    /// with `ESC \`. Kept across calls because a chunk can end right there.
    esc_pending: bool,
    /// Discards accumulated during the current call.
    dropped: usize,
}

impl ImageScanner {
    pub fn new() -> Self {
        Self::default()
    }

    /// Scans one chunk, returning everything the VT parser still has to see plus
    /// the image sequences captured in it.
    pub fn scan(&mut self, bytes: &[u8]) -> ScanOutput {
        let mut out = ScanOutput::default();
        self.dropped = 0;
        let mut pos = 0;
        while pos < bytes.len() {
            pos = match self.state {
                State::Ground => self.step_ground(bytes, pos, &mut out),
                State::Escape => self.step_escape(bytes, pos, &mut out),
                State::Apc => self.step_apc(bytes, pos, &mut out),
                State::Osc => self.step_osc(bytes, pos, &mut out),
                State::DcsHeader => self.step_dcs_header(bytes, pos, &mut out),
                State::Foreign { osc } => self.step_foreign(bytes, pos, osc, &mut out),
                State::Kitty | State::Iterm2 | State::Sixel | State::Notification(_) => {
                    self.step_payload(bytes, pos, &mut out)
                }
            };
        }
        out.dropped = self.dropped;
        out
    }

    /// Bytes currently held for an unterminated sequence, for tests and
    /// diagnostics. Never exceeds [`MAX_PENDING_BYTES`] plus the bounded scratch
    /// buffers.
    pub fn pending_bytes(&self) -> usize {
        self.payload.len() + self.scratch.len()
    }

    // --- State handlers ------------------------------------------------------

    fn step_ground(&mut self, bytes: &[u8], pos: usize, out: &mut ScanOutput) -> usize {
        match bytes[pos..].iter().position(|&b| b == ESC) {
            Some(offset) => {
                out.passthrough.extend_from_slice(&bytes[pos..pos + offset]);
                self.state = State::Escape;
                pos + offset + 1
            }
            None => {
                out.passthrough.extend_from_slice(&bytes[pos..]);
                bytes.len()
            }
        }
    }

    /// The `ESC` has been consumed but not emitted: it may open a sequence we own.
    fn step_escape(&mut self, bytes: &[u8], pos: usize, out: &mut ScanOutput) -> usize {
        let byte = bytes[pos];
        match byte {
            APC_INTRO => self.state = State::Apc,
            OSC_INTRO => {
                self.scratch.clear();
                self.state = State::Osc;
            }
            DCS_INTRO => {
                self.scratch.clear();
                self.saw_intermediate = false;
                self.state = State::DcsHeader;
            }
            // `ESC ESC`: the first one is a printed escape, the second may still
            // open a sequence we own.
            ESC => out.passthrough.push(ESC),
            _ => {
                out.passthrough.extend_from_slice(&[ESC, byte]);
                self.state = State::Ground;
            }
        }
        pos + 1
    }

    fn step_apc(&mut self, bytes: &[u8], pos: usize, out: &mut ScanOutput) -> usize {
        if bytes[pos] == KITTY_APC_BODY {
            self.begin_payload(State::Kitty);
            pos + 1
        } else {
            // A foreign APC: emit what we held back and stream the rest.
            out.passthrough.extend_from_slice(&[ESC, APC_INTRO]);
            self.state = State::Foreign { osc: false };
            pos
        }
    }

    fn step_osc(&mut self, bytes: &[u8], pos: usize, out: &mut ScanOutput) -> usize {
        self.scratch.push(bytes[pos]);
        if let Some((_, kind)) = OSC_INTROS
            .iter()
            .find(|(prefix, _)| *prefix == self.scratch.as_slice())
        {
            let state = match kind {
                OscKind::Iterm2 => State::Iterm2,
                kind => State::Notification(*kind),
            };
            self.begin_payload(state);
        } else if !OSC_INTROS
            .iter()
            .any(|(prefix, _)| prefix.starts_with(&self.scratch))
        {
            // Neither an inline image nor a notification: flush the `ESC ]` and
            // the candidate prefix and stream the rest through verbatim.
            out.passthrough.extend_from_slice(&[ESC, OSC_INTRO]);
            out.passthrough.append(&mut self.scratch);
            // The byte that ruled the prefix out may have been the terminator
            // itself: `ESC ] 9 BEL` is a foreign OSC that is already over, and
            // `ESC ] 9 ESC \` opens its own ST. Waiting for another terminator
            // would hide every sequence that follows.
            self.state = match bytes[pos] {
                BEL | CAN | SUB => State::Ground,
                ESC => {
                    self.esc_pending = true;
                    State::Foreign { osc: true }
                }
                _ => State::Foreign { osc: true },
            };
        }
        pos + 1
    }

    fn step_dcs_header(&mut self, bytes: &[u8], pos: usize, out: &mut ScanOutput) -> usize {
        let byte = bytes[pos];
        match byte {
            // Parameter (0x30..=0x3f) and intermediate (0x20..=0x2f) bytes keep
            // the header going.
            0x20..=0x3f => {
                if byte <= 0x2f {
                    self.saw_intermediate = true;
                }
                self.scratch.push(byte);
                if self.scratch.len() > MAX_HEADER_BYTES {
                    self.flush_foreign_dcs(out);
                }
                pos + 1
            }
            // The final byte decides: only a bare `q` is sixel.
            SIXEL_FINAL if !self.saw_intermediate => {
                self.begin_payload(State::Sixel);
                pos + 1
            }
            0x40..=0x7e => {
                self.flush_foreign_dcs(out);
                out.passthrough.push(byte);
                self.state = State::Ground;
                pos + 1
            }
            // Anything else is a malformed DCS: pass the bytes on and resync.
            _ => {
                out.passthrough.extend_from_slice(&[ESC, DCS_INTRO]);
                out.passthrough.append(&mut self.scratch);
                out.passthrough.push(byte);
                self.state = State::Ground;
                pos + 1
            }
        }
    }

    fn step_foreign(&mut self, bytes: &[u8], pos: usize, osc: bool, out: &mut ScanOutput) -> usize {
        let mut index = pos;
        while index < bytes.len() {
            let byte = bytes[index];
            index += 1;
            out.passthrough.push(byte);

            if self.esc_pending {
                self.esc_pending = byte == ESC;
                if byte == ST_FINAL {
                    self.state = State::Ground;
                    return index;
                }
                continue;
            }
            match byte {
                ESC => self.esc_pending = true,
                CAN | SUB => {
                    self.state = State::Ground;
                    return index;
                }
                BEL if osc => {
                    self.state = State::Ground;
                    return index;
                }
                _ => {}
            }
        }
        index
    }

    fn step_payload(&mut self, bytes: &[u8], mut pos: usize, out: &mut ScanOutput) -> usize {
        let bel_terminates = matches!(self.state, State::Iterm2 | State::Notification(_));
        while pos < bytes.len() {
            // Copy the run of plain payload bytes in one go; only the delimiters
            // need byte-wise attention.
            if !self.esc_pending {
                let stop = next_delimiter(&bytes[pos..], bel_terminates);
                if stop > 0 {
                    if !self.append_payload(&bytes[pos..pos + stop], out) {
                        return pos + stop;
                    }
                    pos += stop;
                }
                if pos >= bytes.len() {
                    return pos;
                }
            }

            let byte = bytes[pos];
            pos += 1;
            if self.esc_pending {
                self.esc_pending = false;
                if byte == ST_FINAL {
                    self.finish_payload(out);
                    return pos;
                }
                // A stray `ESC` inside the payload is data, and does not end the
                // sequence, so nothing after it is swallowed.
                if !self.append_payload(&[ESC], out) {
                    return pos;
                }
                if byte == ESC {
                    self.esc_pending = true;
                    continue;
                }
            } else if byte == ESC {
                self.esc_pending = true;
                continue;
            } else if bel_terminates && byte == BEL {
                self.finish_payload(out);
                return pos;
            } else if byte == CAN || byte == SUB {
                self.abort_payload();
                return pos;
            }
            if !self.append_payload(&[byte], out) {
                return pos;
            }
        }
        pos
    }

    // --- Sequence bookkeeping ------------------------------------------------

    fn begin_payload(&mut self, state: State) {
        self.payload.clear();
        self.scratch.clear();
        self.esc_pending = false;
        self.state = state;
    }

    /// Emits `ESC P` plus the buffered header and streams the rest of the DCS
    /// through, because it is not a sixel image.
    fn flush_foreign_dcs(&mut self, out: &mut ScanOutput) {
        out.passthrough.extend_from_slice(&[ESC, DCS_INTRO]);
        out.passthrough.append(&mut self.scratch);
        self.state = State::Foreign { osc: false };
    }

    /// Adds payload bytes, abandoning the sequence when the hold bound is reached.
    ///
    /// Returns `false` when the sequence was abandoned, so the caller stops
    /// treating the remaining bytes as payload.
    fn append_payload(&mut self, bytes: &[u8], _out: &mut ScanOutput) -> bool {
        let room = MAX_PENDING_BYTES.saturating_sub(self.payload.len());
        if bytes.len() > room {
            self.abort_payload();
            return false;
        }
        self.payload.extend_from_slice(bytes);
        true
    }

    fn abort_payload(&mut self) {
        self.dropped += 1;
        self.payload = Vec::new();
        self.esc_pending = false;
        self.state = State::Ground;
    }

    fn finish_payload(&mut self, out: &mut ScanOutput) {
        let protocol = match self.state {
            State::Kitty => Some(ImageProtocol::Kitty),
            State::Iterm2 => Some(ImageProtocol::Iterm2),
            State::Sixel => Some(ImageProtocol::Sixel),
            _ => None,
        };
        let notification = match self.state {
            State::Notification(kind) => Some(kind),
            _ => None,
        };
        let payload = std::mem::take(&mut self.payload);
        self.esc_pending = false;
        self.state = State::Ground;
        if let Some(kind) = notification {
            if payload.len() > MAX_IMAGE_BYTES {
                self.dropped += 1;
                return;
            }
            if let Some(notification) = notification_from(kind, &payload) {
                out.notifications.push(notification);
            }
            return;
        }
        let Some(protocol) = protocol else {
            return;
        };
        // A body over the accepted maximum, or an empty one, is malformed: it is
        // discarded and counted rather than handed to a decoder (S5.3).
        if payload.is_empty() || payload.len() > MAX_IMAGE_BYTES {
            self.dropped += 1;
            return;
        }
        out.messages.push(ImageMessage { protocol, payload });
    }
}

/// Splits a captured notification body into its title and text.
///
/// The payload is lossily decoded so a mangled UTF-8 run cannot drop the whole
/// notification; the application sanitizes the text before use. A body that is
/// empty once trimmed carries nothing to say, so it is ignored rather than
/// reported: `OSC 9;` alone is not a notification.
fn notification_from(kind: OscKind, payload: &[u8]) -> Option<TerminalNotification> {
    let text = String::from_utf8_lossy(payload);
    let (title, body) = match kind {
        OscKind::Notify9 => {
            let body = text.as_ref();
            if is_progress_report(body) {
                return None;
            }
            (None, body)
        }
        OscKind::Notify777 => {
            let (title, body) = text.split_once(';')?;
            (Some(title), body)
        }
        OscKind::Iterm2 => return None,
    };
    let body = body.trim().to_owned();
    if body.is_empty() {
        return None;
    }
    let title = title
        .map(|title| title.trim().to_owned())
        .filter(|title| !title.is_empty());
    Some(TerminalNotification { title, body })
}

/// Whether an `OSC 9` body is a progress report rather than a message.
///
/// ConEmu and Windows Terminal report progress as `OSC 9 ; 4 ; state ; percent`,
/// sharing the introducer with notifications but carrying no message. Treating it
/// as one would raise a desktop notification on every progress tick.
fn is_progress_report(body: &str) -> bool {
    let mut fields = body.split(';');
    let (Some("4"), Some(state), Some(percent), None) =
        (fields.next(), fields.next(), fields.next(), fields.next())
    else {
        return false;
    };
    state.parse::<u8>().is_ok() && (percent.is_empty() || percent.parse::<u32>().is_ok())
}

/// Offset of the first byte that needs byte-wise handling inside a payload.
fn next_delimiter(bytes: &[u8], bel_terminates: bool) -> usize {
    bytes
        .iter()
        .position(|&byte| {
            byte == ESC || byte == CAN || byte == SUB || (bel_terminates && byte == BEL)
        })
        .unwrap_or(bytes.len())
}

// --- Anchoring model ---------------------------------------------------------

/// Where a decoded image sits in the buffer.
///
/// Pure data: the renderer slice turns `anchor_line` plus the column grid into
/// texture coordinates. `anchor_line` is a buffer line, so it survives scrolling;
/// `remove_before` and `retain_anchored` are how the store follows the grid life
/// cycle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImagePlacement {
    pub protocol: ImageProtocol,
    /// Buffer line the image is anchored to (the cursor line when it arrived).
    pub anchor_line: i32,
    /// Column of the anchor, in cells.
    pub column: u16,
    /// Width of the image, in cells.
    pub cols: u16,
    /// Height of the image, in cells.
    pub rows: u16,
    /// Bytes this image accounts for against the store budget.
    pub bytes: usize,
}

/// Anchored images with a byte budget and oldest-first eviction.
///
/// Decoded images are the largest untrusted allocation in the process (S5.2), so
/// the store keeps a hard budget: the oldest placement is evicted until a new one
/// fits. A placement larger than the whole budget never fits, so it is refused
/// instead of flushing everything else.
#[derive(Clone, Debug)]
pub struct ImageStore {
    placements: Vec<ImagePlacement>,
    bytes: usize,
    budget: usize,
}

impl ImageStore {
    pub fn new(budget: usize) -> Self {
        Self {
            placements: Vec::new(),
            bytes: 0,
            budget,
        }
    }

    /// Byte budget for the images held.
    pub fn budget(&self) -> usize {
        self.budget
    }

    /// Bytes currently accounted for.
    pub fn total_bytes(&self) -> usize {
        self.bytes
    }

    pub fn len(&self) -> usize {
        self.placements.len()
    }

    pub fn is_empty(&self) -> bool {
        self.placements.is_empty()
    }

    /// Placements, oldest first.
    pub fn placements(&self) -> &[ImagePlacement] {
        &self.placements
    }

    /// Inserts a placement, evicting the oldest ones until it fits.
    ///
    /// Returns `false` when the placement alone exceeds the budget; the store is
    /// then left untouched.
    pub fn insert(&mut self, placement: ImagePlacement) -> bool {
        if placement.bytes > self.budget {
            return false;
        }
        self.placements.push(placement);
        self.bytes += placement.bytes;
        while self.bytes > self.budget {
            let evicted = self.placements.remove(0);
            self.bytes -= evicted.bytes;
        }
        true
    }

    /// Drops every placement anchored above `line`, which is what scrollback
    /// truncation reports.
    pub fn remove_before(&mut self, line: i32) {
        self.retain_anchored(|placement| placement.anchor_line >= line);
    }

    /// Drops every placement. Used when the application enters the alternate
    /// screen, where buffer lines are not the ones the images were anchored to.
    pub fn clear(&mut self) {
        self.placements.clear();
        self.bytes = 0;
    }

    /// Keeps only the placements the predicate accepts, recomputing the byte
    /// total. Resizing uses this to drop the anchors whose buffer line the reflow
    /// removed.
    pub fn retain_anchored<F>(&mut self, mut keep: F)
    where
        F: FnMut(&ImagePlacement) -> bool,
    {
        self.placements.retain(|placement| keep(placement));
        self.bytes = self
            .placements
            .iter()
            .map(|placement| placement.bytes)
            .sum();
    }
}

impl Default for ImageStore {
    /// A store with a 64 MiB budget, enough for several maximum-size images while
    /// still bounding the untrusted allocation.
    fn default() -> Self {
        Self::new(64 * 1024 * 1024)
    }
}

#[cfg(test)]
mod notification_tests {
    use super::{
        ImageScanner, OscKind, ScanOutput, TerminalNotification, is_progress_report,
        notification_from,
    };

    #[test]
    fn progress_reports_are_not_notifications() {
        // ConEmu/Windows Terminal progress shares the OSC 9 introducer but is not a
        // message, so it must not reach the notification path.
        for body in ["4;1;50", "4;0;", "4;3;100"] {
            assert!(is_progress_report(body), "{body}");
            assert!(
                notification_from(OscKind::Notify9, body.as_bytes()).is_none(),
                "{body} must not become a notification"
            );
        }
        for body in ["hello", "4; foo", "4;1", "44;1;50", "build done"] {
            assert!(!is_progress_report(body), "{body}");
            assert!(
                notification_from(OscKind::Notify9, body.as_bytes()).is_some(),
                "{body} is a message"
            );
        }
    }
    /// Scans `chunks` with one scanner, merging the per-call results.
    fn scan(chunks: &[&[u8]]) -> ScanOutput {
        let mut scanner = ImageScanner::new();
        let mut merged = ScanOutput::default();
        for chunk in chunks {
            let output = scanner.scan(chunk);
            merged.passthrough.extend(output.passthrough);
            merged.messages.extend(output.messages);
            merged.notifications.extend(output.notifications);
            merged.dropped += output.dropped;
        }
        merged
    }

    fn only_notification(output: &ScanOutput) -> &TerminalNotification {
        assert_eq!(
            output.notifications.len(),
            1,
            "expected exactly one notification, got {:?}",
            output.notifications
        );
        &output.notifications[0]
    }

    #[test]
    fn osc_9_with_a_bel_terminator_is_captured() {
        let output = scan(&[b"before\x1b]9;build finished\x07after"]);
        assert_eq!(output.passthrough, b"beforeafter");
        let notification = only_notification(&output);
        assert_eq!(notification.title, None, "OSC 9 carries no title");
        assert_eq!(notification.body, "build finished");
    }

    #[test]
    fn osc_9_with_an_st_terminator_is_captured() {
        let output = scan(&[b"\x1b]9;done\x1b\\tail"]);
        assert_eq!(output.passthrough, b"tail");
        let notification = only_notification(&output);
        assert_eq!(notification.title, None);
        assert_eq!(notification.body, "done");
    }

    #[test]
    fn osc_777_notify_carries_a_title_and_a_body() {
        let output = scan(&[b"\x1b]777;notify;Build;done in 4s\x07"]);
        assert!(output.passthrough.is_empty());
        let notification = only_notification(&output);
        assert_eq!(notification.title.as_deref(), Some("Build"));
        assert_eq!(notification.body, "done in 4s");

        // ST closes it too.
        let output = scan(&[b"\x1b]777;notify;Build;done in 5s\x1b\\"]);
        let notification = only_notification(&output);
        assert_eq!(notification.title.as_deref(), Some("Build"));
        assert_eq!(notification.body, "done in 5s");
    }

    #[test]
    fn osc_777_with_another_subcommand_is_ignored() {
        const INPUT: &[u8] = b"\x1b]777;other;payload\x07after";
        let output = scan(&[INPUT]);
        assert!(
            output.notifications.is_empty(),
            "only `777;notify` is a desktop notification"
        );
        assert_eq!(
            output.passthrough, INPUT,
            "an OSC we do not own must stay byte-identical"
        );
    }

    #[test]
    fn a_notification_split_into_one_byte_chunks_is_reassembled() {
        const INPUT: &[u8] = b"\x1b]9;hello world\x07tail";
        let chunks: Vec<&[u8]> = INPUT.chunks(1).collect();
        let output = scan(&chunks);
        assert_eq!(output.passthrough, b"tail");
        let notification = only_notification(&output);
        assert_eq!(notification.body, "hello world");

        const TITLED: &[u8] = b"\x1b]777;notify;A title;A body\x1b\\";
        let chunks: Vec<&[u8]> = TITLED.chunks(1).collect();
        let output = scan(&chunks);
        assert!(output.passthrough.is_empty(), "{:?}", output.passthrough);
        let notification = only_notification(&output);
        assert_eq!(notification.title.as_deref(), Some("A title"));
        assert_eq!(notification.body, "A body");
    }

    #[test]
    fn a_foreign_osc_next_to_a_notification_stays_byte_identical() {
        // Everything that is neither an image nor a notification has to reach the
        // parser untouched, even in the same chunk as a captured sequence.
        let input: &[u8] =
            b"\x1b]0;title\x07\x1b]9;note\x07\x1b]8;;https://example.invalid/a\x1b\\\
link\x1b]8;;\x1b\\\x1b]1337;SetMark\x07\x1b]52;c;YWJj\x07text";
        let output = scan(&[input]);
        let expected: &[u8] = b"\x1b]0;title\x07\x1b]8;;https://example.invalid/a\x1b\\\
link\x1b]8;;\x1b\\\x1b]1337;SetMark\x07\x1b]52;c;YWJj\x07text";
        assert_eq!(
            output.passthrough,
            expected,
            "got {:?}",
            String::from_utf8_lossy(&output.passthrough)
        );
        let notification = only_notification(&output);
        assert_eq!(notification.body, "note");
    }

    #[test]
    fn an_unterminated_notification_never_swallows_the_text_after_it() {
        // Abandoned at the pending bound: the scanner resyncs and nothing is
        // reported, exactly like an oversized image payload.
        let mut input = Vec::new();
        input.extend_from_slice(b"\x1b]9;");
        input.extend(std::iter::repeat_n(b'A', super::MAX_PENDING_BYTES + 1));
        let mut scanner = ImageScanner::new();
        let first = scanner.scan(&input);
        assert!(first.notifications.is_empty());
        assert_eq!(
            first.dropped, 1,
            "the abandoned sequence must be counted, not delivered"
        );
        assert_eq!(
            scanner.pending_bytes(),
            0,
            "nothing may stay buffered after abandoning a sequence"
        );
        let second = scanner.scan(b"after");
        assert!(second.notifications.is_empty());
        let mut passthrough = first.passthrough;
        passthrough.extend(second.passthrough);
        assert_eq!(
            passthrough, b"after",
            "the scanner must resync instead of swallowing later text"
        );
    }
}
