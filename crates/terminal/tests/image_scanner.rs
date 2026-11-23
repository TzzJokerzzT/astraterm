//! Tests for the image-sequence scanner and the anchoring model (S5.1, S5.3).
//!
//! The scanner runs in front of the VT parser, so the interesting properties are
//! about bytes, not about drawing: an image-free stream must reach the parser
//! untouched however it is chunked, the three image protocols must be extracted
//! exactly once even when a sequence spans chunks, and a hostile sequence must
//! never grow without limit nor swallow the text that follows it.

use astraterm_terminal::image::{
    ImageMessage, ImagePlacement, ImageProtocol, ImageScanner, ImageStore, MAX_IMAGE_BYTES,
    ScanOutput,
};
use astraterm_terminal::{EngineOptions, TerminalEngine};

/// Scans `chunks` with a single scanner and merges the per-call results.
fn scan_chunks(chunks: &[&[u8]]) -> ScanOutput {
    let mut scanner = ImageScanner::new();
    let mut merged = ScanOutput::default();
    for chunk in chunks {
        let output = scanner.scan(chunk);
        merged.passthrough.extend(output.passthrough);
        merged.messages.extend(output.messages);
        merged.dropped += output.dropped;
    }
    merged
}

/// Splits `bytes` into chunks of one byte.
fn byte_chunks(bytes: &[u8]) -> Vec<&[u8]> {
    bytes.chunks(1).collect()
}

/// Splits `bytes` at every position, twice (an early and a late cut).
fn split_at(bytes: &[u8], cut: usize) -> Vec<&[u8]> {
    vec![&bytes[..cut], &bytes[cut..]]
}

/// A stream with no image sequence in it, but with plenty of real escape codes,
/// including the ones that share an introducer with the protocols we own.
const CORPUS: &[u8] =
    b"\x1b[1;31mred\x1b[0m plain caf\xc3\xa9 \xe2\x9c\x93 \xe6\xbc\xa2\xe5\xad\x97\r\n\
\x1b]0;window title\x07|\x1b]2;styled title\x1b\\|\
\x1b]8;;https://example.invalid/a\x1b\\link\x1b]8;;\x1b\\|\
\x1b]1337;SetMark\x07|\x1b]1337;CurrentDir=/tmp\x07|\
\x1bP1$r0m\x1b\\|\x1bP+q544e\x1b\\|\x1bP>|xterm\x1b\\|\
\x1b_NotKittyPayload\x1b\\|\x1b\x1b[0m|\
_G without an APC prefix, Gf=100 also literal, and a \x1b[?u probe";

// 1. The central invariant: image-free input is byte-identical.
#[test]
fn image_free_stream_passes_through_byte_for_byte() {
    let output = scan_chunks(&[CORPUS]);
    assert!(
        output.messages.is_empty(),
        "no image sequence in the corpus, got {:?}",
        output.messages
    );
    assert_eq!(output.dropped, 0);
    assert_eq!(
        output.passthrough,
        CORPUS,
        "passthrough must be byte-identical: {:?}",
        String::from_utf8_lossy(&output.passthrough)
    );
}

// 2. The same input split into chunks gives the same result.
#[test]
fn arbitrary_chunking_does_not_change_the_result() {
    let whole = scan_chunks(&[CORPUS]);
    let per_byte = scan_chunks(&byte_chunks(CORPUS));
    let early = scan_chunks(&split_at(CORPUS, CORPUS.len() / 3));
    let late = scan_chunks(&split_at(CORPUS, CORPUS.len() - 1));

    for (label, output) in [
        ("one byte", per_byte),
        ("early cut", early),
        ("late cut", late),
    ] {
        assert_eq!(output.passthrough, whole.passthrough, "chunking by {label}");
        assert_eq!(output.passthrough, CORPUS, "chunking by {label}");
        assert!(output.messages.is_empty(), "chunking by {label}");
        assert_eq!(output.dropped, 0, "chunking by {label}");
    }
}

// 3. A kitty APC split in three chunks is extracted exactly once.
#[test]
fn kitty_apc_split_in_three_chunks_is_extracted_once() {
    const SEQUENCE: &[u8] = b"\x1b_Gf=100,a=T;AAAA\x1b\\";

    let output = scan_chunks(&[&SEQUENCE[..5], &SEQUENCE[5..12], &SEQUENCE[12..]]);
    assert!(output.passthrough.is_empty(), "{:?}", output.passthrough);
    assert_eq!(output.dropped, 0);
    assert_eq!(
        output.messages,
        vec![ImageMessage {
            protocol: ImageProtocol::Kitty,
            payload: b"f=100,a=T;AAAA".to_vec(),
        }]
    );

    // No cut position may duplicate or lose the sequence.
    for cut in 1..SEQUENCE.len() {
        let output = scan_chunks(&split_at(SEQUENCE, cut));
        assert_eq!(output.messages.len(), 1, "cut at {cut}");
        assert!(output.passthrough.is_empty(), "cut at {cut}");
    }
}

// 4. iTerm2 inline images, terminated by BEL or by ST.
#[test]
fn iterm2_file_payload_is_extracted_with_bel_and_with_st() {
    let bel = scan_chunks(&[b"\x1b]1337;File=inline=1;size=3:aGk=\x07"]);
    assert!(bel.passthrough.is_empty());
    assert_eq!(
        bel.messages,
        vec![ImageMessage {
            protocol: ImageProtocol::Iterm2,
            payload: b"inline=1;size=3:aGk=".to_vec(),
        }]
    );

    let st = scan_chunks(&[b"\x1b]1337;File=inline=1:aGk=\x1b\\after"]);
    assert_eq!(st.passthrough, b"after");
    assert_eq!(st.messages.len(), 1);
    assert_eq!(st.messages[0].protocol, ImageProtocol::Iterm2);
    assert_eq!(st.messages[0].payload, b"inline=1:aGk=");
    assert_eq!(st.messages[0].bytes(), 13);
}

// 5. Sixel travels in a DCS and ends with ST.
#[test]
fn sixel_dcs_is_extracted_after_the_q_final() {
    let output = scan_chunks(&[b"\x1bP0;0;8q\"1;1;4;4#0;2;0;0;0#1~@\x1b\\tail"]);
    assert_eq!(output.passthrough, b"tail");
    assert_eq!(output.dropped, 0);
    assert_eq!(
        output.messages,
        vec![ImageMessage {
            protocol: ImageProtocol::Sixel,
            payload: b"\"1;1;4;4#0;2;0;0;0#1~@".to_vec(),
        }]
    );
}

// 6. A payload over MAX_IMAGE_BYTES is discarded and counted.
#[test]
fn payload_over_the_image_limit_is_dropped_and_counted() {
    let mut input = Vec::with_capacity(MAX_IMAGE_BYTES + 64);
    input.extend_from_slice(b"\x1b_G");
    input.extend(vec![b'A'; MAX_IMAGE_BYTES + 1]);
    input.extend_from_slice(b"\x1b\\after");

    let output = scan_chunks(&[&input]);
    assert!(
        output.messages.is_empty(),
        "an oversized payload must not reach a decoder"
    );
    assert_eq!(output.dropped, 1, "the discard must be counted");
    assert_eq!(
        output.passthrough, b"after",
        "the sequence must be removed from the stream"
    );
}

// 7. An unterminated sequence past MAX_PENDING_BYTES is dropped, counted, and
//    the text after it still comes out.
#[test]
fn unterminated_sequence_past_the_pending_bound_is_dropped() {
    let mut input = Vec::with_capacity(MAX_IMAGE_BYTES + 16 * 1024);
    input.extend_from_slice(b"\x1b_G");
    input.extend(vec![b'A'; MAX_IMAGE_BYTES + 8 * 1024 + 1]);

    let mut scanner = ImageScanner::new();
    let first = scanner.scan(&input);
    assert!(first.messages.is_empty());
    assert_eq!(first.dropped, 1, "the abandoned sequence must be counted");
    assert_eq!(
        scanner.pending_bytes(),
        0,
        "nothing may stay buffered after abandoning a sequence"
    );

    let second = scanner.scan(b"after");
    assert!(second.messages.is_empty());
    assert_eq!(second.dropped, 0);
    let mut passthrough = first.passthrough;
    passthrough.extend(second.passthrough);
    assert_eq!(
        passthrough, b"after",
        "the scanner must resync so later text is not swallowed"
    );
}

// 8. The anchoring model evicts by budget and follows the grid life cycle.
#[test]
fn store_evicts_the_oldest_and_follows_the_grid() {
    let placement = |anchor_line: i32, bytes: usize| ImagePlacement {
        protocol: ImageProtocol::Kitty,
        anchor_line,
        column: 0,
        cols: 4,
        rows: 2,
        bytes,
    };

    let mut store = ImageStore::new(10);
    assert!(store.insert(placement(1, 4)));
    assert!(store.insert(placement(2, 4)));
    assert_eq!(store.total_bytes(), 8);
    assert_eq!(store.len(), 2);

    // A third image overflows the budget, so the oldest one goes.
    assert!(store.insert(placement(3, 4)));
    assert_eq!(store.len(), 2);
    assert_eq!(store.total_bytes(), 8);
    assert_eq!(
        store
            .placements()
            .iter()
            .map(|placement| placement.anchor_line)
            .collect::<Vec<_>>(),
        vec![2, 3]
    );

    // An image that cannot fit at all is refused instead of flushing the store.
    assert!(!store.insert(placement(4, 11)));
    assert_eq!(store.len(), 2);
    assert_eq!(store.total_bytes(), 8);

    // Scrollback truncation drops the anchors that scrolled away.
    store.remove_before(3);
    assert_eq!(store.len(), 1);
    assert_eq!(store.placements()[0].anchor_line, 3);
    assert_eq!(store.total_bytes(), 4);

    // A resize that cannot map an anchor drops it and keeps the accounting.
    store.retain_anchored(|placement| placement.rows <= 1);
    assert!(store.is_empty());
    assert_eq!(store.total_bytes(), 0);

    // Entering the alternate screen wipes the buffer-line anchors.
    assert!(store.insert(placement(5, 4)));
    store.clear();
    assert!(store.is_empty());
    assert_eq!(store.total_bytes(), 0);
}

// The engine is the only entry point for bytes: it must hand the passthrough to
// the VT parser and keep the images for the renderer slice.
#[test]
fn the_engine_keeps_the_text_and_hands_the_images_over() {
    let mut engine = TerminalEngine::new(EngineOptions {
        cols: 40,
        rows: 4,
        ..Default::default()
    });

    engine.process(b"before\x1b_Gf=100;AAAA\x1b\\after");
    let images = engine.take_images();
    assert_eq!(images.len(), 1);
    assert_eq!(images[0].protocol, ImageProtocol::Kitty);
    assert!(
        engine.take_images().is_empty(),
        "take_images must drain the buffer"
    );

    let snapshot = engine.snapshot();
    assert_eq!(
        snapshot.lines[0].text(),
        "beforeafter",
        "the image bytes must not reach the grid"
    );
}
