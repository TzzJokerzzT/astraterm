//! The single typed configuration schema.
//!
//! Every field is documented in `docs/CONFIGURATION.md`. Fields default through
//! `#[serde(default)]` so a partial TOML file is always valid, and unknown keys
//! are reported as warnings by the loader instead of silently ignored.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::keys::default_keybindings;

/// Complete application configuration.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub window: WindowConfig,
    pub terminal: TerminalConfig,
    pub font: FontConfig,
    pub appearance: AppearanceConfig,
    pub workspace: WorkspaceConfig,
    pub performance: PerformanceConfig,
    pub notifications: NotificationsConfig,
    /// Action name to key chord, for example `new-tab = "ctrl+shift+t"`.
    pub keys: BTreeMap<String, String>,
    /// Named profiles. Nothing selects one implicitly: a session uses the global
    /// settings unless a profile is requested by name (`--profile`).
    pub profiles: Vec<Profile>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            window: WindowConfig::default(),
            terminal: TerminalConfig::default(),
            font: FontConfig::default(),
            appearance: AppearanceConfig::default(),
            workspace: WorkspaceConfig::default(),
            performance: PerformanceConfig::default(),
            notifications: NotificationsConfig::default(),
            keys: default_keybindings(),
            profiles: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WindowConfig {
    /// Initial window width in logical pixels.
    pub width: u32,
    /// Initial window height in logical pixels.
    pub height: u32,
    /// Window opacity, 0.1 (nearly transparent) to 1.0 (opaque).
    pub opacity: f32,
    /// Start maximized.
    pub startup_maximized: bool,
    /// Let the compositor draw client-side decorations.
    pub show_window_controls: bool,
    /// Remember window geometry across restarts.
    pub remember_geometry: bool,
}

impl Default for WindowConfig {
    fn default() -> Self {
        Self {
            width: 1280,
            height: 800,
            opacity: 1.0,
            startup_maximized: false,
            show_window_controls: true,
            remember_geometry: true,
        }
    }
}

/// Where the copy-on-select gesture writes.
///
/// Two selections exist on Linux: the `Ctrl+C`/`Ctrl+V` clipboard, which every
/// application reads and writes, and the primary selection, which is set by selecting text
/// and pasted with the middle mouse button. The explicit copy always uses the clipboard;
/// this only chooses the target of the selection gesture.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CopyOnSelectTarget {
    /// The regular clipboard, the same one `ctrl+shift+c` fills.
    #[default]
    Clipboard,
    /// The X11/Wayland primary selection, which middle-click pasting reads.
    Primary,
}

impl CopyOnSelectTarget {
    pub fn label(self) -> &'static str {
        match self {
            CopyOnSelectTarget::Clipboard => "clipboard",
            CopyOnSelectTarget::Primary => "primary",
        }
    }

    pub const ALL: [CopyOnSelectTarget; 2] =
        [CopyOnSelectTarget::Clipboard, CopyOnSelectTarget::Primary];
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TerminalConfig {
    /// `auto` resolves `$SHELL`; any other value is an executable path or name.
    pub shell: String,
    /// Extra arguments passed to the shell, as an argument vector.
    pub shell_args: Vec<String>,
    /// Advertised `$TERM` value.
    pub term: String,
    /// Advertise and honour the Kitty keyboard protocol.
    pub kitty_keyboard: bool,
    /// Decode and draw images sent by the running program.
    ///
    /// Off by default: the bytes arrive from whatever runs in the terminal,
    /// including a remote host over SSH, and decoding them is a new parser fed by
    /// untrusted input. Turning it on is a deliberate choice.
    pub images: bool,
    /// Scrollback lines kept per session.
    pub scrollback_lines: usize,
    /// Initial working directory policy.
    pub working_directory: WorkingDirectory,
    /// Ask before closing a tab/pane with live child processes.
    pub confirm_on_close: bool,
    /// Copy the selection to the clipboard as soon as it is made.
    pub copy_on_select: bool,
    /// Where the copy-on-select gesture writes.
    ///
    /// The explicit copy keeps its own frame — it always sets the regular clipboard, so
    /// `ctrl+shift+c` behaves the same whatever this says. Only the selection gesture
    /// follows this target. Linux users expect a selection to land in the primary
    /// selection, which is what a middle click pastes; the regular clipboard stays the
    /// default because it is what every other application reads.
    pub copy_on_select_target: CopyOnSelectTarget,
    /// Open a third-party process when the user clicks a detected URL.
    pub open_urls_on_click: bool,
    /// Characters that terminate a word for double-click selection.
    pub word_delimiters: String,
}

impl Default for TerminalConfig {
    fn default() -> Self {
        Self {
            shell: "auto".to_owned(),
            shell_args: Vec::new(),
            term: "xterm-256color".to_owned(),
            kitty_keyboard: true,
            images: false,
            scrollback_lines: 10_000,
            working_directory: WorkingDirectory::home(),
            confirm_on_close: true,
            copy_on_select: false,
            copy_on_select_target: CopyOnSelectTarget::Clipboard,
            open_urls_on_click: true,
            word_delimiters: " /\\()\"'-.,:;<>~!@#$%^&*|+=[]{}~?".to_owned(),
        }
    }
}

/// Working directory policy for a new session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum WorkingDirectory {
    /// `home`, `current`, or `inherit`.
    Policy(String),
    /// An explicit absolute path.
    Path(std::path::PathBuf),
}

impl Default for WorkingDirectory {
    fn default() -> Self {
        Self::Policy("home".to_owned())
    }
}

impl WorkingDirectory {
    pub fn home() -> Self {
        Self::Policy("home".to_owned())
    }

    /// Human readable form used by the settings panel.
    pub fn label(&self) -> String {
        match self {
            WorkingDirectory::Policy(policy) => policy.clone(),
            WorkingDirectory::Path(path) => path.display().to_string(),
        }
    }

    /// Resolves the policy against the process environment and the home dir.
    pub fn resolve(&self, current: Option<&std::path::Path>) -> Option<std::path::PathBuf> {
        match self {
            WorkingDirectory::Path(path) => Some(path.clone()),
            WorkingDirectory::Policy(policy) => match policy.as_str() {
                "current" => current.map(std::path::Path::to_path_buf),
                "inherit" => current.map(std::path::Path::to_path_buf),
                "home" | "" => home_dir(),
                other => Some(std::path::PathBuf::from(other)),
            },
        }
    }
}

/// Returns the user's home directory, if the environment provides one.
pub fn home_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME").map(std::path::PathBuf::from)
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FontConfig {
    pub family: String,
    pub size: f32,
    pub weight: u16,
    pub line_height: f32,
    pub letter_spacing: f32,
    /// Opt-in ligatures (`liga`/`calt`); off by default because the grid assumes
    /// the shaper can lay a row out without glyph substitution.
    pub ligatures: bool,
    /// Keep the cell advance a whole number of pixels by stretching the font.
    ///
    /// A block glyph (`█ ▓ ▒ ░`) drawn at a fractional advance antialiases its edge
    /// against the pixel grid instead of against the neighbouring colour, which puts
    /// a dark seam between the steps of a gradient. On, the font is rasterised at the
    /// size whose advance rounds to a whole pixel, and both the grid and the shaper are
    /// built from that size, so a rounded width can never drift away from the shaped
    /// one. The cost is the stretch: the font is a fraction larger, so a wide pane fits
    /// a few columns fewer. Off, the measured advance is used exactly as it comes out
    /// of the shaper.
    pub integer_cell_width: bool,
    /// Hint glyph outlines to the pixel grid when they are rasterised.
    ///
    /// Off by default, and then cosmic-text builds the swash scaler with
    /// `hint(false)`: a glyph's outline is scaled to the requested size and sampled,
    /// so a stem keeps its fractional width and a dithered block keeps its own
    /// pattern. On, swash snaps stems and edges to whole pixels.
    ///
    /// Off is the default because hinting is what loses weight at the sizes a
    /// terminal actually uses. The dither of `▓▒░` (U+2591..U+2593) is a one-to-two
    /// pixel pattern, so a hinted 18.33 ppem buffer — the size
    /// `integer_cell_width` produces — rounds every other dot onto a different pixel
    /// boundary and the gradient reads as a coarse hatch with dark vertical stripes,
    /// while `·` (U+00B7) and other thin glyphs drop whole pixel rows. Hinted
    /// rasterisation is also the look most other terminal emulators do *not* ship, so
    /// a user who wants the old AstraTerm look sets `hinting = true`.
    pub hinting: bool,
    /// Paint `░▒▓` (`U+2591` LIGHT SHADE, `U+2592` MEDIUM SHADE, `U+2593` DARK
    /// SHADE) as one flat colour per cell instead of the font's own dither.
    ///
    /// Each of the three stands for a coverage of the foreground over the cell's
    /// background: `U+2591` is 25 %, `U+2592` is 50 % and `U+2593` is 75 %, and the
    /// cell is filled with exactly that blend. The glyph is not drawn, so the fine
    /// one-to-two pixel checkerboard the font ships for those characters never
    /// appears.
    ///
    /// Off by default, because it changes how three standard Unicode characters
    /// render. It exists to emulate foot, which does this by default
    /// (`box-drawing-solid-shades = yes`): a gradient drawn with the shade blocks
    /// reads as smooth flat steps there and as a dither here, and a program ported
    /// from foot looks different only because of this substitution. Set it to match.
    pub solid_shades: bool,
    /// Additional families used when a glyph is missing from `family`.
    pub fallbacks: Vec<String>,
}

impl Default for FontConfig {
    fn default() -> Self {
        Self {
            family: "JetBrainsMono Nerd Font".to_owned(),
            size: 13.0,
            weight: 400,
            line_height: 1.15,
            letter_spacing: 0.0,
            ligatures: false,
            integer_cell_width: false,
            hinting: false,
            solid_shades: false,
            fallbacks: vec![
                "JetBrains Mono".to_owned(),
                "Symbols Nerd Font".to_owned(),
                "Noto Color Emoji".to_owned(),
                "DejaVu Sans Mono".to_owned(),
                "monospace".to_owned(),
            ],
        }
    }
}

/// Cursor shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CursorStyle {
    Block,
    Beam,
    Underline,
    Hollow,
}

impl CursorStyle {
    pub fn label(self) -> &'static str {
        match self {
            CursorStyle::Block => "block",
            CursorStyle::Beam => "beam",
            CursorStyle::Underline => "underline",
            CursorStyle::Hollow => "hollow",
        }
    }

    pub const ALL: [CursorStyle; 4] = [
        CursorStyle::Block,
        CursorStyle::Beam,
        CursorStyle::Underline,
        CursorStyle::Hollow,
    ];
}

/// Position of the tab strip.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TabBarPosition {
    Top,
    Bottom,
    /// Vertical strip on the right edge (Warp-style sidebar tabs).
    Right,
    Hidden,
}

impl TabBarPosition {
    pub fn label(self) -> &'static str {
        match self {
            TabBarPosition::Top => "top",
            TabBarPosition::Bottom => "bottom",
            TabBarPosition::Right => "right",
            TabBarPosition::Hidden => "hidden",
        }
    }

    pub const ALL: [TabBarPosition; 3] = [
        TabBarPosition::Top,
        TabBarPosition::Bottom,
        TabBarPosition::Hidden,
    ];
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppearanceConfig {
    /// Theme name, resolved from built-in themes or `~/.config/astraterm/themes`.
    pub theme: String,
    /// Ask the compositor for blur behind a translucent background.
    pub background_blur: bool,
    /// Horizontal terminal padding in logical pixels.
    pub padding_x: u32,
    /// Vertical terminal padding in logical pixels.
    pub padding_y: u32,
    pub cursor: CursorStyle,
    /// Blink the cursor.
    pub cursor_blink: bool,
    /// Visual style identifier, kept for forward compatibility.
    pub style: String,
    pub show_status_bar: bool,
    /// Show CPU usage in the status bar.
    pub status_bar_cpu: bool,
    /// Show RAM usage in the status bar.
    pub status_bar_ram: bool,
    /// Show the clock in the status bar.
    pub status_bar_clock: bool,
    /// Clock format, handed whole to the platform `strftime`, so every conversion
    /// it knows (`%H %M %S %d %m %Y %b %a` and the rest) works and anything unknown
    /// is literal text. Empty or unusable values fall back to `%H:%M`.
    pub clock_format: String,
    pub show_window_controls: bool,
    pub accent_color: String,
    pub tab_bar_position: TabBarPosition,
    /// Width of the vertical tab strip in logical pixels, when the tabs sit on the
    /// right edge.
    pub tab_strip_width: u32,
    /// Use smaller tab labels without the profile hint.
    pub compact_tabs: bool,
    /// Thickness of the divider between panes, in device pixels (not scaled by the
    /// window's scale factor).
    pub divider_thickness: u32,
    /// Gap between panes, in device pixels (not scaled by the window's scale factor).
    pub pane_gap: u32,
}

impl Default for AppearanceConfig {
    fn default() -> Self {
        Self {
            theme: "midnight".to_owned(),
            background_blur: false,
            padding_x: 12,
            padding_y: 10,
            cursor: CursorStyle::Block,
            cursor_blink: true,
            style: "minimal-dark".to_owned(),
            show_status_bar: false,
            status_bar_cpu: true,
            status_bar_ram: true,
            status_bar_clock: true,
            clock_format: "%H:%M".to_owned(),
            show_window_controls: false,
            accent_color: "#8B9CFF".to_owned(),
            tab_bar_position: TabBarPosition::Top,
            tab_strip_width: 180,
            compact_tabs: false,
            divider_thickness: 1,
            pane_gap: 2,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkspaceConfig {
    /// Restore tab/pane layout metadata on startup (layout only, not processes).
    pub restore_on_startup: bool,
    /// Replace the shell with an interactive session on quick-close confirmation.
    pub close_pane_when_shell_exits: bool,
}

impl Default for WorkspaceConfig {
    fn default() -> Self {
        Self {
            restore_on_startup: true,
            close_pane_when_shell_exits: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PerformanceConfig {
    /// Try to use the GPU renderer; falls back to reporting the failure.
    pub gpu_acceleration: bool,
    /// Reduce work: no cursor blink, no effects, batched redraws.
    pub low_power_mode: bool,
    /// Intended upper bound for idle redraws per second. Accepted and validated but
    /// not read yet: idle redraws are still governed by the blink timer alone, so any
    /// value here has no effect today.
    pub max_fps_when_idle: u32,
}

impl Default for PerformanceConfig {
    fn default() -> Self {
        Self {
            gpu_acceleration: true,
            low_power_mode: false,
            max_fps_when_idle: 1,
        }
    }
}

/// When the terminal raises a desktop notification.
///
/// The text comes from an untrusted process, so the default only surfaces it when
/// the user is not looking at that window: a program cannot interrupt the work in
/// progress, only tell the user something happened elsewhere.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NotificationPolicy {
    /// Notify only while the window showing the session does not have focus.
    #[default]
    Unfocused,
    /// Notify even while the user is looking at the window.
    Always,
    /// Never send a desktop notification; the status bar message still appears.
    Never,
}

impl NotificationPolicy {
    pub fn label(self) -> &'static str {
        match self {
            NotificationPolicy::Unfocused => "unfocused",
            NotificationPolicy::Always => "always",
            NotificationPolicy::Never => "never",
        }
    }

    pub const ALL: [NotificationPolicy; 3] = [
        NotificationPolicy::Unfocused,
        NotificationPolicy::Always,
        NotificationPolicy::Never,
    ];
}

/// `[notifications]` — desktop notifications requested by OSC 9 and OSC 777.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct NotificationsConfig {
    /// When to raise a notification: `unfocused` (default), `always` or `never`.
    pub on: NotificationPolicy,
}

impl Default for NotificationsConfig {
    fn default() -> Self {
        Self {
            on: NotificationPolicy::Unfocused,
        }
    }
}

/// Whether a notification should be delivered for a window in this focus state.
///
/// Pure so the policy matrix is testable without a window: the application asks
/// once per notification and the answer decides both the desktop notification and
/// the window attention request.
pub fn should_notify(policy: NotificationPolicy, window_focused: bool) -> bool {
    match policy {
        NotificationPolicy::Unfocused => !window_focused,
        NotificationPolicy::Always => true,
        NotificationPolicy::Never => false,
    }
}

/// A named launch profile.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Profile {
    pub name: String,
    /// Shell override; empty means the global shell setting.
    pub shell: String,
    /// Shell arguments (`-l` for a login shell, `-c` is never used).
    pub args: Vec<String>,
    /// Alternative to `args`: a command vector executed directly, for example
    /// `["ssh", "production"]`.
    pub command: Vec<String>,
    pub working_directory: Option<WorkingDirectory>,
    pub env: BTreeMap<String, String>,
    /// Command typed into the shell once it starts. Never executed through
    /// `sh -c`; it is written to the PTY as keystrokes after startup.
    pub startup_command: Option<String>,
    pub theme: Option<String>,
    pub font_size: Option<f32>,
    /// Free-form label shown in the tab strip, e.g. the SSH host alias.
    pub label: Option<String>,
}

impl Profile {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kitty_keyboard_is_enabled_by_default() {
        assert!(
            TerminalConfig::default().kitty_keyboard,
            "the Kitty keyboard protocol is advertised unless the user opts out"
        );
        assert!(Config::default().terminal.kitty_keyboard);
    }

    #[test]
    fn images_are_off_by_default() {
        assert!(
            !TerminalConfig::default().images,
            "decoding images from untrusted output is opt-in"
        );
        let config: Config = toml::from_str("[terminal]\nimages = true\n").unwrap();
        assert!(config.terminal.images);
        let config: Config = toml::from_str("[terminal]\nscrollback_lines = 100\n").unwrap();
        assert!(!config.terminal.images);
    }

    #[test]
    fn notifications_are_on_for_unfocused_windows_by_default() {
        assert_eq!(
            NotificationsConfig::default().on,
            NotificationPolicy::Unfocused
        );
        assert_eq!(
            Config::default().notifications.on,
            NotificationPolicy::Unfocused
        );
        // Absent table and absent key both fall back to the default.
        let config: Config = toml::from_str("[terminal]\nimages = true\n").unwrap();
        assert_eq!(config.notifications.on, NotificationPolicy::Unfocused);
        let config: Config = toml::from_str("[notifications]\non = \"always\"\n").unwrap();
        assert_eq!(config.notifications.on, NotificationPolicy::Always);
        let config: Config = toml::from_str("[notifications]\non = \"never\"\n").unwrap();
        assert_eq!(config.notifications.on, NotificationPolicy::Never);
    }

    #[test]
    fn the_notification_policy_matrix_only_fires_when_it_should() {
        let policies = [
            NotificationPolicy::Unfocused,
            NotificationPolicy::Always,
            NotificationPolicy::Never,
        ];
        let expected = [
            // (unfocused window, focused window) for each policy.
            (true, false),
            (true, true),
            (false, false),
        ];
        for (policy, (when_unfocused, when_focused)) in policies.into_iter().zip(expected) {
            assert_eq!(
                should_notify(policy, false),
                when_unfocused,
                "{policy:?} with an unfocused window"
            );
            assert_eq!(
                should_notify(policy, true),
                when_focused,
                "{policy:?} with a focused window"
            );
        }
    }

    #[test]
    fn integer_cell_width_defaults_off_and_parses_when_asked() {
        assert!(
            !FontConfig::default().integer_cell_width,
            "whole-pixel cells stretch the font, so they are opt-in"
        );
        assert!(!Config::default().font.integer_cell_width);

        let config: Config = toml::from_str("[font]\ninteger_cell_width = true\n").unwrap();
        assert!(config.font.integer_cell_width);

        // Absent key and absent table both keep the default.
        let config: Config = toml::from_str("[font]\nsize = 12.0\n").unwrap();
        assert!(!config.font.integer_cell_width);
        let config: Config = toml::from_str("[terminal]\nimages = true\n").unwrap();
        assert!(!config.font.integer_cell_width);
    }

    #[test]
    fn an_unknown_sibling_of_integer_cell_width_is_still_reported() {
        let text = "[font]\ninteger_cell_width = true\ninteger_cell_hight = true\n";
        let outcome = crate::loader::parse_str(text, "test").unwrap();
        assert!(outcome.config.font.integer_cell_width);
        assert!(
            outcome
                .issues
                .iter()
                .any(|issue| issue.field.contains("integer_cell_hight")),
            "an unknown key still warns: {:?}",
            outcome.issues
        );
        assert!(
            outcome.validation().is_valid(),
            "an unknown key stays a warning: {:?}",
            outcome.issues
        );
    }

    #[test]
    fn hinting_defaults_off_and_parses_when_asked() {
        assert!(
            !FontConfig::default().hinting,
            "hinted rasterisation is what coarsens a dithered block at a fractional size, \
             so unhinted is the shipped default"
        );
        assert!(!Config::default().font.hinting);

        let config: Config = toml::from_str("[font]\nhinting = true\n").unwrap();
        assert!(config.font.hinting);

        // Absent key and absent table both keep the default.
        let config: Config = toml::from_str("[font]\nsize = 12.0\n").unwrap();
        assert!(!config.font.hinting);
        let config: Config = toml::from_str("[terminal]\nimages = true\n").unwrap();
        assert!(!config.font.hinting);
    }

    #[test]
    fn an_unknown_sibling_of_hinting_is_still_reported() {
        let text = "[font]\nhinting = true\nhintng = false\n";
        let outcome = crate::loader::parse_str(text, "test").unwrap();
        assert!(outcome.config.font.hinting);
        assert!(
            outcome
                .issues
                .iter()
                .any(|issue| issue.field.contains("hintng")),
            "an unknown key still warns: {:?}",
            outcome.issues
        );
        assert!(
            outcome.validation().is_valid(),
            "an unknown key stays a warning: {:?}",
            outcome.issues
        );
    }

    #[test]
    fn solid_shades_defaults_off_and_parses_when_asked() {
        assert!(
            !FontConfig::default().solid_shades,
            "painting a shade as a flat fill changes how three standard Unicode \
             characters render, so it is opt-in"
        );
        assert!(!Config::default().font.solid_shades);

        let config: Config = toml::from_str("[font]\nsolid_shades = true\n").unwrap();
        assert!(config.font.solid_shades);

        // Absent key and absent table both keep the default.
        let config: Config = toml::from_str("[font]\nsize = 12.0\n").unwrap();
        assert!(!config.font.solid_shades);
        let config: Config = toml::from_str("[terminal]\nimages = true\n").unwrap();
        assert!(!config.font.solid_shades);
    }

    #[test]
    fn an_unknown_sibling_of_solid_shades_is_still_reported() {
        let text = "[font]\nsolid_shades = true\nsolid_shade_mode = false\n";
        let outcome = crate::loader::parse_str(text, "test").unwrap();
        assert!(outcome.config.font.solid_shades);
        assert!(
            outcome
                .issues
                .iter()
                .any(|issue| issue.field.contains("solid_shade_mode")),
            "an unknown key still warns: {:?}",
            outcome.issues
        );
        assert!(
            outcome.validation().is_valid(),
            "an unknown key stays a warning: {:?}",
            outcome.issues
        );
    }

    #[test]
    fn copy_on_select_target_defaults_to_the_clipboard() {
        assert_eq!(
            TerminalConfig::default().copy_on_select_target,
            CopyOnSelectTarget::Clipboard,
            "the regular clipboard is what every copy wrote before the key existed"
        );
        assert_eq!(
            Config::default().terminal.copy_on_select_target,
            CopyOnSelectTarget::Clipboard
        );

        // Absent key and absent table both keep the default.
        let config: Config = toml::from_str("[terminal]\ncopy_on_select = true\n").unwrap();
        assert_eq!(config.terminal.copy_on_select_target, CopyOnSelectTarget::Clipboard);
        let config: Config = toml::from_str("[font]\nsize = 12.0\n").unwrap();
        assert_eq!(config.terminal.copy_on_select_target, CopyOnSelectTarget::Clipboard);
    }

    #[test]
    fn copy_on_select_target_parses_both_accepted_values() {
        let config: Config =
            toml::from_str("[terminal]\ncopy_on_select_target = \"primary\"\n").unwrap();
        assert_eq!(config.terminal.copy_on_select_target, CopyOnSelectTarget::Primary);
        let config: Config =
            toml::from_str("[terminal]\ncopy_on_select_target = \"clipboard\"\n").unwrap();
        assert_eq!(config.terminal.copy_on_select_target, CopyOnSelectTarget::Clipboard);
    }

    #[test]
    fn an_unknown_copy_on_select_target_is_rejected_by_the_loader() {
        // The key is enum-valued, so a typo is an error the user has to fix rather than a
        // warning about an ignored setting: the loader refuses the file the way it does
        // for `cursor`.
        let error = crate::loader::parse_str(
            "[terminal]\ncopy_on_select_target = \"mouse\"\n",
            "test",
        )
        .expect_err("an unknown target is not a valid configuration");
        let message = error.to_string();
        assert!(
            matches!(error, crate::loader::LoadError::Parse { .. }),
            "a bad value is a parse error, not an io error: {message}"
        );
        assert!(
            message.contains("copy_on_select_target"),
            "the error names the key: {message}"
        );
        assert!(
            message.contains("clipboard") && message.contains("primary"),
            "the error lists the accepted values: {message}"
        );
    }

    #[test]
    fn kitty_keyboard_defaults_on_when_the_key_is_absent() {
        let config: Config = toml::from_str("[terminal]\nscrollback_lines = 500\n").unwrap();
        assert!(config.terminal.kitty_keyboard);

        let config: Config = toml::from_str("[terminal]\nkitty_keyboard = false\n").unwrap();
        assert!(!config.terminal.kitty_keyboard);
    }
}
