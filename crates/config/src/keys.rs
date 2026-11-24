//! Key chords, action identifiers and keymap resolution.
//!
//! This module is deliberately independent from `winit`: the configuration
//! schema, the TOML writer, the settings panel and the keymap all speak the same
//! neutral chord type, and the application translates platform key events into
//! it. That keeps keybinding tests free of any window system.

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// Modifier state of a chord.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
pub struct Modifiers {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub sup: bool,
}

impl Modifiers {
    pub const NONE: Self = Self {
        ctrl: false,
        alt: false,
        shift: false,
        sup: false,
    };

    pub const fn ctrl() -> Self {
        Self {
            ctrl: true,
            ..Self::NONE
        }
    }

    pub const fn shift() -> Self {
        Self {
            shift: true,
            ..Self::NONE
        }
    }

    pub const fn alt() -> Self {
        Self {
            alt: true,
            ..Self::NONE
        }
    }

    pub const fn ctrl_shift() -> Self {
        Self {
            ctrl: true,
            shift: true,
            ..Self::NONE
        }
    }

    pub const fn alt_ctrl() -> Self {
        Self {
            ctrl: true,
            alt: true,
            ..Self::NONE
        }
    }

    /// Returns true when no modifier is active.
    pub const fn is_empty(self) -> bool {
        !self.ctrl && !self.alt && !self.shift && !self.sup
    }

    /// Canonical text prefix, used by [`Chord`] formatting.
    fn prefix(self) -> String {
        let mut out = String::new();
        if self.ctrl {
            out.push_str("ctrl+");
        }
        if self.alt {
            out.push_str("alt+");
        }
        if self.shift {
            out.push_str("shift+");
        }
        if self.sup {
            out.push_str("super+");
        }
        out
    }
}

/// Non-character keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NamedKey {
    Escape,
    Enter,
    Tab,
    Backspace,
    Delete,
    Insert,
    Home,
    End,
    PageUp,
    PageDown,
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    Function(u8),
}

impl NamedKey {
    fn parse(token: &str) -> Option<Self> {
        Some(match token {
            "escape" | "esc" => NamedKey::Escape,
            "enter" | "return" => NamedKey::Enter,
            "tab" => NamedKey::Tab,
            "backspace" | "bs" => NamedKey::Backspace,
            "delete" | "del" => NamedKey::Delete,
            "insert" | "ins" => NamedKey::Insert,
            "home" => NamedKey::Home,
            "end" => NamedKey::End,
            "pageup" | "pgup" | "prior" => NamedKey::PageUp,
            "pagedown" | "pgdn" | "next" => NamedKey::PageDown,
            "arrowup" | "up" => NamedKey::ArrowUp,
            "arrowdown" | "down" => NamedKey::ArrowDown,
            "arrowleft" | "left" => NamedKey::ArrowLeft,
            "arrowright" | "right" => NamedKey::ArrowRight,
            other => {
                let digits = other.strip_prefix('f')?;
                let number = digits.parse::<u8>().ok()?;
                if (1..=24).contains(&number) {
                    NamedKey::Function(number)
                } else {
                    return None;
                }
            }
        })
    }

    fn as_str(self) -> String {
        match self {
            NamedKey::Escape => "escape".to_owned(),
            NamedKey::Enter => "enter".to_owned(),
            NamedKey::Tab => "tab".to_owned(),
            NamedKey::Backspace => "backspace".to_owned(),
            NamedKey::Delete => "delete".to_owned(),
            NamedKey::Insert => "insert".to_owned(),
            NamedKey::Home => "home".to_owned(),
            NamedKey::End => "end".to_owned(),
            NamedKey::PageUp => "pageup".to_owned(),
            NamedKey::PageDown => "pagedown".to_owned(),
            NamedKey::ArrowUp => "up".to_owned(),
            NamedKey::ArrowDown => "down".to_owned(),
            NamedKey::ArrowLeft => "left".to_owned(),
            NamedKey::ArrowRight => "right".to_owned(),
            NamedKey::Function(number) => format!("f{number}"),
        }
    }
}

/// The key part of a chord.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Key {
    /// A physical-logical character, always stored lowercase.
    Char(char),
    Named(NamedKey),
}

impl Key {
    fn parse(token: &str) -> Option<Self> {
        let token = token.trim().to_ascii_lowercase();
        if token.is_empty() {
            return None;
        }
        if let Some(named) = NamedKey::parse(&token) {
            return Some(Key::Named(named));
        }
        let mut chars = token.chars();
        let first = chars.next()?;
        if chars.next().is_some() {
            // Multi-character tokens that are not named keys.
            return match token.as_str() {
                "space" => Some(Key::Char(' ')),
                "comma" => Some(Key::Char(',')),
                "period" | "dot" => Some(Key::Char('.')),
                "minus" => Some(Key::Char('-')),
                "plus" => Some(Key::Char('+')),
                "equal" => Some(Key::Char('=')),
                "slash" => Some(Key::Char('/')),
                "backslash" => Some(Key::Char('\\')),
                "semicolon" => Some(Key::Char(';')),
                "apostrophe" | "quote" => Some(Key::Char('\'')),
                "bracketleft" => Some(Key::Char('[')),
                "bracketright" => Some(Key::Char(']')),
                "backtick" | "grave" => Some(Key::Char('`')),
                _ => None,
            };
        }
        Some(Key::Char(first))
    }

    fn as_str(self) -> String {
        match self {
            Key::Char(' ') => "space".to_owned(),
            Key::Char(',') => "comma".to_owned(),
            Key::Char('+') => "plus".to_owned(),
            Key::Char('-') => "minus".to_owned(),
            Key::Char('=') => "equal".to_owned(),
            Key::Char(other) => other.to_ascii_lowercase().to_string(),
            Key::Named(named) => named.as_str(),
        }
    }

    /// Best-effort character produced by this key with `shift` applied.
    pub fn shifted_char(self) -> Option<char> {
        match self {
            Key::Char('1') => Some('!'),
            Key::Char('2') => Some('@'),
            Key::Char('3') => Some('#'),
            Key::Char('4') => Some('$'),
            Key::Char('5') => Some('%'),
            Key::Char('6') => Some('^'),
            Key::Char('7') => Some('&'),
            Key::Char('8') => Some('*'),
            Key::Char('9') => Some('('),
            Key::Char('0') => Some(')'),
            Key::Char('-') => Some('_'),
            Key::Char('=') => Some('+'),
            Key::Char('[') => Some('{'),
            Key::Char(']') => Some('}'),
            Key::Char('\\') => Some('|'),
            Key::Char(';') => Some(':'),
            Key::Char('\'') => Some('"'),
            Key::Char(',') => Some('<'),
            Key::Char('.') => Some('>'),
            Key::Char('/') => Some('?'),
            Key::Char('`') => Some('~'),
            Key::Char(other) => Some(other.to_ascii_uppercase()),
            Key::Named(_) => None,
        }
    }
}

/// A key plus modifiers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Chord {
    pub mods: Modifiers,
    pub key: Key,
}

impl Chord {
    pub const fn new(mods: Modifiers, key: Key) -> Self {
        Self { mods, key }
    }

    pub const fn plain(key: Key) -> Self {
        Self {
            mods: Modifiers::NONE,
            key,
        }
    }
}

impl fmt::Display for Chord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", self.mods.prefix(), self.key.as_str())
    }
}

/// Error returned when a chord string cannot be parsed.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("invalid key chord `{chord}`: {reason}")]
pub struct ChordParseError {
    pub chord: String,
    pub reason: String,
}

impl FromStr for Chord {
    type Err = ChordParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let text = value.trim().to_ascii_lowercase();
        let mut mods = Modifiers::NONE;
        let mut parts: Vec<&str> = text.split('+').map(str::trim).collect();

        // A trailing `+` means the key itself is `+`.
        if parts.last().is_some_and(|part| part.is_empty()) {
            parts.pop();
            parts.push("plus");
        }
        if parts.is_empty() {
            return Err(ChordParseError {
                chord: value.to_owned(),
                reason: "empty chord".to_owned(),
            });
        }

        let key_token = parts.pop().unwrap_or_default();
        for modifier in parts {
            match modifier {
                "ctrl" | "control" => mods.ctrl = true,
                "alt" | "option" | "opt" => mods.alt = true,
                "shift" => mods.shift = true,
                "super" | "meta" | "cmd" | "logo" | "win" => mods.sup = true,
                "" => {}
                other => {
                    return Err(ChordParseError {
                        chord: value.to_owned(),
                        reason: format!("unknown modifier `{other}`"),
                    });
                }
            }
        }

        let key = Key::parse(key_token).ok_or_else(|| ChordParseError {
            chord: value.to_owned(),
            reason: format!("unknown key `{key_token}`"),
        })?;
        Ok(Chord { mods, key })
    }
}

/// Parses a `|`-separated list of chords; a comma is a key name, not a separator.
pub fn parse_chord_list(value: &str) -> Result<Vec<Chord>, ChordParseError> {
    let mut out = Vec::new();
    for part in value.split('|') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        out.push(part.parse()?);
    }
    if out.is_empty() {
        return Err(ChordParseError {
            chord: value.to_owned(),
            reason: "no chord in list".to_owned(),
        });
    }
    Ok(out)
}

/// Every user-configurable action.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ActionId {
    NewTab,
    CloseTab,
    RenameTab,
    NextTab,
    PreviousTab,
    MoveTabLeft,
    MoveTabRight,
    Tab1,
    Tab2,
    Tab3,
    Tab4,
    Tab5,
    Tab6,
    Tab7,
    Tab8,
    Tab9,
    NewSplitHorizontal,
    NewSplitVertical,
    ClosePane,
    FocusLeft,
    FocusRight,
    FocusUp,
    FocusDown,
    ResizeLeft,
    ResizeRight,
    ResizeUp,
    ResizeDown,
    EqualizeSplits,
    ToggleZoom,
    IncreaseFontSize,
    DecreaseFontSize,
    ResetFontSize,
    Search,
    SearchNext,
    SearchPrevious,
    Copy,
    Paste,
    ScrollPageUp,
    ScrollPageDown,
    ScrollToTop,
    ScrollToBottom,
    OpenSettings,
    ReloadConfig,
    ToggleStatusBar,
    NewWindow,
    ToggleFloating,
    SendLiteralNext,
    Quit,
}

impl ActionId {
    pub const ALL: [ActionId; 48] = [
        ActionId::NewTab,
        ActionId::CloseTab,
        ActionId::RenameTab,
        ActionId::NextTab,
        ActionId::PreviousTab,
        ActionId::MoveTabLeft,
        ActionId::MoveTabRight,
        ActionId::Tab1,
        ActionId::Tab2,
        ActionId::Tab3,
        ActionId::Tab4,
        ActionId::Tab5,
        ActionId::Tab6,
        ActionId::Tab7,
        ActionId::Tab8,
        ActionId::Tab9,
        ActionId::NewSplitHorizontal,
        ActionId::NewSplitVertical,
        ActionId::ClosePane,
        ActionId::FocusLeft,
        ActionId::FocusRight,
        ActionId::FocusUp,
        ActionId::FocusDown,
        ActionId::ResizeLeft,
        ActionId::ResizeRight,
        ActionId::ResizeUp,
        ActionId::ResizeDown,
        ActionId::EqualizeSplits,
        ActionId::ToggleZoom,
        ActionId::IncreaseFontSize,
        ActionId::DecreaseFontSize,
        ActionId::ResetFontSize,
        ActionId::Search,
        ActionId::SearchNext,
        ActionId::SearchPrevious,
        ActionId::Copy,
        ActionId::Paste,
        ActionId::ScrollPageUp,
        ActionId::ScrollPageDown,
        ActionId::ScrollToTop,
        ActionId::ScrollToBottom,
        ActionId::OpenSettings,
        ActionId::ReloadConfig,
        ActionId::ToggleStatusBar,
        ActionId::NewWindow,
        ActionId::ToggleFloating,
        ActionId::SendLiteralNext,
        ActionId::Quit,
    ];

    /// Kebab-case identifier used in TOML and in the settings panel.
    pub const fn as_str(self) -> &'static str {
        match self {
            ActionId::NewTab => "new-tab",
            ActionId::CloseTab => "close-tab",
            ActionId::RenameTab => "rename-tab",
            ActionId::NextTab => "next-tab",
            ActionId::PreviousTab => "previous-tab",
            ActionId::MoveTabLeft => "move-tab-left",
            ActionId::MoveTabRight => "move-tab-right",
            ActionId::Tab1 => "tab-1",
            ActionId::Tab2 => "tab-2",
            ActionId::Tab3 => "tab-3",
            ActionId::Tab4 => "tab-4",
            ActionId::Tab5 => "tab-5",
            ActionId::Tab6 => "tab-6",
            ActionId::Tab7 => "tab-7",
            ActionId::Tab8 => "tab-8",
            ActionId::Tab9 => "tab-9",
            ActionId::NewSplitHorizontal => "new-split-horizontal",
            ActionId::NewSplitVertical => "new-split-vertical",
            ActionId::ClosePane => "close-pane",
            ActionId::FocusLeft => "focus-left",
            ActionId::FocusRight => "focus-right",
            ActionId::FocusUp => "focus-up",
            ActionId::FocusDown => "focus-down",
            ActionId::ResizeLeft => "resize-left",
            ActionId::ResizeRight => "resize-right",
            ActionId::ResizeUp => "resize-up",
            ActionId::ResizeDown => "resize-down",
            ActionId::EqualizeSplits => "equalize-splits",
            ActionId::ToggleZoom => "toggle-zoom",
            ActionId::IncreaseFontSize => "increase-font-size",
            ActionId::DecreaseFontSize => "decrease-font-size",
            ActionId::ResetFontSize => "reset-font-size",
            ActionId::Search => "search",
            ActionId::SearchNext => "search-next",
            ActionId::SearchPrevious => "search-previous",
            ActionId::Copy => "copy",
            ActionId::Paste => "paste",
            ActionId::ScrollPageUp => "scroll-page-up",
            ActionId::ScrollPageDown => "scroll-page-down",
            ActionId::ScrollToTop => "scroll-to-top",
            ActionId::ScrollToBottom => "scroll-to-bottom",
            ActionId::OpenSettings => "open-settings",
            ActionId::ReloadConfig => "reload-config",
            ActionId::ToggleStatusBar => "toggle-status-bar",
            ActionId::NewWindow => "new-window",
            ActionId::ToggleFloating => "toggle-floating",
            ActionId::SendLiteralNext => "send-literal-next",
            ActionId::Quit => "quit",
        }
    }

    /// Human readable label for the settings panel.
    pub const fn label(self) -> &'static str {
        match self {
            ActionId::NewTab => "New tab",
            ActionId::CloseTab => "Close tab",
            ActionId::RenameTab => "Rename tab",
            ActionId::NextTab => "Next tab",
            ActionId::PreviousTab => "Previous tab",
            ActionId::MoveTabLeft => "Move tab left",
            ActionId::MoveTabRight => "Move tab right",
            ActionId::Tab1 => "Go to tab 1",
            ActionId::Tab2 => "Go to tab 2",
            ActionId::Tab3 => "Go to tab 3",
            ActionId::Tab4 => "Go to tab 4",
            ActionId::Tab5 => "Go to tab 5",
            ActionId::Tab6 => "Go to tab 6",
            ActionId::Tab7 => "Go to tab 7",
            ActionId::Tab8 => "Go to tab 8",
            ActionId::Tab9 => "Go to tab 9",
            ActionId::NewSplitHorizontal => "Split horizontally",
            ActionId::NewSplitVertical => "Split vertically",
            ActionId::ClosePane => "Close pane",
            ActionId::FocusLeft => "Focus pane left",
            ActionId::FocusRight => "Focus pane right",
            ActionId::FocusUp => "Focus pane up",
            ActionId::FocusDown => "Focus pane down",
            ActionId::ResizeLeft => "Resize pane left",
            ActionId::ResizeRight => "Resize pane right",
            ActionId::ResizeUp => "Resize pane up",
            ActionId::ResizeDown => "Resize pane down",
            ActionId::EqualizeSplits => "Equalize splits",
            ActionId::ToggleZoom => "Zoom pane",
            ActionId::IncreaseFontSize => "Increase font size",
            ActionId::DecreaseFontSize => "Decrease font size",
            ActionId::ResetFontSize => "Reset font size",
            ActionId::Search => "Search scrollback",
            ActionId::SearchNext => "Next match",
            ActionId::SearchPrevious => "Previous match",
            ActionId::Copy => "Copy selection",
            ActionId::Paste => "Paste",
            ActionId::ScrollPageUp => "Scroll page up",
            ActionId::ScrollPageDown => "Scroll page down",
            ActionId::ScrollToTop => "Scroll to top",
            ActionId::ScrollToBottom => "Scroll to bottom",
            ActionId::OpenSettings => "Open settings",
            ActionId::ReloadConfig => "Reload configuration",
            ActionId::ToggleStatusBar => "Toggle status bar",
            ActionId::NewWindow => "New window",
            ActionId::ToggleFloating => "Show or hide the floating terminal",
            ActionId::SendLiteralNext => "Send next key literally",
            ActionId::Quit => "Quit",
        }
    }
}

impl fmt::Display for ActionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ActionId {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        ActionId::ALL
            .iter()
            .copied()
            .find(|action| action.as_str() == value.trim())
            .ok_or_else(|| format!("unknown action `{value}`"))
    }
}

/// Default keybindings, matching the specification's suggested defaults.
pub fn default_keybindings() -> BTreeMap<String, String> {
    use ActionId::*;
    let entries: &[(ActionId, &str)] = &[
        (NewTab, "ctrl+shift+t"),
        (CloseTab, "ctrl+shift+alt+w"),
        (RenameTab, "ctrl+shift+r"),
        (NextTab, "ctrl+pagedown"),
        (PreviousTab, "ctrl+pageup"),
        (MoveTabLeft, "ctrl+shift+alt+left"),
        (MoveTabRight, "ctrl+shift+alt+right"),
        (Tab1, "alt+1"),
        (Tab2, "alt+2"),
        (Tab3, "alt+3"),
        (Tab4, "alt+4"),
        (Tab5, "alt+5"),
        (Tab6, "alt+6"),
        (Tab7, "alt+7"),
        (Tab8, "alt+8"),
        (Tab9, "alt+9"),
        (NewSplitHorizontal, "ctrl+shift+d"),
        (NewSplitVertical, "ctrl+shift+e"),
        (ClosePane, "ctrl+shift+w"),
        (FocusLeft, "alt+left"),
        (FocusRight, "alt+right"),
        (FocusUp, "alt+up"),
        (FocusDown, "alt+down"),
        (ResizeLeft, "ctrl+alt+left"),
        (ResizeRight, "ctrl+alt+right"),
        (ResizeUp, "ctrl+alt+up"),
        (ResizeDown, "ctrl+alt+down"),
        (EqualizeSplits, "ctrl+shift+0"),
        (ToggleZoom, "ctrl+shift+z"),
        (IncreaseFontSize, "ctrl+=|ctrl+plus"),
        (DecreaseFontSize, "ctrl+minus"),
        (ResetFontSize, "ctrl+0"),
        (Search, "ctrl+shift+f"),
        (SearchNext, "ctrl+g"),
        (SearchPrevious, "ctrl+shift+g"),
        (Copy, "ctrl+shift+c"),
        (Paste, "ctrl+shift+v"),
        (ScrollPageUp, "shift+pageup"),
        (ScrollPageDown, "shift+pagedown"),
        (ScrollToTop, "ctrl+shift+home"),
        (ScrollToBottom, "ctrl+shift+end"),
        (OpenSettings, "ctrl+comma"),
        (ReloadConfig, "ctrl+shift+comma"),
        (ToggleStatusBar, "ctrl+shift+s"),
        (NewWindow, "ctrl+shift+n"),
        (ToggleFloating, "ctrl+shift+grave"),
        (SendLiteralNext, "ctrl+shift+enter"),
        (Quit, "ctrl+shift+q"),
    ];
    entries
        .iter()
        .map(|(action, chord)| (action.as_str().to_owned(), (*chord).to_owned()))
        .collect()
}

/// A parsed keymap: chord to action.
#[derive(Clone, Debug, Default)]
pub struct Keymap {
    bindings: BTreeMap<Chord, ActionId>,
    /// Actions whose chord could not be parsed, kept for reporting.
    invalid: Vec<(ActionId, String, String)>,
    /// Chords claimed by more than one action.
    conflicts: Vec<(Chord, ActionId, ActionId)>,
}

impl Keymap {
    /// Builds a keymap from the configuration map.
    ///
    /// Invalid chord strings and duplicate chords never panic: they are recorded
    /// so validation can report them, and the first action in alphabetical order
    /// keeps the chord.
    pub fn build(raw: &BTreeMap<String, String>) -> Self {
        let mut bindings = BTreeMap::new();
        let mut invalid = Vec::new();
        let mut conflicts = Vec::new();

        for (name, value) in raw {
            let Ok(action) = name.parse::<ActionId>() else {
                invalid.push((
                    ActionId::Quit,
                    value.clone(),
                    format!("unknown action `{name}`"),
                ));
                continue;
            };
            match parse_chord_list(value) {
                Ok(chords) => {
                    for chord in chords {
                        if let Some(existing) = bindings.get(&chord) {
                            if *existing != action {
                                conflicts.push((chord, *existing, action));
                            }
                        } else {
                            bindings.insert(chord, action);
                        }
                    }
                }
                Err(error) => invalid.push((action, value.clone(), error.to_string())),
            }
        }

        Self {
            bindings,
            invalid,
            conflicts,
        }
    }

    /// Resolves a chord to its action.
    pub fn action_for(&self, chord: &Chord) -> Option<ActionId> {
        self.bindings.get(chord).copied()
    }

    pub fn bindings(&self) -> &BTreeMap<Chord, ActionId> {
        &self.bindings
    }

    pub fn invalid(&self) -> &[(ActionId, String, String)] {
        &self.invalid
    }

    pub fn conflicts(&self) -> &[(Chord, ActionId, ActionId)] {
        &self.conflicts
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_spec_default_chords() {
        let chord: Chord = "ctrl+shift+t".parse().unwrap();
        assert_eq!(chord.mods, Modifiers::ctrl_shift());
        assert_eq!(chord.key, Key::Char('t'));
        assert_eq!(chord.to_string(), "ctrl+shift+t");

        let chord: Chord = "ctrl+=".parse().unwrap();
        assert_eq!(chord.key, Key::Char('='));

        let chord: Chord = "ctrl+,".parse().unwrap();
        assert_eq!(chord.key, Key::Char(','));

        let chord: Chord = "ctrl+-".parse().unwrap();
        assert_eq!(chord.key, Key::Char('-'));

        let chord: Chord = "ctrl+0".parse().unwrap();
        assert_eq!(chord.key, Key::Char('0'));

        let chord: Chord = "alt+arrowleft".parse().unwrap();
        assert_eq!(chord.key, Key::Named(NamedKey::ArrowLeft));
        assert_eq!(chord.to_string(), "alt+left");

        let chord: Chord = "ctrl+pagedown".parse().unwrap();
        assert_eq!(chord.key, Key::Named(NamedKey::PageDown));

        let chord: Chord = "f5".parse().unwrap();
        assert_eq!(chord.key, Key::Named(NamedKey::Function(5)));
    }

    #[test]
    fn parses_plus_key_and_named_comma() {
        let chord: Chord = "ctrl+plus".parse().unwrap();
        assert_eq!(chord.key, Key::Char('+'));
        // A trailing `+` denotes the plus key itself.
        let chord: Chord = "ctrl+".parse().unwrap();
        assert_eq!(chord.key, Key::Char('+'));
        // Space is spelled out, because strings are trimmed before parsing.
        let chord: Chord = "ctrl+space".parse().unwrap();
        assert_eq!(chord.key, Key::Char(' '));
        let chord: Chord = "ctrl+comma".parse().unwrap();
        assert_eq!(chord.key, Key::Char(','));
    }

    #[test]
    fn chord_lists_use_pipe_separator() {
        let chords = parse_chord_list("ctrl+=|ctrl+plus").unwrap();
        assert_eq!(chords.len(), 2);
        assert_eq!(chords[1].key, Key::Char('+'));
    }

    #[test]
    fn rejects_unknown_keys_and_modifiers() {
        assert!("ctrl+nope".parse::<Chord>().is_err());
        assert!("hyper+t".parse::<Chord>().is_err());
    }

    #[test]
    fn default_keymap_matches_specification_table() {
        let keymap = Keymap::build(&default_keybindings());
        let cases = [
            ("ctrl+shift+t", ActionId::NewTab),
            ("ctrl+shift+w", ActionId::ClosePane),
            ("ctrl+shift+alt+w", ActionId::CloseTab),
            ("ctrl+pagedown", ActionId::NextTab),
            ("ctrl+pageup", ActionId::PreviousTab),
            ("ctrl+shift+d", ActionId::NewSplitHorizontal),
            ("ctrl+shift+e", ActionId::NewSplitVertical),
            ("alt+left", ActionId::FocusLeft),
            ("ctrl+=", ActionId::IncreaseFontSize),
            ("ctrl+plus", ActionId::IncreaseFontSize),
            ("ctrl+minus", ActionId::DecreaseFontSize),
            ("ctrl+0", ActionId::ResetFontSize),
            ("ctrl+shift+f", ActionId::Search),
            ("ctrl+shift+c", ActionId::Copy),
            ("ctrl+shift+v", ActionId::Paste),
            ("ctrl+comma", ActionId::OpenSettings),
            ("ctrl+shift+n", ActionId::NewWindow),
            ("ctrl+shift+grave", ActionId::ToggleFloating),
        ];
        for (chord, action) in cases {
            let chord: Chord = chord.parse().unwrap();
            assert_eq!(keymap.action_for(&chord), Some(action), "{chord}");
        }
    }

    #[test]
    fn the_floating_terminal_is_bound_by_default() {
        let keymap = Keymap::build(&default_keybindings());
        let chord: Chord = "ctrl+shift+grave".parse().unwrap();
        assert_eq!(chord.key, Key::Char('`'));
        assert_eq!(keymap.action_for(&chord), Some(ActionId::ToggleFloating));
        assert_eq!(ActionId::ToggleFloating.as_str(), "toggle-floating");
        assert_eq!(
            ActionId::ToggleFloating.label(),
            "Show or hide the floating terminal"
        );
        assert_eq!(
            default_keybindings()
                .get("toggle-floating")
                .map(String::as_str),
            Some("ctrl+shift+grave")
        );
    }

    #[test]
    fn duplicate_chord_is_reported_not_panicking() {
        let mut raw = BTreeMap::new();
        raw.insert("copy".to_owned(), "ctrl+shift+x".to_owned());
        raw.insert("paste".to_owned(), "ctrl+shift+x".to_owned());
        let keymap = Keymap::build(&raw);
        assert_eq!(keymap.conflicts().len(), 1);
        assert_eq!(
            keymap.action_for(&"ctrl+shift+x".parse().unwrap()),
            Some(ActionId::Copy),
            "deterministic winner: alphabetically first action"
        );
    }

    #[test]
    fn unknown_action_name_is_reported() {
        let mut raw = BTreeMap::new();
        raw.insert("not-an-action".to_owned(), "ctrl+t".to_owned());
        let keymap = Keymap::build(&raw);
        assert_eq!(keymap.invalid().len(), 1);
    }

    #[test]
    fn every_action_round_trips_through_its_name() {
        for action in ActionId::ALL {
            assert_eq!(action.as_str().parse::<ActionId>().unwrap(), action);
        }
    }

    #[test]
    fn defaults_cover_every_action() {
        let defaults = default_keybindings();
        for action in ActionId::ALL {
            assert!(
                defaults.contains_key(action.as_str()),
                "missing default for {action}"
            );
        }
    }
}
