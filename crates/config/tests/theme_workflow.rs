//! The theme workflow the settings panel drives: edit a palette, save it as a named
//! theme, select it, and get it back on the next load.

use astraterm_config::service::ConfigService;
use astraterm_config::{Palette, Rgb, resolve_theme, save_theme};

#[test]
fn a_saved_theme_becomes_selectable_and_survives_a_reload() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.toml");
    let themes_dir = dir.path().join("themes");
    std::fs::create_dir_all(&themes_dir).unwrap();

    let mut service = ConfigService::load(&config_path);

    let mut palette = Palette::midnight();
    palette.background = Rgb::new(0x10, 0x14, 0x1c);
    palette.accent = Rgb::new(0xff, 0x7a, 0x00);
    let (name, path) = save_theme(&themes_dir, "Mi Tema", palette.clone()).unwrap();
    assert_eq!(name, "mi-tema");
    assert!(path.ends_with("mi-tema.toml"));

    // The service must list it and persist the selection.
    service.refresh_themes();
    assert!(service.themes().iter().any(|theme| theme.name == name));
    service
        .update(|config| config.appearance.theme = name.clone())
        .unwrap();

    // Selecting it resolves to the saved palette, not to the built-in one.
    let resolved = resolve_theme(&name, &themes_dir);
    assert_eq!(resolved.palette.background, palette.background);
    assert_eq!(resolved.palette.accent, palette.accent);

    // A fresh load keeps the selection and still finds the file.
    let reloaded = ConfigService::load(&config_path);
    assert_eq!(reloaded.config().appearance.theme, name);
    assert_eq!(reloaded.theme().palette.accent, palette.accent);
}

#[test]
fn a_reserved_name_is_refused_and_nothing_is_written() {
    let dir = tempfile::tempdir().unwrap();
    let error = save_theme(dir.path(), "midnight", Palette::midnight()).unwrap_err();
    assert!(error.to_string().contains("built-in"), "{error}");
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}
