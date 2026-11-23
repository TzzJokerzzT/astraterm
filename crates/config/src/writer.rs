//! Persistence of the configuration model.
//!
//! Writes are atomic (temporary file in the destination directory, `fsync`, then
//! `rename`) so an interrupted save can never truncate the user's configuration.
//!
//! Serialization strategy: the graphical settings panel saves a *canonical*
//! file produced from the typed model. Comments in a hand-written file are
//! therefore not preserved across GUI saves; this is documented in
//! `docs/CONFIGURATION.md`, and the header written into the file says so.

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::schema::Config;

/// Error produced while serializing or writing the configuration.
#[derive(Debug, thiserror::Error)]
pub enum WriteError {
    #[error("configuration could not be serialized: {0}")]
    Serialize(String),
    #[error("configuration could not be written to `{path}`: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
}

const HEADER: &str = "\
# AstraTerm configuration (canonical form)
#
# This file is the single source of truth for both the TOML interface and the
# graphical settings panel. Saving from the settings panel rewrites this file in
# canonical form, which does not preserve hand-written comments.
#
# Unknown keys are reported as warnings and ignored. Empty or `none` values in
# [keys] unbind an action.
";

/// Serializes a configuration to canonical TOML text.
pub fn to_toml_string(config: &Config) -> Result<String, WriteError> {
    let body =
        toml::to_string_pretty(config).map_err(|error| WriteError::Serialize(error.to_string()))?;
    Ok(format!("{HEADER}\n{body}"))
}

/// Writes `contents` to `path` atomically.
pub fn write_atomic(path: &Path, contents: &str) -> Result<(), WriteError> {
    let io_error = |source: std::io::Error| WriteError::Io {
        path: path.to_path_buf(),
        source,
    };

    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).map_err(io_error)?;
    }

    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "config.toml".to_owned());
    let temp_path = path.with_file_name(format!(".{file_name}.tmp"));

    {
        let mut file = std::fs::File::create(&temp_path).map_err(io_error)?;
        file.write_all(contents.as_bytes()).map_err(io_error)?;
        file.sync_all().map_err(io_error)?;
    }

    std::fs::rename(&temp_path, path).map_err(|source| {
        let _ = std::fs::remove_file(&temp_path);
        io_error(source)
    })?;

    // Best-effort directory sync so the rename itself is durable.
    if let Some(parent) = path.parent()
        && let Ok(dir) = std::fs::File::open(parent)
    {
        let _ = dir.sync_all();
    }
    Ok(())
}

/// Serializes and writes a configuration.
pub fn save(config: &Config, path: &Path) -> Result<(), WriteError> {
    let text = to_toml_string(config)?;
    write_atomic(path, &text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loader::parse_str;

    #[test]
    fn round_trips_through_text() {
        let config = Config::default();
        let text = to_toml_string(&config).unwrap();
        let reparsed = parse_str(&text, "canonical").unwrap();
        assert_eq!(reparsed.config, config);
        assert!(reparsed.validation().is_valid(), "{:?}", reparsed.issues);
    }

    #[test]
    fn header_explains_canonical_form() {
        let text = to_toml_string(&Config::default()).unwrap();
        assert!(text.starts_with("# AstraTerm configuration"));
        assert!(text.contains("canonical form"));
    }

    #[test]
    fn atomic_write_replaces_contents_completely() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        write_atomic(&path, "first").unwrap();
        write_atomic(&path, "second").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "second");
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().contains("tmp"))
            .collect();
        assert!(leftovers.is_empty(), "temporary file must be renamed away");
    }

    #[test]
    fn creates_missing_parent_directories() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/deeper/config.toml");
        write_atomic(&path, "ok").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "ok");
    }

    #[test]
    fn save_then_load_preserves_values() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut config = Config::default();
        config.font.size = 15.5;
        config.terminal.scrollback_lines = 5000;
        save(&config, &path).unwrap();
        let loaded = crate::loader::load_file(&path).unwrap();
        assert_eq!(loaded.config.font.size, 15.5);
        assert_eq!(loaded.config.terminal.scrollback_lines, 5000);
    }
}
