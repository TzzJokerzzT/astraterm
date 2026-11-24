//! Translation between `winit` input and the platform-neutral types used by the
//! configuration keymap and the terminal input encoder.
//!
//! Two separate concerns live here:
//!
//! * [`chord_of`] answers "which configurable chord is this key press?", used to
//!   dispatch application shortcuts;
//! * [`key_press`] answers "what bytes must the PTY receive?", used when no
//!   shortcut claims the key (and always while literal-next mode is armed).

use std::time::{Duration, Instant};

use astraterm_config::keys::{
    ActionId, Chord, Key as ChordKey, Modifiers as ChordModifiers, NamedKey,
};
use astraterm_core::events::{AppCommand, FocusDirection};
use astraterm_terminal::input::{Direction, KeyEventType, KeyPress, TermKey};
use astraterm_terminal::snapshot::ModeFlags;
use winit::event::KeyEvent;
use winit::keyboard::ModifiersState;
use winit::keyboard::{Key, NamedKey as WinitNamedKey};
use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;

/// Converts a winit key press into a configuration chord, if it maps to one.
pub fn chord_of(event: &KeyEvent, modifiers: ModifiersState) -> Option<Chord> {
    let key = chord_key(&event.logical_key)?;
    Some(Chord {
        mods: chord_modifiers(modifiers, altgr_composition_of(event, modifiers)),
        key,
    })
}

/// The chord modifiers of a press, with AltGr composition excluded from `alt`.
///
/// AltGr types a character (the third level of the layout), so the chords bound
/// to `alt+1` and friends must not fire for it; genuine Meta presses still do.
pub fn chord_modifiers(modifiers: ModifiersState, altgr: bool) -> ChordModifiers {
    ChordModifiers {
        ctrl: modifiers.control_key(),
        alt: modifiers.alt_key() && !altgr,
        shift: modifiers.shift_key(),
        sup: modifiers.super_key(),
    }
}

/// Whether the Alt reported for this press is AltGr composition rather than Meta.
pub fn altgr_composition_of(event: &KeyEvent, modifiers: ModifiersState) -> bool {
    modifiers.alt_key()
        && altgr_composition(
            &event.logical_key,
            event.text.as_deref(),
            &event.key_without_modifiers(),
            modifiers.shift_key(),
        )
}

/// Whether a press produced its character through AltGr rather than as Meta.
///
/// Compositors can report the layout's third level (AltGr) as the Alt modifier,
/// so the modifier alone cannot tell `AltGr+4`, which types `~` on a Spanish
/// layout, from `Alt+4`, which is Meta-4. The produced text decides:
///
/// * the text is one printable character that differs from the logical key, so
///   composition replaced the character; or
/// * the compositor already folded the third level into the logical key
///   (`AltGr+4` arrives as `Character("~")`), so the logical key differs from
///   the key's unmodified character while Shift is not held.
///
/// Alt never changes an xkb layout's character, so plain `Alt+a` stays Meta. Caps
/// Lock only upper-cases the unmodified character, so the second rule compares
/// case-insensitively to keep `Alt+a` Meta under Caps Lock.
pub fn altgr_composition(logical: &Key, text: Option<&str>, unmodified: &Key, shift: bool) -> bool {
    let Some(ch) = single_char(text) else {
        return false;
    };
    if ch.is_control() {
        return false;
    }
    if char_of(logical).is_some_and(|logical| logical != ch) {
        return true;
    }
    if shift {
        return false;
    }
    char_of(unmodified).is_some_and(|base| !base.eq_ignore_ascii_case(&ch))
}

/// How recently a key press must have written text for an IME commit to count as
/// the same keystroke.
pub const IME_ECHO_WINDOW: Duration = Duration::from_millis(200);

/// Whether an IME commit repeats text a key press just wrote.
///
/// A platform that delivers both the keystroke and its commit would otherwise
/// insert the character twice. Text an input method composed from several keys
/// (dead keys, CJK candidates) never produced a key press, so it is written once.
pub fn commit_repeats_key(commit: &str, last: Option<(&str, Instant)>, now: Instant) -> bool {
    match last {
        Some((text, at)) => text == commit && now.saturating_duration_since(at) <= IME_ECHO_WINDOW,
        None => false,
    }
}

/// The single character a text string carries, if it carries exactly one.
fn single_char(text: Option<&str>) -> Option<char> {
    let mut chars = text?.chars();
    let ch = chars.next()?;
    chars.next().is_none().then_some(ch)
}

/// The character a key names, for the keys that carry one.
fn char_of(key: &Key) -> Option<char> {
    match key {
        Key::Character(text) => text.chars().next(),
        Key::Named(WinitNamedKey::Space) => Some(' '),
        _ => None,
    }
}

fn chord_key(key: &Key) -> Option<ChordKey> {
    match key {
        // `Space` is a named key in winit but a character chord in configuration.
        Key::Named(WinitNamedKey::Space) => Some(ChordKey::Char(' ')),
        Key::Named(named) => named_key(*named).map(ChordKey::Named),
        Key::Character(text) => {
            // Normalise to the unshifted character so `ctrl+shift+t` and
            // `ctrl+shift+T` are the same chord.
            let ch = text.chars().next()?;
            let ch = ch.to_lowercase().next().unwrap_or(ch);
            Some(ChordKey::Char(ch))
        }
        _ => None,
    }
}

fn named_key(named: WinitNamedKey) -> Option<NamedKey> {
    Some(match named {
        WinitNamedKey::Escape => NamedKey::Escape,
        WinitNamedKey::Enter => NamedKey::Enter,
        WinitNamedKey::Tab => NamedKey::Tab,
        WinitNamedKey::Backspace => NamedKey::Backspace,
        WinitNamedKey::Delete => NamedKey::Delete,
        WinitNamedKey::Insert => NamedKey::Insert,
        WinitNamedKey::Home => NamedKey::Home,
        WinitNamedKey::End => NamedKey::End,
        WinitNamedKey::PageUp => NamedKey::PageUp,
        WinitNamedKey::PageDown => NamedKey::PageDown,
        WinitNamedKey::ArrowUp => NamedKey::ArrowUp,
        WinitNamedKey::ArrowDown => NamedKey::ArrowDown,
        WinitNamedKey::ArrowLeft => NamedKey::ArrowLeft,
        WinitNamedKey::ArrowRight => NamedKey::ArrowRight,
        WinitNamedKey::F1 => NamedKey::Function(1),
        WinitNamedKey::F2 => NamedKey::Function(2),
        WinitNamedKey::F3 => NamedKey::Function(3),
        WinitNamedKey::F4 => NamedKey::Function(4),
        WinitNamedKey::F5 => NamedKey::Function(5),
        WinitNamedKey::F6 => NamedKey::Function(6),
        WinitNamedKey::F7 => NamedKey::Function(7),
        WinitNamedKey::F8 => NamedKey::Function(8),
        WinitNamedKey::F9 => NamedKey::Function(9),
        WinitNamedKey::F10 => NamedKey::Function(10),
        WinitNamedKey::F11 => NamedKey::Function(11),
        WinitNamedKey::F12 => NamedKey::Function(12),
        _ => return None,
    })
}

/// Converts a winit key press into terminal input.
///
/// Returns `None` for modifier-only presses and for keys the encoder cannot
/// represent, so nothing is silently sent to the child process.
pub fn key_press(event: &KeyEvent, modifiers: ModifiersState) -> Option<KeyPress> {
    let ctrl = modifiers.control_key();
    // AltGr is a character composition, not Meta: `AltGr+4` must send `~`, not
    // `ESC ~`.
    let alt = modifiers.alt_key() && !altgr_composition_of(event, modifiers);
    let shift = modifiers.shift_key();

    let key = match &event.logical_key {
        Key::Named(named) if *named == WinitNamedKey::Space => TermKey::Char(' '),
        Key::Named(named) => match named_key(*named)? {
            NamedKey::ArrowUp => TermKey::Arrow(Direction::Up),
            NamedKey::ArrowDown => TermKey::Arrow(Direction::Down),
            NamedKey::ArrowLeft => TermKey::Arrow(Direction::Left),
            NamedKey::ArrowRight => TermKey::Arrow(Direction::Right),
            NamedKey::Function(number) => TermKey::Function(number),
            NamedKey::Escape => TermKey::Escape,
            NamedKey::Enter => TermKey::Enter,
            NamedKey::Tab => TermKey::Tab,
            NamedKey::Backspace => TermKey::Backspace,
            NamedKey::Delete => TermKey::Delete,
            NamedKey::Insert => TermKey::Insert,
            NamedKey::Home => TermKey::Home,
            NamedKey::End => TermKey::End,
            NamedKey::PageUp => TermKey::PageUp,
            NamedKey::PageDown => TermKey::PageDown,
        },
        Key::Character(text) => {
            // Prefer the produced text so dead keys and layouts behave; fall back
            // to the raw character when the platform reports no text.
            let ch = event
                .text
                .as_ref()
                .and_then(|text| text.chars().next())
                .or_else(|| {
                    let raw = text.chars().next()?;
                    if shift {
                        Some(raw)
                    } else {
                        Some(raw.to_lowercase().next().unwrap_or(raw))
                    }
                })?;
            TermKey::Char(ch)
        }
        // A dead key is usually a prefix, not text: the composed character arrives
        // with the next press. Platforms that do report the character a dead key
        // stands for deliver it as text, and that character is written.
        Key::Dead(_) => TermKey::Char(single_char(event.text.as_deref())?),
        _ => return None,
    };

    Some(KeyPress {
        key,
        ctrl,
        alt,
        shift,
        event: KeyEventType::Press,
    })
}

/// Whether key release and repeat events must reach the PTY.
///
/// Only the Kitty keyboard protocol's `REPORT_EVENT_TYPES` flag gives them an
/// encoding: every other mode reports a key once, on press, so forwarding a
/// release there would duplicate input.
pub fn forwards_release(modes: ModeFlags) -> bool {
    modes.kitty.report_event_types
}

/// The application command a bound action maps to.
pub fn command_for(action: ActionId) -> Option<AppCommand> {
    Some(match action {
        ActionId::NewTab => AppCommand::NewTab { profile: None },
        ActionId::PreviousTab => AppCommand::PreviousTab,
        ActionId::MoveTabLeft => AppCommand::MoveTabRelative { delta: -1 },
        ActionId::MoveTabRight => AppCommand::MoveTabRelative { delta: 1 },
        ActionId::NextTab => AppCommand::NextTab,
        ActionId::Tab1 => AppCommand::SelectTab { index: 0 },
        ActionId::Tab2 => AppCommand::SelectTab { index: 1 },
        ActionId::Tab3 => AppCommand::SelectTab { index: 2 },
        ActionId::Tab4 => AppCommand::SelectTab { index: 3 },
        ActionId::Tab5 => AppCommand::SelectTab { index: 4 },
        ActionId::Tab6 => AppCommand::SelectTab { index: 5 },
        ActionId::Tab7 => AppCommand::SelectTab { index: 6 },
        ActionId::Tab8 => AppCommand::SelectTab { index: 7 },
        ActionId::Tab9 => AppCommand::SelectTab { index: 8 },
        ActionId::NewSplitHorizontal => AppCommand::SplitPane {
            pane: PaneIdPlaceholder::VALUE,
            axis: astraterm_core::SplitAxis::Horizontal,
        },
        ActionId::NewSplitVertical => AppCommand::SplitPane {
            pane: PaneIdPlaceholder::VALUE,
            axis: astraterm_core::SplitAxis::Vertical,
        },
        ActionId::ClosePane => AppCommand::ClosePane {
            pane: PaneIdPlaceholder::VALUE,
        },
        ActionId::CloseTab => AppCommand::CloseTab {
            tab: astraterm_core::TabId::from_raw(0),
        },
        // Rename opens an inline prompt, so it carries no title here.
        ActionId::RenameTab => AppCommand::RenameTab {
            tab: astraterm_core::TabId::from_raw(0),
            title: String::new(),
        },
        ActionId::FocusLeft => AppCommand::FocusDirection {
            direction: FocusDirection::Left,
        },
        ActionId::FocusRight => AppCommand::FocusDirection {
            direction: FocusDirection::Right,
        },
        ActionId::FocusUp => AppCommand::FocusDirection {
            direction: FocusDirection::Up,
        },
        ActionId::FocusDown => AppCommand::FocusDirection {
            direction: FocusDirection::Down,
        },
        ActionId::ResizeLeft => AppCommand::ResizeDivider {
            axis: astraterm_core::SplitAxis::Vertical,
            delta_percent: -2,
        },
        ActionId::ResizeRight => AppCommand::ResizeDivider {
            axis: astraterm_core::SplitAxis::Vertical,
            delta_percent: 2,
        },
        ActionId::ResizeUp => AppCommand::ResizeDivider {
            axis: astraterm_core::SplitAxis::Horizontal,
            delta_percent: -2,
        },
        ActionId::ResizeDown => AppCommand::ResizeDivider {
            axis: astraterm_core::SplitAxis::Horizontal,
            delta_percent: 2,
        },
        ActionId::EqualizeSplits => AppCommand::EqualizeSplits,
        ActionId::ToggleZoom => AppCommand::ToggleZoom,
        ActionId::IncreaseFontSize => AppCommand::IncreaseFontSize,
        ActionId::DecreaseFontSize => AppCommand::DecreaseFontSize,
        ActionId::ResetFontSize => AppCommand::ResetFontSize,
        ActionId::Search => AppCommand::Search { forward: true },
        ActionId::SearchNext => AppCommand::SearchNext,
        ActionId::SearchPrevious => AppCommand::SearchPrevious,
        ActionId::Copy => AppCommand::CopySelection,
        ActionId::Paste => AppCommand::Paste,
        ActionId::ScrollPageUp => AppCommand::ScrollLines { delta: i32::MAX },
        ActionId::ScrollPageDown => AppCommand::ScrollLines { delta: i32::MIN },
        ActionId::ScrollToTop => AppCommand::ScrollLines { delta: i32::MAX },
        ActionId::ScrollToBottom => AppCommand::ScrollToBottom,
        ActionId::OpenSettings => AppCommand::OpenSettings,
        ActionId::ReloadConfig => AppCommand::ReloadConfig,
        ActionId::ToggleStatusBar => AppCommand::ToggleStatusBar,
        ActionId::ToggleFloating => AppCommand::ToggleFloating,
        ActionId::NewWindow => AppCommand::NewWindow { profile: None },
        ActionId::SendLiteralNext => AppCommand::SendLiteralNext,
        ActionId::Quit => AppCommand::Quit,
    })
}

/// Placeholder identifiers used by [`command_for`] for pane- and tab-scoped
/// actions: the application substitutes the focused pane or active tab before
/// dispatching. Keeping the substitution explicit avoids embedding UI state in
/// this pure mapping.
pub struct PaneIdPlaceholder;

impl PaneIdPlaceholder {
    pub const VALUE: astraterm_core::PaneId = astraterm_core::PaneId::from_raw(0);
}

#[cfg(test)]
mod tests {
    use super::*;
    use astraterm_config::keys::Keymap;
    use astraterm_config::keys::default_keybindings;

    fn keymap() -> Keymap {
        Keymap::build(&default_keybindings())
    }

    fn chords() -> Vec<(Chord, ActionId)> {
        let mut out = Vec::new();
        for (chord, action) in keymap().bindings() {
            out.push((*chord, *action));
        }
        out
    }

    #[test]
    fn every_default_chord_resolves_to_an_action() {
        for (chord, action) in chords() {
            assert_eq!(keymap().action_for(&chord), Some(action), "{chord}");
        }
    }

    #[test]
    fn every_action_maps_to_a_command() {
        for action in ActionId::ALL {
            assert!(command_for(action).is_some(), "{action} has no command");
        }
    }

    #[test]
    fn tab_moves_are_relative() {
        assert!(matches!(
            command_for(ActionId::MoveTabLeft),
            Some(AppCommand::MoveTabRelative { delta }) if delta < 0
        ));
        assert!(matches!(
            command_for(ActionId::MoveTabRight),
            Some(AppCommand::MoveTabRelative { delta }) if delta > 0
        ));
    }

    #[test]
    fn pane_and_tab_commands_use_placeholders() {
        let split = command_for(ActionId::NewSplitVertical).unwrap();
        assert!(matches!(
            split,
            AppCommand::SplitPane { pane, axis: astraterm_core::SplitAxis::Vertical } if pane == PaneIdPlaceholder::VALUE
        ));
    }

    #[test]
    fn scroll_actions_use_extreme_deltas_for_paging() {
        assert!(matches!(
            command_for(ActionId::ScrollPageUp),
            Some(AppCommand::ScrollLines { delta }) if delta > 0
        ));
        assert!(matches!(
            command_for(ActionId::ScrollToBottom),
            Some(AppCommand::ScrollToBottom)
        ));
    }

    /// The four cases the layout rule names, stated as the winit data each one
    /// produces: `logical`, the produced `text`, the unmodified key and Shift.
    #[test]
    fn altgr_composition_is_not_meta() {
        // `AltGr+1` on the Spanish layout types `|`: the produced text is not the
        // logical key, so the Alt on the press is composition, not Meta.
        assert!(altgr_composition(
            &Key::Character("1".into()),
            Some("|"),
            &Key::Character("1".into()),
            false
        ));
        // `AltGr+4` types `~` for the same reason.
        assert!(altgr_composition(
            &Key::Character("4".into()),
            Some("~"),
            &Key::Character("4".into()),
            false
        ));
        // `Alt+a` is Meta: the text is exactly the logical key.
        assert!(!altgr_composition(
            &Key::Character("a".into()),
            Some("a"),
            &Key::Character("a".into()),
            false
        ));
        // `Alt+1` is Meta-1: no composition changed the character.
        assert!(!altgr_composition(
            &Key::Character("1".into()),
            Some("1"),
            &Key::Character("1".into()),
            false
        ));
    }

    #[test]
    fn a_layout_that_folds_altgr_into_the_logical_key_is_composition() {
        // The compositor applies the third level before winit reports it, so the
        // logical key already carries `|` and only the unmodified key (`1`) shows
        // that Alt was AltGr.
        assert!(altgr_composition(
            &Key::Character("|".into()),
            Some("|"),
            &Key::Character("1".into()),
            false
        ));
        // A layout's second level is Shift, not AltGr: `Alt+Shift+a` stays Meta.
        assert!(!altgr_composition(
            &Key::Character("A".into()),
            Some("A"),
            &Key::Character("a".into()),
            true
        ));
        // Caps Lock only upper-cases the unmodified character: still Meta.
        assert!(!altgr_composition(
            &Key::Character("A".into()),
            Some("A"),
            &Key::Character("a".into()),
            false
        ));
    }

    #[test]
    fn only_single_printable_text_is_composition() {
        assert!(!altgr_composition(
            &Key::Character("a".into()),
            None,
            &Key::Character("a".into()),
            false
        ));
        assert!(!altgr_composition(
            &Key::Character("a".into()),
            Some("abc"),
            &Key::Character("a".into()),
            false
        ));
        assert!(!altgr_composition(
            &Key::Named(WinitNamedKey::Enter),
            Some("\r"),
            &Key::Named(WinitNamedKey::Enter),
            false
        ));
    }

    /// AltGr types a character, so the chords bound to `alt+1..9` must not fire;
    /// genuine Meta chords still must.
    #[test]
    fn altgr_does_not_reach_meta_bindings() {
        let altgr = chord_modifiers(ModifiersState::ALT, true);
        let meta = chord_modifiers(ModifiersState::ALT, false);
        let digit = Chord {
            mods: altgr,
            key: ChordKey::Char('1'),
        };
        assert_eq!(
            keymap().action_for(&digit),
            None,
            "AltGr+1 types |, it must not switch to tab 1"
        );
        assert_eq!(
            keymap().action_for(&Chord {
                mods: meta,
                key: ChordKey::Char('1')
            }),
            Some(ActionId::Tab1),
            "Alt+1 is still the tab-1 binding"
        );
    }

    #[test]
    fn an_ime_commit_that_repeats_the_last_key_is_dropped() {
        let now = Instant::now();
        assert!(commit_repeats_key("a", Some(("a", now)), now));
        assert!(!commit_repeats_key("a", Some(("b", now)), now));
        assert!(!commit_repeats_key("a", None, now));
        // Composition takes longer than the echo window: it is new text.
        assert!(!commit_repeats_key(
            "a",
            Some(("a", now - IME_ECHO_WINDOW - Duration::from_millis(1))),
            now
        ));
        // A dead key commits text no key press wrote, so it is always delivered.
        assert!(!commit_repeats_key("á", Some(("a", now)), now));
    }

    #[test]
    fn releases_are_forwarded_only_with_report_event_types() {
        let mut modes = ModeFlags::default();
        assert!(
            !forwards_release(modes),
            "an xterm session must never see a release"
        );

        modes.kitty.disambiguate = true;
        modes.kitty.report_all_keys_as_esc = true;
        modes.kitty.report_associated_text = true;
        assert!(
            !forwards_release(modes),
            "the other Kitty flags do not ask for event types"
        );

        modes.kitty.report_event_types = true;
        assert!(forwards_release(modes));
    }
}
