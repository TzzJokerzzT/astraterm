//! XDG state paths for runtime data (workspace metadata, logs).

use std::path::PathBuf;

/// Paths under `$XDG_STATE_HOME/astraterm`.
#[derive(Clone, Debug)]
pub struct StatePaths {
    root: PathBuf,
}

impl StatePaths {
    /// Resolves the state directory, honouring `XDG_STATE_HOME`.
    pub fn resolve() -> Self {
        let root = astraterm_config::state_dir();
        Self { root }
    }

    /// Uses an explicit root (tests, `--config` overrides).
    pub fn with_root(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn root(&self) -> &std::path::Path {
        &self.root
    }

    /// File that stores the restored workspace layout.
    pub fn workspace_file(&self) -> PathBuf {
        self.root.join("workspace.toml")
    }

    /// File that stores window geometry.
    pub fn geometry_file(&self) -> PathBuf {
        self.root.join("window.toml")
    }

    /// File that stores runtime preferences (currently the zoom level).
    pub fn preferences_file(&self) -> PathBuf {
        self.root.join("preferences.toml")
    }

    /// Creates the state directory.
    pub fn ensure(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.root)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_live_under_the_state_root() {
        let paths = StatePaths::with_root(PathBuf::from("/tmp/astraterm-state"));
        assert!(paths.workspace_file().starts_with("/tmp/astraterm-state"));
        assert!(paths.geometry_file().ends_with("window.toml"));
        assert!(paths.preferences_file().ends_with("preferences.toml"));
    }

    #[test]
    fn root_is_writable_after_ensure() {
        let dir = std::env::temp_dir().join(format!("astraterm-state-{}", std::process::id()));
        let paths = StatePaths::with_root(dir.clone());
        paths.ensure().unwrap();
        assert!(dir.is_dir());
        std::fs::remove_dir_all(&dir).ok();
    }
}
