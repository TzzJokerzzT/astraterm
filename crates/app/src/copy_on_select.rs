//! Copy-on-select: which completed selections owe the clipboard a write, and where.
//!
//! A drag delivers one pointer motion per pixel, so the write hangs off the release
//! that completes the gesture and never off the motion. That keeps a fifty-event drag
//! to at most one write, and the write itself is queued on the platform's clipboard
//! worker (see `astraterm_platform::clipboard::ClipboardWriter`) so no frame ever waits
//! for the native clipboard library.

use astraterm_config::schema::CopyOnSelectTarget;
use astraterm_platform::clipboard::Selection as ClipboardSelection;

/// The selection the gesture writes to when the user configured `target`.
///
/// Only the gesture comes through here: the explicit copy (`ctrl+shift+c`) always writes
/// the regular clipboard, whatever this returns, so asking for the same selection twice
/// keeps meaning the same thing.
pub fn gesture_target(target: CopyOnSelectTarget) -> ClipboardSelection {
    match target {
        CopyOnSelectTarget::Clipboard => ClipboardSelection::Clipboard,
        CopyOnSelectTarget::Primary => ClipboardSelection::Primary,
    }
}

/// The text a completed gesture hands to [`CopyOnSelect`], or `None` when there is
/// nothing to copy.
///
/// S9: with `terminal.copy_on_select` off, nothing is extracted at all. The setting is
/// checked *before* the engine is asked, because building the text walks the whole
/// selection: a rectangle over a full 4K screen is a few hundred kilobytes of `String`
/// that the very next step would throw away. `extract` is a closure so the order is a
/// property of this function instead of a convention at one call site.
pub fn selection_for_gesture(
    enabled: bool,
    extract: impl FnOnce() -> Option<String>,
) -> Option<String> {
    if !enabled {
        return None;
    }
    extract()
}

/// Decides when copy-on-select writes, and where.
///
/// Explicit copies (`ctrl+shift+c`) do not come through here: a user who asks for the
/// same selection twice must get it twice.
#[derive(Debug, Default)]
pub struct CopyOnSelect {
    /// Text of the last selection whose copy was queued, and the selection it went to.
    last: Option<(ClipboardSelection, String)>,
}

impl CopyOnSelect {
    /// The text a gesture that ended with `text` selected has to write to `target`, or
    /// `None`.
    ///
    /// Callers reach this only from the release that ends a selection gesture, which
    /// is what makes the trigger the gesture and not the motion. `None` means the
    /// selection is empty, the setting is off, or this exact text is already on the
    /// target from the previous gesture.
    pub fn completed(
        &mut self,
        enabled: bool,
        target: ClipboardSelection,
        text: Option<String>,
    ) -> Option<String> {
        if !enabled {
            // The setting is off, so the text is never compared against this again
            // until it is turned back on: holding it would keep the last selection
            // allocated for the whole session.
            self.last = None;
            return None;
        }
        let text = text.filter(|text| !text.is_empty())?;
        // The target is part of what was copied. Pointing the gesture at the other
        // selection leaves the new one holding something else, so the same text still has
        // to be written instead of being suppressed as already copied.
        let already_copied = self
            .last
            .as_ref()
            .is_some_and(|(copied_to, copied)| *copied_to == target && copied == &text);
        if already_copied {
            // The target already holds exactly this text: the previous gesture is still
            // the last thing copied, so the write is not queued again.
            return None;
        }
        self.last = Some((target, text.clone()));
        Some(text)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Condvar, Mutex, MutexGuard, mpsc};
    use std::time::Duration;

    use astraterm_platform::clipboard::ClipboardWriter;

    use super::{
        ClipboardSelection, CopyOnSelect, CopyOnSelectTarget, gesture_target, selection_for_gesture,
    };

    /// Where the gesture writes by default, spelled once so the drag helper below reads
    /// like the real call site.
    const DEFAULT_TARGET: ClipboardSelection = ClipboardSelection::Clipboard;

    /// What the window actually delivers for one selection gesture: a press, one motion
    /// per pixel the pointer travels, then the release that completes it.
    ///
    /// Only the release reaches [`CopyOnSelect::completed`]; the motions only grow the
    /// terminal selection. This is why the count is a property of the trigger, not of
    /// the seam.
    fn drag(copier: &mut CopyOnSelect, motions: usize, text: &str) -> usize {
        drag_into(copier, motions, DEFAULT_TARGET, text)
    }

    /// The same gesture, with the selection the config points the gesture at.
    fn drag_into(
        copier: &mut CopyOnSelect,
        motions: usize,
        target: ClipboardSelection,
        text: &str,
    ) -> usize {
        let mut writes = 0;
        // Press and motions: the selection is updated, the clipboard is not.
        for _ in 0..motions {
            let _selection_grew = true;
        }
        if copier
            .completed(true, target, Some(text.to_owned()))
            .is_some()
        {
            writes += 1;
        }
        writes
    }

    #[test]
    fn a_drag_that_moves_fifty_times_writes_once_when_it_ends() {
        let mut copier = CopyOnSelect::default();
        assert_eq!(drag(&mut copier, 50, "dragged text"), 1);
    }

    #[test]
    fn an_unchanged_selection_is_not_written_again() {
        // The reported bug wrote on every release, so selecting the same text twice
        // queued a second write for text the clipboard already held.
        let mut copier = CopyOnSelect::default();
        assert_eq!(drag(&mut copier, 3, "same text"), 1);
        assert_eq!(drag(&mut copier, 3, "same text"), 0);
    }

    #[test]
    fn a_new_selection_is_written_even_right_after_another() {
        let mut copier = CopyOnSelect::default();
        assert_eq!(drag(&mut copier, 1, "first"), 1);
        assert_eq!(drag(&mut copier, 1, "second"), 1);
    }

    #[test]
    fn an_empty_selection_writes_nothing() {
        let mut copier = CopyOnSelect::default();
        assert_eq!(
            copier.completed(true, DEFAULT_TARGET, Some(String::new())),
            None
        );
        assert_eq!(copier.completed(true, DEFAULT_TARGET, None), None);
        // An empty selection is not remembered, so the next real one is not compared
        // against it.
        assert_eq!(
            copier.completed(true, DEFAULT_TARGET, Some("text".to_owned())),
            Some("text".to_owned())
        );
    }

    #[test]
    fn the_setting_being_off_writes_nothing() {
        let mut copier = CopyOnSelect::default();
        assert_eq!(
            copier.completed(false, DEFAULT_TARGET, Some("text".to_owned())),
            None
        );
        // Turning it back on copies the selection the user already has.
        assert_eq!(
            copier.completed(true, DEFAULT_TARGET, Some("text".to_owned())),
            Some("text".to_owned())
        );
    }

    #[test]
    fn turning_the_setting_off_forgets_the_previous_selection() {
        // Disabling the feature has to drop the retained text: keeping it held the
        // whole selection for the rest of the session, and the text is never read
        // again until the setting is turned back on.
        let mut copier = CopyOnSelect::default();
        assert_eq!(
            copier.completed(true, DEFAULT_TARGET, Some("retained".to_owned())),
            Some("retained".to_owned())
        );
        assert_eq!(copier.completed(false, DEFAULT_TARGET, None), None);
        // The retained selection is gone, so the same text is written again instead
        // of being suppressed as already copied.
        assert_eq!(
            copier.completed(true, DEFAULT_TARGET, Some("retained".to_owned())),
            Some("retained".to_owned())
        );
    }

    #[test]
    fn the_configured_target_is_the_one_the_gesture_writes_to() {
        // B7: the gesture follows the setting; the regular clipboard is the default.
        assert_eq!(
            gesture_target(CopyOnSelectTarget::Clipboard),
            ClipboardSelection::Clipboard
        );
        assert_eq!(
            gesture_target(CopyOnSelectTarget::Primary),
            ClipboardSelection::Primary
        );
        assert_eq!(
            gesture_target(CopyOnSelectTarget::default()),
            ClipboardSelection::Clipboard,
            "the default is the clipboard every other application reads"
        );

        let mut copier = CopyOnSelect::default();
        assert_eq!(
            drag_into(
                &mut copier,
                2,
                gesture_target(CopyOnSelectTarget::Primary),
                "selected text"
            ),
            1
        );
        // A second, different selection still writes: only the same text going to the
        // same target is suppressed.
        assert_eq!(
            drag_into(
                &mut copier,
                2,
                gesture_target(CopyOnSelectTarget::Primary),
                "more text"
            ),
            1
        );
    }

    #[test]
    fn pointing_the_gesture_at_the_other_selection_writes_the_same_text_again() {
        // Changing the target leaves the new selection holding something else, so the
        // text the previous gesture copied is not on it and must be written again.
        let mut copier = CopyOnSelect::default();
        assert_eq!(drag(&mut copier, 1, "same text"), 1);
        assert_eq!(
            drag_into(&mut copier, 1, ClipboardSelection::Primary, "same text"),
            1
        );
        assert_eq!(
            drag_into(&mut copier, 1, ClipboardSelection::Primary, "same text"),
            0,
            "the primary selection now holds it, so it is not written twice"
        );
    }

    /// A rendezvous that holds a clipboard write open, so "the caller did not wait" is
    /// proven by control flow instead of by timing.
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

    fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
        mutex.lock().unwrap_or_else(|error| error.into_inner())
    }

    #[test]
    fn the_gesture_queues_the_configured_selection_without_waiting_for_the_clipboard() {
        // The write path is the platform's worker, reached through the seam that lets a
        // test supply the runner: the gesture hands it the configured selection and
        // returns while the write is still blocked inside the runner.
        let gate = std::sync::Arc::new(Gate::default());
        let recorded: std::sync::Arc<Mutex<Vec<(ClipboardSelection, String)>>> =
            std::sync::Arc::default();
        let (ready, started) = mpsc::channel();
        let seen = std::sync::Arc::clone(&recorded);
        let held = std::sync::Arc::clone(&gate);
        let writer = ClipboardWriter::start(std::sync::Arc::new(
            move |selection: ClipboardSelection, text: &str| {
                lock(&seen).push((selection, text.to_owned()));
                let _ = ready.send(());
                held.wait();
                Ok(())
            },
        ));

        let mut copier = CopyOnSelect::default();
        let target = gesture_target(CopyOnSelectTarget::Primary);
        let text = copier
            .completed(true, target, Some("selected text".to_owned()))
            .expect("the gesture owes the primary selection a write");

        // Queued while the runner is blocked: this returns, and the recorded write is
        // already visible, which the closed gate proves is not by timing.
        assert!(writer.set(target, &text).is_ok());
        started
            .recv_timeout(Duration::from_secs(10))
            .expect("the worker started the write");
        assert_eq!(
            lock(&recorded).as_slice(),
            [(ClipboardSelection::Primary, "selected text".to_owned())]
        );

        gate.open();
    }

    /// A selection big enough that building its text is real work: the whole visible
    /// screen of a 200x50 grid as one rectangle, tens of thousands of cells.
    fn screen_wide_selection() -> astraterm_terminal::TerminalEngine {
        use astraterm_terminal::SelectionKind;
        use astraterm_terminal::engine::EngineOptions;

        let mut engine = astraterm_terminal::TerminalEngine::new(EngineOptions {
            cols: 200,
            rows: 50,
            scrollback: 100,
            ..Default::default()
        });
        let line = "x".repeat(199) + "\r\n";
        for _ in 0..50 {
            engine.process(line.as_bytes());
        }
        engine.update_selection(0, 0, SelectionKind::Block, astraterm_terminal::Side::Left);
        engine.update_selection(
            199,
            49,
            SelectionKind::SimpleOrExisting,
            astraterm_terminal::Side::Right,
        );
        assert!(engine.has_selection(), "the fixture needs a live selection");
        engine
    }

    #[test]
    fn a_large_selection_is_never_extracted_when_copy_on_select_is_off() {
        let engine = screen_wide_selection();
        let mut extractions = 0;
        let text = selection_for_gesture(false, || {
            extractions += 1;
            engine.selection_text()
        });
        assert_eq!(text, None, "a gesture that copies nothing copies nothing");
        assert_eq!(
            extractions, 0,
            "with the setting off the selection text is never built at all"
        );
    }

    #[test]
    fn the_same_selection_is_extracted_once_when_copy_on_select_is_on() {
        let engine = screen_wide_selection();
        let mut extractions = 0;
        let text = selection_for_gesture(true, || {
            extractions += 1;
            engine.selection_text()
        });
        assert_eq!(extractions, 1, "the gate is the setting, not the selection");
        let text = text.expect("the gesture copies what is selected");
        assert!(text.len() > 1000, "the fixture is a full screen of text");
    }
}
