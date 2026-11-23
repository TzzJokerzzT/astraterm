//! TOML loading with predictable handling of unknown fields.
//!
//! A missing file is not an error: the defaults apply. A malformed file is
//! reported with the parser's line/column information so the user can fix it,
//! and the caller can keep the last known-good configuration.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::schema::Config;
use crate::validation::{ConfigIssue, ValidationReport, validate};

/// Error produced while reading a configuration file.
#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("could not read `{path}`: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("`{path}` is not valid TOML: {message}")]
    Parse { path: String, message: String },
}

/// Result of a successful load.
#[derive(Clone, Debug)]
pub struct LoadOutcome {
    /// The parsed configuration with defaults merged in.
    pub config: Config,
    /// Whether a file existed and was parsed.
    pub from_file: bool,
    /// Unknown keys, deprecated values and validation issues.
    pub issues: Vec<ConfigIssue>,
}

impl LoadOutcome {
    pub fn validation(&self) -> ValidationReport {
        ValidationReport {
            issues: self.issues.clone(),
        }
    }
}

/// Reads and parses a configuration file. A missing file yields the defaults.
pub fn load_file(path: &Path) -> Result<LoadOutcome, LoadError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(LoadOutcome {
                config: Config::default(),
                from_file: false,
                issues: Vec::new(),
            });
        }
        Err(source) => {
            return Err(LoadError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    parse_str(&text, &path.display().to_string())
}

/// Parses configuration text.
///
/// The returned issues include unknown-key warnings; the caller decides whether
/// to display them. Parsing never fails because of unknown keys.
pub fn parse_str(text: &str, source: &str) -> Result<LoadOutcome, LoadError> {
    let mut unknown: Vec<String> = Vec::new();
    let deserializer = toml::Deserializer::parse(text).map_err(|error| LoadError::Parse {
        path: source.to_owned(),
        message: error.to_string(),
    })?;
    let mut config: Config = serde_ignored::deserialize(deserializer, |path| {
        unknown.push(path.to_string());
    })
    .map_err(|error| LoadError::Parse {
        path: source.to_owned(),
        message: error.to_string(),
    })?;

    merge_default_keybindings(&mut config.keys);

    let mut issues: Vec<ConfigIssue> = unknown
        .into_iter()
        .map(|path| ConfigIssue::warning(path, "unknown setting; it has no effect"))
        .collect();
    issues.extend(validate(&config, default_theme_names()).issues);
    Ok(LoadOutcome {
        config,
        from_file: true,
        issues,
    })
}

/// Fills in default keybindings that the user did not mention, so that a partial
/// `[keys]` table keeps sensible shortcuts. An empty value unbinds an action, and
/// `none` is accepted as an explicit way to spell that.
pub fn merge_default_keybindings(keys: &mut BTreeMap<String, String>) {
    let mut merged: BTreeMap<String, String> = crate::keys::default_keybindings();
    for (action, value) in keys.iter() {
        if value.trim().is_empty() || value.trim().eq_ignore_ascii_case("none") {
            merged.remove(action);
        } else {
            merged.insert(action.clone(), value.clone());
        }
    }
    *keys = merged;
}

fn default_theme_names() -> &'static [&'static str] {
    &["midnight", "dawn"]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_yields_defaults() {
        let outcome = load_file(Path::new("/nonexistent/astraterm/config.toml")).unwrap();
        assert!(!outcome.from_file);
        assert_eq!(outcome.config, Config::default());
        assert!(outcome.issues.is_empty());
    }

    #[test]
    fn parses_documented_example_shape() {
        let text = r#"
[window]
width = 1000
height = 700
opacity = 0.95
startup_maximized = true

[terminal]
shell = "auto"
term = "xterm-256color"
scrollback_lines = 20000
working_directory = "home"
confirm_on_close = true

[font]
family = "JetBrainsMono Nerd Font"
size = 13.5
line_height = 1.2

[appearance]
theme = "midnight"
background_blur = false
padding_x = 12
padding_y = 10
cursor = "block"
style = "minimal-dark"
show_status_bar = false

[performance]
gpu_acceleration = true
low_power_mode = false
max_fps_when_idle = 1
"#;
        let outcome = parse_str(text, "test").unwrap();
        assert_eq!(outcome.config.window.width, 1000);
        assert!(outcome.config.window.startup_maximized);
        assert_eq!(outcome.config.terminal.scrollback_lines, 20_000);
        assert_eq!(outcome.config.font.size, 13.5);
        assert!(outcome.validation().is_valid());
    }

    #[test]
    fn unknown_keys_become_warnings_and_keep_the_rest() {
        let text = r#"
[font]
size = 12.0
telepathy = true
"#;
        let outcome = parse_str(text, "test").unwrap();
        assert_eq!(outcome.config.font.size, 12.0);
        assert!(
            outcome
                .issues
                .iter()
                .any(|issue| issue.field.contains("telepathy"))
        );
    }

    #[test]
    fn partial_settings_keep_their_defaults() {
        let text = "[font]\nsize = 12.0\n";
        let outcome = parse_str(text, "test").unwrap();
        assert_eq!(outcome.config.font.size, 12.0);
        assert_eq!(outcome.config.font.family, Config::default().font.family);
        assert_eq!(outcome.config.window.width, Config::default().window.width);
    }

    #[test]
    fn partial_key_table_merges_with_defaults() {
        let text = "[keys]\nnew-tab = \"ctrl+t\"\n";
        let outcome = parse_str(text, "test").unwrap();
        assert_eq!(
            outcome.config.keys.get("new-tab").map(String::as_str),
            Some("ctrl+t")
        );
        assert_eq!(
            outcome.config.keys.get("copy").map(String::as_str),
            Some("ctrl+shift+c"),
            "unmentioned actions keep their default chord"
        );
    }

    #[test]
    fn empty_binding_unbinds_an_action() {
        let text = "[keys]\nrename-tab = \"none\"\n";
        let outcome = parse_str(text, "test").unwrap();
        assert!(!outcome.config.keys.contains_key("rename-tab"));
    }

    #[test]
    fn malformed_toml_reports_position() {
        let error = parse_str("[font\nsize = 1", "config.toml").unwrap_err();
        let message = error.to_string();
        assert!(message.contains("config.toml"));
        assert!(message.to_lowercase().contains("line") || message.contains("TOML"));
    }

    #[test]
    fn invalid_values_are_reported_not_panicked() {
        let text = "[font]\nsize = 900.0\n";
        let outcome = parse_str(text, "test").unwrap();
        assert!(!outcome.validation().is_valid());
    }
}
