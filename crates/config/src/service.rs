//! Configuration service: the single owner of the loaded configuration.
//!
//! The TOML loader, the atomic writer and the graphical settings panel all go
//! through this service, which guarantees that
//!
//! * every mutation is validated before it can be applied or persisted,
//! * values are only applied in memory after successful validation,
//! * a save never silently overwrites changes made to the file by another
//!   program, and
//! * the last known-good configuration survives a broken external edit.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::loader::{self, LoadError};
use crate::schema::Config;
use crate::themes::{Theme, available_themes, resolve_theme};
use crate::validation::{ConfigIssue, ValidationReport, validate};
use crate::writer;

/// Result of a save attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SaveOutcome {
    /// The file was rewritten.
    Written,
    /// The file already contained exactly this configuration.
    Unchanged,
}

/// Why a save did not happen.
#[derive(Debug, thiserror::Error)]
pub enum SaveError {
    #[error("configuration is invalid")]
    Invalid(Vec<ConfigIssue>),
    #[error("the configuration file changed on disk; reload before saving")]
    ExternalChange,
    #[error(transparent)]
    Write(#[from] writer::WriteError),
}

/// Result of reloading from disk.
#[derive(Debug)]
pub enum ReloadOutcome {
    /// Nothing changed on disk.
    Unchanged,
    /// A valid file was loaded and applied.
    Reloaded(Box<Config>),
    /// The file is invalid; the previous configuration is still active.
    Invalid(Vec<ConfigIssue>),
    /// The file disappeared; defaults are now active.
    Reset(Box<Config>),
}

/// Owns the loaded configuration and its relationship with the file on disk.
pub struct ConfigService {
    path: PathBuf,
    themes_dir: PathBuf,
    config: Config,
    themes: Vec<Theme>,
    issues: Vec<ConfigIssue>,
    load_error: Option<String>,
    disk_hash: Option<u64>,
    from_file: bool,
}

impl ConfigService {
    /// Loads the configuration at `path`, falling back to defaults on any error.
    ///
    /// Errors are reported through [`ConfigService::issues`] instead of aborting
    /// startup: a broken file must never prevent the terminal from opening.
    pub fn load(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let themes_dir = path
            .parent()
            .map(|parent| parent.join("themes"))
            .unwrap_or_else(crate::themes_dir);
        let (themes, theme_errors) = available_themes(&themes_dir);

        let mut service = Self {
            path,
            themes_dir,
            config: Config::default(),
            themes,
            issues: Vec::new(),
            load_error: None,
            disk_hash: None,
            from_file: false,
        };

        for error in theme_errors {
            service
                .issues
                .push(ConfigIssue::warning("themes", error.to_string()));
        }
        service.apply_load(service.path.clone());
        service
    }

    fn apply_load(&mut self, path: PathBuf) {
        self.disk_hash = read_hash(&path);
        match loader::load_file(&path) {
            Ok(outcome) => {
                self.config = outcome.config;
                self.from_file = outcome.from_file;
                self.issues = outcome.issues;
                self.load_error = None;
            }
            Err(error) => {
                self.load_error = Some(error.to_string());
                self.issues = vec![ConfigIssue::error(
                    "config",
                    match &error {
                        LoadError::Parse { message, .. } => message.clone(),
                        LoadError::Io { .. } => error.to_string(),
                    },
                )];
                // Keep the last known-good configuration, or defaults on first load.
            }
        }
        self.revalidate();
    }

    /// Re-runs validation with the currently known theme names.
    fn revalidate(&mut self) {
        let names: Vec<&str> = self
            .themes
            .iter()
            .map(|theme| theme.name.as_str())
            .collect();
        let report = validate(&self.config, &names);
        // Merge: keep loader/theme issues, replace previous validation issues.
        self.issues.retain(|issue| {
            !(issue.field == "appearance.theme"
                || issue.field == "keys"
                || issue.field == "profiles"
                || issue.field.starts_with("appearance.")
                || issue.field.starts_with("font.")
                || issue.field.starts_with("window.")
                || issue.field.starts_with("terminal.")
                || issue.field.starts_with("performance.")
                || issue.field == "working_directory")
        });
        self.issues.extend(
            report
                .issues
                .into_iter()
                .filter(|issue| issue.field != "themes"),
        );
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn themes_dir(&self) -> &Path {
        &self.themes_dir
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Whether a configuration file existed when the service last loaded it.
    pub fn from_file(&self) -> bool {
        self.from_file
    }

    pub fn issues(&self) -> &[ConfigIssue] {
        &self.issues
    }

    pub fn validation(&self) -> ValidationReport {
        ValidationReport {
            issues: self.issues.clone(),
        }
    }

    /// Error that prevented the file from being parsed, if any.
    pub fn load_error(&self) -> Option<&str> {
        self.load_error.as_deref()
    }

    pub fn themes(&self) -> &[Theme] {
        &self.themes
    }

    /// The theme currently selected by the configuration.
    pub fn theme(&self) -> Theme {
        resolve_theme(&self.config.appearance.theme, &self.themes_dir)
    }

    pub fn theme_by_name(&self, name: &str) -> Option<Theme> {
        self.themes
            .iter()
            .find(|theme| theme.name.eq_ignore_ascii_case(name))
            .cloned()
    }

    /// Reloads the theme list from disk (used after a config reload).
    pub fn refresh_themes(&mut self) {
        let (themes, errors) = available_themes(&self.themes_dir);
        self.themes = themes;
        for error in errors {
            self.issues
                .push(ConfigIssue::warning("themes", error.to_string()));
        }
        self.revalidate();
    }

    /// Validates and applies a replacement configuration, then persists it.
    ///
    /// Returns the issues found when the configuration is invalid; in that case
    /// neither the in-memory model nor the file change.
    pub fn replace(&mut self, config: Config) -> Result<SaveOutcome, SaveError> {
        let names: Vec<&str> = self
            .themes
            .iter()
            .map(|theme| theme.name.as_str())
            .collect();
        let report = validate(&config, &names);
        if !report.is_valid() {
            return Err(SaveError::Invalid(report.errors().cloned().collect()));
        }
        let previous = std::mem::replace(&mut self.config, config);
        match self.persist() {
            Ok(outcome) => {
                self.issues = report.issues;
                Ok(outcome)
            }
            Err(error) => {
                self.config = previous;
                Err(error)
            }
        }
    }

    /// Applies a mutation through [`ConfigService::replace`].
    pub fn update(&mut self, mutate: impl FnOnce(&mut Config)) -> Result<SaveOutcome, SaveError> {
        let mut draft = self.config.clone();
        mutate(&mut draft);
        self.replace(draft)
    }

    /// Writes the current configuration to disk if it differs from the file.
    fn persist(&mut self) -> Result<SaveOutcome, SaveError> {
        let current_hash = read_hash(&self.path);
        if current_hash != self.disk_hash {
            return Err(SaveError::ExternalChange);
        }
        let text = writer::to_toml_string(&self.config)?;
        let new_hash = hash_bytes(text.as_bytes());
        if current_hash == Some(new_hash) {
            return Ok(SaveOutcome::Unchanged);
        }
        writer::write_atomic(&self.path, &text)?;
        self.disk_hash = Some(new_hash);
        self.from_file = true;
        Ok(SaveOutcome::Written)
    }

    /// Detects an external edit and reloads it.
    pub fn reload_if_changed(&mut self) -> ReloadOutcome {
        let current_hash = read_hash(&self.path);
        if current_hash == self.disk_hash {
            return ReloadOutcome::Unchanged;
        }
        self.disk_hash = current_hash;
        self.refresh_themes();
        let Some(text) = std::fs::read_to_string(&self.path).ok() else {
            self.config = Config::default();
            self.from_file = false;
            self.revalidate();
            return ReloadOutcome::Reset(Box::new(self.config.clone()));
        };
        match loader::parse_str(&text, &self.path.display().to_string()) {
            Ok(outcome) => {
                self.config = outcome.config;
                self.from_file = true;
                self.load_error = None;
                self.issues = outcome.issues;
                self.revalidate();
                ReloadOutcome::Reloaded(Box::new(self.config.clone()))
            }
            Err(error) => {
                let issue = ConfigIssue::error("config", error.to_string());
                self.load_error = Some(error.to_string());
                self.issues.push(issue.clone());
                ReloadOutcome::Invalid(vec![issue])
            }
        }
    }
}

fn hash_bytes(bytes: &[u8]) -> u64 {
    use std::hash::Hasher;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    hasher.write(bytes);
    hasher.finish()
}

fn read_hash(path: &Path) -> Option<u64> {
    std::fs::read(path).ok().map(|bytes| hash_bytes(&bytes))
}

/// Watches the configuration file and reports changes on a channel.
///
/// The watcher only signals that *something* changed; the service decides
/// whether the new content is valid and whether it differs. Debouncing avoids
/// reacting to the temporary files written by editors and by our own atomic
/// saves.
pub struct ConfigWatcher {
    _watcher: notify::RecommendedWatcher,
}

impl ConfigWatcher {
    /// Starts watching `path` and its parent directory.
    pub fn watch(
        path: &Path,
        sender: crossbeam_channel::Sender<()>,
    ) -> notify::Result<ConfigWatcher> {
        use notify::{RecursiveMode, Watcher};

        let mut watcher =
            notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
                let Ok(event) = event else { return };
                if matches!(event.kind, notify::EventKind::Access(_)) {
                    return;
                }
                let _ = sender.try_send(());
            })?;

        let directory = path.parent().unwrap_or(Path::new("."));
        watcher.watch(directory, RecursiveMode::NonRecursive)?;

        // Avoid a storm of events from our own writes by draining them lazily in
        // the consumer; the debounce window lives there because it needs the
        // event loop.
        let _ = Duration::from_millis(200);
        Ok(ConfigWatcher { _watcher: watcher })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service_with_file(dir: &tempfile::TempDir) -> ConfigService {
        let path = dir.path().join("config.toml");
        ConfigService::load(path)
    }

    #[test]
    fn missing_file_loads_defaults_without_errors() {
        let dir = tempfile::tempdir().unwrap();
        let service = service_with_file(&dir);
        assert_eq!(*service.config(), Config::default());
        assert!(!service.from_file());
        assert!(service.validation().is_valid());
    }

    #[test]
    fn invalid_file_falls_back_to_defaults_and_reports() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[font\nsize = 1").unwrap();
        let service = ConfigService::load(&path);
        assert_eq!(*service.config(), Config::default());
        assert!(service.load_error().is_some());
    }

    #[test]
    fn update_persists_and_is_reloadable() {
        let dir = tempfile::tempdir().unwrap();
        let mut service = service_with_file(&dir);
        service.update(|config| config.font.size = 14.0).unwrap();
        let reloaded = ConfigService::load(service.path());
        assert_eq!(reloaded.config().font.size, 14.0);
    }

    #[test]
    fn invalid_update_does_not_touch_disk_or_memory() {
        let dir = tempfile::tempdir().unwrap();
        let mut service = service_with_file(&dir);
        let error = service
            .update(|config| config.font.size = 1000.0)
            .unwrap_err();
        assert!(matches!(error, SaveError::Invalid(_)));
        assert_eq!(service.config().font.size, Config::default().font.size);
        assert!(!service.path().exists());
    }

    #[test]
    fn external_edit_is_detected_instead_of_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let mut service = service_with_file(&dir);
        service.update(|config| config.font.size = 14.0).unwrap();

        let external = std::fs::read_to_string(service.path())
            .unwrap()
            .replace("size = 14.0", "size = 16.0");
        std::fs::write(service.path(), &external).unwrap();

        let error = service
            .update(|config| config.font.size = 15.0)
            .unwrap_err();
        assert!(matches!(error, SaveError::ExternalChange));
        assert!(
            std::fs::read_to_string(service.path())
                .unwrap()
                .contains("size = 16.0")
        );
    }

    #[test]
    fn reload_picks_up_external_edits() {
        let dir = tempfile::tempdir().unwrap();
        let mut service = service_with_file(&dir);
        service.update(|config| config.font.size = 14.0).unwrap();
        let external = std::fs::read_to_string(service.path())
            .unwrap()
            .replace("size = 14.0", "size = 15.0");
        std::fs::write(service.path(), external).unwrap();

        match service.reload_if_changed() {
            ReloadOutcome::Reloaded(config) => assert_eq!(config.font.size, 15.0),
            other => panic!("expected reload, got {other:?}"),
        }
        assert_eq!(service.config().font.size, 15.0);
    }

    #[test]
    fn reload_reports_invalid_external_edit_and_keeps_previous() {
        let dir = tempfile::tempdir().unwrap();
        let mut service = service_with_file(&dir);
        service.update(|config| config.font.size = 14.0).unwrap();
        std::fs::write(service.path(), "[font\nbroken").unwrap();

        let outcome = service.reload_if_changed();
        assert!(matches!(outcome, ReloadOutcome::Invalid(_)));
        assert_eq!(
            service.config().font.size,
            14.0,
            "last known-good config stays active"
        );
    }

    #[test]
    fn reload_without_changes_is_a_noop() {
        let dir = tempfile::tempdir().unwrap();
        let mut service = service_with_file(&dir);
        service.update(|config| config.font.size = 14.0).unwrap();
        assert!(matches!(
            service.reload_if_changed(),
            ReloadOutcome::Unchanged
        ));
    }

    #[test]
    fn deleting_the_file_resets_to_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let mut service = service_with_file(&dir);
        service.update(|config| config.font.size = 14.0).unwrap();
        std::fs::remove_file(service.path()).unwrap();
        assert!(matches!(
            service.reload_if_changed(),
            ReloadOutcome::Reset(_)
        ));
        assert_eq!(service.config().font.size, Config::default().font.size);
    }

    #[test]
    fn theme_resolution_prefers_the_configured_theme() {
        let dir = tempfile::tempdir().unwrap();
        let mut service = service_with_file(&dir);
        service
            .update(|config| config.appearance.theme = "dawn".to_owned())
            .unwrap();
        assert_eq!(service.theme().name, "dawn");
    }
}
