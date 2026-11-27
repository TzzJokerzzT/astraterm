//! Linux platform services: clipboard, notifications, URLs, XDG paths and font
//! discovery.
//!
//! Every external program is executed with an explicit argument vector, never
//! through `sh -c`, and every service degrades gracefully when the underlying
//! tool is unavailable: a missing `notify-send` must not crash the terminal.
//! The clipboard uses `arboard`, so its absence is a build-time matter.

pub mod clipboard;
pub mod fonts;
pub mod linux;
pub mod notifications;
pub mod paths;
pub mod system;

pub use clipboard::Clipboard;
pub use notifications::Notifications;
pub use paths::StatePaths;
pub use system::{SystemMonitor, SystemSnapshot, format_clock};
