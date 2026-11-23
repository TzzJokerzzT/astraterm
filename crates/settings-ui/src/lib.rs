//! Native settings panel: a keyboard-driven, text-projected settings surface.
//!
//! The panel is *not* a webview and not a GUI framework: it exposes a model
//! (sections, rows, focused row, validation issues) plus a plain-text projection,
//! and the application draws both with the same GPU pipeline as the terminal. That
//! keeps the settings experience native, lightweight and consistent with the
//! minimal terminal workspace.
//!
//! Every edit operates on a `Config` draft; the panel never writes files. When the
//! user confirms, it hands the draft to the application, which persists it through
//! the single [`astraterm_config::ConfigService`].

pub mod fields;
pub mod panel;

pub use fields::{Field, FieldKind, PaletteEntry};
pub use panel::{PanelAction, PanelKey, PanelLine, PanelStyle, Section, SettingsPanel};
