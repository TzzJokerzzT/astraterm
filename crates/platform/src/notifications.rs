//! Desktop notifications through `notify-send`.
//!
//! A notification title and body come from whatever runs in the terminal, which
//! includes a remote host reached over SSH, so the text is treated as untrusted
//! data: control characters are removed, whitespace is collapsed and both parts
//! are cut to a fixed character budget. Nothing is ever executed: the helper is
//! invoked with an argument vector, never through `sh -c`.
//!
//! A missing `notify-send` degrades silently. The terminal is still usable, so a
//! notification that cannot be delivered must not raise a dialog, retry in a
//! loop, or spam the log: the first failure is reported once at `warn` level.

use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Longest title kept, in characters.
pub const MAX_TITLE_CHARS: usize = 120;

/// Longest body kept, in characters.
pub const MAX_BODY_CHARS: usize = 1024;

/// Name the notification daemon attributes the notification to.
const APP_NAME: &str = "AstraTerm";

/// Cleans untrusted notification text for display.
///
/// Control characters (including newlines and escape bytes) become word
/// separators instead of being deleted, so `one\0two` does not turn into
/// `onetwo`; runs of whitespace collapse to a single space; the result is
/// trimmed and cut to `max_chars` characters, never bytes, so a multibyte
/// character is never split.
pub fn sanitize(text: &str, max_chars: usize) -> String {
    let mut cleaned = String::with_capacity(text.len().min(max_chars.saturating_mul(4)));
    let mut pending_space = false;
    for ch in text.chars() {
        if ch.is_control() || ch.is_whitespace() {
            pending_space = !cleaned.is_empty();
            continue;
        }
        if pending_space {
            cleaned.push(' ');
            pending_space = false;
        }
        cleaned.push(ch);
    }
    cleaned.truncate(
        cleaned
            .char_indices()
            .nth(max_chars)
            .map(|(index, _)| index)
            .unwrap_or_else(|| cleaned.len()),
    );
    cleaned.trim_end().to_owned()
}

/// Builds the argv for one desktop notification.
///
/// Pure, so its shape can be asserted without a desktop session: a program name
/// plus an argument vector, never a shell string. The text is sanitized here so
/// no caller can hand raw terminal bytes to the daemon.
pub fn notify_command(title: &str, body: &str) -> (&'static str, Vec<String>) {
    let title = match sanitize(title, MAX_TITLE_CHARS) {
        title if title.is_empty() => APP_NAME.to_owned(),
        title => title,
    };
    let body = sanitize(body, MAX_BODY_CHARS);
    // `--` ends option parsing: the title and body come from untrusted output, and
    // without it a message starting with `-` is read as a notify-send flag such as
    // `--urgency=critical` or `--wait`, spoofing the notification or parking a
    // child process until the user closes it.
    let mut args = vec![format!("--app-name={APP_NAME}"), "--".to_owned(), title];
    if !body.is_empty() {
        args.push(body);
    }
    ("notify-send", args)
}

/// Desktop notification service.
///
/// Cloned freely; the "already warned" flag is shared so a machine without
/// `notify-send` logs the failure once, not once per notification.
#[derive(Clone, Debug, Default)]
pub struct Notifications {
    warned: Arc<AtomicBool>,
}

impl Notifications {
    pub fn new() -> Self {
        Self::default()
    }

    /// Delivers one notification, or silently does nothing when the helper is
    /// missing.
    ///
    /// The helper is spawned and reaped on a short-lived thread, so a slow or
    /// hanging notification daemon never blocks the event loop.
    pub fn send(&self, title: &str, body: &str) {
        let (program, args) = notify_command(title, body);
        let spawned = Command::new(program)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        match spawned {
            Ok(mut child) => {
                let _ = std::thread::Builder::new()
                    .name("astraterm-notify".to_owned())
                    .spawn(move || {
                        let _ = child.wait();
                    });
            }
            Err(error) => {
                if !self.warned.swap(true, Ordering::Relaxed) {
                    tracing::warn!(
                        error = %error,
                        "desktop notifications are unavailable; terminal output is unaffected"
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_BODY_CHARS, MAX_TITLE_CHARS, Notifications, notify_command, sanitize};

    #[test]
    fn control_characters_are_removed_and_whitespace_is_collapsed() {
        assert_eq!(sanitize("hello\n\tworld", 64), "hello world");
        assert_eq!(sanitize("  padded   text  ", 64), "padded text");
        // A control character separates words instead of merging them.
        assert_eq!(sanitize("one\u{0}two\u{1b}three", 64), "one two three");
        // A body of nothing but control characters sanitizes to nothing.
        assert_eq!(sanitize("\u{7}\u{1b}\n\t", 64), "");
    }

    #[test]
    fn both_parts_are_cut_to_their_character_budget() {
        assert_eq!(sanitize("abcdef", 3), "abc");
        assert_eq!(sanitize("abcdef", 0), "");
        assert_eq!(
            sanitize(&"x".repeat(MAX_TITLE_CHARS + 40), MAX_TITLE_CHARS)
                .chars()
                .count(),
            MAX_TITLE_CHARS
        );
        assert_eq!(
            sanitize(&"y".repeat(MAX_BODY_CHARS + 40), MAX_BODY_CHARS)
                .chars()
                .count(),
            MAX_BODY_CHARS
        );
        // A cut through a run of spaces must not leave a trailing space behind.
        assert_eq!(sanitize("ab   cd", 3), "ab");
    }

    #[test]
    fn multibyte_characters_are_never_split() {
        // Two bytes per character in UTF-8: cutting by bytes would be invalid.
        let accented = "é".repeat(MAX_TITLE_CHARS + 10);
        let cleaned = sanitize(&accented, MAX_TITLE_CHARS);
        assert_eq!(cleaned.chars().count(), MAX_TITLE_CHARS);
        assert_eq!(cleaned, "é".repeat(MAX_TITLE_CHARS));

        // Four bytes per character, and an emoji counts as one character.
        let emoji = "🚀".repeat(10);
        let cleaned = sanitize(&emoji, 3);
        assert_eq!(cleaned.chars().count(), 3);
        assert_eq!(cleaned, "🚀🚀🚀".to_owned());
    }

    #[test]
    fn an_option_like_title_is_still_a_message() {
        // The title and body come from terminal output. Without the `--` terminator a
        // message starting with `-` is parsed as a notify-send flag.
        let (_, args) = notify_command("--urgency=critical", "--wait");
        let terminator = args
            .iter()
            .position(|arg| arg == "--")
            .expect("the argv ends option parsing");
        let message: Vec<&str> = args[terminator + 1..].iter().map(String::as_str).collect();
        assert_eq!(message, ["--urgency=critical", "--wait"]);
        assert!(
            args[..terminator]
                .iter()
                .all(|arg| arg.starts_with("--app-name=")),
            "only our own options may precede the terminator: {args:?}"
        );
    }

    #[test]
    fn the_helper_is_invoked_with_an_argument_vector() {
        let (program, args) = notify_command("Build", "done in 4s");
        assert_eq!(program, "notify-send");
        assert_eq!(
            args.first().map(String::as_str),
            Some("--app-name=AstraTerm"),
            "the notification is attributed to the terminal"
        );
        assert!(args.contains(&"Build".to_owned()), "{args:?}");
        assert!(args.contains(&"done in 4s".to_owned()), "{args:?}");
        assert!(
            args.iter().all(|arg| !arg.contains('\0')),
            "no argument may carry a NUL byte"
        );

        // An empty summary falls back to the application name, and an empty body
        // is left out instead of being sent as one empty argument.
        let (_, args) = notify_command("", "   ");
        assert_eq!(args, vec!["--app-name=AstraTerm", "--", "AstraTerm"]);

        // The sanitizer runs before the argv is built, so nothing control-ish
        // reaches the notification daemon.
        let (_, args) = notify_command("Ti\ntle", "Bo\u{7}dy");
        assert_eq!(args.last().map(String::as_str), Some("Bo dy"));
    }

    #[test]
    fn sending_without_a_helper_degrades_silently() {
        // No desktop session in a test runner: this must never panic, block or
        // report failure to the caller.
        let notifications = Notifications::default();
        notifications.send("Build", "done in 4s");
    }
}
