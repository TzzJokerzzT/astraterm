//! Clipboard access through the session's native tools.
//!
//! Wayland sessions use `wl-copy`/`wl-paste`; X11 sessions fall back to
//! `xclip`/`xsel`. Both are invoked with an argument vector and their output is
//! read from stdin/stdout, so no shell is involved and no clipboard content is
//! ever logged.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

/// How long one copy waits for its helper before leaving it to finish on its own.
///
/// `wl-copy` forks and exits in milliseconds and `xclip` may hold the selection in the
/// foreground for as long as it owns it, so a copy that waited for the process to end
/// would wait forever. The grace period is long enough for a helper to accept the text
/// and short enough that a newer selection is not held back by an older helper.
const HELPER_GRACE: Duration = Duration::from_millis(250);

/// Reported when the worker thread never started, which is the only reason a queued
/// write can be refused.
const WORKER_UNAVAILABLE: &str = "the clipboard worker is not running";

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
    /// Blocks the calling thread for at most [`HELPER_GRACE`]: a helper that is still
    /// running then is left to a detached reaper thread and the copy is reported as
    /// done, because a helper that keeps running usually holds the selection. Use
    /// [`Clipboard::writer`] anywhere a frame must not wait at all.
    ///
    /// `Err` says whether the helper is missing or it ran and failed, and carries
    /// the helper's own words when there are any.
    pub fn set(&self, selection: Selection, text: &str) -> Result<(), String> {
        let Some((program, args)) = self.set_command(selection) else {
            return Err(unavailable_message());
        };
        run_helper(self.helper(program, args), program, text, HELPER_GRACE)
    }

    /// A writer that runs the helpers on a background thread.
    ///
    /// This is the path for anything the UI thread touches: the spawn, the stdin write,
    /// the stdin close and the reap all happen on the worker, so a `wl-copy` or `xclip`
    /// that never returns cannot freeze a frame. With no backend at all the writer
    /// refuses every copy with the same message [`Clipboard::set`] would return.
    pub fn writer(&self) -> ClipboardWriter {
        if !self.is_available() {
            return ClipboardWriter::refusing(unavailable_message());
        }
        let clipboard = self.clone();
        ClipboardWriter::start(Arc::new(move |selection, text| {
            clipboard.set(selection, text)
        }))
    }

    /// Reads the requested selection.
    ///
    /// `Err` carries a message that distinguishes "no helper installed" from "the
    /// helper ran and failed": blaming the installation for a runtime failure sends
    /// people to install something they already have.
    ///
    /// This waits for the helper to exit and its output to arrive, so a huge clipboard
    /// costs the reading thread that whole duration. That thread is never the UI thread
    /// — see `Application::paste` — and no selection gesture reaches this path.
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

/// A copy queued for the worker thread.
struct Job {
    selection: Selection,
    text: String,
    done: Option<Done>,
}

/// Called once with the outcome of the helper that ran the copy.
type Done = Box<dyn FnOnce(Result<(), String>) + Send + 'static>;

/// Runs one helper and returns its outcome; injected in tests so no real clipboard
/// helper is ever run and so a stuck helper can be simulated exactly.
type Runner = dyn Fn(Selection, &str) -> Result<(), String> + Send + Sync + 'static;

/// State shared with the worker thread.
struct Shared {
    /// The newest write that has not started yet. One slot, not a queue: a burst of
    /// selections replaces the pending write instead of building a backlog of helpers.
    pending: Mutex<Option<Job>>,
    ready: Condvar,
    runner: Arc<Runner>,
    /// Set when this writer can never run a helper; every copy is refused with it.
    refusal: Option<String>,
}

/// Writes to the clipboard from a single background thread.
///
/// Cloning is cheap: every clone shares the one worker and its one pending slot.
#[derive(Clone)]
pub struct ClipboardWriter {
    shared: Arc<Shared>,
}

impl std::fmt::Debug for ClipboardWriter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("ClipboardWriter").finish()
    }
}

impl ClipboardWriter {
    fn start(runner: Arc<Runner>) -> Self {
        let shared = Arc::new(Shared {
            pending: Mutex::new(None),
            ready: Condvar::new(),
            runner,
            refusal: None,
        });
        let worker = Arc::clone(&shared);
        let spawned = std::thread::Builder::new()
            .name("astraterm-clipboard".to_owned())
            .spawn(move || work(&worker));
        if spawned.is_err() {
            return Self::refusing(WORKER_UNAVAILABLE.to_owned());
        }
        Self { shared }
    }

    /// A writer that starts no worker: every copy is refused with `refusal`.
    ///
    /// This is how a machine without any clipboard helper keeps reporting that instead
    /// of silently accepting copies nobody can read.
    fn refusing(refusal: String) -> Self {
        let never_used = refusal.clone();
        Self {
            shared: Arc::new(Shared {
                pending: Mutex::new(None),
                ready: Condvar::new(),
                // No copy ever reaches this runner: the refusal is returned first.
                runner: Arc::new(move |_: Selection, _: &str| Err(never_used.clone()))
                    as Arc<Runner>,
                refusal: Some(refusal),
            }),
        }
    }

    /// Queues `text` and returns without waiting for the helper.
    ///
    /// A write that has not started yet is replaced, so two selections in quick
    /// succession leave the clipboard holding the second one.
    pub fn set(&self, selection: Selection, text: &str) -> Result<(), String> {
        self.set_reported(selection, text, |_| {})
    }

    /// Queues `text` and reports the helper's outcome to `done` from the worker.
    ///
    /// A queued write that a newer one replaces never reports: it never ran.
    pub fn set_reported(
        &self,
        selection: Selection,
        text: &str,
        done: impl FnOnce(Result<(), String>) + Send + 'static,
    ) -> Result<(), String> {
        if let Some(refusal) = &self.shared.refusal {
            return Err(refusal.clone());
        }
        {
            let mut slot = lock(&self.shared.pending);
            *slot = Some(Job {
                selection,
                text: text.to_owned(),
                done: Some(Box::new(done)),
            });
        }
        self.shared.ready.notify_one();
        Ok(())
    }
}

/// The worker loop: take the newest queued write, run it, report it, repeat.
fn work(shared: &Shared) {
    loop {
        let job = next_job(shared);
        let result = (shared.runner)(job.selection, &job.text);
        if let Some(done) = job.done {
            done(result);
        }
    }
}

/// Waits for a queued write. Never exits, so the worker outlives every copy.
fn next_job(shared: &Shared) -> Job {
    let mut slot = lock(&shared.pending);
    loop {
        if let Some(job) = slot.take() {
            return job;
        }
        slot = shared
            .ready
            .wait(slot)
            .unwrap_or_else(|error| error.into_inner());
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}

/// The helper's own verdict, or the news that it is still running.
enum Completion {
    /// The helper exited and left this outcome behind.
    Exited(Result<(), String>),
    /// The helper outlived the grace period and was left running, detached.
    StillRunning,
}

/// Runs a helper: spawn, feed the text, close its stdin and reap it.
///
/// Closing stdin is what makes both helpers publish the payload. The wait is bounded —
/// a helper still alive after `grace` is handed to a detached thread that reaps it
/// whenever it finally exits — so a helper that never returns cannot hold up the next
/// copy.
fn run_helper(
    mut command: Command,
    program: &'static str,
    text: &str,
    grace: Duration,
) -> Result<(), String> {
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let Ok(mut child) = command.spawn() else {
        return Err(format!("could not run {program}"));
    };
    // A helper that died before reading the payload still has to be reaped, so the
    // send failure is remembered and reported only after the child is accounted for.
    let send_failed = match child.stdin.take() {
        Some(mut stdin) => stdin.write_all(text.as_bytes()).is_err(),
        None => true,
    };
    let (sender, receiver) = mpsc::channel();
    // The waiter owns the child from here on: it reaps it even if this copy already
    // stopped waiting, which is what keeps a slow helper from becoming a zombie.
    let waiting = std::thread::Builder::new()
        .name("astraterm-clipboard-reap".to_owned())
        .spawn(move || {
            let outcome = child.wait_with_output();
            let _ = sender.send(outcome);
        });
    if waiting.is_err() {
        return Err(format!("could not wait for {program}"));
    }
    let completion = match receiver.recv_timeout(grace) {
        Ok(Ok(output)) => {
            if send_failed {
                return Err(format!("could not send the text to {program}"));
            }
            if output.status.success() {
                Completion::Exited(Ok(()))
            } else {
                Completion::Exited(Err(describe_failure(program, &output.stderr)))
            }
        }
        Ok(Err(_)) | Err(RecvTimeoutError::Disconnected) => {
            Completion::Exited(Err(format!("could not wait for {program}")))
        }
        Err(RecvTimeoutError::Timeout) => Completion::StillRunning,
    };
    match completion {
        // A helper that keeps running holds the selection, which is what a copy wants.
        Completion::StillRunning => Ok(()),
        Completion::Exited(result) => result,
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
    use std::sync::{Arc, Condvar, Mutex, mpsc};
    use std::time::Duration;

    use super::*;

    /// A rendezvous that lets a test hold a fake helper open for as long as it needs,
    /// so "the caller did not wait" is proven by control flow and never by timing.
    #[derive(Default)]
    struct Gate {
        open: Mutex<bool>,
        changed: Condvar,
    }

    impl Gate {
        fn wait(&self) {
            let mut open = lock(&self.open);
            while !*open {
                open = self
                    .changed
                    .wait(open)
                    .unwrap_or_else(|error| error.into_inner());
            }
        }

        fn open(&self) {
            *lock(&self.open) = true;
            self.changed.notify_all();
        }
    }

    /// A fake helper: records every copy it is asked to run and reports them.
    #[derive(Clone, Default)]
    struct Fake {
        started: Arc<Mutex<Vec<String>>>,
    }

    impl Fake {
        fn started(&self) -> Vec<String> {
            lock(&self.started).clone()
        }

        /// A runner that records the text and then blocks on `gate`.
        fn gated(&self, gate: Arc<Gate>, signal: mpsc::Sender<String>) -> Arc<Runner> {
            let started = Arc::clone(&self.started);
            Arc::new(move |_selection, text: &str| {
                lock(&started).push(text.to_owned());
                let _ = signal.send(text.to_owned());
                gate.wait();
                Ok(())
            })
        }
    }

    fn receive<T>(receiver: &mpsc::Receiver<T>, what: &str) -> T {
        receiver
            .recv_timeout(Duration::from_secs(10))
            .unwrap_or_else(|error| panic!("the worker never reported {what}: {error}"))
    }

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
        assert!(
            clipboard
                .writer()
                .set(Selection::Clipboard, "text")
                .is_err()
        );
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

    #[test]
    fn queueing_a_copy_returns_before_the_helper_finishes() {
        // The reporter's `wl-copy` never returns; a copy that waited for it froze the
        // whole window. The caller must get past both enqueues while the helper is
        // still blocked, which the gate proves without measuring time.
        let fake = Fake::default();
        let gate = Arc::new(Gate::default());
        let (signal, signalled) = mpsc::channel();
        let writer = ClipboardWriter::start(fake.gated(Arc::clone(&gate), signal));

        assert!(writer.set(Selection::Clipboard, "first").is_ok());
        assert_eq!(receive(&signalled, "the first copy"), "first");
        // The helper is still running here, and this returns regardless.
        assert!(writer.set(Selection::Clipboard, "second").is_ok());
        assert_eq!(fake.started(), vec!["first".to_owned()]);

        gate.open();
        assert_eq!(receive(&signalled, "the second copy"), "second");
        assert_eq!(
            fake.started(),
            vec!["first".to_owned(), "second".to_owned()],
            "the write that had to wait still ran, after the one in flight"
        );
    }

    #[test]
    fn a_newer_copy_replaces_the_pending_one_instead_of_queueing_it() {
        // A drag can report several selections while one helper is running. The slot
        // holds only the newest, so the clipboard ends up holding that one and no
        // backlog of helpers is ever started.
        let fake = Fake::default();
        let gate = Arc::new(Gate::default());
        let (signal, signalled) = mpsc::channel();
        let writer = ClipboardWriter::start(fake.gated(Arc::clone(&gate), signal));

        assert!(writer.set(Selection::Clipboard, "first").is_ok());
        assert_eq!(receive(&signalled, "the first copy"), "first");
        assert!(writer.set(Selection::Clipboard, "superseded").is_ok());
        assert!(writer.set(Selection::Clipboard, "last").is_ok());

        gate.open();
        assert_eq!(receive(&signalled, "the last copy"), "last");
        assert_eq!(
            fake.started(),
            vec!["first".to_owned(), "last".to_owned()],
            "the superseded copy never started"
        );
    }

    #[test]
    fn an_explicit_copy_is_never_deduped() {
        // `ctrl+shift+c` twice on the same selection must copy twice: the "unchanged
        // selection" rule belongs to the copy-on-select gesture, not to the write path,
        // so the same text reaching this writer twice really runs the helper twice.
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let recorded = Arc::clone(&seen);
        let writer = ClipboardWriter::start(Arc::new(move |_selection, text: &str| {
            lock(&recorded).push(text.to_owned());
            Ok(())
        }));

        for _ in 0..2 {
            let (report, reported) = mpsc::channel();
            assert!(
                writer
                    .set_reported(Selection::Clipboard, "same text", move |result| {
                        let _ = report.send(result);
                    })
                    .is_ok()
            );
            assert!(receive(&reported, "the explicit copy").is_ok());
        }

        assert_eq!(*lock(&seen), vec!["same text".to_owned(); 2]);
    }

    #[test]
    fn a_helper_that_fails_does_not_panic_nor_lose_the_next_write() {
        let attempts = Arc::new(Mutex::new(0_u32));
        let counter = Arc::clone(&attempts);
        let writer = ClipboardWriter::start(Arc::new(move |_selection, text: &str| {
            let mut count = lock(&counter);
            *count += 1;
            if text == "first" {
                return Err("wl-copy: no seat".to_owned());
            }
            Ok(())
        }));

        let (report, reported) = mpsc::channel();
        assert!(
            writer
                .set_reported(Selection::Clipboard, "first", move |result| {
                    let _ = report.send(result);
                })
                .is_ok()
        );
        assert_eq!(
            receive(&reported, "the failed copy").unwrap_err(),
            "wl-copy: no seat"
        );

        let (report, reported) = mpsc::channel();
        assert!(
            writer
                .set_reported(Selection::Clipboard, "second", move |result| {
                    let _ = report.send(result);
                })
                .is_ok()
        );
        assert!(
            receive(&reported, "the copy after the failure").is_ok(),
            "the worker survives a helper failure"
        );
        assert_eq!(*lock(&attempts), 2);
    }

    #[test]
    fn a_helper_that_exits_non_zero_is_reported_and_the_caller_returns() {
        // A real child, reaped on the worker: this is the shape of an `xclip` that
        // reports its own failure.
        let result = run_helper(
            shell("printf boom >&2; exit 3"),
            "sh",
            "text",
            Duration::from_secs(10),
        );
        assert_eq!(result.unwrap_err(), "sh: boom");
    }

    #[test]
    fn a_helper_that_never_exits_is_left_running_instead_of_waited_for() {
        // The reported freeze: the helper accepts the text and never returns. The copy
        // gives up after the grace period, detaches the waiter that reaps it and lets
        // the worker move on to the next selection.
        let started = std::time::Instant::now();
        let result = run_helper(
            shell("exec sleep 5"),
            "sh",
            "text",
            Duration::from_millis(50),
        );
        assert!(
            result.is_ok(),
            "a helper that keeps running still owns the text"
        );
        assert!(
            started.elapsed() < Duration::from_secs(4),
            "the copy returned long before the helper exited"
        );
    }

    #[test]
    fn a_helper_that_dies_before_reading_is_reported_and_reaped() {
        // Enough text that a child which exits without reading cannot swallow it: the
        // write fails, and the child is still waited for rather than left as a zombie.
        let text = "x".repeat(1024 * 1024);
        let result = run_helper(shell("exit 0"), "sh", &text, Duration::from_secs(10));
        assert_eq!(result.unwrap_err(), "could not send the text to sh");
    }

    /// A child that runs one shell command.
    fn shell(script: &str) -> Command {
        let mut command = Command::new("/bin/sh");
        command.arg("-c").arg(script);
        command
    }
}
