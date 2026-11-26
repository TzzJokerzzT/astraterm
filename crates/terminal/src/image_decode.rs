//! Pure image decoders for the three protocols the scanner captures.
//!
//! [`crate::image`] pulls kitty graphics, iTerm2 `OSC 1337;File=` and sixel out
//! of the byte stream and hands them over as raw, untrusted [`ImageMessage`]s.
//! This module turns those bytes into [`DecodedImage`]: width, height and a
//! tightly packed RGBA buffer (4 bytes per pixel, red first). Nothing here
//! touches the GPU, a window or the renderer, so every rule below is testable
//! from fixtures built inside the tests.
//!
//! S5.2: the bytes come from untrusted processes, including a remote host over
//! SSH, so every decoder is bounded:
//!
//! * the payload length is checked against [`DecodeLimits::max_input_bytes`]
//!   before any decoding;
//! * a declared or decoded dimension is checked against
//!   [`DecodeLimits::max_dimension`] and [`DecodeLimits::max_pixels`] *before*
//!   the pixel buffer is allocated, so a lying PNG header cannot reserve an
//!   enormous buffer;
//! * a payload that claims a feature this slice does not implement (iTerm2
//!   download-only, kitty formats other than PNG/RGB/RGBA, sixel HLS colour
//!   spaces) is refused with [`DecodeError::Unsupported`] instead of being
//!   decoded wrongly;
//! * the state a kitty chunked transfer needs is bounded too: [`ImageDecoder`]
//!   holds at most [`MAX_IN_FLIGHT_TRANSFERS`] transfers, caps each one at
//!   [`DecodeLimits::max_input_bytes`] and their total at twice that, and evicts
//!   the oldest transfer when either bound is reached (S1.3).
//!
//! Base64 is decoded here rather than through a new dependency: the three
//! protocols all wrap their payload in the standard alphabet, so the decoder is
//! a few dozen lines and keeps the dependency graph flat.

use std::fmt;

use crate::image::{ImageMessage, ImageProtocol, MAX_IMAGE_BYTES};

/// Default pixel budget: 2048x2048 RGBA, 16 MiB of decoded pixels.
pub const DEFAULT_MAX_PIXELS: usize = 2048 * 2048;

/// Default per-axis bound for a decoded image, in pixels.
pub const DEFAULT_MAX_DIMENSION: u32 = 4096;

/// Bounds applied by [`decode`].
///
/// The defaults are conservative on purpose: a terminal image is a decorative
/// payload, and the cost of refusing an oversized one is a dropped image, while
/// the cost of accepting it is an unbounded allocation from untrusted bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DecodeLimits {
    /// Largest payload accepted, before base64 decoding or PNG expansion.
    pub max_input_bytes: usize,
    /// Largest accepted pixel count (`width * height`).
    pub max_pixels: usize,
    /// Largest accepted width or height, in pixels.
    pub max_dimension: u32,
}

impl Default for DecodeLimits {
    fn default() -> Self {
        Self {
            max_input_bytes: MAX_IMAGE_BYTES,
            max_pixels: DEFAULT_MAX_PIXELS,
            max_dimension: DEFAULT_MAX_DIMENSION,
        }
    }
}

/// A decoded image: tightly packed RGBA, 4 bytes per pixel, in row-major order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodedImage {
    pub width: u32,
    pub height: u32,
    /// `width * height * 4` bytes.
    pub rgba: Vec<u8>,
}

impl DecodedImage {
    /// RGBA of one pixel, or `None` when the coordinates are outside the image.
    pub fn pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let offset = ((y as usize) * (self.width as usize) + (x as usize)) * 4;
        self.rgba
            .get(offset..offset + 4)
            .map(|px| [px[0], px[1], px[2], px[3]])
    }
}

/// Why a payload was not decoded.
///
/// Every variant is a refusal, never a partial image: a malformed sequence is
/// discarded so it cannot break the grid or the text stream (S5.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeError {
    /// The protocol, or a feature of it, is outside this slice (iTerm2
    /// download-only, kitty formats other than PNG/RGB/RGBA, sixel HLS colour
    /// definitions).
    Unsupported,
    /// The payload does not parse as the protocol it claims.
    Malformed(&'static str),
    /// A dimension, pixel count or the input length exceeded the limits.
    ///
    /// `width` and `height` describe the refused image; both are `0` when the
    /// input payload itself was too large to decode.
    TooLarge {
        width: u32,
        height: u32,
        max_dimension: u32,
        max_pixels: usize,
    },
    /// The base64 wrapper is not valid standard base64.
    Base64,
    /// The payload decodes to nothing.
    Empty,
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported => f.write_str("image feature not supported"),
            Self::Malformed(reason) => write!(f, "malformed image payload: {reason}"),
            Self::TooLarge {
                width,
                height,
                max_dimension,
                max_pixels,
            } => write!(
                f,
                "image {width}x{height} exceeds limits (max dimension {max_dimension}, max pixels {max_pixels})"
            ),
            Self::Base64 => f.write_str("invalid base64 payload"),
            Self::Empty => f.write_str("empty image payload"),
        }
    }
}

impl std::error::Error for DecodeError {}

/// Decodes one captured image sequence into RGBA.
///
/// This is the one-message entry point: it keeps no state, so it cannot
/// reassemble a kitty chunked transfer. Use [`ImageDecoder`] for a stream, which
/// is what the application does.
///
/// The payload length is checked first, then the protocol decides. Nothing is
/// allocated from a dimension the payload merely declares: every decoder checks
/// the width and height against `limits` before it sizes a pixel buffer.
pub fn decode(message: &ImageMessage, limits: &DecodeLimits) -> Result<DecodedImage, DecodeError> {
    match ImageDecoder::new().decode(message, limits)? {
        Some(image) => Ok(image),
        None => Err(DecodeError::Malformed("incomplete kitty chunked transfer")),
    }
}

/// Largest number of kitty chunked transfers [`ImageDecoder`] holds at once.
pub const MAX_IN_FLIGHT_TRANSFERS: usize = 4;

/// The `key=value` fields of one kitty APC header.
#[derive(Clone, Copy, Debug)]
struct KittyHeader {
    /// `f=`: 100 is PNG, 24 raw RGB, 32 raw RGBA.
    format: u32,
    /// `s=`: pixels per row, required by the raw formats.
    width: Option<u32>,
    /// `v=`: rows, required by the raw formats.
    height: Option<u32>,
    /// `i=`: image id, what a chunked transfer is keyed on.
    id: Option<u32>,
    /// `m=`: `Some(true)` when more chunks follow. `None` means the sender left
    /// the field out, which kitty reads as "this is the last chunk".
    more: Option<bool>,
}

/// One kitty transfer being reassembled from `m=1` chunks.
#[derive(Debug)]
struct KittyTransfer {
    /// `i=` of the transfer; every chunk of it carries the same value.
    id: u32,
    /// `f=` of the first chunk; later chunks do not repeat it.
    format: u32,
    /// `s=` of the first chunk, for the raw formats.
    width: Option<u32>,
    /// `v=` of the first chunk, for the raw formats.
    height: Option<u32>,
    /// Base64 text of every chunk so far, in arrival order.
    data: Vec<u8>,
}

/// Decodes a stream of image sequences, keeping the kitty chunked-transfer state.
///
/// kitty splits a transfer over several APCs: every chunk but the last carries
/// `m=1` and an `i=<id>`, and the last carries `m=0` or no `m` at all (S1.1). The
/// scanner hands each APC over as its own [`ImageMessage`], so the reassembly has
/// to live here rather than in [`crate::image`].
///
/// Transfers of different ids accumulate separately and may interleave; each one
/// is decoded when its final chunk arrives, with the `f=`, `s=` and `v=` of its
/// first chunk (S1.2). The state is bounded (S1.3): a transfer may not grow past
/// [`DecodeLimits::max_input_bytes`], the transfers in flight may not total more
/// than twice that, and at most [`MAX_IN_FLIGHT_TRANSFERS`] are held. Reaching
/// either of the last two bounds evicts the oldest transfer — freeing its buffer
/// and counting it in [`ImageDecoder::dropped`] — so a sender cannot make the
/// decoder grow without limit. A refused or evicted transfer never yields a
/// partial image (S1.4).
///
/// A decoder belongs to one session and dies with it, so a buffer left in flight
/// by a closed session is released with it.
#[derive(Debug, Default)]
pub struct ImageDecoder {
    /// Transfers in flight, oldest first; the front is the eviction candidate.
    transfers: Vec<KittyTransfer>,
    /// Transfers discarded to stay within the bounds.
    dropped: usize,
}

impl ImageDecoder {
    /// A decoder with nothing in flight.
    pub fn new() -> Self {
        Self::default()
    }

    /// Decodes one captured sequence, reassembling kitty chunks across calls.
    ///
    /// `Ok(None)` means "accepted, but the transfer is not complete yet": a
    /// `m=1` chunk is neither an image nor an error. Every other case returns the
    /// same image or error the equivalent single-message [`decode`] would.
    pub fn decode(
        &mut self,
        message: &ImageMessage,
        limits: &DecodeLimits,
    ) -> Result<Option<DecodedImage>, DecodeError> {
        if message.payload.is_empty() {
            return Err(DecodeError::Empty);
        }
        if message.payload.len() > limits.max_input_bytes {
            return Err(too_large(limits));
        }
        match message.protocol {
            ImageProtocol::Kitty => self.decode_kitty(&message.payload, limits),
            ImageProtocol::Iterm2 => decode_iterm2(&message.payload, limits).map(Some),
            ImageProtocol::Sixel => decode_sixel(&message.payload, limits).map(Some),
        }
    }

    /// Number of chunked transfers in flight.
    pub fn pending_transfers(&self) -> usize {
        self.transfers.len()
    }

    /// Base64 bytes held for the transfers in flight.
    pub fn pending_bytes(&self) -> usize {
        self.transfers
            .iter()
            .map(|transfer| transfer.data.len())
            .sum()
    }

    /// Transfers discarded so far to stay within the bounds.
    ///
    /// Only evictions count. A transfer refused with an error is reported through
    /// the returned [`DecodeError`] instead, so the two are never confused.
    pub fn dropped(&self) -> usize {
        self.dropped
    }

    /// Handles one kitty APC, returning an image only when it completes a transfer.
    fn decode_kitty(
        &mut self,
        payload: &[u8],
        limits: &DecodeLimits,
    ) -> Result<Option<DecodedImage>, DecodeError> {
        let (header, data) = match split_once(payload, b';') {
            Some((header, data)) => (header, data),
            None => (payload, &[][..]),
        };
        let header = parse_kitty_header(header)?;

        if header.more == Some(true) {
            // A chunk is useless without the id that groups it with the rest.
            let id = header.id.ok_or(DecodeError::Malformed(
                "kitty chunked transfer without an image id",
            ))?;
            if data.is_empty() {
                return Err(DecodeError::Empty);
            }
            self.push_chunk(id, header, data, limits)?;
            return Ok(None);
        }

        // A final chunk (`m=0` or no `m`) either closes a transfer this decoder
        // holds, or is a complete single-chunk image. Kitty lets the sender spell
        // `m=0` out even when there is only one chunk, so an explicit last-chunk
        // flag without state is just an image, not a mistake.
        if let Some(id) = header.id
            && let Some(index) = self.position(id)
        {
            let mut transfer = self.transfers.remove(index);
            if transfer.data.len() + data.len() > limits.max_input_bytes {
                return Err(too_large(limits));
            }
            transfer.data.extend_from_slice(data);
            return finish_transfer(&transfer, limits).map(Some);
        }

        decode_kitty_data(&header, data, limits).map(Some)
    }

    /// Appends one `m=1` chunk, keeping the in-flight bounds (S1.3).
    fn push_chunk(
        &mut self,
        id: u32,
        header: KittyHeader,
        data: &[u8],
        limits: &DecodeLimits,
    ) -> Result<(), DecodeError> {
        if let Some(index) = self.position(id)
            && self.transfers[index].data.len() + data.len() > limits.max_input_bytes
        {
            // The sender overran the per-transfer bound: refuse and free the buffer
            // instead of growing it or decoding a partial image.
            self.transfers.remove(index);
            return Err(too_large(limits));
        }
        let appending = self.position(id).is_some();
        self.make_room(
            id,
            data.len(),
            limits.max_input_bytes.saturating_mul(2),
            appending,
        );
        match self.position(id) {
            Some(index) => self.transfers[index].data.extend_from_slice(data),
            None => self.transfers.push(KittyTransfer {
                id,
                format: header.format,
                width: header.width,
                height: header.height,
                data: data.to_vec(),
            }),
        }
        Ok(())
    }

    fn position(&self, id: u32) -> Option<usize> {
        self.transfers.iter().position(|transfer| transfer.id == id)
    }

    /// Evicts the oldest transfers until one more of `incoming` bytes fits.
    ///
    /// `keep` is the transfer the caller is about to append to, and is never the
    /// one evicted: its own size is bounded by the per-transfer limit, which is
    /// half the aggregate bound, so evicting the others always makes room.
    ///
    /// The live-transfer count only matters when a **new** transfer is created:
    /// appending to one already in flight does not increase the count, so a
    /// continuation chunk must never evict a sibling transfer.
    fn make_room(&mut self, keep: u32, incoming: usize, aggregate_max: usize, appending: bool) {
        while (!appending && self.transfers.len() >= MAX_IN_FLIGHT_TRANSFERS)
            || self.pending_bytes() + incoming > aggregate_max
        {
            let Some(index) = self
                .transfers
                .iter()
                .position(|transfer| transfer.id != keep)
            else {
                break;
            };
            self.transfers.remove(index);
            self.dropped += 1;
        }
    }
}

/// The `TooLarge` a byte bound produces: no declared dimensions to report.
fn too_large(limits: &DecodeLimits) -> DecodeError {
    DecodeError::TooLarge {
        width: 0,
        height: 0,
        max_dimension: limits.max_dimension,
        max_pixels: limits.max_pixels,
    }
}

/// Decodes a completed transfer, using the header of its first chunk.
fn finish_transfer(
    transfer: &KittyTransfer,
    limits: &DecodeLimits,
) -> Result<DecodedImage, DecodeError> {
    let header = KittyHeader {
        format: transfer.format,
        width: transfer.width,
        height: transfer.height,
        id: Some(transfer.id),
        more: None,
    };
    decode_kitty_data(&header, &transfer.data, limits)
}

/// Refuses a width/height pair that breaks the configured bounds.
///
/// The RGBA buffer is `pixels * 4` bytes, so the pixel count is also capped at
/// what a slice can address; that keeps every allocation derived from a checked
/// dimension representable and free of overflow.
fn check_dimensions(width: u32, height: u32, limits: &DecodeLimits) -> Result<(), DecodeError> {
    const MAX_RGBA_PIXELS: u64 = (isize::MAX as u64) / 4;
    if width == 0 || height == 0 {
        return Err(DecodeError::Malformed("zero dimension"));
    }
    let pixels = u64::from(width) * u64::from(height);
    if width > limits.max_dimension
        || height > limits.max_dimension
        || pixels > limits.max_pixels as u64
        || pixels > MAX_RGBA_PIXELS
    {
        return Err(DecodeError::TooLarge {
            width,
            height,
            max_dimension: limits.max_dimension,
            max_pixels: limits.max_pixels,
        });
    }
    Ok(())
}

// --- Base64 ------------------------------------------------------------------

/// Decodes standard base64 (alphabet `+/`, `=` padding).
///
/// Line breaks, spaces and tabs are ignored because all three protocols wrap
/// long payloads; everything else outside the alphabet, a length that is not a
/// multiple of four and non-zero leftover bits are refused instead of guessed.
fn decode_base64(input: &[u8]) -> Result<Vec<u8>, DecodeError> {
    let mut out = Vec::with_capacity(input.len() / 4 * 3);
    let mut symbols = 0usize;
    let mut padding = 0usize;
    let mut accumulator: u32 = 0;
    let mut bits: u32 = 0;
    for &byte in input {
        match byte {
            b'\n' | b'\r' | b' ' | b'\t' => continue,
            b'=' => {
                padding += 1;
                if padding > 2 {
                    return Err(DecodeError::Base64);
                }
                symbols += 1;
                continue;
            }
            _ => {}
        }
        if padding > 0 {
            // Data after the padding would silently change the decoded bytes.
            return Err(DecodeError::Base64);
        }
        let value = base64_value(byte).ok_or(DecodeError::Base64)?;
        symbols += 1;
        accumulator = (accumulator << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((accumulator >> bits) & 0xff) as u8);
        }
    }
    if symbols == 0 || !symbols.is_multiple_of(4) {
        return Err(DecodeError::Base64);
    }
    if bits > 0 && accumulator & ((1 << bits) - 1) != 0 {
        return Err(DecodeError::Base64);
    }
    Ok(out)
}

fn base64_value(byte: u8) -> Option<u32> {
    let value = match byte {
        b'A'..=b'Z' => byte - b'A',
        b'a'..=b'z' => byte - b'a' + 26,
        b'0'..=b'9' => byte - b'0' + 52,
        b'+' => 62,
        b'/' => 63,
        _ => return None,
    };
    Some(u32::from(value))
}

// --- PNG ---------------------------------------------------------------------

/// Decodes a PNG payload into RGBA.
///
/// S5.2: the IHDR is read first and checked against the limits, so a header that
/// claims 60000x60000 is refused before the decoder allocates anything for those
/// dimensions. Only then is the image expanded and normalised to RGBA.
fn decode_png(bytes: &[u8], limits: &DecodeLimits) -> Result<DecodedImage, DecodeError> {
    use png::Transformations;

    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    let (width, height) = decoder
        .read_header_info()
        .map_err(|_| DecodeError::Malformed("png header"))?
        .size();
    check_dimensions(width, height, limits)?;

    decoder.set_transformations(Transformations::normalize_to_color8() | Transformations::ALPHA);
    let mut reader = decoder
        .read_info()
        .map_err(|_| DecodeError::Malformed("png info"))?;
    let buffer_size = reader.output_buffer_size().ok_or(DecodeError::TooLarge {
        width,
        height,
        max_dimension: limits.max_dimension,
        max_pixels: limits.max_pixels,
    })?;
    let mut buffer = vec![0u8; buffer_size];
    let frame = reader
        .next_frame(&mut buffer)
        .map_err(|_| DecodeError::Malformed("png data"))?;

    // An APNG frame may be smaller than the canvas; re-check what was decoded.
    check_dimensions(frame.width, frame.height, limits)?;
    let pixels = (frame.width as usize) * (frame.height as usize);
    let source = &buffer[..frame.buffer_size()];
    let (color, depth) = reader.output_color_type();
    let rgba = png_to_rgba(source, pixels, color, depth)?;
    Ok(DecodedImage {
        width: frame.width,
        height: frame.height,
        rgba,
    })
}

/// Expands the decoder's output into tightly packed RGBA.
fn png_to_rgba(
    source: &[u8],
    pixels: usize,
    color: png::ColorType,
    depth: png::BitDepth,
) -> Result<Vec<u8>, DecodeError> {
    use png::{BitDepth, ColorType};

    let channels = match (color, depth) {
        (ColorType::Rgba, BitDepth::Eight) => 4,
        (ColorType::Rgb, BitDepth::Eight) => 3,
        (ColorType::GrayscaleAlpha, BitDepth::Eight) => 2,
        (ColorType::Grayscale, BitDepth::Eight) => 1,
        // `normalize_to_color8` plus `ALPHA` should have produced one of these.
        _ => return Err(DecodeError::Malformed("png pixel format")),
    };
    let expected = pixels
        .checked_mul(channels)
        .ok_or(DecodeError::Malformed("png pixel data"))?;
    if source.len() != expected {
        return Err(DecodeError::Malformed("png pixel data"));
    }
    let mut rgba = Vec::with_capacity(pixels * 4);
    match channels {
        4 => rgba.extend_from_slice(source),
        3 => {
            for pixel in source.chunks(3) {
                if let [red, green, blue] = pixel {
                    rgba.extend_from_slice(&[*red, *green, *blue, 0xff]);
                }
            }
        }
        2 => {
            for pixel in source.chunks(2) {
                if let [gray, alpha] = pixel {
                    rgba.extend_from_slice(&[*gray, *gray, *gray, *alpha]);
                }
            }
        }
        _ => {
            for &gray in source {
                rgba.extend_from_slice(&[gray, gray, gray, 0xff]);
            }
        }
    }
    Ok(rgba)
}

// --- iTerm2 ------------------------------------------------------------------

/// Decodes an iTerm2 `OSC 1337;File=` payload: `<args>:<base64>`.
///
/// `inline=0` asks the terminal to save the file instead of showing it; this
/// slice has no download path (S5.2 forbids fetching anything), so that case is
/// `Unsupported` rather than an image. `width`, `height` and
/// `preserveAspectRatio` are display metadata: they scale the image when it is
/// drawn, never the pixels, so they are parsed and deliberately ignored here.
fn decode_iterm2(payload: &[u8], limits: &DecodeLimits) -> Result<DecodedImage, DecodeError> {
    let (args, data) =
        split_once(payload, b':').ok_or(DecodeError::Malformed("iterm2 payload without data"))?;
    for arg in args.split(|&byte| byte == b';') {
        if arg.is_empty() {
            continue;
        }
        let (key, value) =
            split_once(arg, b'=').ok_or(DecodeError::Malformed("iterm2 argument without value"))?;
        if key == b"inline" && value == b"0" {
            return Err(DecodeError::Unsupported);
        }
    }
    if data.is_empty() {
        return Err(DecodeError::Empty);
    }
    let bytes = decode_base64(data)?;
    decode_png(&bytes, limits)
}

// --- kitty graphics ----------------------------------------------------------

/// Decodes a kitty graphics payload: `<key=value,...>;<base64 data>`.
///
/// Supported in this slice: `f=100` (PNG) and `f=24`/`f=32` (raw RGB or RGBA with
/// `s=`/`v=`). Whether the payload arrived in one APC or in several is not this
/// function's business — [`ImageDecoder`] reassembles the latter — so it only sees
/// the concatenated base64 text and the header of the first chunk.
fn decode_kitty_data(
    header: &KittyHeader,
    data: &[u8],
    limits: &DecodeLimits,
) -> Result<DecodedImage, DecodeError> {
    if data.is_empty() {
        return Err(DecodeError::Empty);
    }

    match header.format {
        100 => {
            let png = decode_base64(data)?;
            decode_png(&png, limits)
        }
        24 | 32 => {
            let (Some(width), Some(height)) = (header.width, header.height) else {
                return Err(DecodeError::Malformed("kitty raw image without size"));
            };
            check_dimensions(width, height, limits)?;
            let bytes = decode_base64(data)?;
            let channels = if header.format == 24 { 3 } else { 4 };
            raw_to_rgba(&bytes, width, height, channels)
        }
        _ => Err(DecodeError::Unsupported),
    }
}

/// Parses the `key=value` fields of a kitty APC header.
///
/// `m=` is kept rather than judged here, because whether `m=0` is legal depends on
/// the state [`ImageDecoder`] holds.
fn parse_kitty_header(header: &[u8]) -> Result<KittyHeader, DecodeError> {
    let mut parsed = KittyHeader {
        format: 32,
        width: None,
        height: None,
        id: None,
        more: None,
    };
    for field in header.split(|&byte| byte == b',') {
        if field.is_empty() {
            continue;
        }
        let (key, value) =
            split_once(field, b'=').ok_or(DecodeError::Malformed("kitty header field"))?;
        match key {
            b"m" => {
                parsed.more = Some(match value {
                    b"0" => false,
                    b"1" => true,
                    _ => return Err(DecodeError::Malformed("kitty chunk flag")),
                });
            }
            b"i" => parsed.id = Some(parse_kitty_number(value, "kitty image id")?),
            b"f" => parsed.format = parse_kitty_number(value, "kitty format")?,
            b"s" => parsed.width = Some(parse_kitty_dimension(value)?),
            b"v" => parsed.height = Some(parse_kitty_dimension(value)?),
            // `a=T`/`a=t` transfer the image; every other action (query, put,
            // delete) is a no-image message this slice does not implement.
            b"a" if value == b"T" || value == b"t" => {}
            b"a" => return Err(DecodeError::Unsupported),
            // Display hints (`c`, `r`, `x`, `y`, `X`, `Y`, `z`, `p`, `q`, `C`) and
            // the placement id (`I`) affect placement or are no-ops for a pure
            // decoder.
            _ => {}
        }
    }
    Ok(parsed)
}

fn parse_kitty_number(value: &[u8], reason: &'static str) -> Result<u32, DecodeError> {
    parse_number(value)
        .map(|(value, _)| value)
        .map_err(|_| DecodeError::Malformed(reason))
}

fn parse_kitty_dimension(value: &[u8]) -> Result<u32, DecodeError> {
    parse_kitty_number(value, "kitty dimension")
}

/// Wraps raw RGB or RGBA bytes as an RGBA image.
fn raw_to_rgba(
    bytes: &[u8],
    width: u32,
    height: u32,
    channels: usize,
) -> Result<DecodedImage, DecodeError> {
    let pixels = (width as usize) * (height as usize);
    let expected = pixels
        .checked_mul(channels)
        .ok_or(DecodeError::Malformed("kitty raw size"))?;
    if bytes.len() != expected {
        return Err(DecodeError::Malformed("kitty raw size"));
    }
    let mut rgba = Vec::with_capacity(pixels * 4);
    if channels == 4 {
        rgba.extend_from_slice(bytes);
    } else {
        for pixel in bytes.chunks(3) {
            if let [red, green, blue] = pixel {
                rgba.extend_from_slice(&[*red, *green, *blue, 0xff]);
            }
        }
    }
    Ok(DecodedImage {
        width,
        height,
        rgba,
    })
}

// --- sixel -------------------------------------------------------------------

/// Default sixel palette: a 16-entry VT340-like base palette repeated over the
/// 256 registers, so a payload that selects a colour without defining it still
/// paints something deterministic.
fn default_sixel_palette() -> [[u8; 4]; 256] {
    const BASE: [[u8; 3]; 16] = [
        [0, 0, 0],
        [0, 0, 100],
        [0, 100, 0],
        [0, 100, 100],
        [100, 0, 0],
        [100, 0, 100],
        [100, 100, 0],
        [100, 100, 100],
        [50, 50, 50],
        [0, 0, 50],
        [0, 50, 0],
        [0, 50, 50],
        [50, 0, 0],
        [50, 0, 50],
        [50, 50, 0],
        [80, 80, 80],
    ];
    let mut palette = [[0u8; 4]; 256];
    for (index, entry) in palette.iter_mut().enumerate() {
        let base = BASE[index % BASE.len()];
        entry[0] = scale_percent(base[0]);
        entry[1] = scale_percent(base[1]);
        entry[2] = scale_percent(base[2]);
        entry[3] = 0xff;
    }
    palette
}

/// Converts a 0-100 sixel colour component to 0-255, clamping out-of-range input.
fn scale_percent(value: u8) -> u8 {
    (u32::from(value.min(100)) * 255 / 100) as u8
}

/// Decodes a sixel DCS payload into an RGBA image with a transparent background.
///
/// The payload is parsed twice: the first pass validates every painted pixel
/// against the limits and returns the final extent, the second pass paints into
/// an exactly-sized buffer. That keeps the allocation equal to the painted area
/// and means an oversized image is refused before anything is reserved. A
/// declared raster size (`"Pan;Pad;Ph;Pv`) is only validated against the limits,
/// never used to size the canvas: the pixels outside the painted area would be
/// transparent anyway.
fn decode_sixel(payload: &[u8], limits: &DecodeLimits) -> Result<DecodedImage, DecodeError> {
    let (width, height) = run_sixel(payload, limits, |_, _, _| {})?;
    if width == 0 || height == 0 {
        return Err(DecodeError::Empty);
    }
    check_dimensions(width, height, limits)?;
    let stride = width as usize;
    let mut rgba = vec![0u8; stride * height as usize * 4];
    run_sixel(payload, limits, |x, y, color| {
        let offset = (y as usize * stride + x as usize) * 4;
        rgba[offset..offset + 4].copy_from_slice(&color);
    })?;
    Ok(DecodedImage {
        width,
        height,
        rgba,
    })
}

/// Runs the sixel state machine, reporting every painted pixel to `paint`.
///
/// Returns the painted extent (`width`, `height`) in pixels.
fn run_sixel<F>(
    payload: &[u8],
    limits: &DecodeLimits,
    mut paint: F,
) -> Result<(u32, u32), DecodeError>
where
    F: FnMut(u32, u32, [u8; 4]),
{
    let mut palette = default_sixel_palette();
    let mut color = 0usize;
    let mut x: u32 = 0;
    let mut band: u32 = 0;
    let mut width: u32 = 0;
    let mut height: u32 = 0;
    let mut repeat: Option<u32> = None;
    let mut pos = 0;

    while pos < payload.len() {
        let byte = payload[pos];
        match byte {
            b'"' => {
                pos = parse_raster_attributes(payload, pos + 1, limits)?;
                repeat = None;
            }
            b'#' => {
                pos = parse_sixel_color(payload, pos + 1, &mut palette, &mut color)?;
                repeat = None;
            }
            b'!' => {
                let (count, next) = parse_number_at(payload, pos + 1)?;
                if count == 0 {
                    return Err(DecodeError::Malformed("sixel repeat count"));
                }
                repeat = Some(count);
                pos = next;
            }
            b'$' => {
                x = 0;
                repeat = None;
                pos += 1;
            }
            b'-' => {
                band = band
                    .checked_add(1)
                    .ok_or(DecodeError::Malformed("sixel band overflow"))?;
                x = 0;
                repeat = None;
                pos += 1;
            }
            b'?'..=b'~' => {
                let bits = byte - b'?'; // six vertical pixels in one character
                let times = repeat.take().unwrap_or(1);
                for _ in 0..times {
                    for bit in 0..6u32 {
                        if bits & (1 << bit) == 0 {
                            continue;
                        }
                        let y = band
                            .checked_mul(6)
                            .and_then(|base| base.checked_add(bit))
                            .ok_or(DecodeError::Malformed("sixel band overflow"))?;
                        let extent_width = x
                            .checked_add(1)
                            .ok_or(DecodeError::Malformed("sixel column overflow"))?;
                        let extent_height = y
                            .checked_add(1)
                            .ok_or(DecodeError::Malformed("sixel row overflow"))?;
                        let extent_width = width.max(extent_width);
                        let extent_height = height.max(extent_height);
                        if extent_width > limits.max_dimension
                            || extent_height > limits.max_dimension
                            || u64::from(extent_width) * u64::from(extent_height)
                                > limits.max_pixels as u64
                        {
                            return Err(DecodeError::TooLarge {
                                width: extent_width,
                                height: extent_height,
                                max_dimension: limits.max_dimension,
                                max_pixels: limits.max_pixels,
                            });
                        }
                        width = extent_width;
                        height = extent_height;
                        paint(x, y, palette[color]);
                    }
                    x = x
                        .checked_add(1)
                        .ok_or(DecodeError::Malformed("sixel column overflow"))?;
                }
                pos += 1;
            }
            _ => {
                // Whitespace and unknown controls are not pixel data.
                repeat = None;
                pos += 1;
            }
        }
    }
    Ok((width, height))
}

/// Parses `"Pan;Pad;Ph;Pv` and checks a declared size against the limits.
fn parse_raster_attributes(
    payload: &[u8],
    mut pos: usize,
    limits: &DecodeLimits,
) -> Result<usize, DecodeError> {
    let mut values: [u32; 4] = [0; 4];
    let mut count = 0;
    while pos < payload.len() && payload[pos].is_ascii_digit() && count < values.len() {
        let (value, next) = parse_number_at(payload, pos)?;
        values[count] = value;
        count += 1;
        pos = next;
        if pos < payload.len() && payload[pos] == b';' {
            pos += 1;
        } else {
            break;
        }
    }
    if count >= 4 {
        let (width, height) = (values[2], values[3]);
        if width > limits.max_dimension
            || height > limits.max_dimension
            || u64::from(width) * u64::from(height) > limits.max_pixels as u64
        {
            return Err(DecodeError::TooLarge {
                width,
                height,
                max_dimension: limits.max_dimension,
                max_pixels: limits.max_pixels,
            });
        }
    }
    Ok(pos)
}

/// Parses `#Pc` (select) and `#Pc;2;Pr;Pg;Pb` (define RGB, components 0-100).
fn parse_sixel_color(
    payload: &[u8],
    pos: usize,
    palette: &mut [[u8; 4]; 256],
    color: &mut usize,
) -> Result<usize, DecodeError> {
    let (index, mut pos) = parse_number_at(payload, pos)?;
    if index > 255 {
        return Err(DecodeError::Malformed("sixel colour index"));
    }
    *color = index as usize;
    if pos >= payload.len() || payload[pos] != b';' {
        return Ok(pos);
    }
    // A definition: `;Pu;Px;Py;Pz`.
    let (space, next) = parse_number_at(payload, pos + 1)?;
    pos = next;
    match space {
        // HLS colour space is legal sixel but not implemented in this slice.
        1 => return Err(DecodeError::Unsupported),
        2 => {}
        _ => return Err(DecodeError::Malformed("sixel colour space")),
    }
    let mut components = [0u8; 3];
    for component in &mut components {
        if pos >= payload.len() || payload[pos] != b';' {
            return Err(DecodeError::Malformed("sixel colour definition"));
        }
        let (value, next) = parse_number_at(payload, pos + 1)?;
        *component = value.min(100) as u8;
        pos = next;
    }
    palette[*color] = [
        scale_percent(components[0]),
        scale_percent(components[1]),
        scale_percent(components[2]),
        0xff,
    ];
    Ok(pos)
}

// --- byte helpers ------------------------------------------------------------

fn split_once(bytes: &[u8], delimiter: u8) -> Option<(&[u8], &[u8])> {
    let index = bytes.iter().position(|&byte| byte == delimiter)?;
    Some((&bytes[..index], &bytes[index + 1..]))
}

/// Parses decimal digits at `pos`, returning the value and the position after it.
fn parse_number_at(bytes: &[u8], mut pos: usize) -> Result<(u32, usize), DecodeError> {
    let start = pos;
    let mut value: u32 = 0;
    while pos < bytes.len() && bytes[pos].is_ascii_digit() {
        value = value
            .checked_mul(10)
            .and_then(|value| value.checked_add(u32::from(bytes[pos] - b'0')))
            .ok_or(DecodeError::Malformed("number overflow"))?;
        pos += 1;
    }
    if pos == start {
        return Err(DecodeError::Malformed("number expected"));
    }
    Ok((value, pos))
}

/// Parses a whole field as a decimal number, rejecting trailing bytes.
fn parse_number(bytes: &[u8]) -> Result<(u32, usize), DecodeError> {
    let (value, pos) = parse_number_at(bytes, 0)?;
    if pos != bytes.len() {
        return Err(DecodeError::Malformed("trailing bytes in number"));
    }
    Ok((value, pos))
}

#[cfg(test)]
mod tests {
    use super::*;

    const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    fn b64_encode(bytes: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        for chunk in bytes.chunks(3) {
            let b0 = chunk[0] as u32;
            let b1 = *chunk.get(1).unwrap_or(&0) as u32;
            let b2 = *chunk.get(2).unwrap_or(&0) as u32;
            let n = (b0 << 16) | (b1 << 8) | b2;
            out.push(B64[(n >> 18) as usize & 63]);
            out.push(B64[(n >> 12) as usize & 63]);
            out.push(if chunk.len() > 1 {
                B64[(n >> 6) as usize & 63]
            } else {
                b'='
            });
            out.push(if chunk.len() > 2 {
                B64[n as usize & 63]
            } else {
                b'='
            });
        }
        out
    }

    fn png_bytes(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut out, width, height);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(rgba).unwrap();
            writer.finish().unwrap();
        }
        out
    }

    fn truncated_png(width: u32, height: u32) -> Vec<u8> {
        // Signature (8 bytes) plus the IHDR chunk (4 length + 4 type + 13 data + 4
        // CRC) is the whole header: no IDAT at all. If a decoder expanded before
        // checking the dimensions it would fail here with `Malformed`; the tests
        // rely on that difference.
        let png = png_bytes(width, height, &vec![0u8; (width * height * 4) as usize]);
        png[..33].to_vec()
    }

    fn message(protocol: ImageProtocol, payload: impl Into<Vec<u8>>) -> ImageMessage {
        ImageMessage {
            protocol,
            payload: payload.into(),
        }
    }

    fn kitty_png(png: &[u8]) -> ImageMessage {
        let mut payload = b"f=100;".to_vec();
        payload.extend_from_slice(&b64_encode(png));
        message(ImageProtocol::Kitty, payload)
    }

    fn iterm2_png(png: &[u8]) -> ImageMessage {
        let mut payload = b"inline=1;width=2;height=2;preserveAspectRatio=1:".to_vec();
        payload.extend_from_slice(&b64_encode(png));
        message(ImageProtocol::Iterm2, payload)
    }

    #[test]
    fn base64_handles_padding_line_breaks_and_invalid_alphabet() {
        assert_eq!(decode_base64(b"aGVsbG8=").unwrap(), b"hello");
        assert_eq!(decode_base64(b"YQ==").unwrap(), b"a");
        assert_eq!(decode_base64(b"YWI=").unwrap(), b"ab");
        assert_eq!(
            decode_base64(b"aGVs\nbG8=\r\n").unwrap(),
            b"hello",
            "line breaks inside the payload are ignored"
        );
        assert_eq!(decode_base64(b" aG Vs bG8= ").unwrap(), b"hello");
        assert_eq!(decode_base64(b"aGVs*bG8="), Err(DecodeError::Base64));
        assert_eq!(decode_base64(b"aGVsbG8"), Err(DecodeError::Base64));
        assert_eq!(decode_base64(b"YQ==YQ=="), Err(DecodeError::Base64));
        assert_eq!(decode_base64(b""), Err(DecodeError::Base64));
    }

    #[test]
    fn png_over_max_dimension_is_rejected_before_expanding() {
        let limits = DecodeLimits {
            max_dimension: 2,
            ..DecodeLimits::default()
        };
        let err = decode(&kitty_png(&truncated_png(4, 2)), &limits).unwrap_err();
        assert!(
            matches!(
                err,
                DecodeError::TooLarge {
                    width: 4,
                    height: 2,
                    ..
                }
            ),
            "expected TooLarge from the IHDR, got {err:?}"
        );
    }

    #[test]
    fn png_over_max_pixels_is_rejected() {
        let limits = DecodeLimits {
            max_pixels: 8,
            ..DecodeLimits::default()
        };
        let err = decode(&kitty_png(&truncated_png(4, 4)), &limits).unwrap_err();
        assert!(
            matches!(
                err,
                DecodeError::TooLarge {
                    width: 4,
                    height: 4,
                    ..
                }
            ),
            "expected TooLarge from the IHDR, got {err:?}"
        );
    }

    #[test]
    fn png_roundtrip_keeps_rgba_order() {
        let rgba = [
            255, 0, 0, 255, // red
            0, 255, 0, 255, // green
            0, 0, 255, 255, // blue
            10, 20, 30, 40, // translucent
        ];
        let image = decode(
            &iterm2_png(&png_bytes(2, 2, &rgba)),
            &DecodeLimits::default(),
        )
        .unwrap();
        assert_eq!((image.width, image.height), (2, 2));
        assert_eq!(image.rgba, rgba);
        assert_eq!(image.pixel(0, 0), Some([255, 0, 0, 255]));
        assert_eq!(image.pixel(1, 1), Some([10, 20, 30, 40]));
        assert_eq!(image.pixel(2, 0), None);
    }

    #[test]
    fn iterm2_inline_zero_is_unsupported() {
        let payload = b"inline=0;width=2;height=2:not-base64!!".to_vec();
        assert_eq!(
            decode(
                &message(ImageProtocol::Iterm2, payload),
                &DecodeLimits::default()
            ),
            Err(DecodeError::Unsupported)
        );
    }

    #[test]
    fn iterm2_inline_image_decodes_to_the_expected_pixels() {
        let rgba = [1, 2, 3, 255, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];
        let image = decode(
            &iterm2_png(&png_bytes(2, 2, &rgba)),
            &DecodeLimits::default(),
        )
        .unwrap();
        assert_eq!((image.width, image.height), (2, 2));
        assert_eq!(image.pixel(0, 0), Some([1, 2, 3, 255]));
        assert_eq!(image.pixel(1, 0), Some([4, 5, 6, 7]));
        assert_eq!(image.pixel(0, 1), Some([8, 9, 10, 11]));
        assert_eq!(image.pixel(1, 1), Some([12, 13, 14, 15]));
    }

    #[test]
    fn kitty_single_chunk_png_decodes_to_the_expected_pixels() {
        let rgba = [9, 8, 7, 255, 6, 5, 4, 3, 2, 1, 0, 255, 100, 110, 120, 130];
        let image = decode(
            &kitty_png(&png_bytes(2, 2, &rgba)),
            &DecodeLimits::default(),
        )
        .unwrap();
        assert_eq!((image.width, image.height), (2, 2));
        assert_eq!(image.pixel(0, 0), Some([9, 8, 7, 255]));
        assert_eq!(image.pixel(1, 1), Some([100, 110, 120, 130]));
    }

    #[test]
    fn kitty_wire_chunking_is_accepted_and_reassembled() {
        // This used to be refused with `Unsupported`; S1.1 makes it the legal way
        // to send a payload that does not fit in one APC.
        let limits = DecodeLimits::default();
        let mut decoder = ImageDecoder::new();
        // One 1x2 RGBA image (8 bytes, 12 base64 symbols) split in two APCs; the
        // second one repeats only the id, exactly as kitty sends it.
        let encoded = b64_encode(&[1, 2, 3, 4, 5, 6, 7, 8]);
        let mut first = b"i=3,f=32,s=1,v=2,m=1;".to_vec();
        first.extend_from_slice(&encoded[..6]);
        let mut last = b"i=3;".to_vec();
        last.extend_from_slice(&encoded[6..]);
        assert_eq!(
            decoder
                .decode(&message(ImageProtocol::Kitty, first), &limits)
                .unwrap(),
            None,
            "an m=1 chunk is not an image yet"
        );
        let image = decoder
            .decode(&message(ImageProtocol::Kitty, last), &limits)
            .unwrap()
            .expect("the final chunk completes the transfer");
        // `f=`, `s=` and `v=` came from the first chunk, which the last one does
        // not repeat.
        assert_eq!((image.width, image.height), (1, 2));
        assert_eq!(image.pixel(0, 0), Some([1, 2, 3, 4]));
        assert_eq!(image.pixel(0, 1), Some([5, 6, 7, 8]));
    }

    #[test]
    fn kitty_raw_rgb_is_expanded_to_rgba() {
        let mut payload = b"f=24,s=2,v=1;".to_vec();
        payload.extend_from_slice(&b64_encode(&[1, 2, 3, 4, 5, 6]));
        let image = decode(
            &message(ImageProtocol::Kitty, payload),
            &DecodeLimits::default(),
        )
        .unwrap();
        assert_eq!((image.width, image.height), (2, 1));
        assert_eq!(image.pixel(0, 0), Some([1, 2, 3, 255]));
        assert_eq!(image.pixel(1, 0), Some([4, 5, 6, 255]));
    }

    #[test]
    fn kitty_raw_rgba_keeps_the_alpha_channel() {
        let mut payload = b"f=32,s=1,v=2;".to_vec();
        payload.extend_from_slice(&b64_encode(&[1, 2, 3, 4, 5, 6, 7, 8]));
        let image = decode(
            &message(ImageProtocol::Kitty, payload),
            &DecodeLimits::default(),
        )
        .unwrap();
        assert_eq!((image.width, image.height), (1, 2));
        assert_eq!(image.pixel(0, 0), Some([1, 2, 3, 4]));
        assert_eq!(image.pixel(0, 1), Some([5, 6, 7, 8]));
    }

    #[test]
    fn sixel_colors_repeats_and_band_moves_paint_the_expected_pixels() {
        // Red across the first four columns, `$` back to column 0, blue over rows
        // 0 and 1, `-` to the next band and red again on columns 0 and 1.
        let payload = b"#0;2;100;0;0#0!4@$#1;2;0;0;100#1!4B-#0!2@".to_vec();
        let image = decode(
            &message(ImageProtocol::Sixel, payload),
            &DecodeLimits::default(),
        )
        .unwrap();
        assert_eq!((image.width, image.height), (4, 7));
        assert_eq!(
            image.pixel(0, 0),
            Some([0, 0, 255, 255]),
            "blue wins the second pass"
        );
        assert_eq!(image.pixel(3, 0), Some([0, 0, 255, 255]));
        assert_eq!(image.pixel(0, 1), Some([0, 0, 255, 255]));
        assert_eq!(image.pixel(3, 1), Some([0, 0, 255, 255]));
        assert_eq!(
            image.pixel(0, 2),
            Some([0, 0, 0, 0]),
            "background is transparent"
        );
        assert_eq!(image.pixel(2, 1), Some([0, 0, 255, 255]));
        assert_eq!(image.pixel(0, 6), Some([255, 0, 0, 255]));
        assert_eq!(image.pixel(1, 6), Some([255, 0, 0, 255]));
        assert_eq!(image.pixel(2, 6), Some([0, 0, 0, 0]));
    }

    #[test]
    fn sixel_over_max_pixels_is_refused() {
        let limits = DecodeLimits {
            max_pixels: 16,
            ..DecodeLimits::default()
        };
        let payload = b"#0;2;0;0;0#0!100~".to_vec();
        let err = decode(&message(ImageProtocol::Sixel, payload), &limits).unwrap_err();
        assert!(
            matches!(err, DecodeError::TooLarge { .. }),
            "expected TooLarge, got {err:?}"
        );
    }

    // --- kitty chunked transfers (S1.1 - S1.4) -------------------------------

    /// One kitty APC: `a=T,<extra>[,i=<id>][,m=<flag>];<base64 data>`.
    fn kitty_chunk(extra: &str, id: Option<u32>, more: Option<bool>, data: &[u8]) -> ImageMessage {
        let mut header = String::from("a=T");
        if !extra.is_empty() {
            header.push(',');
            header.push_str(extra);
        }
        if let Some(id) = id {
            header.push_str(&format!(",i={id}"));
        }
        match more {
            Some(true) => header.push_str(",m=1"),
            Some(false) => header.push_str(",m=0"),
            None => {}
        }
        let mut payload = header.into_bytes();
        payload.push(b';');
        payload.extend_from_slice(data);
        message(ImageProtocol::Kitty, payload)
    }

    /// Splits `data` into at most `count` even pieces, so one fixture can be sent
    /// over several chunks.
    fn split_evenly(data: &[u8], count: usize) -> Vec<Vec<u8>> {
        let size = data.len().div_ceil(count).max(1);
        data.chunks(size).map(|piece| piece.to_vec()).collect()
    }

    /// `count` chunks of one transfer: every one but the last carries `m=1`, the
    /// last carries `m=0`.
    fn chunked(extra: &str, id: u32, data: &[u8], count: usize) -> Vec<ImageMessage> {
        let pieces = split_evenly(data, count);
        let last = pieces.len() - 1;
        pieces
            .iter()
            .enumerate()
            .map(|(index, piece)| kitty_chunk(extra, Some(id), Some(index < last), piece))
            .collect()
    }

    /// Feeds `messages` in order and collects the images they produced.
    fn decode_all(
        decoder: &mut ImageDecoder,
        messages: &[ImageMessage],
        limits: &DecodeLimits,
    ) -> Vec<DecodedImage> {
        messages
            .iter()
            .filter_map(|message| decoder.decode(message, limits).expect("chunk accepted"))
            .collect()
    }

    #[test]
    fn a_kitty_png_split_in_two_chunks_decodes_like_a_single_chunk() {
        let rgba = [1, 2, 3, 255, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];
        let png = png_bytes(2, 2, &rgba);
        let limits = DecodeLimits::default();
        let whole = decode(&kitty_png(&png), &limits).unwrap();

        let mut decoder = ImageDecoder::new();
        let messages = chunked("f=100", 1, &b64_encode(&png), 2);
        let images = decode_all(&mut decoder, &messages, &limits);
        assert_eq!(images.len(), 1, "only the last chunk produces an image");
        assert_eq!(images[0], whole);
        assert_eq!(images[0].pixel(0, 0), Some([1, 2, 3, 255]));
        assert_eq!(images[0].pixel(1, 1), Some([12, 13, 14, 15]));
        assert_eq!(
            decoder.pending_transfers(),
            0,
            "the finished transfer frees its buffer"
        );
        assert_eq!(decoder.pending_bytes(), 0);
    }

    #[test]
    fn a_kitty_png_split_in_three_chunks_decodes_like_a_single_chunk() {
        let rgba = [3, 1, 4, 255, 1, 5, 9, 2, 6, 5, 3, 5, 9, 7, 9, 200];
        let png = png_bytes(2, 2, &rgba);
        let limits = DecodeLimits::default();
        let whole = decode(&kitty_png(&png), &limits).unwrap();

        let mut decoder = ImageDecoder::new();
        let messages = chunked("f=100", 2, &b64_encode(&png), 3);
        assert_eq!(messages.len(), 3);
        let images = decode_all(&mut decoder, &messages, &limits);
        assert_eq!(images.len(), 1);
        assert_eq!(images[0], whole);
        assert_eq!(images[0].pixel(1, 0), Some([1, 5, 9, 2]));
        assert_eq!(images[0].pixel(1, 1), Some([9, 7, 9, 200]));
        assert_eq!(decoder.pending_transfers(), 0);
    }

    #[test]
    fn two_interleaved_kitty_transfers_decode_separately() {
        let a = [1u8, 2, 3, 255, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];
        let b = [
            90u8, 91, 92, 93, 94, 95, 96, 97, 98, 99, 100, 101, 102, 103, 104, 105,
        ];
        let limits = DecodeLimits::default();
        let chunks_a = chunked("f=100", 1, &b64_encode(&png_bytes(2, 2, &a)), 2);
        let chunks_b = chunked("f=100", 2, &b64_encode(&png_bytes(2, 2, &b)), 2);

        let mut decoder = ImageDecoder::new();
        assert_eq!(decoder.decode(&chunks_a[0], &limits).unwrap(), None);
        assert_eq!(decoder.decode(&chunks_b[0], &limits).unwrap(), None);
        assert_eq!(decoder.pending_transfers(), 2, "both ids are in flight");

        let first = decoder
            .decode(&chunks_a[1], &limits)
            .unwrap()
            .expect("A finished");
        let second = decoder
            .decode(&chunks_b[1], &limits)
            .unwrap()
            .expect("B finished");
        assert_eq!((first.width, first.height), (2, 2));
        assert_eq!(first.pixel(0, 0), Some([1, 2, 3, 255]));
        assert_eq!(first.pixel(1, 1), Some([12, 13, 14, 15]));
        assert_eq!((second.width, second.height), (2, 2));
        assert_eq!(second.pixel(0, 0), Some([90, 91, 92, 93]));
        assert_eq!(second.pixel(1, 1), Some([102, 103, 104, 105]));
        assert_eq!(decoder.pending_transfers(), 0);
    }

    #[test]
    fn a_chunk_without_an_image_id_is_malformed() {
        let mut decoder = ImageDecoder::new();
        let message = kitty_chunk("f=100", None, Some(true), b"AAAA");
        assert_eq!(
            decoder.decode(&message, &DecodeLimits::default()),
            Err(DecodeError::Malformed(
                "kitty chunked transfer without an image id"
            ))
        );
        assert_eq!(
            decoder.pending_transfers(),
            0,
            "a refused chunk must not leave state behind"
        );
        assert_eq!(decoder.pending_bytes(), 0);
    }

    #[test]
    fn an_explicit_last_chunk_without_state_is_a_single_chunk_image() {
        // Kitty lets the sender spell `m=0` on a lone chunk. Refusing it would drop
        // images the previous decoder accepted, so it decodes as one chunk here.
        let limits = DecodeLimits::default();
        let mut decoder = ImageDecoder::new();
        let raw = [1u8, 2, 3, 4, 5, 6, 7, 8];
        let message = kitty_chunk("f=32,s=2,v=1", Some(7), Some(false), &b64_encode(&raw));
        let image = decoder
            .decode(&message, &limits)
            .unwrap()
            .expect("an image, not a chunk");
        assert_eq!((image.width, image.height), (2, 1));
        assert_eq!(decoder.pending_transfers(), 0);
    }

    #[test]
    fn a_transfer_over_the_per_transfer_bound_is_too_large() {
        let limits = DecodeLimits {
            max_input_bytes: 80,
            ..DecodeLimits::default()
        };
        let mut decoder = ImageDecoder::new();
        let data = vec![b'A'; 40];
        for _ in 0..2 {
            assert_eq!(
                decoder
                    .decode(&kitty_chunk("f=100", Some(1), Some(true), &data), &limits)
                    .unwrap(),
                None
            );
        }
        let err = decoder
            .decode(&kitty_chunk("f=100", Some(1), Some(true), &data), &limits)
            .unwrap_err();
        assert!(
            matches!(err, DecodeError::TooLarge { .. }),
            "expected TooLarge, got {err:?}"
        );
        assert_eq!(
            decoder.pending_transfers(),
            0,
            "the overrun transfer is discarded, not grown"
        );
        assert_eq!(decoder.pending_bytes(), 0);
    }

    #[test]
    fn a_continuation_chunk_never_evicts_another_transfer() {
        // Four transfers live is the bound, not a reason to evict when a chunk
        // simply continues one of them: the count is not growing.
        let limits = DecodeLimits::default();
        let mut decoder = ImageDecoder::new();
        let mut chunks = Vec::new();
        for id in 1..=4u32 {
            let raw = [id as u8, 2, 3, 4, 5, 6, 7, 8];
            chunks.push(chunked("f=32,s=2,v=1", id, &b64_encode(&raw), 2));
        }
        for chunk in &chunks {
            decoder.decode(&chunk[0], &limits).unwrap();
        }
        assert_eq!(decoder.pending_transfers(), MAX_IN_FLIGHT_TRANSFERS);
        assert_eq!(decoder.dropped(), 0);

        let image = decoder
            .decode(&chunks[0][1], &limits)
            .unwrap()
            .expect("the continued transfer finishes");
        assert_eq!((image.width, image.height), (2, 1));
        assert_eq!(decoder.dropped(), 0, "no sibling transfer was evicted");
        assert_eq!(decoder.pending_transfers(), 3);
    }

    #[test]
    fn the_oldest_transfer_is_evicted_so_the_other_ones_still_decode() {
        let limits = DecodeLimits::default();
        let mut decoder = ImageDecoder::new();
        // Each transfer is one 2x1 RGBA image (8 bytes, 12 base64 symbols) sent in
        // two chunks, so every id can be finished and checked afterwards.
        let mut finals = Vec::new();
        for id in 1..=5u32 {
            let raw = [id as u8, 2, 3, 4, 5, 6, 7, 8];
            let chunks = chunked("f=32,s=2,v=1", id, &b64_encode(&raw), 2);
            assert_eq!(
                decoder.decode(&chunks[0], &limits).unwrap(),
                None,
                "id {id}"
            );
            finals.push(chunks[1].clone());
        }
        assert_eq!(
            decoder.pending_transfers(),
            MAX_IN_FLIGHT_TRANSFERS,
            "only four transfers are held"
        );
        assert_eq!(
            decoder.dropped(),
            1,
            "the oldest transfer is evicted and counted"
        );

        // Id 1 lost its state. Its final chunk is now read as a standalone image
        // and, being half a frame, must not decode: no silent success either way.
        assert!(
            decoder.decode(&finals[0], &limits).is_err(),
            "half a frame must not decode as an image"
        );
        for (offset, final_chunk) in finals[1..].iter().enumerate() {
            let id = offset as u8 + 2;
            let image = decoder
                .decode(final_chunk, &limits)
                .unwrap()
                .unwrap_or_else(|| panic!("id {id} must still finish"));
            assert_eq!((image.width, image.height), (2, 1));
            assert_eq!(image.pixel(0, 0), Some([id, 2, 3, 4]));
            assert_eq!(image.pixel(1, 0), Some([5, 6, 7, 8]));
        }
        assert_eq!(decoder.pending_transfers(), 0);
    }

    #[test]
    fn the_aggregate_bound_evicts_the_oldest_transfer() {
        // A 5x5 RGBA image is 100 bytes, 136 base64 symbols. With a 200-byte
        // per-message bound the aggregate bound is 400 symbols: one nearly-full
        // transfer plus the first chunk of three others no longer fits.
        let limits = DecodeLimits {
            max_input_bytes: 200,
            ..DecodeLimits::default()
        };
        let raw: Vec<u8> = (0..100u8).collect();
        let encoded = b64_encode(&raw);
        assert_eq!(encoded.len(), 136);
        let body = |id: u32, more: bool, data: &[u8]| {
            kitty_chunk("f=32,s=5,v=5", Some(id), Some(more), data)
        };

        let mut decoder = ImageDecoder::new();
        // Id 1 holds 170 symbols of a payload that is never finished.
        let filler = vec![b'A'; 170];
        assert_eq!(
            decoder.decode(&body(1, true, &filler), &limits).unwrap(),
            None
        );
        // Ids 2 and 3 hold the first chunk of a real image each.
        for id in 2..=3u32 {
            assert_eq!(
                decoder
                    .decode(&body(id, true, &encoded[..100]), &limits)
                    .unwrap(),
                None
            );
        }
        assert_eq!(decoder.pending_bytes(), 370);

        // The fourth first-chunk pushes the total to 470, past the 400-symbol
        // aggregate bound, so the oldest transfer is evicted — not the newest.
        assert_eq!(
            decoder
                .decode(&body(4, true, &encoded[..100]), &limits)
                .unwrap(),
            None
        );
        assert_eq!(decoder.dropped(), 1);
        assert_eq!(decoder.pending_transfers(), 3);
        assert_eq!(decoder.pending_bytes(), 300);

        // The evicted id is gone; the ones that remain still finish normally.
        assert!(matches!(
            decoder.decode(&body(1, false, b"AAAA"), &limits),
            Err(DecodeError::Malformed(_))
        ));
        let image = decoder
            .decode(&body(2, false, &encoded[100..]), &limits)
            .unwrap()
            .expect("id 2 finished");
        assert_eq!((image.width, image.height), (5, 5));
        assert_eq!(image.pixel(0, 0), Some([0, 1, 2, 3]));
        assert_eq!(image.pixel(4, 4), Some([96, 97, 98, 99]));
    }

    #[test]
    fn a_lone_chunk_does_not_disturb_a_later_single_chunk_image() {
        let limits = DecodeLimits::default();
        let mut decoder = ImageDecoder::new();
        let rgba = [7, 8, 9, 255, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];
        let png = png_bytes(2, 2, &rgba);

        // A transfer that never finishes stays in flight; it must not be spliced
        // into the single-chunk image that arrives next.
        let stray = chunked("f=100", 4, &b64_encode(&png), 2);
        assert_eq!(decoder.decode(&stray[0], &limits).unwrap(), None);
        assert!(decoder.pending_bytes() > 0);

        let image = decoder
            .decode(&kitty_png(&png), &limits)
            .unwrap()
            .expect("a single-chunk image");
        assert_eq!(image.pixel(0, 0), Some([7, 8, 9, 255]));
        assert_eq!(image.pixel(1, 1), Some([9, 10, 11, 12]));
        assert_eq!(
            decoder.pending_transfers(),
            1,
            "the stray chunk is still in flight, untouched"
        );
        assert!(decoder.pending_bytes() > 0);
    }
}
