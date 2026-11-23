//! Preferences that live in the state directory instead of the user's config.
//!
//! The font size chosen with `increase-font-size` / `decrease-font-size` is not a
//! configuration decision the user has to edit by hand, so it is persisted as
//! runtime state. Writing it into `config.toml` instead would retrigger the file
//! watcher and re-apply the whole configuration on every zoom step.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// Smallest font size the zoom keys can reach.
pub const MIN_FONT_SIZE: f32 = 4.0;

/// Largest font size the zoom keys can reach.
pub const MAX_FONT_SIZE: f32 = 96.0;

/// One zoom step, in points.
pub const FONT_SIZE_STEP: f32 = 0.5;

/// Runtime preferences stored next to the workspace state.
///
/// Unknown keys are ignored on load so a newer build can add options without
/// making an older one discard the whole file.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    /// Font size chosen with the zoom keys; `None` means the configured size.
    pub font_size: Option<f32>,
}

impl Preferences {
    pub fn with_font_size(font_size: Option<f32>) -> Self {
        Self { font_size }
    }

    /// Reads the file, falling back to defaults when it is missing or unreadable.
    ///
    /// A damaged preferences file must never stop the application from starting:
    /// the worst case is losing the zoom, not losing the session.
    pub fn load(path: &Path) -> Self {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        match toml::from_str(&text) {
            Ok(preferences) => preferences,
            Err(error) => {
                tracing::warn!(error = %error, "ignoring unreadable preferences file");
                Self::default()
            }
        }
    }

    /// Writes the file atomically, so a crash cannot leave a truncated state file.
    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        let text = toml::to_string(self)?;
        astraterm_config::writer::write_atomic(path, &text)?;
        Ok(())
    }

    /// Font size to apply: clamped to the supported range, non-finite values dropped.
    pub fn sanitized_font_size(&self) -> Option<f32> {
        self.font_size
            .filter(|size| size.is_finite())
            .map(|size| size.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE))
    }
}

/// The font size one zoom step away from `current`, clamped to the supported range.
pub fn next_font_size(current: f32, delta: f32) -> f32 {
    let current = if current.is_finite() { current } else { 13.0 };
    (current + delta).clamp(MIN_FONT_SIZE, MAX_FONT_SIZE)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("astraterm-prefs-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("preferences.toml")
    }

    #[test]
    fn saved_font_size_round_trips() {
        let path = scratch("round-trip");
        Preferences::with_font_size(Some(15.5)).save(&path).unwrap();
        assert_eq!(Preferences::load(&path).sanitized_font_size(), Some(15.5));
    }

    #[test]
    fn cleared_font_size_round_trips() {
        let path = scratch("cleared");
        Preferences::with_font_size(Some(15.5)).save(&path).unwrap();
        Preferences::with_font_size(None).save(&path).unwrap();
        assert_eq!(Preferences::load(&path).sanitized_font_size(), None);
    }

    #[test]
    fn missing_file_loads_defaults() {
        let path = scratch("missing");
        let missing = path.with_file_name("does-not-exist.toml");
        assert_eq!(Preferences::load(&missing).sanitized_font_size(), None);
    }

    #[test]
    fn corrupt_file_loads_defaults() {
        let path = scratch("corrupt");
        std::fs::write(&path, "font_size = [not a number").unwrap();
        assert_eq!(Preferences::load(&path).sanitized_font_size(), None);
    }

    #[test]
    fn unknown_keys_do_not_discard_the_font_size() {
        let path = scratch("unknown-keys");
        std::fs::write(&path, "font_size = 17.0\nfuture_option = true\n").unwrap();
        assert_eq!(Preferences::load(&path).sanitized_font_size(), Some(17.0));
    }

    #[test]
    fn out_of_range_font_size_is_clamped() {
        let too_big = Preferences::with_font_size(Some(500.0));
        let too_small = Preferences::with_font_size(Some(0.5));
        assert_eq!(too_big.sanitized_font_size(), Some(MAX_FONT_SIZE));
        assert_eq!(too_small.sanitized_font_size(), Some(MIN_FONT_SIZE));
    }

    #[test]
    fn non_finite_font_size_is_ignored() {
        for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert_eq!(
                Preferences::with_font_size(Some(value)).sanitized_font_size(),
                None
            );
        }
    }

    #[test]
    fn next_font_size_steps_and_clamps() {
        assert_eq!(next_font_size(13.0, 0.5), 13.5);
        assert_eq!(next_font_size(13.0, -0.5), 12.5);
        assert_eq!(next_font_size(MAX_FONT_SIZE, 0.5), MAX_FONT_SIZE);
        assert_eq!(next_font_size(MIN_FONT_SIZE, -0.5), MIN_FONT_SIZE);
    }
}
