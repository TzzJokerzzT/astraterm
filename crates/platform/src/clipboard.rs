//! Clipboard access through the session's native tools.
//!
//! Wayland sessions use `wl-copy`/`wl-paste`; X11 sessions fall back to
//! `xclip`/`xsel`. Both are invoked with an argument vector and their output is
//! read from stdin/stdout, so no shell is involved and no clipboard content is
//! ever logged.

use std::io::Write;
use std::process::{Command, Stdio};

/// Which selection to use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Selection {
    /// `Ctrl+C`/`Ctrl+V` clipboard.
    Clipboard,
    /// X11/Wayland primary selection (copy-on-select target).
    Primary,
}

/// Clipboard backend detected at startup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    WlClipboard,
    Xclip,
    Xsel,
    /// No helper available: copy/paste is a no-op that reports failure.
    Unavailable,
}

/// Clipboard service.
#[derive(Clone, Debug)]
pub struct Clipboard {
    backend: Backend,
    wayland: bool,
}

impl Clipboard {
    /// Detects the available backend.
    pub fn detect() -> Self {
        let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some_and(|value| !value.is_empty());
        let has = |program: &str| which(program).is_some();

        let backend = if wayland && has("wl-copy") && has("wl-paste") {
            Backend::WlClipboard
        } else if has("xclip") {
            Backend::Xclip
        } else if has("xsel") {
            Backend::Xsel
        } else if has("wl-copy") && has("wl-paste") {
            Backend::WlClipboard
        } else {
            Backend::Unavailable
        };
        Self { backend, wayland }
    }

    pub fn backend(&self) -> Backend {
        self.backend
    }

    pub fn is_available(&self) -> bool {
        self.backend != Backend::Unavailable
    }

    /// Whether the session is Wayland (used to decide about primary selection).
    pub fn is_wayland(&self) -> bool {
        self.wayland
    }

    /// Copies `text` to the requested selection. Returns whether it worked.
    pub fn set(&self, selection: Selection, text: &str) -> bool {
        let Some((program, args)) = self.set_command(selection) else {
            return false;
        };
        let child = Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        let Ok(mut child) = child else { return false };
        if let Some(stdin) = child.stdin.as_mut()
            && stdin.write_all(text.as_bytes()).is_err()
        {
            return false;
        }
        // `wl-copy` forks and keeps the selection alive; waiting is still correct.
        child.wait().map(|status| status.success()).unwrap_or(false)
    }

    /// Reads the requested selection.
    pub fn get(&self, selection: Selection) -> Option<String> {
        let (program, args) = self.get_command(selection)?;
        let output = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        String::from_utf8(output.stdout).ok()
    }

    fn set_command(&self, selection: Selection) -> Option<(&'static str, Vec<&'static str>)> {
        match self.backend {
            Backend::WlClipboard => {
                let mut args = Vec::new();
                if selection == Selection::Primary {
                    args.push("--primary");
                }
                Some(("wl-copy", args))
            }
            Backend::Xclip => {
                let selection = if selection == Selection::Primary {
                    "primary"
                } else {
                    "clipboard"
                };
                Some(("xclip", vec!["-selection", selection]))
            }
            Backend::Xsel => {
                let flag = if selection == Selection::Primary {
                    "--primary"
                } else {
                    "--clipboard"
                };
                Some(("xsel", vec![flag, "--input"]))
            }
            Backend::Unavailable => None,
        }
    }

    fn get_command(&self, selection: Selection) -> Option<(&'static str, Vec<&'static str>)> {
        match self.backend {
            Backend::WlClipboard => {
                let mut args = vec!["--no-newline"];
                if selection == Selection::Primary {
                    args.push("--primary");
                }
                Some(("wl-paste", args))
            }
            Backend::Xclip => {
                let selection = if selection == Selection::Primary {
                    "primary"
                } else {
                    "clipboard"
                };
                Some(("xclip", vec!["-selection", selection, "-o"]))
            }
            Backend::Xsel => {
                let flag = if selection == Selection::Primary {
                    "--primary"
                } else {
                    "--clipboard"
                };
                Some(("xsel", vec![flag, "--output"]))
            }
            Backend::Unavailable => None,
        }
    }
}

/// Looks a program up in `PATH`.
fn which(program: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detection_never_panics() {
        let clipboard = Clipboard::detect();
        // Either a backend is available or the service reports unavailability.
        assert!(clipboard.is_available() || clipboard.backend() == Backend::Unavailable);
    }

    #[test]
    fn unavailable_backend_reports_failure_instead_of_panicking() {
        let clipboard = Clipboard {
            backend: Backend::Unavailable,
            wayland: false,
        };
        assert!(!clipboard.set(Selection::Clipboard, "text"));
        assert!(clipboard.get(Selection::Clipboard).is_none());
    }

    #[test]
    fn commands_are_argument_vectors_without_a_shell() {
        let wayland = Clipboard {
            backend: Backend::WlClipboard,
            wayland: true,
        };
        let (program, args) = wayland.set_command(Selection::Primary).unwrap();
        assert_eq!(program, "wl-copy");
        assert_eq!(args, vec!["--primary"]);
        assert!(!program.contains(' '), "no shell string is ever built");
    }
}
