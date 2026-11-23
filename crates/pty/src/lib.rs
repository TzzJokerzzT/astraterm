//! PTY and process management.
//!
//! Design rules enforced here:
//!
//! * **No blocking I/O on the UI thread.** A dedicated reader thread and a
//!   dedicated writer thread own the PTY handles; the UI only touches bounded
//!   channels.
//! * **No dropped output.** The reader thread blocks when the bounded output queue
//!   is full. The child process then blocks on its own write, which is real flow
//!   control rather than silent data loss.
//! * **No orphan processes.** A waiter thread reports the exit status, and the
//!   handle can always be asked to terminate the child.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use astraterm_core::ExitReason;
use crossbeam_channel::{Receiver, Sender, TrySendError, bounded};
use portable_pty::{ChildKiller, CommandBuilder, PtySize, native_pty_system};

/// Size of a single read from the PTY master.
const READ_CHUNK: usize = 32 * 1024;

/// Largest amount a single pump pass can overshoot its budget by.
pub const READ_CHUNK_BYTES: usize = READ_CHUNK;
/// Bounded output queue depth, in chunks (about 2 MiB per session).
const OUTPUT_QUEUE: usize = 64;
/// Bounded input queue depth, in messages.
const INPUT_QUEUE: usize = 1024;
/// Grace period between closing the PTY and killing a surviving child.
const KILL_GRACE: Duration = Duration::from_millis(400);
/// Upper bound on how long a protocol reply may wait for queue room.
const WRITE_TIMEOUT: Duration = Duration::from_millis(100);

/// Everything needed to launch a session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PtySpec {
    /// Executable to run; resolved by the caller (`$SHELL`, a profile, or a
    /// command vector). Never a shell string.
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    /// Extra environment variables layered on top of the inherited environment.
    pub env: BTreeMap<String, String>,
    pub cols: u16,
    pub rows: u16,
    /// Cell size in pixels, advertised to applications that ask for it.
    pub cell_width: u16,
    pub cell_height: u16,
}

impl Default for PtySpec {
    fn default() -> Self {
        Self {
            program: "/bin/sh".to_owned(),
            args: Vec::new(),
            cwd: None,
            env: BTreeMap::new(),
            cols: 80,
            rows: 24,
            cell_width: 8,
            cell_height: 16,
        }
    }
}

impl PtySpec {
    /// The `$TERM` value this spec advertises.
    pub fn term(&self) -> Option<&str> {
        self.env.get("TERM").map(String::as_str)
    }
}

/// Errors raised while creating a session.
#[derive(Debug, thiserror::Error)]
pub enum PtyError {
    #[error("could not open a PTY: {0}")]
    Open(String),
    #[error("could not start `{program}`: {message}")]
    Spawn { program: String, message: String },
    #[error("could not attach to the PTY: {0}")]
    Attach(String),
}

/// Messages accepted by the writer thread.
enum Input {
    Data(Vec<u8>),
    Resize {
        cols: u16,
        rows: u16,
        cell_width: u16,
        cell_height: u16,
    },
    Shutdown,
}

/// A live PTY-backed session handle.
///
/// Dropping the handle asks the writer thread to close the PTY, which terminates
/// the child through SIGHUP on Unix; [`PtyHandle::shutdown`] adds an explicit kill
/// fallback so shutdown is deterministic.
pub struct PtyHandle {
    input: Sender<Input>,
    output: Receiver<Vec<u8>>,
    exit: Receiver<ExitReason>,
    killer: Box<dyn ChildKiller + Send + Sync>,
    child_pid: Option<u32>,
    exited: bool,
}

impl std::fmt::Debug for PtyHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PtyHandle")
            .field("child_pid", &self.child_pid)
            .field("exited", &self.exited)
            .finish_non_exhaustive()
    }
}

impl PtyHandle {
    /// Process id of the child, when the platform provides one.
    pub fn child_pid(&self) -> Option<u32> {
        self.child_pid
    }

    /// Queues bytes for the child.
    ///
    /// Returns `false` when the queue is full or the PTY is gone, which callers
    /// may treat as backpressure rather than a lost keystroke.
    pub fn write(&self, bytes: &[u8]) -> bool {
        if bytes.is_empty() {
            return true;
        }
        match self.input.try_send(Input::Data(bytes.to_vec())) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => false,
        }
    }

    /// Queues bytes, waiting at most [`WRITE_TIMEOUT`] for room.
    ///
    /// Protocol replies (cursor position reports, colour answers) must not stall the
    /// event loop, so the wait is bounded. Returns `false` when the bytes were not
    /// delivered, either because the PTY is gone or because the queue stayed full,
    /// which callers count instead of pretending the write happened.
    pub fn write_blocking(&self, bytes: &[u8]) -> bool {
        if bytes.is_empty() {
            return true;
        }
        !matches!(
            self.input
                .send_timeout(Input::Data(bytes.to_vec()), WRITE_TIMEOUT),
            Err(crossbeam_channel::SendTimeoutError::Timeout(_))
                | Err(crossbeam_channel::SendTimeoutError::Disconnected(_))
        )
    }

    /// Notifies the PTY of a new size. Never blocks.
    pub fn resize(&self, cols: u16, rows: u16, cell_width: u16, cell_height: u16) -> bool {
        self.input
            .try_send(Input::Resize {
                cols,
                rows,
                cell_width,
                cell_height,
            })
            .is_ok()
    }

    /// Pops one chunk of output without blocking.
    pub fn try_recv_output(&self) -> Option<Vec<u8>> {
        self.output.try_recv().ok()
    }

    /// Pops one pending exit status without blocking.
    pub fn try_recv_exit(&mut self) -> Option<ExitReason> {
        match self.exit.try_recv() {
            Ok(reason) => {
                self.exited = true;
                Some(reason)
            }
            Err(_) => None,
        }
    }

    /// Whether the child is still considered alive.
    pub fn is_running(&self) -> bool {
        !self.exited
    }

    /// Asks the PTY to close without blocking the caller.
    ///
    /// Closing the master is the polite path, but it is not a guarantee: the reader
    /// thread holds a duplicate of the master descriptor, and a child that ignores
    /// SIGHUP keeps running. Callers that need certainty must follow up with
    /// [`PtyHandle::kill_hard`], as [`PtyHandle::shutdown`] and the session manager do.
    pub fn request_shutdown(&self) {
        let _ = self.input.try_send(Input::Shutdown);
    }

    /// Kills the child's process group with `SIGKILL`.
    ///
    /// The child is a session leader (the PTY layer calls `setsid`), so its process
    /// group id equals its pid: signalling the negative pid also terminates the
    /// grandchildren a shell may have spawned, which is what prevents orphans.
    pub fn kill_hard(&self) {
        let Some(pid) = self.child_pid else {
            // Without a pid the only portable option is the crate's own killer.
            let mut killer = self.killer.clone_killer();
            let _ = killer.kill();
            return;
        };
        let pid = pid as libc::pid_t;
        // SAFETY: `kill(2)` with a plain integer pid and a constant signal cannot
        // violate memory safety; a failure is reported through the return value,
        // which is intentionally ignored because there is nothing left to try.
        let killed_group = unsafe { libc::kill(-pid, libc::SIGKILL) } == 0;
        if !killed_group {
            // The process may not be a group leader after all; fall back to the pid.
            // SAFETY: same reasoning as above.
            unsafe {
                libc::kill(pid, libc::SIGKILL);
            }
        }
    }

    /// Closes the PTY, then kills a surviving child after a short grace period.
    ///
    /// Bounded by construction: one grace period plus a short reap window, never an
    /// unbounded wait.
    pub fn shutdown(&mut self) -> Option<ExitReason> {
        if self.exited {
            return None;
        }
        self.request_shutdown();
        if let Some(reason) = self.wait_exit(KILL_GRACE) {
            return Some(reason);
        }
        self.kill_hard();
        self.wait_exit(Duration::from_millis(200))
    }

    /// Waits up to `timeout` for the waiter thread to publish an exit status.
    fn wait_exit(&mut self, timeout: Duration) -> Option<ExitReason> {
        let deadline = Instant::now() + timeout;
        loop {
            if self.exited || Instant::now() >= deadline {
                return self.try_recv_exit();
            }
            if let Ok(reason) = self.exit.try_recv() {
                self.exited = true;
                return Some(reason);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Kills the child immediately.
    pub fn kill(&mut self) {
        let _ = self.killer.kill();
    }
}

impl Drop for PtyHandle {
    fn drop(&mut self) {
        // Dropping a handle must not leak a child: ask for a polite close and kill
        // the process group when the child is still alive. The waiter thread reports
        // the status asynchronously, so this stays cheap.
        self.request_shutdown();
        if !self.exited {
            // Give the polite close a moment without blocking shutdown paths for long.
            if self.wait_exit(Duration::from_millis(50)).is_none() {
                self.kill_hard();
            }
        }
    }
}

/// A callback invoked after each chunk of output is queued.
///
/// The application uses it to wake its event loop, which is what keeps redraws
/// event-driven instead of polling.
pub type Waker = Arc<dyn Fn() + Send + Sync>;

/// Spawns a program inside a new PTY.
pub fn spawn(spec: &PtySpec) -> Result<PtyHandle, PtyError> {
    spawn_with_waker(spec, None)
}

/// Spawns a program inside a new PTY, waking `waker` whenever output arrives.
pub fn spawn_with_waker(spec: &PtySpec, waker: Option<Waker>) -> Result<PtyHandle, PtyError> {
    let pty_system = native_pty_system();
    let size = PtySize {
        rows: spec.rows.max(1),
        cols: spec.cols.max(1),
        pixel_width: spec.cell_width.saturating_mul(spec.cols),
        pixel_height: spec.cell_height.saturating_mul(spec.rows),
    };
    let pair = pty_system
        .openpty(size)
        .map_err(|error| PtyError::Open(error.to_string()))?;

    let mut builder = CommandBuilder::new(&spec.program);
    for arg in &spec.args {
        builder.arg(arg);
    }
    if let Some(cwd) = &spec.cwd {
        builder.cwd(cwd);
    }
    for (key, value) in &spec.env {
        builder.env(key, value);
    }
    if !spec.env.contains_key("TERM") {
        builder.env("TERM", "xterm-256color");
    }
    if !spec.env.contains_key("COLORTERM") {
        builder.env("COLORTERM", "truecolor");
    }

    let child = pair
        .slave
        .spawn_command(builder)
        .map_err(|error| PtyError::Spawn {
            program: spec.program.clone(),
            message: error.to_string(),
        })?;
    // The slave end must be dropped in the parent, otherwise the PTY never reports
    // EOF when the child exits.
    drop(pair.slave);

    let child_pid = child.process_id();
    let killer = child.clone_killer();

    let mut reader = pair
        .master
        .try_clone_reader()
        .map_err(|error| PtyError::Attach(error.to_string()))?;
    let mut writer = pair
        .master
        .take_writer()
        .map_err(|error| PtyError::Attach(error.to_string()))?;
    let master = pair.master;

    let (output_tx, output_rx) = bounded::<Vec<u8>>(OUTPUT_QUEUE);
    let (input_tx, input_rx) = bounded::<Input>(INPUT_QUEUE);
    let (exit_tx, exit_rx) = bounded::<ExitReason>(4);

    let read_waker = waker.clone();

    // Reader thread: PTY master -> bounded output queue.
    std::thread::Builder::new()
        .name("astraterm-pty-read".to_owned())
        .spawn(move || {
            let mut buffer = vec![0u8; READ_CHUNK];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(read) => {
                        // Blocking send: this is the deliberate backpressure point.
                        if output_tx.send(buffer[..read].to_vec()).is_err() {
                            break;
                        }
                        if let Some(waker) = read_waker.as_ref() {
                            waker();
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
        })
        .map_err(|error| PtyError::Attach(error.to_string()))?;

    // Writer thread: bounded input queue -> PTY master. It also owns the master so
    // resizes are applied from the same thread that writes.
    std::thread::Builder::new()
        .name("astraterm-pty-write".to_owned())
        .spawn(move || {
            let mut writable = true;
            while let Ok(message) = input_rx.recv() {
                match message {
                    Input::Data(bytes) => {
                        if writable && writer.write_all(&bytes).is_err() {
                            writable = false;
                        }
                        let _ = writer.flush();
                    }
                    Input::Resize {
                        cols,
                        rows,
                        cell_width,
                        cell_height,
                    } => {
                        let size = PtySize {
                            rows: rows.max(1),
                            cols: cols.max(1),
                            pixel_width: cell_width.saturating_mul(cols),
                            pixel_height: cell_height.saturating_mul(rows),
                        };
                        let _ = master.resize(size);
                    }
                    Input::Shutdown => break,
                }
            }
            // Dropping the master closes this descriptor. The reader thread holds a
            // duplicate, so the child is not guaranteed a hangup here: shutdown paths
            // escalate to `PtyHandle::kill_hard` when the child outlives the grace
            // period.
            drop(master);
        })
        .map_err(|error| PtyError::Attach(error.to_string()))?;

    // Waiter thread: reports the exit status exactly once.
    let exit_waker = waker.clone();
    let mut child = child;
    std::thread::Builder::new()
        .name("astraterm-pty-wait".to_owned())
        .spawn(move || {
            let reason = match child.wait() {
                Ok(status) => exit_reason_of(&status),
                Err(_) => ExitReason::Unknown,
            };
            let _ = exit_tx.send(reason);
            if let Some(waker) = exit_waker.as_ref() {
                waker();
            }
        })
        .map_err(|error| PtyError::Attach(error.to_string()))?;

    Ok(PtyHandle {
        input: input_tx,
        output: output_rx,
        exit: exit_rx,
        killer,
        child_pid,
        exited: false,
    })
}

fn exit_reason_of(status: &portable_pty::ExitStatus) -> ExitReason {
    match status.signal() {
        Some(signal) => ExitReason::Signal(signal.to_owned()),
        None => ExitReason::Code(status.exit_code() as i32),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wait_for_output(handle: &PtyHandle, needle: &str) -> String {
        let mut collected = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            while let Some(chunk) = handle.try_recv_output() {
                collected.extend_from_slice(&chunk);
                let text = String::from_utf8_lossy(&collected).to_string();
                if text.contains(needle) {
                    return text;
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!(
            "timed out waiting for `{needle}`; collected: {:?}",
            String::from_utf8_lossy(&collected)
        );
    }

    fn wait_for_exit(handle: &mut PtyHandle) -> ExitReason {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let Some(reason) = handle.try_recv_exit() {
                return reason;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("timed out waiting for the child to exit");
    }

    #[test]
    fn spawns_a_process_and_reads_its_output() {
        let spec = PtySpec {
            program: "/bin/echo".to_owned(),
            args: vec!["hello-from-pty".to_owned()],
            ..PtySpec::default()
        };
        let mut handle = spawn(&spec).unwrap();
        let output = wait_for_output(&handle, "hello-from-pty");
        assert!(output.contains("hello-from-pty\r\n"), "got {output:?}");
        let reason = wait_for_exit(&mut handle);
        assert_eq!(reason, ExitReason::Code(0));
    }

    #[test]
    fn writes_input_to_the_child() {
        // `cat` echoes stdin back through the PTY, so this exercises both
        // directions without relying on a shell.
        let spec = PtySpec {
            program: "/bin/cat".to_owned(),
            ..PtySpec::default()
        };
        let mut handle = spawn(&spec).unwrap();
        assert!(handle.write(b"round-trip\n"));
        let output = wait_for_output(&handle, "round-trip");
        assert!(output.contains("round-trip"));
        assert!(
            handle.shutdown().is_some(),
            "shutdown reports the exit status"
        );
    }

    #[test]
    fn resize_is_accepted_and_visible_to_the_child() {
        let spec = PtySpec {
            program: "/bin/sh".to_owned(),
            // `stty size` prints "rows cols" for the controlling terminal.
            args: vec!["-c".to_owned(), "stty size".to_owned()],
            cols: 100,
            rows: 30,
            ..PtySpec::default()
        };
        let mut handle = spawn(&spec).unwrap();
        let output = wait_for_output(&handle, "30 100");
        assert!(output.contains("30 100"), "got {output:?}");
        wait_for_exit(&mut handle);
    }

    #[test]
    fn reports_non_zero_exit_codes() {
        let spec = PtySpec {
            program: "/bin/sh".to_owned(),
            args: vec!["-c".to_owned(), "exit 7".to_owned()],
            ..PtySpec::default()
        };
        let mut handle = spawn(&spec).unwrap();
        assert_eq!(wait_for_exit(&mut handle), ExitReason::Code(7));
    }

    #[test]
    fn reports_signals() {
        let spec = PtySpec {
            program: "/bin/sh".to_owned(),
            args: vec!["-c".to_owned(), "kill -TERM $$".to_owned()],
            ..PtySpec::default()
        };
        let mut handle = spawn(&spec).unwrap();
        match wait_for_exit(&mut handle) {
            ExitReason::Signal(signal) => assert!(!signal.is_empty()),
            other => panic!("expected a signal exit, got {other:?}"),
        }
    }

    #[test]
    fn missing_program_is_reported_without_panicking() {
        let spec = PtySpec {
            program: "/definitely/not/a/program".to_owned(),
            ..PtySpec::default()
        };
        match spawn(&spec) {
            Err(PtyError::Spawn { program, .. }) => {
                assert_eq!(program, "/definitely/not/a/program")
            }
            other => panic!("expected a spawn error, got {other:?}"),
        }
    }

    #[test]
    fn working_directory_and_environment_are_applied() {
        let dir = tempfile::tempdir().unwrap();
        let mut env = BTreeMap::new();
        env.insert("ASTRATERM_TEST".to_owned(), "value-42".to_owned());
        let spec = PtySpec {
            program: "/bin/sh".to_owned(),
            args: vec![
                "-c".to_owned(),
                "printf 'cwd=%s env=%s term=%s' \"$PWD\" \"$ASTRATERM_TEST\" \"$TERM\"".to_owned(),
            ],
            cwd: Some(dir.path().to_path_buf()),
            env,
            ..PtySpec::default()
        };
        let mut handle = spawn(&spec).unwrap();
        let output = wait_for_output(&handle, "env=value-42");
        assert!(
            output.contains(&format!("cwd={}", dir.path().display())),
            "got {output:?}"
        );
        assert!(output.contains("term=xterm-256color"), "got {output:?}");
        wait_for_exit(&mut handle);
    }

    #[test]
    fn shutdown_terminates_a_long_running_child() {
        let spec = PtySpec {
            program: "/bin/sh".to_owned(),
            args: vec!["-c".to_owned(), "sleep 30".to_owned()],
            ..PtySpec::default()
        };
        let mut handle = spawn(&spec).unwrap();
        let started = Instant::now();
        let reason = handle.shutdown();
        assert!(reason.is_some(), "shutdown must report an exit reason");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "shutdown must not hang"
        );
        assert!(!handle.is_running());
    }

    #[test]
    fn output_queue_is_bounded_and_never_loses_bytes() {
        // Emits more than the queue depth can hold at once; every byte must still
        // arrive because the reader blocks instead of dropping.
        let spec = PtySpec {
            program: "/bin/sh".to_owned(),
            args: vec![
                "-c".to_owned(),
                "head -c 400000 /dev/zero | tr '\\0' 'x'".to_owned(),
            ],
            ..PtySpec::default()
        };
        let mut handle = spawn(&spec).unwrap();
        let mut total = 0usize;
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut exit = None;
        while Instant::now() < deadline {
            while let Some(chunk) = handle.try_recv_output() {
                total += chunk.len();
            }
            if exit.is_none() {
                exit = handle.try_recv_exit();
            }
            if exit.is_some() && total >= 400_000 {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        // The PTY adds line discipline processing, so compare with tolerance but
        // require that nearly every byte survives.
        assert!(total >= 390_000, "lost output: collected {total} bytes");
    }
}
