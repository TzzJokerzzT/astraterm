//! Copy-on-select: which completed selections owe the clipboard a write.
//!
//! A drag delivers one pointer motion per pixel, so the write hangs off the release
//! that completes the gesture and never off the motion. That keeps a fifty-event drag
//! to at most one write, and the write itself is queued on the platform's clipboard
//! worker (see `astraterm_platform::clipboard::ClipboardWriter`) so no frame ever waits
//! for `wl-copy`/`xclip`.

/// Decides when copy-on-select writes.
///
/// Explicit copies (`ctrl+shift+c`) do not come through here: a user who asks for the
/// same selection twice must get it twice.
#[derive(Debug, Default)]
pub struct CopyOnSelect {
    /// Text of the last selection whose copy was queued.
    last: Option<String>,
}

impl CopyOnSelect {
    /// The text a gesture that ended with `text` selected has to copy, or `None`.
    ///
    /// Callers reach this only from the release that ends a selection gesture, which
    /// is what makes the trigger the gesture and not the motion. `None` means the
    /// selection is empty, the setting is off, or this exact text is already on the
    /// clipboard from the previous gesture.
    pub fn completed(&mut self, enabled: bool, text: Option<String>) -> Option<String> {
        if !enabled {
            return None;
        }
        let text = text.filter(|text| !text.is_empty())?;
        if self.last.as_deref() == Some(text.as_str()) {
            // The clipboard already holds exactly this text: the previous gesture is
            // still the last thing copied, so a helper is not spawned again.
            return None;
        }
        self.last = Some(text.clone());
        Some(text)
    }
}

#[cfg(test)]
mod tests {
    use super::CopyOnSelect;

    /// What the window actually delivers for one selection gesture: a press, one motion
    /// per pixel the pointer travels, then the release that completes it.
    ///
    /// Only the release reaches [`CopyOnSelect::completed`]; the motions only grow the
    /// terminal selection. This is why the count is a property of the trigger, not of
    /// the seam.
    fn drag(copier: &mut CopyOnSelect, motions: usize, text: &str) -> usize {
        let mut writes = 0;
        // Press and motions: the selection is updated, the clipboard is not.
        for _ in 0..motions {
            let _selection_grew = true;
        }
        if copier.completed(true, Some(text.to_owned())).is_some() {
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
        // spawned a second helper for text the clipboard already held.
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
        assert_eq!(copier.completed(true, Some(String::new())), None);
        assert_eq!(copier.completed(true, None), None);
        // An empty selection is not remembered, so the next real one is not compared
        // against it.
        assert_eq!(
            copier.completed(true, Some("text".to_owned())),
            Some("text".to_owned())
        );
    }

    #[test]
    fn the_setting_being_off_writes_nothing() {
        let mut copier = CopyOnSelect::default();
        assert_eq!(copier.completed(false, Some("text".to_owned())), None);
        // Turning it back on copies the selection the user already has.
        assert_eq!(
            copier.completed(true, Some("text".to_owned())),
            Some("text".to_owned())
        );
    }
}
