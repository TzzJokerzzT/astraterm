//! The audible bell (S6, `[terminal] bell = "audible"`).
//!
//! Ringing the speaker is the one bell mode that leaves the program: it runs another
//! program on this machine, and every one of those can misbehave. Two rules follow
//! from the clipboard's lesson, where opening the native handle on the UI thread
//! could stall a frame:
//!
//! * **detection never runs anything.** Resolving a backend only looks at `PATH` and
//!   at the sound files the desktop ships, which are `stat` calls an unavailable
//!   sound server cannot block. A machine with no backend is known to have none
//!   before the first bell arrives, so the caller can degrade to the visual bell
//!   instead of discovering it mid-frame;
//! * **a bell never runs on the UI thread.** The helper is spawned and reaped by one
//!   worker thread fed through a one-slot channel: a ringing bell is coalesced with
//!   the next one instead of queueing a process per `BEL`, and a helper that waits
//!   for a sound server can only ever occupy that thread.
//!
//! `canberra-gtk-play` is preferred because it asks the sound theme for its own bell
//! and needs no file; `paplay` and `pw-play` play one of the freedesktop sounds. The
//! argument vector is explicit and never goes through `sh -c`, so nothing in the
//! configuration or in the terminal's output can reach a shell.

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::thread;

/// Sound files the sample players can play, in preference order.
///
/// They are the freedesktop sound theme's bell, then its "complete" cue, then the
/// plain ALSA sample: the first that exists wins.
const SOUND_FILES: &[&str] = &[
    "/usr/share/sounds/freedesktop/stereo/bell.oga",
    "/usr/share/sounds/freedesktop/stereo/complete.oga",
    "/usr/share/sounds/alsa/Front_Center.wav",
];

/// Program that asks the sound theme for a bell cue.
const CANBERRA: &str = "canberra-gtk-play";

/// Sample players, in preference order, for [`SOUND_FILES`].
const PLAYERS: &[&str] = &["paplay", "pw-play"];

/// One way to ring the speaker: a helper program and the argument vector it takes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Backend {
    pub program: String,
    pub args: Vec<String>,
}

/// Resolves the helper to use from what exists on this machine.
///
/// `exists` answers whether a path or a program name is available; a name is looked
/// up in `PATH` and an absolute path is checked on disk (see [`in_path`]). The
/// function is pure in that answer, so the preference order and the "no backend"
/// result are testable without a sound server, a display or a helper installed.
pub fn plan(mut exists: impl FnMut(&str) -> bool) -> Option<Backend> {
    if exists(CANBERRA) {
        return Some(Backend {
            program: CANBERRA.to_owned(),
            args: vec!["-i".to_owned(), "bell".to_owned()],
        });
    }
    let sound = SOUND_FILES.iter().copied().find(|path| exists(path))?;
    let program = PLAYERS.iter().copied().find(|player| exists(player))?;
    Some(Backend {
        program: program.to_owned(),
        args: vec![sound.to_owned()],
    })
}

/// Whether `program` is an executable file this process could run.
///
/// A bare name is looked up in `PATH` and an absolute path is checked directly, the
/// same split `execvp` makes. Only `stat` calls are involved: nothing is executed, so
/// a missing or wedged sound server cannot delay the caller.
pub fn in_path(program: &str) -> bool {
    if program.contains('/') {
        return is_executable(Path::new(program));
    }
    std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path)
            .map(|directory| directory.join(program))
            .any(|candidate| is_executable(&candidate))
    })
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    path.metadata()
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

/// The audible bell, rung off the UI thread.
pub struct Bell {
    ring: Option<SyncSender<()>>,
}

impl Bell {
    /// Resolves the backend on this machine and starts the worker that rings it.
    ///
    /// Detection is `stat`-only, so this cannot block on a sound server. A machine
    /// with no backend, or a worker thread that could not be started, leaves
    /// [`Bell::ring`] answering `false`, which is the caller's cue to keep the
    /// visual bell.
    pub fn detect() -> Self {
        let backend = plan(|candidate| {
            if candidate.starts_with('/') {
                is_executable(Path::new(candidate))
            } else {
                in_path(candidate)
            }
        });
        let Some(backend) = backend else {
            tracing::debug!("no audible bell backend on this machine");
            return Self { ring: None };
        };
        // One slot: a bell that arrives while the previous helper is still running is
        // the same bell as far as the user is concerned.
        let (tx, rx) = mpsc::sync_channel::<()>(1);
        let worker = backend.clone();
        match thread::Builder::new()
            .name("astraterm-bell".to_owned())
            .spawn(move || ring_loop(worker, rx))
        {
            Ok(_) => {
                tracing::debug!(program = %backend.program, "audible bell backend resolved");
                Self { ring: Some(tx) }
            }
            Err(error) => {
                tracing::warn!(error = %error, "the bell worker could not start");
                Self { ring: None }
            }
        }
    }

    /// Whether the speaker can be rung at all.
    pub fn available(&self) -> bool {
        self.ring.is_some()
    }

    /// Asks for one ring.
    ///
    /// Returns `false` only when no backend exists: the caller then answers the bell
    /// the visual way instead of dropping it. `true` means a helper will run, either
    /// for this bell or for the one already queued — a queue of bells is one sound.
    pub fn ring(&self) -> bool {
        let Some(ring) = &self.ring else {
            return false;
        };
        match ring.try_send(()) {
            Ok(()) | Err(TrySendError::Full(())) => true,
            Err(TrySendError::Disconnected(())) => false,
        }
    }
}

/// Rings the speaker until the application drops its bell.
///
/// Waiting for the helper is deliberate and confined here: a sound server that never
/// answers can only park this thread, never a frame.
fn ring_loop(backend: Backend, rx: Receiver<()>) {
    while rx.recv().is_ok() {
        let spawned = Command::new(&backend.program)
            .args(&backend.args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        match spawned {
            Ok(mut child) => {
                let _ = child.wait();
            }
            Err(error) => tracing::debug!(
                program = %backend.program,
                error = %error,
                "the audible bell helper could not run"
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sound_theme_helper_is_preferred_when_it_exists() {
        let backend = plan(|candidate| candidate == CANBERRA || candidate == "paplay")
            .expect("canberra is available");
        assert_eq!(backend.program, CANBERRA);
        assert_eq!(backend.args, ["-i", "bell"]);
    }

    #[test]
    fn a_sample_player_plays_the_first_sound_file_that_exists() {
        // No canberra, two sample files on disk and only the second player installed:
        // the player that exists plays the first file that does.
        let backend = plan(|candidate| {
            candidate == "pw-play" || candidate == SOUND_FILES[0] || candidate == SOUND_FILES[2]
        })
        .expect("a player and a sound file are available");
        assert_eq!(backend.program, "pw-play");
        assert_eq!(backend.args, [SOUND_FILES[0]]);
    }

    #[test]
    fn the_backend_is_only_resolved_from_what_exists() {
        assert_eq!(
            plan(|_| false),
            None,
            "a machine with no helper and no sound file rings nothing, and says so"
        );
        // A player without a file it can play cannot ring either.
        assert_eq!(plan(|candidate| candidate == "paplay"), None);
        // A file without a player that can play it is the same.
        assert_eq!(plan(|candidate| candidate == SOUND_FILES[0]), None);
    }

    #[test]
    fn the_argument_vector_is_explicit_and_never_a_shell_string() {
        let backend = plan(|candidate| candidate == "paplay" || candidate == SOUND_FILES[0])
            .expect("a sample player exists");
        assert_eq!(backend.program, "paplay");
        assert_eq!(backend.args, [SOUND_FILES[0]]);
        assert!(
            !backend.program.contains(' '),
            "the program is one name, never a command line"
        );
        assert!(
            backend
                .args
                .iter()
                .all(|arg| !arg.contains('\'') && !arg.contains(';')),
            "no shell metacharacters are involved, because no shell is"
        );
    }

    #[test]
    fn a_path_with_no_executable_bit_is_not_a_backend() {
        // The test binary's own path exists and is executable; `/` does not name a file.
        assert!(!is_executable(Path::new("/")));
        assert!(!in_path("astraterm-no-such-helper-for-this-test"));
    }
}
