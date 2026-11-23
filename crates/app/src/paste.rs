//! Paste handling: the clipboard is read off the event loop and the payload bounded.
//!
//! Reading a selection spawns `wl-paste`/`xclip` and waits for it, which blocks for
//! as long as the selection owner takes to answer. That wait belongs on a worker
//! thread, never on the event loop, and the result comes back as a `UserEvent`.

use astraterm_terminal::input::encode_paste;
use astraterm_terminal::snapshot::ModeFlags;

/// Largest clipboard payload a paste will send, in bytes.
pub const MAX_PASTE_BYTES: usize = 8 * 1024 * 1024;

/// Why a paste was refused before reaching the PTY.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PasteError {
    /// The clipboard was empty; there is nothing to send.
    Empty,
    /// The payload is larger than `MAX_PASTE_BYTES`.
    TooLarge { limit: usize },
}

impl PasteError {
    /// Human-facing message for the status bar.
    pub fn message(self) -> String {
        match self {
            Self::Empty => "nothing to paste".to_owned(),
            Self::TooLarge { limit } => format!(
                "paste is larger than {} MiB; copy a smaller range",
                limit / (1024 * 1024)
            ),
        }
    }
}

/// Encodes pasted text for the PTY, refusing empty and oversized payloads.
///
/// Normalisation and bracketed-paste wrapping stay in `encode_paste` so a paste
/// sends exactly what a typed prompt would.
pub fn encode_pasted_text(text: &str, modes: ModeFlags) -> Result<Vec<u8>, PasteError> {
    if text.is_empty() {
        return Err(PasteError::Empty);
    }
    if text.len() > MAX_PASTE_BYTES {
        return Err(PasteError::TooLarge {
            limit: MAX_PASTE_BYTES,
        });
    }
    Ok(encode_paste(text, modes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bracketed() -> ModeFlags {
        ModeFlags {
            bracketed_paste: true,
            ..ModeFlags::default()
        }
    }

    #[test]
    fn plain_text_is_encoded_as_is() {
        let bytes = encode_pasted_text("echo hola", ModeFlags::default()).unwrap();
        assert_eq!(bytes, b"echo hola");
    }

    #[test]
    fn bracketed_paste_is_still_wrapped() {
        let bytes = encode_pasted_text("ls -la", bracketed()).unwrap();
        assert_eq!(bytes, b"\x1b[200~ls -la\x1b[201~");
    }

    #[test]
    fn empty_paste_is_refused() {
        assert_eq!(
            encode_pasted_text("", ModeFlags::default()),
            Err(PasteError::Empty)
        );
    }

    #[test]
    fn a_paste_at_the_limit_is_accepted() {
        let text = "a".repeat(MAX_PASTE_BYTES);
        assert!(encode_pasted_text(&text, ModeFlags::default()).is_ok());
    }

    #[test]
    fn a_paste_over_the_limit_is_refused_with_the_limit_in_the_message() {
        let text = "a".repeat(MAX_PASTE_BYTES + 1);
        assert_eq!(
            encode_pasted_text(&text, ModeFlags::default()),
            Err(PasteError::TooLarge {
                limit: MAX_PASTE_BYTES
            })
        );
        let message = PasteError::TooLarge {
            limit: MAX_PASTE_BYTES,
        }
        .message();
        assert!(message.contains("8 MiB"), "message was {message}");
    }

    #[test]
    fn multiline_and_control_bytes_survive_the_limit_check() {
        let text = "linea uno\rlinea dos\nlinea tres\ttab";
        let bytes = encode_pasted_text(text, ModeFlags::default()).unwrap();
        assert_eq!(bytes, b"linea uno\rlinea dos\rlinea tres\ttab");
    }
}
