//! Property tests for the image and notification scanner (backlog B4).
//!
//! The scanner ([`astraterm_terminal::image::ImageScanner`]) is a parser fed by
//! bytes from a remote host, and it already shipped one desynchronisation bug:
//! `ESC ] 9 BEL` left it waiting for a second terminator, so every sequence after
//! it went unrecognised. Chunk boundaries are the other half of that failure
//! mode, because PTY reads arrive in blocks and one sequence routinely spans two
//! or three of them.
//!
//! Three properties cover that ground:
//!
//! 1. **Byte-exact passthrough.** A stream with no image or notification sequence
//!    reaches the VT parser unchanged, byte for byte, at any chunk boundary.
//! 2. **Nothing unbounded, nothing panicking.** Arbitrary and adversarial bytes,
//!    truncated sequences and interleavings never panic and never push the
//!    scanner past its hold bound.
//! 3. **Chunking is invisible.** For arbitrary bytes, splitting the input never
//!    changes passthrough, captured sequences or discard accounting. This is the
//!    general form of the desynchronisation bug.
//!
//! The case counts and input sizes are bounded so the normal suite stays fast
//! (see the `ProptestConfig` values below): a hundred and twenty-eight cases per
//! property over at most a kilobyte in at most nine chunks, plus the saturation
//! property at [`MAX_PENDING_BYTES`]. The whole file costs a few hundred
//! milliseconds.

use astraterm_terminal::image::{
    ImageScanner, MAX_IMAGE_BYTES, MAX_PENDING_BYTES, ScanOutput, TerminalNotification,
};
use proptest::prelude::*;
use proptest::sample::select;

/// Largest generated input for property 1, in atoms (about 1 KiB of bytes).
const MAX_ATOMS: usize = 24;
/// Smallest/largest hostile token runs. Kept short: the scanner is O(n) per call
/// and every case is scanned twice (chunked and whole) by the invariance
/// property.
const MAX_TOKENS: usize = 16;
/// For raw arbitrary bytes, the length bound in bytes.
const MAX_RAW_BYTES: usize = 512;
/// Largest cut position generated. Clamped to the input length before use, so it
/// only has to be an upper bound that covers the biggest generated input.
const MAX_CUT_POSITION: usize = 1024;
/// Number of chunk cuts per case. Eight cuts already produce nine chunks, which
/// is more boundaries than a real read loop sees for one sequence.
const MAX_CUTS: usize = 8;
/// Extra room over [`MAX_PENDING_BYTES`] for the scanner's bounded scratch
/// buffers. The crate holds at most 64 bytes of DCS header plus the longest OSC
/// introducer candidate, so 128 leaves slack while still failing on unbounded
/// growth.
const PENDING_ALLOWANCE: usize = 128;

// --- Scanning helpers --------------------------------------------------------

/// Result of scanning one input, with the per-chunk observations the growth
/// property needs.
struct Scanned {
    output: ScanOutput,
    /// `pending_bytes()` after each chunk, in order.
    pending_after_chunk: Vec<usize>,
    scanner: ImageScanner,
}

/// Splits `bytes` at `cuts`, clamped to the input and sorted; duplicates and
/// out-of-range positions collapse, so a degenerate cut vector is still a valid
/// chunking.
fn chunks_at<'a>(bytes: &'a [u8], cuts: &[usize]) -> Vec<&'a [u8]> {
    let mut bounds: Vec<usize> = cuts.iter().map(|&cut| cut.min(bytes.len())).collect();
    bounds.push(0);
    bounds.push(bytes.len());
    bounds.sort_unstable();
    bounds.dedup();
    bounds.windows(2).map(|pair| &bytes[pair[0]..pair[1]]).collect()
}

/// Scans `bytes` in the chunks delimited by `cuts`, merging the per-call results
/// and recording the held bytes after every chunk.
fn scan_at(bytes: &[u8], cuts: &[usize]) -> Scanned {
    let mut scanner = ImageScanner::new();
    let mut output = ScanOutput::default();
    let mut pending_after_chunk = Vec::new();
    for chunk in chunks_at(bytes, cuts) {
        let step = scanner.scan(chunk);
        output.passthrough.extend(step.passthrough);
        output.messages.extend(step.messages);
        output.notifications.extend(step.notifications);
        output.dropped += step.dropped;
        pending_after_chunk.push(scanner.pending_bytes());
    }
    Scanned {
        output,
        pending_after_chunk,
        scanner,
    }
}

// --- Generators --------------------------------------------------------------

/// Atoms whose every concatenation is free of image and notification sequences.
///
/// Each atom either carries no `ESC` at all or is a *self-contained* sequence
/// that the scanner resolves back to the ground state: a CSI, a foreign OSC, a
/// foreign APC, a foreign DCS, or a malformed near-miss of one of the sequences
/// we own (`ESC ] 9 BEL`, `ESC ] 1337;Fil BEL`, …). Because no atom ends inside a
/// sequence, concatenating any of them can never complete a sequence we capture
/// — which is what makes property 1's premise checkable rather than assumed.
///
/// In particular there is no bare `ESC` atom: a trailing `ESC` is genuinely
/// incomplete, the scanner is entitled to hold it until the next byte arrives,
/// and no byte-for-byte promise is possible for it.
const SAFE_ATOMS: &[&[u8]] = &[
    // Plain text, including multibyte UTF-8 and high bytes.
    b"plain",
    b"caf\xc3\xa9",
    b"\xe2\x9c\x93",
    b"\xe6\xbc\xa2\xe5\xad\x97",
    b"\r\n",
    b"\xff\xfe",
    b"\x07",
    b"\x18",
    b"\x1a",
    b"\x9b",
    // Text that spells an introducer but is never preceded by ESC.
    b"1337;File=",
    b"777;notify;",
    b"9;",
    b"_G",
    b"P0;0;8q",
    b"Gf=100;aGk=",
    // CSIs, including one that shares a letter with kitty graphics.
    b"\x1b[1;31m",
    b"\x1b[0m",
    b"\x1b[?u",
    b"\x1b[2J\x1b[H",
    // Foreign OSCs: a title, a hyperlink open and close, a clipboard body and
    // the iTerm2 lookalike subcommands.
    b"\x1b]0;window title\x07",
    b"\x1b]2;styled title\x1b\\",
    b"\x1b]8;;https://example.invalid/a\x1b\\",
    b"\x1b]8;;\x1b\\",
    b"\x1b]52;c;YWJj\x07",
    b"\x1b]1337;SetMark\x07",
    b"\x1b]1337;CurrentDir=/tmp\x07",
    b"\x1b]777;other;payload\x07",
    // Malformed near-misses of the sequences we own: each one rules itself out on
    // its last byte, which is exactly the shape of the bug that shipped.
    b"\x1b]9\x07",
    b"\x1b]9\x1b\\",
    b"\x1b]777;notif\x07",
    b"\x1b]777;notify\x07",
    b"\x1b]1337;Fil\x07",
    b"\x1b]1337;File\x1b\\",
    // Foreign APC, foreign DCS (fully parameterised, and with intermediates), and
    // ESC ESC followed by a CSI.
    b"\x1b_NotKittyPayload\x1b\\",
    b"\x1b_N\x1b\\",
    b"\x1bP1$r0m\x1b\\",
    b"\x1bP+q544e\x1b\\",
    b"\x1bP>|xterm\x1b\\",
    b"\x1bP$qm\x1b\\",
    b"\x1b\x1b[0m",
];

/// Adversarial fragments: the introducers, their bytes, their terminators and
/// valid sequence prefixes that are deliberately left unterminated.
const HOSTILE_TOKENS: &[&[u8]] = &[
    b"\x1b",
    b"]",
    b"P",
    b"_",
    b"8",
    b"9",
    b"7",
    b"1",
    b"3",
    b"4",
    b"0",
    b";",
    b"q",
    b"G",
    b"$",
    b"|",
    b" ",
    b"\\",
    b"\x07",
    b"\x18",
    b"\x1a",
    b"\xff",
    b"A",
    b"File=",
    b"notify;",
    b"1337;",
    b"\x1b]",
    b"\x1bP",
    b"\x1b_",
    b"\x1b\\",
    b"\x1b_G",
    b"\x1b]9;",
    b"\x1b]777;notify;",
    b"\x1b]1337;File=",
    b"\x1bP0;0;8q",
    b"\x1b_Gf=100;aGk=",
    b"\x1b_Gf=100;aGk=\x1b\\",
    b"\x1b]1337;File=inline=1:aGk=",
    b"\x1b]9;note\x07",
    b"\x1b]777;notify;T;B\x1b\\",
    b"\x1bP0;0;8q\"1;1;4;4\x1b\\",
];

/// Sequence introducers that only end on a terminator, paired with a payload byte
/// that is not one. Used by the saturation property.
const UNTERMINATED_INTROS: &[&[u8]] = &[
    b"\x1b_G",
    b"\x1b]1337;File=",
    b"\x1b]9;",
    b"\x1b]777;notify;",
    b"\x1bP0;0;8q",
];

/// Payload bytes that never terminate any of the sequences above.
const INERT_FILLERS: &[u8] = &[b'A', b'0', b';', b'q', b'G', b'"', 0xff];

fn safe_atom() -> impl Strategy<Value = &'static [u8]> {
    select(SAFE_ATOMS)
}

fn hostile_token() -> impl Strategy<Value = &'static [u8]> {
    select(HOSTILE_TOKENS)
}

fn cuts() -> impl Strategy<Value = Vec<usize>> {
    prop::collection::vec(0..=MAX_CUT_POSITION, 0..=MAX_CUTS)
}

// --- Property 1: byte-exact passthrough at arbitrary chunk boundaries ---------

proptest! {
    #![proptest_config(ProptestConfig { cases: 128, ..ProptestConfig::default() })]

    /// An image- and notification-free stream reaches the VT parser byte for
    /// byte, no matter where the chunks are cut.
    #[test]
    fn image_free_stream_passes_through_byte_for_byte(
        atoms in prop::collection::vec(safe_atom(), 0..=MAX_ATOMS),
        cuts in cuts(),
    ) {
        let input: Vec<u8> = atoms.concat();
        let scanned = scan_at(&input, &cuts);

        prop_assert!(
            scanned.output.messages.is_empty(),
            "the generated stream must carry no image sequence, got {:?} for {:?}",
            scanned.output.messages,
            String::from_utf8_lossy(&input),
        );
        prop_assert!(
            scanned.output.notifications.is_empty(),
            "the generated stream must carry no notification, got {:?} for {:?}",
            scanned.output.notifications,
            String::from_utf8_lossy(&input),
        );
        prop_assert_eq!(scanned.output.dropped, 0, "nothing to discard in {:?}",
            String::from_utf8_lossy(&input));
        prop_assert_eq!(
            scanned.output.passthrough.as_slice(),
            input.as_slice(),
            "passthrough must be byte-identical for cuts {:?}",
            cuts,
        );
        prop_assert_eq!(
            scanned.scanner.pending_bytes(),
            0,
            "a resolved stream leaves nothing held",
        );
    }
}

/// The same promise at *every* cut position, and at one byte per chunk, for a
/// byte soup built from the same atoms. The property test samples cut positions;
/// this one is exhaustive over them for a fixed soup.
#[test]
fn every_single_cut_position_keeps_the_passthrough_byte_identical() {
    let mut soup = Vec::new();
    for atom in SAFE_ATOMS.iter().step_by(3) {
        soup.extend_from_slice(atom);
    }
    let whole = scan_at(&soup, &[]);
    assert_eq!(whole.output.passthrough, soup, "uncut passthrough");

    let per_byte = scan_at(&soup, &(0..soup.len()).collect::<Vec<_>>());
    assert_eq!(
        per_byte.output.passthrough, soup,
        "one byte per chunk must not change the bytes"
    );

    for cut in 1..soup.len() {
        let output = scan_at(&soup, &[cut]);
        assert!(output.output.messages.is_empty(), "cut at {cut}");
        assert!(output.output.notifications.is_empty(), "cut at {cut}");
        assert_eq!(output.output.dropped, 0, "cut at {cut}");
        assert_eq!(
            output.output.passthrough, soup,
            "cut at {cut} must not change the passthrough: {:?}",
            String::from_utf8_lossy(&output.output.passthrough)
        );
    }
}

// --- Property 2: no panic, no unbounded growth -------------------------------

/// Asserts the invariants that bound what the scanner may keep or hand on.
fn assert_bounded(scanned: &Scanned, input: &[u8]) {
    let bound = MAX_PENDING_BYTES + PENDING_ALLOWANCE;
    for (index, &pending) in scanned.pending_after_chunk.iter().enumerate() {
        assert!(
            pending <= bound,
            "chunk {index} left {pending} bytes held, over the {bound} bound, for {:?}",
            String::from_utf8_lossy(input)
        );
    }
    assert!(
        scanned.scanner.pending_bytes() <= bound,
        "the scanner holds {} bytes at the end, over the {bound} bound",
        scanned.scanner.pending_bytes()
    );

    // The per-transfer budget: whatever a decoder is handed must be non-empty and
    // within MAX_IMAGE_BYTES.
    let mut delivered = 0;
    for message in &scanned.output.messages {
        assert!(
            !message.is_empty(),
            "an empty payload must never reach a decoder"
        );
        assert!(
            message.bytes() <= MAX_IMAGE_BYTES,
            "payload of {} bytes is over MAX_IMAGE_BYTES",
            message.bytes()
        );
        delivered += message.bytes();
    }
    assert!(
        delivered <= input.len(),
        "more payload bytes ({delivered}) than input bytes ({})",
        input.len()
    );
    for TerminalNotification { title, body } in &scanned.output.notifications {
        assert!(!body.trim().is_empty(), "an empty body is not a notification");
        assert!(
            title.as_ref().is_none_or(|title| !title.trim().is_empty()),
            "an empty title is dropped, not reported"
        );
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 128, ..ProptestConfig::default() })]

    /// Arbitrary bytes never panic and never grow the scanner past its hold
    /// bound, whatever the chunk boundaries.
    #[test]
    fn arbitrary_bytes_never_panic_nor_grow_without_bound(
        bytes in prop::collection::vec(any::<u8>(), 0..=MAX_RAW_BYTES),
        cuts in cuts(),
    ) {
        let scanned = scan_at(&bytes, &cuts);
        assert_bounded(&scanned, &bytes);
    }

    /// The same for adversarial fragments: introducers, digits, terminators,
    /// truncated valid sequences and interleavings of several of them.
    #[test]
    fn adversarial_fragments_never_panic_nor_grow_without_bound(
        tokens in prop::collection::vec(hostile_token(), 0..=MAX_TOKENS),
        cuts in cuts(),
    ) {
        let input: Vec<u8> = tokens.concat();
        let scanned = scan_at(&input, &cuts);
        assert_bounded(&scanned, &input);
    }
}

// The hold bound under saturation: a sequence that never terminates is abandoned
// at [`MAX_PENDING_BYTES`], counted, and the text after it still comes out.
//
// Every introducer runs in every case (the generated filler is the variable), so
// one run costs five 8 MiB scans. Two cases keep that under a couple of hundred
// milliseconds; this is the only property in the file that deliberately touches
// the byte cap.
proptest! {
    #![proptest_config(ProptestConfig { cases: 2, ..ProptestConfig::default() })]

    #[test]
    fn an_unterminated_sequence_is_abandoned_at_the_pending_bound(
        filler in select(INERT_FILLERS),
    ) {
        for intro in UNTERMINATED_INTROS {
            let mut input = Vec::with_capacity(MAX_PENDING_BYTES + 64);
            input.extend_from_slice(intro);
            input.extend(std::iter::repeat_n(filler, MAX_PENDING_BYTES + 1));
            input.extend_from_slice(b"tail");

            let scanned = scan_at(&input, &[]);

            prop_assert!(
                scanned.output.dropped >= 1,
                "the abandoned sequence must be counted, for {:?}",
                String::from_utf8_lossy(intro),
            );
            prop_assert!(
                scanned.output.messages.is_empty(),
                "an unterminated payload must not reach a decoder, for {:?}",
                String::from_utf8_lossy(intro),
            );
            prop_assert!(
                scanned.output.notifications.is_empty(),
                "an unterminated payload must not become a notification, for {:?}",
                String::from_utf8_lossy(intro),
            );
            prop_assert!(
                scanned.output.passthrough.ends_with(b"tail"),
                "the scanner must resync so the text after the sequence comes out, for {:?}",
                String::from_utf8_lossy(intro),
            );
            prop_assert!(
                scanned.scanner.pending_bytes() <= MAX_PENDING_BYTES + PENDING_ALLOWANCE,
                "held {} bytes after abandoning {:?}",
                scanned.scanner.pending_bytes(),
                String::from_utf8_lossy(intro),
            );
        }
    }
}

// --- Property 3: chunking is invisible ---------------------------------------

proptest! {
    #![proptest_config(ProptestConfig { cases: 128, ..ProptestConfig::default() })]

    /// Splitting the stream never changes what the scanner reports. This is the
    /// general form of the desynchronisation bug: any chunk-dependent branch —
    /// holding a byte back, flushing a candidate prefix, ending a sequence —
    /// shows up as a difference here.
    #[test]
    fn chunk_boundaries_do_not_change_the_outcome(
        bytes in prop::collection::vec(any::<u8>(), 0..=MAX_RAW_BYTES),
        tokens in prop::collection::vec(hostile_token(), 0..=MAX_TOKENS),
        cuts in cuts(),
    ) {
        let mut input = bytes;
        input.extend(tokens.concat());

        let whole = scan_at(&input, &[]);
        let chunked = scan_at(&input, &cuts);

        prop_assert_eq!(
            chunked.output.passthrough.as_slice(),
            whole.output.passthrough.as_slice(),
            "passthrough differs with cuts {:?} for {:?}",
            cuts,
            String::from_utf8_lossy(&input),
        );
        prop_assert_eq!(&chunked.output.messages, &whole.output.messages, "images differ");
        prop_assert_eq!(
            &chunked.output.notifications,
            &whole.output.notifications,
            "notifications differ"
        );
        prop_assert_eq!(
            chunked.output.dropped,
            whole.output.dropped,
            "discard accounting differs"
        );
    }
}

/// The desync shape that already shipped, checked at every cut position: a
/// malformed OSC followed by a real image sequence. No cut may blind the scanner.
#[test]
fn a_malformed_osc_never_blinds_the_scanner_at_any_cut() {
    for malformed in [b"\x1b]9\x07".as_slice(), b"\x1b]9\x1b\\", b"\x1b]1337;Fil\x07"] {
        let mut stream = malformed.to_vec();
        stream.extend_from_slice(b"\x1b_Gf=100;aGk=\x1b\\\x1b]9;note\x07");

        let whole = scan_at(&stream, &[]);
        assert_eq!(whole.output.messages.len(), 1, "{malformed:?} uncut");
        assert_eq!(whole.output.notifications.len(), 1, "{malformed:?} uncut");

        for cut in 0..=stream.len() {
            let chunked = scan_at(&stream, &[cut]);
            assert_eq!(
                chunked.output.messages, whole.output.messages,
                "cut at {cut} of {malformed:?}"
            );
            assert_eq!(
                chunked.output.notifications, whole.output.notifications,
                "cut at {cut} of {malformed:?}"
            );
            assert_eq!(
                chunked.output.passthrough, whole.output.passthrough,
                "cut at {cut} of {malformed:?}"
            );
            assert_eq!(
                chunked.output.dropped, whole.output.dropped,
                "cut at {cut} of {malformed:?}"
            );
        }
    }
}
