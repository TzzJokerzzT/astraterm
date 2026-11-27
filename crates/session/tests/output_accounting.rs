//! Output accounting and exit reporting.
//!
//! `PumpReport::bytes_processed` is what the performance documentation and the
//! flow-control story rely on, so it must count real bytes, not budget arithmetic.

use std::time::{Duration, Instant};

use astraterm_core::{PaneId, SessionEvent};
use astraterm_pty::PtySpec;
use astraterm_session::lifecycle::EngineSpec;
use astraterm_session::{SessionManager, SessionSpec};
use astraterm_terminal::CursorShape;

fn flood_spec(program: &str, args: &[&str]) -> SessionSpec {
    SessionSpec {
        pty: PtySpec {
            program: program.to_owned(),
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
            ..PtySpec::default()
        },
        engine: EngineSpec {
            scrollback: 1000,
            allow_osc52: false,
            kitty_keyboard: true,
            // Not what these tests exercise: the cursor keeps today's default.
            cursor_shape: CursorShape::Block,
            cursor_blink: true,
        },
        profile: None,
        label: None,
        startup_input: None,
    }
}

#[test]
fn bytes_processed_matches_the_bytes_the_child_wrote() {
    let total = 4_000_000usize;
    let mut manager = SessionManager::new();
    manager
        .create(
            PaneId::next(),
            &flood_spec(
                "/bin/sh",
                &["-c", &format!("head -c {total} /dev/zero | tr '\\0' 'y'")],
            ),
            80,
            24,
        )
        .unwrap();

    let mut reported = 0usize;
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut finished = false;
    while Instant::now() < deadline {
        let report = manager.pump();
        reported += report.bytes_processed;
        for event in manager.drain_events() {
            if matches!(event, SessionEvent::Exited { .. }) {
                finished = true;
            }
        }
        if finished && reported >= total {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    // Drain anything still queued after the exit.
    while reported < total {
        let report = manager.pump();
        if report.bytes_processed == 0 {
            break;
        }
        reported += report.bytes_processed;
    }

    assert_eq!(
        reported, total,
        "reported bytes must match what the child wrote"
    );
}

#[test]
fn exit_is_reported_exactly_once() {
    let mut manager = SessionManager::new();
    manager
        .create(PaneId::next(), &flood_spec("/bin/true", &[]), 80, 24)
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
    assert_eq!(exits, 1, "the first exit must be reported");

    for _ in 0..5 {
        manager.pump();
        for event in manager.drain_events() {
            assert!(
                !matches!(event, SessionEvent::Exited { .. }),
                "exit reported twice"
            );
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}
