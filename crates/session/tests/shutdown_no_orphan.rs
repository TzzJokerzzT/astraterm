//! Shutdown must not leave a child behind, even when it ignores SIGHUP.
//!
//! Closing the PTY and sending SIGHUP is the polite path; a child that traps HUP
//! would survive it, so the session layer has to escalate.

use std::path::Path;
use std::time::Duration;

use astraterm_core::PaneId;
use astraterm_pty::PtySpec;
use astraterm_session::lifecycle::EngineSpec;
use astraterm_session::{SessionManager, SessionSpec};

fn spec(args: &[&str]) -> SessionSpec {
    SessionSpec {
        pty: PtySpec {
            program: "/bin/sh".to_owned(),
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
            ..PtySpec::default()
        },
        engine: EngineSpec {
            scrollback: 1000,
            allow_osc52: false,
        },
        profile: None,
        label: None,
        startup_input: None,
    }
}

fn wait_gone(pid: u32, timeout: Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if !Path::new(&format!("/proc/{pid}")).exists() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

#[test]
fn a_sighup_ignoring_child_must_not_survive_shutdown() {
    let mut manager = SessionManager::new();
    let id = manager
        .create(
            PaneId::next(),
            &spec(&["-c", "trap '' HUP; sleep 600"]),
            80,
            24,
        )
        .unwrap();
    std::thread::sleep(Duration::from_millis(300));
    let pid = manager
        .get(id)
        .and_then(|session| session.child_pid)
        .expect("child pid");

    manager.shutdown_all();

    assert!(
        wait_gone(pid, Duration::from_secs(3)),
        "child {pid} survived shutdown (orphan)"
    );
}

#[test]
fn a_plain_child_is_terminated_promptly() {
    let mut manager = SessionManager::new();
    let id = manager
        .create(PaneId::next(), &spec(&["-c", "sleep 600"]), 80, 24)
        .unwrap();
    std::thread::sleep(Duration::from_millis(200));
    let pid = manager
        .get(id)
        .and_then(|session| session.child_pid)
        .expect("child pid");

    let started = std::time::Instant::now();
    manager.shutdown_all();
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "shutdown must stay bounded"
    );

    assert!(
        wait_gone(pid, Duration::from_secs(3)),
        "child {pid} survived shutdown"
    );
}

#[test]
fn shutdown_of_many_sessions_stays_bounded() {
    let mut manager = SessionManager::new();
    for _ in 0..4 {
        manager
            .create(
                PaneId::next(),
                &spec(&["-c", "trap '' HUP; sleep 600"]),
                80,
                24,
            )
            .unwrap();
    }
    std::thread::sleep(Duration::from_millis(300));

    let started = std::time::Instant::now();
    manager.shutdown_all();
    // One shared grace period, not one per session.
    assert!(
        started.elapsed() < Duration::from_millis(1500),
        "shutdown took {:?} for four sessions",
        started.elapsed()
    );
}
