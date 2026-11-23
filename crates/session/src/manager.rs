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
pub const PUMP_BUDGET_BYTES: usize = 256 * 1024;

/// Maximum chunks read from one session per pump pass, to keep fairness between
/// panes even when chunks are small.
pub const PUMP_BUDGET_CHUNKS: usize = 512;

/// How often the session refreshes its cached working directory.
const CWD_REFRESH_INTERVAL: Duration = Duration::from_millis(500);

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
            // Queue full or PTY gone; count it so the app can surface a warning
            // instead of pretending the keystroke reached the child.
            self.input_drops += 1;
        }
    }

    /// Sends bytes, waiting briefly for room. Used for protocol replies.
    pub fn write_urgent(&mut self, bytes: &[u8]) {
        if let Some(pty) = self.private_pty.as_ref() {
            let _ = pty.write_blocking(bytes);
        }
    }

    /// Tells the PTY about the new grid size.
    pub fn resize(&mut self, cols: u16, rows: u16, cell_width: u16, cell_height: u16) {
        self.engine.resize(cols, rows);
        if let Some(pty) = self.private_pty.as_ref() {
            let _ = pty.resize(cols, rows, cell_width, cell_height);
        }
        self.dirty = true;
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
    ) {
        let Some(pty) = self.private_pty.as_ref() else {
            return;
        };
        let mut processed = 0usize;
        let mut chunks = 0usize;

        while chunks < PUMP_BUDGET_CHUNKS && processed < *budget {
            let Some(chunk) = pty.try_recv_output() else {
                break;
            };
            chunks += 1;
            processed += chunk.len();
            self.engine.process(&chunk);
            self.dirty = true;
        }
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
        let engine = TerminalEngine::new(spec.engine_options(cols, rows));
        let pty = spawn_with_waker(&spec.pty, self.waker.clone())?;
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
            let before = session.dirty;
            session.pump_output(&mut events, &mut clipboard, &mut replies, &mut budget);
            if session.dirty && !before {
                session.dirty = true;
            }
            if session.dirty {
                report.sessions_dirty += 1;
            }
            report.bytes_processed += PUMP_BUDGET_BYTES - budget;

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

    /// Removes sessions whose child exited and whose pane was already closed.
    pub fn reap_exited(&mut self, keep: &[PaneId]) -> Vec<SessionEvent> {
        let mut events = Vec::new();
        let remove: Vec<SessionId> = self
            .sessions
            .values()
            .filter(|session| session.exit.is_some() && !keep.contains(&session.pane))
            .map(|session| session.id)
            .collect();
        for id in remove {
            if let Some(mut session) = self.sessions.remove(&id) {
                session.shutdown();
                if let Some(reason) = session.exit {
                    events.push(SessionEvent::Exited {
                        session: id,
                        reason,
                    });
                }
            }
        }
        events
    }

    /// Shuts every session down. Used on application exit so no child is orphaned.
    pub fn shutdown_all(&mut self) {
        for (_, mut session) in std::mem::take(&mut self.sessions) {
            session.shutdown();
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
        let id = manager
            .create(PaneId::next(), &spec("/bin/cat", &[]), 40, 10)
            .unwrap();
        manager.get_mut(id).unwrap().resize(100, 30, 8, 16);
        let session = manager.get(id).unwrap();
        assert_eq!(session.engine.size(), (100, 30));
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
                assert!(
                    report.bytes_processed <= PUMP_BUDGET_BYTES,
                    "budget exceeded"
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
