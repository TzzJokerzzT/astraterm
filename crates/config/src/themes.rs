//! Themes: built-in palettes plus user-provided theme files.
//!
//! Theme files live under `~/.config/astraterm/themes/*.toml` and use the same
//! [`Palette`] schema as the built-ins, so the settings panel can list, preview
//! and apply any of them through one code path.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::palette::Palette;

/// A named palette.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Theme {
    pub name: String,
    /// Marks light themes so the settings panel can group them sensibly.
    #[serde(default)]
    pub light: bool,
    #[serde(flatten)]
    pub palette: Palette,
}

impl Theme {
    pub fn dark(name: impl Into<String>, palette: Palette) -> Self {
        Self {
            name: name.into(),
            light: false,
            palette,
        }
    }

    /// Background colour used behind the whole window.
    pub fn is_light(&self) -> bool {
        self.palette.background.luminance() > 0.5
    }
}

/// Error raised when a theme file cannot be read or parsed.
#[derive(Debug, thiserror::Error)]
pub enum ThemeError {
    #[error("theme file `{path}` could not be read: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("theme file `{path}` is not valid: {message}")]
    Parse { path: PathBuf, message: String },
}

/// Every theme shipped with the application.
pub fn builtin_themes() -> Vec<Theme> {
    vec![
        Theme::dark("midnight", Palette::midnight()),
        Theme::dark("dawn", Palette::dawn()),
    ]
}

/// Looks up a built-in theme by name (case-insensitive).
pub fn theme_by_name(name: &str) -> Option<Theme> {
    builtin_themes()
        .into_iter()
        .find(|theme| theme.name.eq_ignore_ascii_case(name))
}

/// Loads a single theme file.
pub fn load_theme_file(path: &Path) -> Result<Theme, ThemeError> {
    let text = std::fs::read_to_string(path).map_err(|source| ThemeError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let mut theme: Theme = toml::from_str(&text).map_err(|error| ThemeError::Parse {
        path: path.to_path_buf(),
        message: error.to_string(),
    })?;
    if theme.name.trim().is_empty() {
        theme.name = path
            .file_stem()
            .map(|stem| stem.to_string_lossy().to_string())
            .unwrap_or_else(|| "unnamed".to_owned());
    }
    Ok(theme)
}

/// Loads every theme file in `dir`, ignoring broken files but reporting them.
pub fn load_theme_dir(dir: &Path) -> (Vec<Theme>, Vec<ThemeError>) {
    let mut themes = Vec::new();
    let mut errors = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (themes, errors);
    };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "toml"))
        .collect();
    paths.sort();
    for path in paths {
        match load_theme_file(&path) {
            Ok(theme) => themes.push(theme),
            Err(error) => errors.push(error),
        }
    }
    (themes, errors)
}

/// Turns a user-provided theme name into a safe file name stem.
///
/// Lowercases, collapses anything that is not `[a-z0-9]` into a single dash and
/// trims dashes, so a name like `Mi Tema / Oscuro` becomes `mi-tema-oscuro`. Returns
/// `None` when nothing usable is left.
pub fn sanitize_theme_name(name: &str) -> Option<String> {
    let mut out = String::new();
    let mut last_dash = true;
    for ch in name.trim().to_lowercase().chars() {
        if out.chars().count() >= 40 {
            break;
        }
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() { None } else { Some(out) }
}

/// Error raised while saving a theme.
#[derive(Debug, thiserror::Error)]
pub enum ThemeSaveError {
    #[error("the theme name needs at least one letter or digit")]
    EmptyName,
    #[error("`{0}` is a built-in theme name; choose another one")]
    ReservedName(String),
    #[error("theme file `{path}` could not be written: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: crate::writer::WriteError,
    },
}

/// Saves `palette` as a user theme called `name` inside `dir`.
///
/// The file is written atomically and can be read back with [`load_theme_file`]. A
/// name that collides with a built-in theme is refused so a user theme can never
/// silently shadow the shipped palette.
pub fn save_theme(
    dir: &Path,
    name: &str,
    palette: Palette,
) -> Result<(String, PathBuf), ThemeSaveError> {
    let stem = sanitize_theme_name(name).ok_or(ThemeSaveError::EmptyName)?;
    if theme_by_name(&stem).is_some() {
        return Err(ThemeSaveError::ReservedName(stem));
    }
    let theme = Theme {
        name: stem.clone(),
        light: palette.background.luminance() > 0.5,
        palette,
    };
    let text = toml::to_string_pretty(&theme).map_err(|error| ThemeSaveError::Write {
        path: dir.join(format!("{stem}.toml")),
        source: crate::writer::WriteError::Serialize(error.to_string()),
    })?;
    let path = dir.join(format!("{stem}.toml"));
    crate::writer::write_atomic(&path, &text).map_err(|source| ThemeSaveError::Write {
        path: path.clone(),
        source,
    })?;
    Ok((stem, path))
}

/// Built-in themes followed by user themes, with user themes winning on a name
/// clash so a personal theme can override a built-in one deliberately.
pub fn available_themes(user_dir: &Path) -> (Vec<Theme>, Vec<ThemeError>) {
    let (user, errors) = load_theme_dir(user_dir);
    let mut themes = builtin_themes();
    for theme in user {
        themes.retain(|existing| !existing.name.eq_ignore_ascii_case(&theme.name));
        themes.push(theme);
    }
    (themes, errors)
}

/// Resolves a theme name against built-ins and the user theme directory,
/// falling back to the default theme.
pub fn resolve_theme(name: &str, user_dir: &Path) -> Theme {
    if let Some(theme) = theme_by_name(name) {
        return theme;
    }
    let (themes, _) = available_themes(user_dir);
    if let Some(theme) = themes
        .into_iter()
        .find(|theme| theme.name.eq_ignore_ascii_case(name))
    {
        return theme;
    }
    Theme::dark("midnight", Palette::midnight())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_lookup_is_case_insensitive() {
        assert!(theme_by_name("MIDNIGHT").is_some());
        assert_eq!(theme_by_name("midnight").unwrap().name, "midnight");
        assert!(theme_by_name("nope").is_none());
    }

    #[test]
    fn builtin_themes_differ_in_background() {
        let themes = builtin_themes();
        assert_eq!(themes.len(), 2);
        assert!(!themes[0].is_light());
        assert!(themes[1].is_light());
    }

    #[test]
    fn unknown_theme_falls_back_to_midnight() {
        let dir = std::path::Path::new("/nonexistent-theme-dir");
        assert_eq!(resolve_theme("does-not-exist", dir).name, "midnight");
    }

    #[test]
    fn names_are_sanitised_into_file_stems() {
        assert_eq!(
            sanitize_theme_name("Mi Tema / Oscuro").as_deref(),
            Some("mi-tema-oscuro")
        );
        assert_eq!(sanitize_theme_name("  neon!!  ").as_deref(), Some("neon"));
        assert_eq!(sanitize_theme_name("---").as_deref(), None);
        assert_eq!(sanitize_theme_name("").as_deref(), None);
        let long = "x".repeat(60);
        assert!(sanitize_theme_name(&long).unwrap().chars().count() <= 40);
    }

    #[test]
    fn saving_a_theme_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let mut palette = Palette::midnight();
        palette.accent = crate::palette::Rgb::new(0xff, 0x88, 0x00);
        let (name, path) = save_theme(dir.path(), "Mi Tema", palette.clone()).unwrap();
        assert_eq!(name, "mi-tema");
        assert_eq!(path.file_name().unwrap(), "mi-tema.toml");
        let loaded = load_theme_file(&path).unwrap();
        assert_eq!(loaded.name, "mi-tema");
        assert_eq!(loaded.palette.accent, palette.accent);
        assert!(!loaded.light);
    }

    #[test]
    fn saving_refuses_builtin_names_and_empty_names() {
        let dir = tempfile::tempdir().unwrap();
        let palette = Palette::midnight();
        assert!(matches!(
            save_theme(dir.path(), "midnight", palette.clone()),
            Err(ThemeSaveError::ReservedName(_))
        ));
        assert!(matches!(
            save_theme(dir.path(), "!!!", palette),
            Err(ThemeSaveError::EmptyName)
        ));
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            0,
            "nothing is written"
        );
    }

    #[test]
    fn a_saved_theme_overrides_a_user_theme_with_the_same_name() {
        let dir = tempfile::tempdir().unwrap();
        save_theme(dir.path(), "custom", Palette::midnight()).unwrap();
        let mut second = Palette::midnight();
        second.background = crate::palette::Rgb::new(1, 2, 3);
        save_theme(dir.path(), "custom", second.clone()).unwrap();
        let (themes, errors) = available_themes(dir.path());
        assert!(errors.is_empty(), "{errors:?}");
        let found = themes.iter().find(|theme| theme.name == "custom").unwrap();
        assert_eq!(found.palette.background, second.background);
    }

    #[test]
    fn theme_file_round_trips_through_toml() {
        let theme = Theme::dark("custom", Palette::midnight());
        let text = toml::to_string_pretty(&theme).unwrap();
        let parsed: Theme = toml::from_str(&text).unwrap();
        assert_eq!(parsed, theme);
    }
}
