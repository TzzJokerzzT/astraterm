//! Pane identity and per-pane metadata.

use std::path::PathBuf;

use astraterm_core::{PaneId, ProfileId, SessionId};

pub use crate::layout::Placement as PaneRect;

/// A terminal pane: a stable identity plus the metadata the UI needs.
///
/// The pane deliberately does not own its terminal state or PTY: those live in
/// the session layer, bound through `session`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pane {
    pub id: PaneId,
    /// Session bound to this pane once the PTY has been created.
    pub session: Option<SessionId>,
    /// Title reported by the running application (OSC 0/2) or the fallback.
    pub title: String,
    /// Last known working directory, used for restoration and for new splits.
    pub cwd: Option<PathBuf>,
    /// Profile that created this pane, if any.
    pub profile: Option<ProfileId>,
}

impl Pane {
    pub fn new(id: PaneId) -> Self {
        Self {
            id,
            session: None,
            title: String::new(),
            cwd: None,
            profile: None,
        }
    }

    /// Title shown in the tab strip and window title.
    ///
    /// Falls back to the directory name, then to the shell name, so a tab is never
    /// labelled with an empty string.
    pub fn display_title(&self, shell: &str) -> String {
        if !self.title.trim().is_empty() {
            return self.title.clone();
        }
        if let Some(cwd) = &self.cwd
            && let Some(name) = cwd.file_name().filter(|name| !name.is_empty())
        {
            return name.to_string_lossy().to_string();
        }
        let shell_name = shell.rsplit('/').next().unwrap_or(shell);
        if shell_name.trim().is_empty() {
            "shell".to_owned()
        } else {
            shell_name.to_owned()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_prefers_the_application_title() {
        let mut pane = Pane::new(PaneId::next());
        pane.title = "nvim".to_owned();
        assert_eq!(pane.display_title("zsh"), "nvim");
    }

    #[test]
    fn title_falls_back_to_directory_then_shell() {
        let mut pane = Pane::new(PaneId::next());
        pane.cwd = Some(PathBuf::from("/home/user/project"));
        assert_eq!(pane.display_title("zsh"), "project");

        let pane = Pane::new(PaneId::next());
        assert_eq!(pane.display_title("/usr/bin/fish"), "fish");
        assert_eq!(pane.display_title(""), "shell");
    }
}
