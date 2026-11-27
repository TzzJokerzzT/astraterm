//! Session manager: PTY lifecycle, output pumping and per-session state.
//!
//! Responsibilities:
//!
//! * create and destroy PTY-backed processes;
//! * bind a session to a pane identifier supplied by the workspace;
//! * pump output into the terminal engine with a per-session byte budget;
//! * translate emulator events (title, bell, clipboard, colour queries) into
//!   session events, and answer the PTY when the protocol requires it;
//! * report child exit exactly once and clean up deterministically.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use astraterm_core::{ExitReason, PaneId, ProfileId, SessionEvent, SessionId};
use astraterm_pty::{PtyError, PtyHandle, Waker, spawn_with_waker};
use astraterm_terminal::engine::TerminalEvent;
use astraterm_terminal::{SearchState, Snapshot, TerminalEngine};

use crate::lifecycle::SessionSpec;

/// Maximum bytes fed into one session's engine per pump pass.
///
/// Together with the bounded PTY queue this is what keeps a `yes` flood from
/// making the UI unresponsive: one pass does a bounded amount of parsing work and
/// the remaining bytes stay queued with backpressure applied to the child.
///
/// A pass overshoots by at most one read chunk ([`astraterm_pty::READ_CHUNK_BYTES`])
/// because the budget is checked before each chunk, not inside it.
pub const PUMP_BUDGET_BYTES: usize = 256 * 1024;

/// Maximum chunks read from one session per pump pass, to keep fairness between
/// panes even when chunks are small.
pub const PUMP_BUDGET_CHUNKS: usize = 512;

/// How often the session refreshes its cached working directory.
const CWD_REFRESH_INTERVAL: Duration = Duration::from_millis(500);

/// Shared grace period for shutting down every session on exit.
const SHUTDOWN_GRACE: Duration = Duration::from_millis(400);

/// Errors raised while creating a session.
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("could not start the session: {0}")]
    Spawn(#[from] PtyError),
    #[error("the session budget for this window is exhausted")]
    TooManySessions,
}

/// One running terminal session.
pub struct Session {
    pub id: SessionId,
    pub pane: PaneId,
    pub profile: Option<ProfileId>,
    pub engine: TerminalEngine,
    pub search: SearchState,
    /// Title set by the application through OSC 0/2.
    pub title: Option<String>,
    /// Label from the profile, used before a title exists.
    pub label: Option<String>,
    /// Cached working directory, refreshed from `/proc/<pid>/cwd`.
    pub cwd: Option<PathBuf>,
    pub child_pid: Option<u32>,
    pub exit: Option<ExitReason>,
    /// Set when the session produced output or changed state since the last pump.
    pub dirty: bool,
    /// Set when the bell rang and the pane is not focused.
    pub bell: bool,
    private_pty: Option<PtyHandle>,
    /// Winsize the PTY last accepted.
    ///
    /// `None` means the last attempt did not land, so the next `sync_sizes` has to
    /// try again even when the emulation grid already matches (S5).
    pty_size: Option<(u16, u16)>,
    /// Test seam: makes [`Session::resize`] report a winsize the PTY refused, which
    /// is otherwise a race with the PTY writer thread.
    #[cfg(test)]
    reject_winsize: bool,
    last_cwd_check: Instant,
    /// Bytes dropped by the input queue (should stay zero; reported for honesty).
    pub input_drops: u64,
    /// Wake callback used by PTY threads (kept so a session can be re-wired).
    waker: Option<Waker>,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("id", &self.id)
            .field("pane", &self.pane)
            .field("title", &self.title)
            .field("exit", &self.exit)
            .finish_non_exhaustive()
    }
}

impl Session {
    /// Sends bytes to the child process.
    pub fn write(&mut self, bytes: &[u8]) {
        let Some(pty) = self.private_pty.as_ref() else {
            return;
        };
        if !pty.write(bytes) {
            // Queue full or PTY gone. Count it and warn once per session so a dropped
            // keystroke is visible instead of silently lost.
            self.input_drops += 1;
            if self.input_drops == 1 {
                tracing::warn!(
                    session = %self.id,
                    "input queue is full; keystrokes may be dropped"
                );
            }
        }
    }

    /// Sends bytes, waiting briefly for room. Used for protocol replies.
    pub fn write_urgent(&mut self, bytes: &[u8]) {
        if let Some(pty) = self.private_pty.as_ref() {
            let _ = pty.write_blocking(bytes);
        }
    }

    /// Tells the PTY about the new grid size.
    ///
    /// The engine always follows the grid, and the PTY is asked on every call
    /// because the previous attempt may not have landed: the engine matching the
    /// grid is no evidence that the child's winsize does (S5).
    ///
    /// Returns whether the PTY took the size. A refused winsize is not remembered as
    /// applied, so the next call with the same target retries it.
    pub fn resize(&mut self, cols: u16, rows: u16, cell_width: u16, cell_height: u16) -> bool {
        self.engine.resize(cols, rows);
        self.dirty = true;
        if self.apply_winsize(cols, rows, cell_width, cell_height) {
            self.pty_size = Some((cols, rows));
            true
        } else {
            self.pty_size = None;
            tracing::warn!(
                session = %self.id,
                cols,
                rows,
                "the PTY did not take the winsize; the child may still see an older size"
            );
            false
        }
    }

    /// Whether the PTY has been told exactly this grid.
    ///
    /// `sync_sizes` asks this before deciding a session is already at the right size,
    /// so a PTY that fell behind the grid is retried instead of being skipped for the
    /// rest of the session's life.
    pub fn pty_matches(&self, cols: u16, rows: u16) -> bool {
        self.pty_size == Some((cols, rows))
    }

    /// Hands the winsize to the PTY, reporting whether it was taken.
    fn apply_winsize(&self, cols: u16, rows: u16, cell_width: u16, cell_height: u16) -> bool {
        #[cfg(test)]
        if self.reject_winsize {
            return false;
        }
        match self.private_pty.as_ref() {
            Some(pty) => pty.resize(cols, rows, cell_width, cell_height),
            None => false,
        }
    }

    /// Takes a snapshot of the visible screen.
    pub fn snapshot(&mut self) -> Snapshot {
        self.dirty = false;
        self.engine.snapshot()
    }

    /// Whether the session still has a running child.
    pub fn is_running(&self) -> bool {
        self.exit.is_none()
    }

    /// Refreshes the cached working directory from `/proc`.
    ///
    /// Reading `/proc/<pid>/cwd` is cheap and needs no extra dependency; it is
    /// throttled so the event loop never reads it more than twice per second.
    pub fn refresh_cwd(&mut self) {
        if self.last_cwd_check.elapsed() < CWD_REFRESH_INTERVAL {
            return;
        }
        self.last_cwd_check = Instant::now();
        let Some(pid) = self.child_pid else { return };
        if let Ok(path) = std::fs::read_link(format!("/proc/{pid}/cwd"))
            && self.cwd.as_deref() != Some(path.as_path())
        {
            self.cwd = Some(path);
            self.dirty = true;
        }
    }

    /// The text of the current selection, if any.
    pub fn selection_text(&self) -> Option<String> {
        self.engine.selection_text()
    }

    /// Clears the selection.
    pub fn clear_selection(&mut self) {
        self.engine.clear_selection();
    }

    /// Asks the PTY to close and returns the exit reason when known.
    pub fn shutdown(&mut self) -> Option<ExitReason> {
        let reason = self.private_pty.as_mut().and_then(PtyHandle::shutdown);
        if self.exit.is_none() {
            self.exit = reason.clone().or(Some(ExitReason::Unknown));
        }
        reason
    }

    /// Kills the child immediately.
    pub fn kill(&mut self) {
        if let Some(pty) = self.private_pty.as_mut() {
            pty.kill();
        }
    }

    /// Pumps queued output into the engine, respecting the byte budget.
    ///
    /// Returns the number of bytes processed. Emulator events are collected into
    /// `events` so the caller can route them (clipboard, title, bell…).
    fn pump_output(
        &mut self,
        events: &mut Vec<SessionEvent>,
        clipboard: &mut Vec<String>,
        replies: &mut Vec<(SessionId, Vec<u8>)>,
        budget: &mut usize,
    ) -> usize {
        let Some(pty) = self.private_pty.as_ref() else {
            return 0;
        };
        let mut processed = 0usize;
        let mut chunks = 0usize;

        while chunks < PUMP_BUDGET_CHUNKS && processed < *budget {
            let Some(chunk) = pty.try_recv_output() else {
                break;
            };
            chunks += 1;
            processed += chunk.len();
            // Borrow the chunk's buffer for the parse; it goes back to the reader when
            // the chunk is dropped at the end of this iteration.
            self.engine.process(chunk.as_slice());
            self.dirty = true;
        }
        // Saturating: a chunk may overshoot the budget by at most one read, and the
        // exact count is reported through the return value.
        *budget = budget.saturating_sub(processed);

        for event in self.engine.drain_events() {
            match event {
                TerminalEvent::Title(title) => {
                    self.title = Some(title.clone());
                    events.push(SessionEvent::Title {
                        session: self.id,
                        title: Some(title),
                    });
                }
                TerminalEvent::ResetTitle => {
                    self.title = None;
                    events.push(SessionEvent::Title {
                        session: self.id,
                        title: None,
                    });
                }
                TerminalEvent::Bell => {
                    self.bell = true;
                    events.push(SessionEvent::Bell { session: self.id });
                }
                TerminalEvent::Notification { title, body } => {
                    events.push(SessionEvent::Notification {
                        session: self.id,
                        title,
                        body,
                    });
                }
                TerminalEvent::ClipboardStore(text) => clipboard.push(text),
                TerminalEvent::ClipboardLoad => clipboard.push(String::new()),
                TerminalEvent::PtyReply(bytes) => replies.push((self.id, bytes)),
                TerminalEvent::ColorQuery(index) => {
                    // Colour queries need the active theme, which the application
                    // owns; it answers through `SessionManager::reply`.
                    events.push(SessionEvent::ColorQuery {
                        session: self.id,
                        index,
                    });
                }
                TerminalEvent::Wakeup | TerminalEvent::Exit => {}
            }
        }

        if processed > 0 {
            events.push(SessionEvent::Output { session: self.id });
        }
        processed
    }

    /// Asks the PTY to close without blocking the caller.
    pub fn request_shutdown(&self) {
        if let Some(pty) = self.private_pty.as_ref() {
            pty.request_shutdown();
        }
    }

    /// Kills the child's process group. Used after the shared grace period.
    pub fn kill_hard(&mut self) {
        if let Some(pty) = self.private_pty.as_ref() {
            pty.kill_hard();
        }
    }

    /// Whether the child is still alive.
    ///
    /// Combines the exit status observed through the waiter thread with a live
    /// `/proc` check, so a child that exited during a grace period is not treated as
    /// alive.
    pub fn child_alive(&self) -> bool {
        if self.exit.is_some() {
            return false;
        }
        match self.private_pty.as_ref() {
            Some(pty) => pty.child_is_alive(),
            None => false,
        }
    }

    /// Reaps a pending exit status so a killed child does not stay a zombie.
    pub fn take_exit(&mut self) -> Option<ExitReason> {
        let reason = self
            .private_pty
            .as_mut()
            .and_then(|pty| pty.try_recv_exit());
        if let Some(reason) = &reason {
            self.exit = Some(reason.clone());
        }
        reason
    }
}

/// What one pump pass did, so the app can decide whether a redraw is needed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PumpReport {
    pub sessions_dirty: usize,
    pub bytes_processed: usize,
    pub exits: usize,
    /// Clipboard writes requested by applications (OSC 52 is off by default, so
    /// this stays empty unless the user enabled it).
    pub clipboard_stores: Vec<String>,
    /// Bytes that must be written back to a session's PTY.
    pub replies: Vec<(SessionId, Vec<u8>)>,
}

/// Owns every live session.
#[derive(Default)]
pub struct SessionManager {
    sessions: BTreeMap<SessionId, Session>,
    events: Vec<SessionEvent>,
    max_sessions: usize,
    waker: Option<Waker>,
}

/// The manager also owns the wake callback used by PTY threads, so output never
/// relies on the event loop polling.
#[derive(Default)]
pub struct SessionManagerConfig {
    pub waker: Option<Waker>,
    pub max_sessions: usize,
}

impl SessionManager {
    pub fn new() -> Self {
        Self {
            sessions: BTreeMap::new(),
            events: Vec::new(),
            max_sessions: 64,
            waker: None,
        }
    }

    /// Number of sessions currently tracked.
    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    /// Sets the callback invoked when any session produces output or exits.
    pub fn set_waker(&mut self, waker: Waker) {
        self.waker = Some(waker);
        for session in self.sessions.values_mut() {
            session.waker = self.waker.clone();
        }
    }

    /// Creates a session for `pane` from `spec`.
    pub fn create(
        &mut self,
        pane: PaneId,
        spec: &SessionSpec,
        cols: u16,
        rows: u16,
    ) -> Result<SessionId, SessionError> {
        if self.sessions.len() >= self.max_sessions {
            return Err(SessionError::TooManySessions);
        }
        let cols = cols.max(1);
        let rows = rows.max(1);
        let engine = TerminalEngine::new(spec.engine_options(cols, rows));
        // The creation grid is the one the app paints, so it also decides the winsize
        // the child starts with; the spec's own size is not authoritative (S5).
        let mut pty_spec = spec.pty.clone();
        pty_spec.cols = cols;
        pty_spec.rows = rows;
        let pty = spawn_with_waker(&pty_spec, self.waker.clone())?;
        let id = SessionId::next();
        let child_pid = pty.child_pid();
        let session = Session {
            id,
            pane,
            profile: spec.profile,
            engine,
            search: SearchState::default(),
            title: None,
            label: spec.label.clone(),
            cwd: spec.pty.cwd.clone(),
            child_pid,
            exit: None,
            dirty: true,
            bell: false,
            private_pty: Some(pty),
            pty_size: Some((cols, rows)),
            #[cfg(test)]
            reject_winsize: false,
            last_cwd_check: Instant::now(),
            input_drops: 0,
            waker: self.waker.clone(),
        };
        self.sessions.insert(id, session);

        if let Some(input) = &spec.startup_input {
            // Startup commands are typed into the shell, never interpreted by an
            // intermediate `sh -c`.
            let mut bytes = input.clone().into_bytes();
            bytes.push(b'\r');
            if let Some(session) = self.sessions.get_mut(&id) {
                session.write_urgent(&bytes);
            }
        }
        Ok(id)
    }

    pub fn get(&self, id: SessionId) -> Option<&Session> {
        self.sessions.get(&id)
    }

    pub fn get_mut(&mut self, id: SessionId) -> Option<&mut Session> {
        self.sessions.get_mut(&id)
    }

    /// Finds the session bound to a pane.
    pub fn for_pane(&self, pane: PaneId) -> Option<&Session> {
        self.sessions.values().find(|session| session.pane == pane)
    }

    pub fn for_pane_mut(&mut self, pane: PaneId) -> Option<&mut Session> {
        self.sessions
            .values_mut()
            .find(|session| session.pane == pane)
    }

    /// Removes and shuts down a session.
    pub fn close(&mut self, id: SessionId) -> Option<ExitReason> {
        let mut session = self.sessions.remove(&id)?;
        let reason = session.shutdown();
        Some(reason.unwrap_or(ExitReason::Unknown))
    }

    /// Sends bytes to a session by identifier.
    pub fn write(&mut self, id: SessionId, bytes: &[u8]) {
        if let Some(session) = self.sessions.get_mut(&id) {
            session.write(bytes);
        }
    }

    /// Answers a PTY query (for example a colour reply).
    pub fn reply(&mut self, id: SessionId, bytes: &[u8]) {
        if let Some(session) = self.sessions.get_mut(&id) {
            session.write_urgent(bytes);
        }
    }

    /// Pumps every session once, with a global fairness budget split per session.
    ///
    /// The per-session budget is `PUMP_BUDGET_BYTES`; the global ceiling prevents a
    /// pathological pass from freezing the event loop when many panes are busy.
    pub fn pump(&mut self) -> PumpReport {
        let mut report = PumpReport::default();
        let mut events = Vec::new();
        let mut clipboard = Vec::new();
        let mut replies = Vec::new();

        for session in self.sessions.values_mut() {
            let mut budget = PUMP_BUDGET_BYTES;
            let processed =
                session.pump_output(&mut events, &mut clipboard, &mut replies, &mut budget);
            if session.dirty {
                report.sessions_dirty += 1;
            }
            report.bytes_processed += processed;

            if session.exit.is_none()
                && let Some(pty) = session.private_pty.as_mut()
                && let Some(reason) = pty.try_recv_exit()
            {
                session.exit = Some(reason.clone());
                events.push(SessionEvent::Exited {
                    session: session.id,
                    reason,
                });
                report.exits += 1;
            }
        }

        report.clipboard_stores = clipboard;
        report.replies = replies;
        self.events.extend(events);
        report
    }

    /// Drains pending session events (output, title, bell, exit).
    pub fn drain_events(&mut self) -> Vec<SessionEvent> {
        std::mem::take(&mut self.events)
    }

    /// Shuts every session down. Used on application exit so no child is orphaned.
    ///
    /// One shared grace period for all sessions, then `SIGKILL` for anything that
    /// ignored the polite close. This is bounded and cannot leak a child that traps
    /// SIGHUP, and it does not cost one grace period per session.
    pub fn shutdown_all(&mut self) {
        let mut sessions: Vec<Session> = std::mem::take(&mut self.sessions).into_values().collect();
        if sessions.is_empty() {
            return;
        }
        for session in &sessions {
            session.request_shutdown();
        }

        let deadline = Instant::now() + SHUTDOWN_GRACE;
        loop {
            // Reap pending statuses first: `child_alive` alone would report a child
            // that already exited as alive and escalate it to a hard kill.
            let mut alive = false;
            for session in &mut sessions {
                let _ = session.take_exit();
                if session.child_alive() {
                    alive = true;
                }
            }
            if !alive || Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }

        for session in &mut sessions {
            // `kill_hard` is guarded by the child's start time, so a recycled pid is
            // never signalled even if the status was not observed in time.
            if session.child_alive() {
                session.kill_hard();
            }
        }
        // Reap the killed children so no zombie is left behind.
        for session in &mut sessions {
            let _ = session.take_exit();
        }
    }

    /// Iterates sessions for the UI.
    pub fn iter(&self) -> impl Iterator<Item = &Session> {
        self.sessions.values()
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut Session> {
        self.sessions.values_mut()
    }
}

impl Drop for SessionManager {
    fn drop(&mut self) {
        self.shutdown_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use astraterm_config::Config;
    use astraterm_core::PaneId;

    use crate::lifecycle::session_spec;

    fn spec(program: &str, args: &[&str]) -> SessionSpec {
        let config = Config::default();
        let mut spec = session_spec(&config, None, None, None, 40, 10, (8, 16));
        spec.pty.program = program.to_owned();
        spec.pty.args = args.iter().map(|arg| (*arg).to_owned()).collect();
        spec
    }

    fn pump_until(manager: &mut SessionManager, needle: &str) -> String {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            manager.pump();
            for session in manager.iter_mut() {
                let text = session.engine.snapshot().text();
                if text.contains(needle) {
                    return text;
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("timed out waiting for `{needle}`");
    }

    #[test]
    fn creates_a_session_and_reads_output() {
        let mut manager = SessionManager::new();
        let pane = PaneId::next();
        manager
            .create(pane, &spec("/bin/echo", &["session-output"]), 40, 10)
            .unwrap();
        assert_eq!(manager.len(), 1);
        let text = pump_until(&mut manager, "session-output");
        assert!(text.contains("session-output"));
    }

    #[test]
    fn a_notification_sequence_becomes_a_session_event() {
        let mut manager = SessionManager::new();
        manager
            .create(
                PaneId::next(),
                &spec(
                    "/bin/sh",
                    &["-c", "printf '%b' '\\033]9;hello from the child\\007'"],
                ),
                40,
                10,
            )
            .unwrap();

        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            manager.pump();
            for event in manager.drain_events() {
                if let SessionEvent::Notification { title, body, .. } = event {
                    assert_eq!(title, None);
                    assert_eq!(body, "hello from the child");
                    return;
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("timed out waiting for the notification event");
    }

    #[test]
    fn exit_is_reported_once() {
        let mut manager = SessionManager::new();
        manager
            .create(PaneId::next(), &spec("/bin/true", &[]), 40, 10)
            .unwrap();
        let mut exits = 0;
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            manager.pump();
            for event in manager.drain_events() {
                if matches!(event, SessionEvent::Exited { .. }) {
                    exits += 1;
                }
            }
            if exits > 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        // Additional pumps must not report the same exit again.
        for _ in 0..3 {
            manager.pump();
            for event in manager.drain_events() {
                assert!(
                    !matches!(event, SessionEvent::Exited { .. }),
                    "exit reported twice"
                );
            }
        }
        assert_eq!(exits, 1);
    }

    #[test]
    fn title_events_are_propagated() {
        let mut manager = SessionManager::new();
        let spec = spec(
            "/bin/sh",
            &["-c", "printf '\\033]0;my-session-title\\007'; sleep 0.1"],
        );
        manager.create(PaneId::next(), &spec, 40, 10).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut seen = false;
        while Instant::now() < deadline && !seen {
            manager.pump();
            for event in manager.drain_events() {
                if let SessionEvent::Title {
                    title: Some(title), ..
                } = event
                    && title == "my-session-title"
                {
                    seen = true;
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(seen, "title event was not propagated");
    }

    #[test]
    fn input_reaches_the_child() {
        let mut manager = SessionManager::new();
        let id = manager
            .create(PaneId::next(), &spec("/bin/cat", &[]), 40, 10)
            .unwrap();
        manager.write(id, b"typed-input\n");
        let text = pump_until(&mut manager, "typed-input");
        assert!(text.contains("typed-input"));
    }

    #[test]
    fn resize_updates_the_engine_and_the_pty() {
        let mut manager = SessionManager::new();
        // The child reads one line and then prints the winsize it has, so the resize
        // below is ordered before the read returns instead of racing it: the PTY
        // writer thread applies the resize before it writes the line.
        let id = manager
            .create(
                PaneId::next(),
                &spec("/bin/sh", &["-c", "read line; stty size"]),
                40,
                10,
            )
            .unwrap();
        assert_eq!(manager.get(id).unwrap().engine.size(), (40, 10));
        assert!(
            manager.get_mut(id).unwrap().resize(100, 30, 8, 16),
            "the PTY must take the winsize"
        );
        manager.write(id, b"go\n");
        let text = pump_until(&mut manager, "30 100");
        assert!(
            text.contains("30 100"),
            "the child kept another winsize: {text:?}"
        );
        let session = manager.get(id).unwrap();
        assert_eq!(session.engine.size(), (100, 30));
    }

    #[test]
    fn a_session_created_at_a_grid_tells_the_child_that_winsize() {
        let mut manager = SessionManager::new();
        // The creation grid is what the app paints, so it must also be the winsize the
        // child starts with; the spec's own size (40x10 here) is not authoritative.
        manager
            .create(
                PaneId::next(),
                &spec("/bin/sh", &["-c", "stty size"]),
                100,
                30,
            )
            .unwrap();
        let text = pump_until(&mut manager, "30 100");
        assert!(
            text.contains("30 100"),
            "the child started at another winsize: {text:?}"
        );
    }

    #[test]
    fn a_winsize_the_pty_rejects_is_retried_by_the_next_resize() {
        let mut manager = SessionManager::new();
        let id = manager
            .create(
                PaneId::next(),
                &spec("/bin/sh", &["-c", "read line; stty size"]),
                40,
                10,
            )
            .unwrap();
        // The seam stands for a lost queue send or a refused ioctl, which is what a
        // winsize the PTY never took looks like to the session.
        let session = manager.get_mut(id).unwrap();
        session.reject_winsize = true;
        assert!(
            !session.resize(100, 30, 8, 16),
            "a winsize the PTY dropped must be reported as not applied"
        );
        assert_eq!(
            session.engine.size(),
            (100, 30),
            "the grid still follows the window"
        );
        assert!(
            !session.pty_matches(100, 30),
            "a dropped winsize must not be remembered as applied"
        );
        session.reject_winsize = false;
        // The engine already matches the target, so a retry can only come from the
        // session remembering that the PTY is behind it.
        assert!(
            session.resize(100, 30, 8, 16),
            "the retry with the same target must reach the PTY"
        );
        assert!(session.pty_matches(100, 30));
        manager.write(id, b"go\n");
        let text = pump_until(&mut manager, "30 100");
        assert!(
            text.contains("30 100"),
            "the retried winsize never reached the child: {text:?}"
        );
    }

    #[test]
    fn pump_budget_limits_work_per_pass() {
        // The budget is per session per pass; verify a large flood needs more than
        // one pass, which is what keeps the UI responsive.
        let mut manager = SessionManager::new();
        let spec = spec(
            "/bin/sh",
            &["-c", "head -c 2000000 /dev/zero | tr '\\0' 'y'"],
        );
        manager.create(PaneId::next(), &spec, 40, 10).unwrap();
        let mut passes = 0;
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            let report = manager.pump();
            passes += 1;
            if report.bytes_processed > 0 {
                // The loop stops once the budget is reached, so a pass can overshoot
                // by at most the chunk it was already reading.
                assert!(
                    report.bytes_processed <= PUMP_BUDGET_BYTES + astraterm_pty::READ_CHUNK_BYTES,
                    "budget exceeded: {} bytes in one pass",
                    report.bytes_processed
                );
            }
            let done = manager.iter().all(|session| session.exit.is_some());
            if done {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(passes > 1, "a 2 MB flood must be split across pump passes");
    }

    #[test]
    fn passthrough_accounting_is_unchanged_by_the_pump() {
        // The pump hands each chunk straight to the engine, so the reported byte count
        // is the child's exact output and the input path keeps counting nothing.
        let total = 1_000_000usize;
        let mut manager = SessionManager::new();
        let id = manager
            .create(
                PaneId::next(),
                &spec(
                    "/bin/sh",
                    &["-c", &format!("head -c {total} /dev/zero | tr '\\0' 'y'")],
                ),
                80,
                24,
            )
            .unwrap();
        let mut reported = 0usize;
        let mut passes = 0usize;
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            let report = manager.pump();
            passes += 1;
            reported += report.bytes_processed;
            // The typed input path is untouched by the output change and must keep
            // reporting no drops.
            manager.write(id, b"");
            if reported >= total && manager.iter().all(|session| session.exit.is_some()) {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(
            reported, total,
            "the pump must report exactly the bytes the child wrote"
        );
        assert!(passes > 1, "a 1 MB flood must span several pump passes");
        assert_eq!(
            manager.get(id).unwrap().input_drops,
            0,
            "no input may be counted as dropped"
        );
    }

    #[test]
    fn closing_a_session_terminates_the_child() {
        let mut manager = SessionManager::new();
        let id = manager
            .create(
                PaneId::next(),
                &spec("/bin/sh", &["-c", "sleep 30"]),
                40,
                10,
            )
            .unwrap();
        let reason = manager.close(id);
        assert!(reason.is_some());
        assert_eq!(manager.len(), 0);
    }

    #[test]
    fn shutdown_all_leaves_no_sessions() {
        let mut manager = SessionManager::new();
        manager
            .create(PaneId::next(), &spec("/bin/cat", &[]), 40, 10)
            .unwrap();
        manager
            .create(PaneId::next(), &spec("/bin/cat", &[]), 40, 10)
            .unwrap();
        assert_eq!(manager.len(), 2);
        manager.shutdown_all();
        assert!(manager.is_empty());
    }

    #[test]
    fn startup_input_is_typed_into_the_shell() {
        let mut manager = SessionManager::new();
        let mut spec = spec("/bin/cat", &[]);
        spec.startup_input = Some("startup-echo".to_owned());
        manager.create(PaneId::next(), &spec, 40, 10).unwrap();
        let text = pump_until(&mut manager, "startup-echo");
        assert!(text.contains("startup-echo"));
    }

    #[test]
    fn sessions_are_found_by_pane() {
        let mut manager = SessionManager::new();
        let pane = PaneId::next();
        let id = manager
            .create(pane, &spec("/bin/cat", &[]), 40, 10)
            .unwrap();
        assert_eq!(manager.for_pane(pane).map(|session| session.id), Some(id));
        assert!(manager.for_pane(PaneId::next()).is_none());
    }
}
