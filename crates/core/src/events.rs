//! Explicit message types exchanged between AstraTerm subsystems.
//!
//! The application event loop is the only place that mutates cross-cutting UI
//! state. Everything else communicates through these values (or through the
//! bounded channels declared in the PTY crate), which keeps the data flow
//! auditable and avoids hidden global state.

use crate::geometry::SplitAxis;
use crate::ids::{PaneId, ProfileId, SessionId, TabId, WindowId};

/// How a child process ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExitReason {
    /// The process exited normally with `code`.
    Code(i32),
    /// The process was terminated by `signal` (for example `SIGKILL`).
    Signal(String),
    /// The PTY closed without a waitable status (for example after a read error).
    Unknown,
}

impl ExitReason {
    /// Human readable, single-line description used in the UI.
    pub fn describe(&self) -> String {
        match self {
            ExitReason::Code(0) => "exited normally".to_owned(),
            ExitReason::Code(code) => format!("exited with status {code}"),
            ExitReason::Signal(signal) => format!("terminated by {signal}"),
            ExitReason::Unknown => "closed".to_owned(),
        }
    }

    /// Whether the session ended successfully.
    pub fn is_success(&self) -> bool {
        matches!(self, ExitReason::Code(0))
    }
}

/// User-intent level commands produced by keybindings, the CLI, menus or the
/// settings panel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AppCommand {
    NewTab {
        profile: Option<ProfileId>,
    },
    CloseTab {
        tab: TabId,
    },
    RenameTab {
        tab: TabId,
        title: String,
    },
    MoveTab {
        from: usize,
        to: usize,
    },
    /// Moves the active tab by `delta` positions (negative moves left).
    MoveTabRelative {
        delta: i32,
    },
    NextTab,
    PreviousTab,
    SelectTab {
        index: usize,
    },
    SplitPane {
        pane: PaneId,
        axis: SplitAxis,
    },
    ClosePane {
        pane: PaneId,
    },
    FocusPane {
        pane: PaneId,
    },
    FocusDirection {
        direction: FocusDirection,
    },
    ResizeDivider {
        axis: SplitAxis,
        delta_percent: i16,
    },
    EqualizeSplits,
    ToggleZoom,
    NewWindow {
        profile: Option<ProfileId>,
    },
    CloseWindow {
        window: WindowId,
    },
    CopySelection,
    Paste,
    Search {
        forward: bool,
    },
    SearchNext,
    SearchPrevious,
    CloseSearch,
    ScrollLines {
        delta: i32,
    },
    ScrollToBottom,
    /// Scrolls the viewport to the previous or the next shell prompt.
    ///
    /// The prompts come from the `OSC 133` marks of the focused pane's session, so a
    /// program that never sends them makes this command do nothing at all. `forward`
    /// moves towards the live screen.
    JumpToPrompt {
        forward: bool,
    },
    IncreaseFontSize,
    DecreaseFontSize,
    ResetFontSize,
    OpenSettings,
    ReloadConfig,
    ConfirmClose,
    ToggleStatusBar,
    /// Shows, hides or creates the window's floating terminal.
    ToggleFloating,
    SendLiteralNext,
    Quit,
}

/// Direction used by keyboard pane navigation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FocusDirection {
    Left,
    Right,
    Up,
    Down,
}

/// Events emitted by the PTY/session layer towards the application.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionEvent {
    /// New bytes are ready in the session's output buffer; the pane is dirty.
    Output { session: SessionId },
    /// The process title changed (OSC 0/2) or was reset.
    Title {
        session: SessionId,
        title: Option<String>,
    },
    /// The terminal requested a bell.
    Bell { session: SessionId },
    /// The running program asked for a desktop notification (OSC 9 or
    /// `OSC 777;notify;`). The text is untrusted and still raw: the application
    /// sanitizes it and applies the `[notifications]` policy.
    Notification {
        session: SessionId,
        title: Option<String>,
        body: String,
    },
    /// An application asked for a colour (OSC 4/10/11). The application resolves it
    /// against the active theme and replies through the session.
    ColorQuery { session: SessionId, index: usize },
    /// The child process ended.
    Exited {
        session: SessionId,
        reason: ExitReason,
    },
}

/// Events delivered to the application event loop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AppEvent {
    /// A subcommand originating from the UI (keybinding, mouse, menu).
    Command(AppCommand),
    /// Session-layer notification.
    Session(SessionEvent),
    /// The configuration file changed on disk.
    ConfigFileChanged,
    /// The settings panel changed a value; the app persists it.
    SettingsChanged,
    /// Redraw was requested (coalesced).
    Redraw,
    /// The application should terminate.
    Quit,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_reason_description_is_stable() {
        assert_eq!(ExitReason::Code(0).describe(), "exited normally");
        assert_eq!(ExitReason::Code(2).describe(), "exited with status 2");
        assert_eq!(
            ExitReason::Signal("SIGKILL".to_owned()).describe(),
            "terminated by SIGKILL"
        );
        assert_eq!(ExitReason::Unknown.describe(), "closed");
    }

    #[test]
    fn only_zero_exit_code_counts_as_success() {
        assert!(ExitReason::Code(0).is_success());
        assert!(!ExitReason::Code(1).is_success());
        assert!(!ExitReason::Signal("SIGTERM".to_owned()).is_success());
        assert!(!ExitReason::Unknown.is_success());
    }
}
