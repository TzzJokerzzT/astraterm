//! Paste handling: the clipboard is read off the event loop and the payload bounded.
//!
//! A read waits for the selection owner to answer, which blocks for as long as that
//! owner takes. That wait belongs on a worker thread, never on the event loop, and the
//! result comes back as a `UserEvent`. Text is encoded here; an image arrives already
//! written to a temporary file by the platform, so what is encoded for it is the path.

use std::path::Path;

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
    /// The pasted image's path cannot be sent as text.
    PathNotUtf8,
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
            Self::PathNotUtf8 => "the pasted image path is not valid UTF-8".to_owned(),
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

/// Encodes the path of a pasted image so the terminal receives it as if it were typed.
///
/// The image itself stays in its temporary file: what the shell gets is the path, which
/// any program can consume. The encoding is the text one — same normalisation, same
/// bracketed-paste wrapping — so a path arrives exactly like a typed one.
pub fn encode_pasted_image(path: &Path, modes: ModeFlags) -> Result<Vec<u8>, PasteError> {
    let text = path.to_str().ok_or(PasteError::PathNotUtf8)?;
    encode_pasted_text(text, modes)
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

    #[test]
    fn an_image_path_is_inserted_as_plain_text() {
        let path = Path::new("/tmp/astraterm-paste-4242-0-17.png");
        let bytes = encode_pasted_image(path, ModeFlags::default()).unwrap();
        assert_eq!(bytes, b"/tmp/astraterm-paste-4242-0-17.png");
    }

    #[test]
    fn an_image_path_follows_the_bracketed_paste_setting() {
        let path = Path::new("/tmp/astraterm-paste-4242-1-42.png");
        let bytes = encode_pasted_image(path, bracketed()).unwrap();
        assert_eq!(
            bytes,
            b"\x1b[200~/tmp/astraterm-paste-4242-1-42.png\x1b[201~"
        );
    }

    #[test]
    fn an_image_path_that_is_not_utf8_is_refused() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let path = Path::new(OsStr::from_bytes(b"/tmp/astraterm-paste-\xff.png"));
        assert_eq!(
            encode_pasted_image(path, ModeFlags::default()),
            Err(PasteError::PathNotUtf8)
        );
        assert_eq!(
            PasteError::PathNotUtf8.message(),
            "the pasted image path is not valid UTF-8"
        );
    }
}
