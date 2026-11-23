//! Embedded defaults and first-run bootstrap.
//!
//! Defaults live in code (in [`crate::schema`]) so the application always has a
//! complete configuration even with no file on disk. This module adds the
//! first-run experience: writing a documented file so the user has something to
//! edit.

use std::path::{Path, PathBuf};

use crate::schema::Config;
use crate::writer::{self, WriteError};

/// The built-in default configuration.
pub fn default_config() -> Config {
    Config::default()
}

/// Canonical TOML text for the default configuration.
pub fn default_config_toml() -> Result<String, WriteError> {
    writer::to_toml_string(&Config::default())
}

/// Creates the configuration file with defaults when it does not exist yet.
///
/// Returns the path that was written, or `None` when the file already existed.
/// A failure is not fatal for the application: defaults remain in memory.
pub fn ensure_config_file(path: &Path) -> Result<Option<PathBuf>, WriteError> {
    if path.exists() {
        return Ok(None);
    }
    let text = default_config_toml()?;
    writer::write_atomic(path, &text)?;
    tracing::info!(path = %path.display(), "wrote default configuration");
    Ok(Some(path.to_path_buf()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_creates_once_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        assert_eq!(ensure_config_file(&path).unwrap(), Some(path.clone()));
        assert!(path.exists());
        assert_eq!(ensure_config_file(&path).unwrap(), None);
    }

    #[test]
    fn written_defaults_parse_back_to_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        ensure_config_file(&path).unwrap();
        let outcome = crate::loader::load_file(&path).unwrap();
        assert_eq!(outcome.config, Config::default());
        assert!(outcome.validation().is_valid());
    }
}
