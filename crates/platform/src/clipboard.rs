//! Clipboard access through the session's native tools.
//!
//! Wayland sessions use `wl-copy`/`wl-paste`; X11 sessions fall back to
//! `xclip`/`xsel`. Both are invoked with an argument vector and their output is
//! read from stdin/stdout, so no shell is involved and no clipboard content is
//! ever logged.

use std::io::Write;
use std::path::{Path, PathBuf};
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

    /// Copies `text` to the requested selection.
    ///
    /// `Err` says whether the helper is missing or it ran and failed, and carries
    /// the helper's own words when there are any.
    pub fn set(&self, selection: Selection, text: &str) -> Result<(), String> {
        let Some((program, args)) = self.set_command(selection) else {
            return Err(unavailable_message());
        };
        let mut command = self.helper(program, args);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let Ok(mut child) = command.spawn() else {
            return Err(format!("could not run {program}"));
        };
        if let Some(mut stdin) = child.stdin.take()
            && stdin.write_all(text.as_bytes()).is_err()
        {
            return Err(format!("could not send the text to {program}"));
        }
        // `wl-copy` forks and keeps the selection alive; waiting is still correct.
        match child.wait_with_output() {
            Ok(output) if output.status.success() => Ok(()),
            Ok(output) => Err(describe_failure(program, &output.stderr)),
            Err(_) => Err(format!("could not wait for {program}")),
        }
    }

    /// Reads the requested selection.
    ///
    /// `Err` carries a message that distinguishes "no helper installed" from "the
    /// helper ran and failed": blaming the installation for a runtime failure sends
    /// people to install something they already have.
    pub fn get(&self, selection: Selection) -> Result<String, String> {
        let Some((program, args)) = self.get_command(selection) else {
            return Err(unavailable_message());
        };
        let mut command = self.helper(program, args);
        command.stdin(Stdio::null()).stderr(Stdio::piped());
        let output = command
            .output()
            .map_err(|_| format!("could not run {program}"))?;
        if !output.status.success() {
            return Err(describe_failure(program, &output.stderr));
        }
        String::from_utf8(output.stdout)
            .map_err(|_| format!("{program} returned bytes that are not valid UTF-8"))
    }

    /// A helper command whose environment cannot poison it.
    ///
    /// `wl-copy` and `xclip` create temporary files of their own, and they fail with
    /// `mkdtemp: No such file or directory` when `TMPDIR` names a directory that has
    /// gone away — which is exactly what a terminal that outlives its session temp
    /// directory hands down. Dropping the variable lets the helper fall back to /tmp.
    fn helper(&self, program: &'static str, args: Vec<&'static str>) -> Command {
        let mut command = Command::new(program);
        command.args(args);
        match usable_tmpdir(std::env::var_os("TMPDIR").map(PathBuf::from)) {
            Some(dir) => {
                command.env("TMPDIR", dir);
            }
            None => {
                command.env_remove("TMPDIR");
            }
        }
        command
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
/// Message for a machine with no clipboard helper at all.
fn unavailable_message() -> String {
    "clipboard is unavailable (install wl-clipboard or xclip)".to_owned()
}

/// A helper failure, quoting the helper when it said anything.
fn describe_failure(program: &str, stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let text = text.trim();
    if text.is_empty() {
        format!("{program} failed without a message")
    } else {
        format!("{program}: {text}")
    }
}

/// `TMPDIR` only when it still points at a directory.
fn usable_tmpdir(raw: Option<PathBuf>) -> Option<PathBuf> {
    usable_tmpdir_with(raw, |path| path.is_dir())
}

/// The decision, with the filesystem check injected so it can be tested.
fn usable_tmpdir_with(raw: Option<PathBuf>, is_dir: impl Fn(&Path) -> bool) -> Option<PathBuf> {
    raw.filter(|dir| is_dir(dir))
}

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
        assert!(clipboard.set(Selection::Clipboard, "text").is_err());
        assert!(clipboard.get(Selection::Clipboard).is_err());
    }

    #[test]
    fn a_tmpdir_that_no_longer_exists_is_dropped() {
        // `wl-copy` reports `mkdtemp: No such file or directory` and exits 1 when
        // TMPDIR names a directory that has gone away, so a terminal that outlives
        // its session temp directory would fail every copy until restarted.
        let missing = PathBuf::from("/tmp/gone-with-the-session");
        assert_eq!(usable_tmpdir_with(Some(missing), |_| false), None);
        assert_eq!(usable_tmpdir_with(None, |_| true), None);

        let present = PathBuf::from("/tmp");
        assert_eq!(
            usable_tmpdir_with(Some(present.clone()), |_| true),
            Some(present),
            "a real directory is passed through"
        );
    }

    #[test]
    fn a_failure_quotes_the_helper() {
        assert_eq!(
            describe_failure("wl-copy", b"mkdtemp: No such file or directory\n"),
            "wl-copy: mkdtemp: No such file or directory"
        );
        assert_eq!(
            describe_failure("wl-copy", b"   "),
            "wl-copy failed without a message"
        );
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
