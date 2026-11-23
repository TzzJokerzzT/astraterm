//! Input encoding: neutral key and mouse events to VT/xterm byte sequences.
//!
//! This is deliberately independent from `winit`: the application translates
//! platform events into [`KeyPress`]/[`MouseEvent`] and this module produces the
//! exact bytes a real terminal would send, honouring the modes the running
//! application requested (application cursor keys, bracketed paste, mouse
//! reporting, SGR mouse).

use crate::snapshot::{KittyFlags, ModeFlags};

/// Arrow/relative movement direction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Up,
    Down,
    Left,
    Right,
}

/// A keyboard key, independent from any platform.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TermKey {
    Char(char),
    Enter,
    Tab,
    Backspace,
    Escape,
    Delete,
    Insert,
    Home,
    End,
    PageUp,
    PageDown,
    Arrow(Direction),
    Function(u8),
}

/// A key press with its modifiers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyPress {
    pub key: TermKey,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub event: KeyEventType,
}

/// Key event kinds the Kitty keyboard protocol can distinguish.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum KeyEventType {
    #[default]
    Press,
    Repeat,
    Release,
}

impl KeyEventType {
    /// Value of the protocol's event-type parameter.
    const fn code(self) -> u8 {
        match self {
            Self::Press => 1,
            Self::Repeat => 2,
            Self::Release => 3,
        }
    }
}

impl KeyPress {
    pub const fn new(key: TermKey) -> Self {
        Self {
            key,
            ctrl: false,
            alt: false,
            shift: false,
            event: KeyEventType::Press,
        }
    }

    pub const fn with_mods(key: TermKey, ctrl: bool, alt: bool, shift: bool) -> Self {
        Self {
            key,
            ctrl,
            alt,
            shift,
            event: KeyEventType::Press,
        }
    }

    /// Marks the press as a repeat or a release.
    pub const fn with_event(mut self, event: KeyEventType) -> Self {
        self.event = event;
        self
    }

    /// xterm modifier parameter: 1 + shift(1) + alt(2) + ctrl(4).
    fn modifier_param(&self) -> u8 {
        1 + u8::from(self.shift) + 2 * u8::from(self.alt) + 4 * u8::from(self.ctrl)
    }

    fn has_modifiers(&self) -> bool {
        self.ctrl || self.alt || self.shift
    }
}

/// Encodes a key press into the bytes the PTY should receive.
///
/// Returns an empty vector when the combination has no representation (for
/// example a bare modifier), in which case nothing must be sent.
pub fn encode_key(press: KeyPress, modes: ModeFlags) -> Vec<u8> {
    if press.event != KeyEventType::Press {
        // Only the Kitty protocol reports anything other than a press. Falling back
        // to the xterm path would send the key twice, once on press and once on
        // release.
        if !modes.kitty.enabled() {
            return Vec::new();
        }
        return encode_kitty_key(press, modes.kitty).unwrap_or_default();
    }
    if modes.kitty.enabled()
        && let Some(bytes) = encode_kitty_key(press, modes.kitty)
    {
        return bytes;
    }
    encode_xterm_key(press, modes)
}

/// The classic xterm encoding, unchanged since before the Kitty protocol existed.
fn encode_xterm_key(press: KeyPress, modes: ModeFlags) -> Vec<u8> {
    match press.key {
        TermKey::Char(ch) => encode_char(ch, press),
        TermKey::Enter => {
            let mut out = Vec::new();
            if press.alt {
                out.push(0x1b);
            }
            out.push(b'\r');
            out
        }
        TermKey::Tab if press.shift => b"\x1b[Z".to_vec(),
        TermKey::Tab => vec![b'\t'],
        TermKey::Backspace => {
            let byte = if press.ctrl { 0x08 } else { 0x7f };
            with_alt(vec![byte], press.alt)
        }
        TermKey::Escape => with_alt(vec![0x1b], press.alt),
        TermKey::Arrow(direction) => {
            let final_byte = match direction {
                Direction::Up => b'A',
                Direction::Down => b'B',
                Direction::Right => b'C',
                Direction::Left => b'D',
            };
            if modes.app_cursor && !press.has_modifiers() {
                vec![0x1b, b'O', final_byte]
            } else {
                csi_with_modifier(final_byte, press.modifier_param())
            }
        }
        TermKey::Home => csi_with_modifier(b'H', press.modifier_param()),
        TermKey::End => csi_with_modifier(b'F', press.modifier_param()),
        TermKey::Insert => tilde_sequence(2, press.modifier_param()),
        TermKey::Delete => tilde_sequence(3, press.modifier_param()),
        TermKey::PageUp => tilde_sequence(5, press.modifier_param()),
        TermKey::PageDown => tilde_sequence(6, press.modifier_param()),
        TermKey::Function(number) => encode_function(number, press),
    }
}

/// Encodes a key in the Kitty `CSI u` form, or `None` when the key keeps its xterm
/// or plain-text encoding.
fn encode_kitty_key(press: KeyPress, kitty: KittyFlags) -> Option<Vec<u8>> {
    if press.event != KeyEventType::Press && !kitty.report_event_types {
        return None;
    }
    if !kitty_escape_form(press, kitty) {
        return None;
    }
    let code = kitty_key_code(press.key)?;
    let number = kitty_key_number(press, kitty, code);
    let text = if kitty.report_associated_text {
        kitty_text(press)
    } else {
        Vec::new()
    };
    let modifiers = press.modifier_param();
    let report_event = kitty.report_event_types;

    let mut out = Vec::with_capacity(16);
    out.extend_from_slice(b"\x1b[");
    out.extend_from_slice(number.as_bytes());
    // The modifier parameter is optional only when it is the default and nothing
    // follows it; the protocol otherwise needs the placeholder.
    if modifiers != 1 || report_event || !text.is_empty() {
        out.push(b';');
        out.extend_from_slice(modifiers.to_string().as_bytes());
        if report_event {
            out.push(b':');
            out.push(b'0' + press.event.code());
        }
    }
    if !text.is_empty() {
        out.push(b';');
        for (index, codepoint) in text.iter().enumerate() {
            if index > 0 {
                out.push(b':');
            }
            out.extend_from_slice(codepoint.to_string().as_bytes());
        }
    }
    out.push(b'u');
    Some(out)
}

/// Whether the protocol reports this key as `CSI u` instead of its xterm form.
fn kitty_escape_form(press: KeyPress, kitty: KittyFlags) -> bool {
    if kitty.report_all_keys_as_esc {
        return true;
    }
    if !kitty.disambiguate {
        return false;
    }
    match press.key {
        // Escape alone is indistinguishable from the start of a sequence.
        TermKey::Escape => true,
        // Unmodified Enter/Tab/Backspace stay as their control bytes.
        TermKey::Enter | TermKey::Tab | TermKey::Backspace => press.has_modifiers(),
        TermKey::Char(_) => press.ctrl || press.alt || (kitty.report_alternate_keys && press.shift),
        // Arrows and the other functional keys already have unambiguous forms.
        _ => false,
    }
}

/// The key number, with the alternate codepoints when the application asked for them.
fn kitty_key_number(press: KeyPress, kitty: KittyFlags, code: u32) -> String {
    if kitty.report_alternate_keys
        && press.shift
        && let TermKey::Char(ch) = press.key
        && let Some(shifted) = shifted_character(ch)
    {
        return format!("{code}:{}:{code}", shifted as u32);
    }
    code.to_string()
}

/// Kitty key numbers: Unicode codepoints for text, fixed numbers for functional keys.
fn kitty_key_code(key: TermKey) -> Option<u32> {
    Some(match key {
        TermKey::Char(ch) => ch as u32,
        TermKey::Enter => 13,
        TermKey::Tab => 9,
        TermKey::Backspace => 127,
        TermKey::Escape => 27,
        TermKey::Insert => 57_348,
        TermKey::Delete => 57_349,
        TermKey::Arrow(Direction::Left) => 57_350,
        TermKey::Arrow(Direction::Right) => 57_351,
        TermKey::Arrow(Direction::Up) => 57_352,
        TermKey::Arrow(Direction::Down) => 57_353,
        TermKey::PageUp => 57_354,
        TermKey::PageDown => 57_355,
        TermKey::Home => 57_356,
        TermKey::End => 57_357,
        TermKey::Function(number) if (1..=35).contains(&number) => 57_363 + number as u32,
        TermKey::Function(_) => return None,
    })
}

/// Codepoints of the text a key produced, for the associated-text parameter.
fn kitty_text(press: KeyPress) -> Vec<u32> {
    if press.ctrl || press.alt {
        return Vec::new();
    }
    match press.key {
        TermKey::Char(ch) => {
            let ch = if press.shift {
                shifted_character(ch).unwrap_or(ch)
            } else {
                ch
            };
            vec![ch as u32]
        }
        _ => Vec::new(),
    }
}

fn encode_char(ch: char, press: KeyPress) -> Vec<u8> {
    // Control characters produced by Ctrl+<key>, following the xterm mapping.
    if press.ctrl
        && let Some(control) = control_byte(ch)
    {
        return with_alt(vec![control], press.alt);
    }

    let mut text = String::new();
    if press.shift {
        match crate::input::shifted_character(ch) {
            Some(shifted) => text.push(shifted),
            None => text.push(ch),
        }
    } else {
        text.push(ch);
    }
    let mut bytes = Vec::new();
    if press.alt {
        bytes.push(0x1b);
    }
    bytes.extend_from_slice(text.as_bytes());
    bytes
}

/// Maps a character to its Ctrl control code.
fn control_byte(ch: char) -> Option<u8> {
    let lower = ch.to_ascii_lowercase();
    match lower {
        'a'..='z' => Some(lower as u8 - b'a' + 1),
        '[' => Some(0x1b),
        '\\' => Some(0x1c),
        ']' => Some(0x1d),
        '^' => Some(0x1e),
        '_' => Some(0x1f),
        '@' => Some(0x00),
        ' ' => Some(0x00),
        '2' => Some(0x00),
        '3' => Some(0x1b),
        '/' => Some(0x1f),
        _ => None,
    }
}

/// Shifted variant of a printable key, for layouts where the platform reports the
/// unshifted character only.
pub(crate) fn shifted_character(ch: char) -> Option<char> {
    Some(match ch {
        '1' => '!',
        '2' => '@',
        '3' => '#',
        '4' => '$',
        '5' => '%',
        '6' => '^',
        '7' => '&',
        '8' => '*',
        '9' => '(',
        '0' => ')',
        '-' => '_',
        '=' => '+',
        '[' => '{',
        ']' => '}',
        '\\' => '|',
        ';' => ':',
        '\'' => '"',
        ',' => '<',
        '.' => '>',
        '/' => '?',
        '`' => '~',
        other if other.is_ascii_lowercase() => other.to_ascii_uppercase(),
        _ => return None,
    })
}

fn with_alt(mut bytes: Vec<u8>, alt: bool) -> Vec<u8> {
    if alt {
        bytes.insert(0, 0x1b);
    }
    bytes
}

/// `CSI 1 ; <modifier> <final>` when modified, plain `CSI <final>` otherwise.
fn csi_with_modifier(final_byte: u8, modifier: u8) -> Vec<u8> {
    if modifier <= 1 {
        vec![0x1b, b'[', final_byte]
    } else {
        let mut out = b"\x1b[1;".to_vec();
        out.extend_from_slice(modifier.to_string().as_bytes());
        out.push(final_byte);
        out
    }
}

/// `CSI <code> ~` or `CSI <code> ; <modifier> ~`.
fn tilde_sequence(code: u8, modifier: u8) -> Vec<u8> {
    let mut out = b"\x1b[".to_vec();
    out.extend_from_slice(code.to_string().as_bytes());
    if modifier > 1 {
        out.push(b';');
        out.extend_from_slice(modifier.to_string().as_bytes());
    }
    out.push(b'~');
    out
}

fn encode_function(number: u8, press: KeyPress) -> Vec<u8> {
    let modifier = press.modifier_param();
    let code: u8 = match number {
        1 => {
            if modifier > 1 {
                return csi_with_modifier(b'P', modifier);
            }
            return b"\x1bOP".to_vec();
        }
        2 => {
            if modifier > 1 {
                return csi_with_modifier(b'Q', modifier);
            }
            return b"\x1bOQ".to_vec();
        }
        3 => {
            if modifier > 1 {
                return csi_with_modifier(b'R', modifier);
            }
            return b"\x1bOR".to_vec();
        }
        4 => {
            if modifier > 1 {
                return csi_with_modifier(b'S', modifier);
            }
            return b"\x1bOS".to_vec();
        }
        5 => 15,
        6 => 17,
        7 => 18,
        8 => 19,
        9 => 20,
        10 => 21,
        11 => 23,
        12 => 24,
        13 => 25,
        14 => 26,
        15 => 28,
        16 => 29,
        17 => 31,
        18 => 32,
        19 => 33,
        20 => 34,
        _ => return Vec::new(),
    };
    tilde_sequence(code, modifier)
}

/// Encodes pasted text, wrapping it in bracketed-paste markers when the running
/// application asked for that mode.
pub fn encode_paste(text: &str, modes: ModeFlags) -> Vec<u8> {
    // Normalise line endings: terminals send CR inside a paste.
    let normalised = text.replace("\r\n", "\r").replace('\n', "\r");
    if modes.bracketed_paste {
        let mut out = b"\x1b[200~".to_vec();
        out.extend_from_slice(normalised.as_bytes());
        out.extend_from_slice(b"\x1b[201~");
        out
    } else {
        normalised.into_bytes()
    }
}

/// Mouse gesture kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseKind {
    Press,
    Release,
    Motion,
}

/// Which button a mouse event refers to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    None,
}

/// A mouse event in grid coordinates (zero-based column and row).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MouseEvent {
    pub kind: MouseKind,
    pub button: MouseButton,
    pub column: u16,
    pub row: u16,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

impl MouseEvent {
    fn button_code(&self) -> u8 {
        match self.button {
            MouseButton::Left => 0,
            MouseButton::Middle => 1,
            MouseButton::Right => 2,
            MouseButton::None => 3,
        }
    }

    fn modifier_bits(&self) -> u8 {
        u8::from(self.shift) * 4 + u8::from(self.alt) * 8 + u8::from(self.ctrl) * 16
    }
}

/// Encodes a mouse event for the application, honouring its reporting mode.
///
/// Returns an empty vector when the application does not want mouse input, in
/// which case the UI should handle the event locally (selection, scrolling).
pub fn encode_mouse(event: MouseEvent, modes: ModeFlags) -> Vec<u8> {
    if !modes.mouse_reporting() {
        return Vec::new();
    }
    if event.kind == MouseKind::Motion && !modes.mouse_motion && !modes.mouse_drag {
        return Vec::new();
    }

    let mut code = event.button_code() + event.modifier_bits();
    // Legacy X10 encoding reports a release with the `3` button code; SGR reports
    // the released button with a lowercase `m` terminator instead.
    match event.kind {
        MouseKind::Release => {
            if !modes.sgr_mouse {
                code += 3;
            }
        }
        MouseKind::Motion => code += 32,
        MouseKind::Press => {}
    }

    if modes.sgr_mouse {
        let final_byte = if event.kind == MouseKind::Release {
            b'm'
        } else {
            b'M'
        };
        let mut out = format!("\x1b[<{};{};{}", code, event.column + 1, event.row + 1).into_bytes();
        out.push(final_byte);
        out
    } else if modes.utf8_mouse {
        let mut out = b"\x1b[M".to_vec();
        out.push(32 + code);
        push_utf8_coord(&mut out, event.column + 1);
        push_utf8_coord(&mut out, event.row + 1);
        out
    } else {
        // Classic X10 encoding clamps to the byte range.
        let column = (event.column + 1).min(223) as u8;
        let row = (event.row + 1).min(223) as u8;
        vec![0x1b, b'[', b'M', 32 + code, 32 + column, 32 + row]
    }
}

fn push_utf8_coord(out: &mut Vec<u8>, value: u16) {
    let codepoint = 32 + value as u32;
    if let Some(ch) = char::from_u32(codepoint) {
        let mut buffer = [0u8; 4];
        out.extend_from_slice(ch.encode_utf8(&mut buffer).as_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn modes() -> ModeFlags {
        ModeFlags {
            show_cursor: true,
            ..ModeFlags::default()
        }
    }

    fn kitty(flags: KittyFlags) -> ModeFlags {
        ModeFlags {
            kitty: flags,
            ..ModeFlags::default()
        }
    }

    #[test]
    fn disambiguate_turns_ctrl_letters_into_escape_codes() {
        let press = KeyPress::with_mods(TermKey::Char('a'), true, false, false);
        let modes = kitty(KittyFlags {
            disambiguate: true,
            ..KittyFlags::default()
        });
        assert_eq!(encode_key(press, modes), b"\x1b[97;5u");
    }

    #[test]
    fn disambiguate_reports_escape_as_a_key_code() {
        let modes = kitty(KittyFlags {
            disambiguate: true,
            ..KittyFlags::default()
        });
        assert_eq!(
            encode_key(KeyPress::new(TermKey::Escape), modes),
            b"\x1b[27u"
        );
    }

    #[test]
    fn disambiguate_leaves_plain_text_alone() {
        let modes = kitty(KittyFlags {
            disambiguate: true,
            ..KittyFlags::default()
        });
        assert_eq!(encode_key(KeyPress::new(TermKey::Char('a')), modes), b"a");
        assert_eq!(encode_key(KeyPress::new(TermKey::Enter), modes), b"\r");
        assert_eq!(encode_key(KeyPress::new(TermKey::Tab), modes), b"\t");
    }

    #[test]
    fn disambiguate_reports_modified_enter_and_tab() {
        let modes = kitty(KittyFlags {
            disambiguate: true,
            ..KittyFlags::default()
        });
        let ctrl_enter = KeyPress::with_mods(TermKey::Enter, true, false, false);
        assert_eq!(encode_key(ctrl_enter, modes), b"\x1b[13;5u");
        let shift_tab = KeyPress::with_mods(TermKey::Tab, false, false, true);
        assert_eq!(encode_key(shift_tab, modes), b"\x1b[9;2u");
    }

    #[test]
    fn all_keys_as_esc_wraps_plain_text() {
        let modes = kitty(KittyFlags {
            report_all_keys_as_esc: true,
            ..KittyFlags::default()
        });
        assert_eq!(
            encode_key(KeyPress::new(TermKey::Char('a')), modes),
            b"\x1b[97u"
        );
        assert_eq!(
            encode_key(KeyPress::new(TermKey::Insert), modes),
            b"\x1b[57348u"
        );
    }

    #[test]
    fn associated_text_is_appended_when_requested() {
        let modes = kitty(KittyFlags {
            report_all_keys_as_esc: true,
            report_associated_text: true,
            ..KittyFlags::default()
        });
        assert_eq!(
            encode_key(KeyPress::new(TermKey::Char('a')), modes),
            b"\x1b[97;1;97u"
        );
    }

    #[test]
    fn alternate_keys_report_the_shifted_codepoint() {
        let modes = kitty(KittyFlags {
            disambiguate: true,
            report_alternate_keys: true,
            ..KittyFlags::default()
        });
        let shifted = KeyPress::with_mods(TermKey::Char('1'), false, false, true);
        assert_eq!(encode_key(shifted, modes), b"\x1b[49:33:49;2u");
    }

    #[test]
    fn event_types_are_reported_only_when_requested() {
        let with_types = kitty(KittyFlags {
            disambiguate: true,
            report_event_types: true,
            ..KittyFlags::default()
        });
        let repeated = KeyPress::with_mods(TermKey::Char('a'), true, false, false)
            .with_event(KeyEventType::Repeat);
        assert_eq!(encode_key(repeated, with_types), b"\x1b[97;5:2u");

        let released = KeyPress::new(TermKey::Escape).with_event(KeyEventType::Release);
        assert_eq!(encode_key(released, with_types), b"\x1b[27;1:3u");

        let without_types = kitty(KittyFlags {
            disambiguate: true,
            ..KittyFlags::default()
        });
        assert!(encode_key(released, without_types).is_empty());
    }

    #[test]
    fn a_release_never_falls_back_to_xterm() {
        let released = KeyPress::new(TermKey::Escape).with_event(KeyEventType::Release);
        assert!(encode_key(released, ModeFlags::default()).is_empty());
    }

    #[test]
    fn xterm_encoding_is_unchanged_with_kitty_off() {
        let modes = ModeFlags::default();
        let ctrl_a = KeyPress::with_mods(TermKey::Char('a'), true, false, false);
        assert_eq!(encode_key(ctrl_a, modes), b"\x01");
        assert_eq!(encode_key(KeyPress::new(TermKey::Escape), modes), b"\x1b");
        assert_eq!(
            encode_key(
                KeyPress::with_mods(TermKey::Char('a'), false, true, false),
                modes
            ),
            b"\x1ba"
        );
    }

    #[test]
    fn plain_characters_are_sent_verbatim() {
        let press = KeyPress::new(TermKey::Char('a'));
        assert_eq!(encode_key(press, modes()), b"a");
    }

    #[test]
    fn ctrl_letters_become_control_codes() {
        let press = KeyPress::with_mods(TermKey::Char('c'), true, false, false);
        assert_eq!(encode_key(press, modes()), vec![0x03]);
        let press = KeyPress::with_mods(TermKey::Char('d'), true, false, false);
        assert_eq!(encode_key(press, modes()), vec![0x04]);
    }

    #[test]
    fn ctrl_specials_match_xterm() {
        for (ch, expected) in [
            ('[', 0x1b),
            ('\\', 0x1c),
            (']', 0x1d),
            ('^', 0x1e),
            ('_', 0x1f),
        ] {
            let press = KeyPress::with_mods(TermKey::Char(ch), true, false, false);
            assert_eq!(encode_key(press, modes()), vec![expected], "ctrl+{ch}");
        }
    }

    #[test]
    fn alt_prefixes_escape() {
        let press = KeyPress::with_mods(TermKey::Char('b'), false, true, false);
        assert_eq!(encode_key(press, modes()), b"\x1bb");
    }

    #[test]
    fn shift_produces_the_shifted_character() {
        let press = KeyPress::with_mods(TermKey::Char('1'), false, false, true);
        assert_eq!(encode_key(press, modes()), b"!");
    }

    #[test]
    fn arrows_respect_application_cursor_mode() {
        let press = KeyPress::new(TermKey::Arrow(Direction::Up));
        assert_eq!(encode_key(press, modes()), b"\x1b[A");
        let app = ModeFlags {
            app_cursor: true,
            ..modes()
        };
        assert_eq!(encode_key(press, app), b"\x1bOA");
    }

    #[test]
    fn modified_arrows_use_csi_parameters() {
        let press = KeyPress::with_mods(TermKey::Arrow(Direction::Left), false, true, false);
        assert_eq!(encode_key(press, modes()), b"\x1b[1;3D");
        let press = KeyPress::with_mods(TermKey::Arrow(Direction::Right), true, false, true);
        assert_eq!(encode_key(press, modes()), b"\x1b[1;6C");
    }

    #[test]
    fn navigation_keys_use_expected_sequences() {
        assert_eq!(encode_key(KeyPress::new(TermKey::Home), modes()), b"\x1b[H");
        assert_eq!(encode_key(KeyPress::new(TermKey::End), modes()), b"\x1b[F");
        assert_eq!(
            encode_key(KeyPress::new(TermKey::PageUp), modes()),
            b"\x1b[5~"
        );
        assert_eq!(
            encode_key(KeyPress::new(TermKey::PageDown), modes()),
            b"\x1b[6~"
        );
        assert_eq!(
            encode_key(KeyPress::new(TermKey::Delete), modes()),
            b"\x1b[3~"
        );
        assert_eq!(
            encode_key(KeyPress::new(TermKey::Insert), modes()),
            b"\x1b[2~"
        );
    }

    #[test]
    fn function_keys_use_xterm_sequences() {
        assert_eq!(
            encode_key(KeyPress::new(TermKey::Function(1)), modes()),
            b"\x1bOP"
        );
        assert_eq!(
            encode_key(KeyPress::new(TermKey::Function(4)), modes()),
            b"\x1bOS"
        );
        assert_eq!(
            encode_key(KeyPress::new(TermKey::Function(5)), modes()),
            b"\x1b[15~"
        );
        assert_eq!(
            encode_key(KeyPress::new(TermKey::Function(12)), modes()),
            b"\x1b[24~"
        );
        let modified = KeyPress::with_mods(TermKey::Function(5), false, false, true);
        assert_eq!(encode_key(modified, modes()), b"\x1b[15;2~");
    }

    #[test]
    fn enter_tab_and_backspace_are_correct() {
        assert_eq!(encode_key(KeyPress::new(TermKey::Enter), modes()), b"\r");
        assert_eq!(encode_key(KeyPress::new(TermKey::Tab), modes()), b"\t");
        let shift_tab = KeyPress::with_mods(TermKey::Tab, false, false, true);
        assert_eq!(encode_key(shift_tab, modes()), b"\x1b[Z");
        assert_eq!(
            encode_key(KeyPress::new(TermKey::Backspace), modes()),
            vec![0x7f]
        );
        let ctrl_backspace = KeyPress::with_mods(TermKey::Backspace, true, false, false);
        assert_eq!(encode_key(ctrl_backspace, modes()), vec![0x08]);
    }

    #[test]
    fn utf8_characters_are_encoded_as_utf8() {
        let press = KeyPress::new(TermKey::Char('ñ'));
        assert_eq!(encode_key(press, modes()), "ñ".as_bytes());
        let press = KeyPress::new(TermKey::Char('🚀'));
        assert_eq!(encode_key(press, modes()), "🚀".as_bytes());
    }

    #[test]
    fn paste_respects_bracketed_paste_mode() {
        let plain = encode_paste("hello\nworld", modes());
        assert_eq!(plain, b"hello\rworld");

        let bracketed = encode_paste(
            "hello",
            ModeFlags {
                bracketed_paste: true,
                ..modes()
            },
        );
        assert_eq!(bracketed, b"\x1b[200~hello\x1b[201~");
    }

    #[test]
    fn paste_normalises_crlf() {
        assert_eq!(encode_paste("a\r\nb\nc", modes()), b"a\rb\rc");
    }

    #[test]
    fn mouse_is_ignored_when_reporting_is_off() {
        let event = MouseEvent {
            kind: MouseKind::Press,
            button: MouseButton::Left,
            column: 0,
            row: 0,
            ctrl: false,
            alt: false,
            shift: false,
        };
        assert!(encode_mouse(event, modes()).is_empty());
    }

    #[test]
    fn sgr_mouse_encoding_is_one_based() {
        let modes = ModeFlags {
            mouse_click: true,
            sgr_mouse: true,
            ..modes()
        };
        let event = MouseEvent {
            kind: MouseKind::Press,
            button: MouseButton::Left,
            column: 4,
            row: 2,
            ctrl: false,
            alt: false,
            shift: false,
        };
        assert_eq!(encode_mouse(event, modes), b"\x1b[<0;5;3M");

        let release = MouseEvent {
            kind: MouseKind::Release,
            ..event
        };
        assert_eq!(encode_mouse(release, modes), b"\x1b[<0;5;3m");
    }

    #[test]
    fn classic_mouse_encoding_uses_offset_bytes() {
        let modes = ModeFlags {
            mouse_click: true,
            ..modes()
        };
        let event = MouseEvent {
            kind: MouseKind::Press,
            button: MouseButton::Right,
            column: 0,
            row: 0,
            ctrl: false,
            alt: false,
            shift: false,
        };
        assert_eq!(
            encode_mouse(event, modes),
            vec![0x1b, b'[', b'M', 34, 33, 33]
        );
    }

    #[test]
    fn motion_requires_a_motion_mode() {
        let event = MouseEvent {
            kind: MouseKind::Motion,
            button: MouseButton::None,
            column: 1,
            row: 1,
            ctrl: false,
            alt: false,
            shift: false,
        };
        let click_only = ModeFlags {
            mouse_click: true,
            sgr_mouse: true,
            ..modes()
        };
        assert!(encode_mouse(event, click_only).is_empty());
        let motion = ModeFlags {
            mouse_motion: true,
            sgr_mouse: true,
            ..modes()
        };
        assert_eq!(encode_mouse(event, motion), b"\x1b[<35;2;2M");
    }
}
