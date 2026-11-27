//! Typed configuration model shared by the TOML interface and the native
//! settings panel.
//!
//! There is exactly one configuration schema. The TOML loader, the atomic
//! writer and the graphical settings surface all operate on the types in
//! [`schema`], which means the two interfaces can never drift apart.

pub mod defaults;
pub mod keys;
pub mod loader;
pub mod palette;
pub mod schema;
pub mod service;
pub mod themes;
pub mod validation;
pub mod writer;

pub use keys::{ActionId, Chord, Key, Keymap, Modifiers, NamedKey};
pub use palette::{Color, Palette, Rgb};
pub use schema::{
    AppearanceConfig, BellMode, Config, CursorStyle, FontConfig, PerformanceConfig, Profile,
    TabBarPosition, TerminalConfig, WindowConfig, WorkingDirectory, WorkspaceConfig,
};
pub use service::{ConfigService, SaveOutcome};
pub use themes::{
    Theme, ThemeSaveError, available_themes, builtin_themes, load_theme_file, resolve_theme,
    sanitize_theme_name, save_theme, theme_by_name,
};
pub use validation::{ConfigIssue, IssueSeverity, ValidationReport};

/// XDG configuration directory for AstraTerm (`~/.config/astraterm`).
pub fn config_dir() -> std::path::PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME").map(|home| std::path::PathBuf::from(home).join(".config"))
        })
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"));
    base.join("astraterm")
}

/// Default configuration file path: `~/.config/astraterm/config.toml`.
pub fn default_config_path() -> std::path::PathBuf {
    config_dir().join("config.toml")
}

/// Directory that holds user theme files: `~/.config/astraterm/themes`.
pub fn themes_dir() -> std::path::PathBuf {
    config_dir().join("themes")
}

/// XDG state directory for AstraTerm (`~/.local/state/astraterm`).
pub fn state_dir() -> std::path::PathBuf {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(std::path::PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME").map(|home| std::path::PathBuf::from(home).join(".local/state"))
        })
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"));
    base.join("astraterm")
}
