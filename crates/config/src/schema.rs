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
    /// Action name to key chord, for example `new-tab = "ctrl+shift+t"`.
    pub keys: BTreeMap<String, String>,
    /// Named profiles. The first profile is the implicit default unless a
    /// profile is requested explicitly.
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TerminalConfig {
    /// `auto` resolves `$SHELL`; any other value is an executable path or name.
    pub shell: String,
    /// Extra arguments passed to the shell, as an argument vector.
    pub shell_args: Vec<String>,
    /// Advertised `$TERM` value.
    pub term: String,
    /// Scrollback lines kept per session.
    pub scrollback_lines: usize,
    /// Initial working directory policy.
    pub working_directory: WorkingDirectory,
    /// Ask before closing a tab/pane with live child processes.
    pub confirm_on_close: bool,
    /// Copy the selection to the primary clipboard as soon as it is made.
    pub copy_on_select: bool,
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
            scrollback_lines: 10_000,
            working_directory: WorkingDirectory::home(),
            confirm_on_close: true,
            copy_on_select: false,
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
    /// Clock format: `%H %M %S %d %m %Y %b %a` are supported, everything else is
    /// literal text.
    pub clock_format: String,
    pub show_window_controls: bool,
    pub accent_color: String,
    pub tab_bar_position: TabBarPosition,
    /// Width of the vertical tab strip in logical pixels, when the tabs sit on the
    /// right edge.
    pub tab_strip_width: u32,
    /// Use smaller tab labels without the profile hint.
    pub compact_tabs: bool,
    /// Thickness of the divider between panes, in logical pixels.
    pub divider_thickness: u32,
    /// Gap between panes, in logical pixels.
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
    /// Upper bound for idle redraws per second (0 means unlimited on demand).
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
