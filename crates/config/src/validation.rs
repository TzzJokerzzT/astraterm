//! Validation of the configuration model.
//!
//! Validation is pure: it takes a [`Config`] (plus the set of known theme names)
//! and returns actionable issues. Errors block persistence; warnings are shown
//! but do not stop the user. The same rules run for TOML input and for edits
//! made in the graphical settings panel.

use std::path::Path;

use crate::keys::Keymap;
use crate::palette::Rgb;
use crate::schema::{Config, CursorStyle, WorkingDirectory};

/// Severity of a configuration issue.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum IssueSeverity {
    Warning,
    Error,
}

/// A single problem found in the configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigIssue {
    pub severity: IssueSeverity,
    /// Dotted field path, for example `font.size`.
    pub field: String,
    pub message: String,
}

impl ConfigIssue {
    pub fn error(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            severity: IssueSeverity::Error,
            field: field.into(),
            message: message.into(),
        }
    }

    pub fn warning(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            severity: IssueSeverity::Warning,
            field: field.into(),
            message: message.into(),
        }
    }

    /// Single-line rendering used by the settings panel and the CLI.
    pub fn describe(&self) -> String {
        let kind = match self.severity {
            IssueSeverity::Error => "error",
            IssueSeverity::Warning => "warning",
        };
        format!("{kind}: {}: {}", self.field, self.message)
    }
}

/// Result of validating a configuration.
#[derive(Clone, Debug, Default)]
pub struct ValidationReport {
    pub issues: Vec<ConfigIssue>,
}

impl ValidationReport {
    pub fn is_valid(&self) -> bool {
        !self
            .issues
            .iter()
            .any(|issue| issue.severity == IssueSeverity::Error)
    }

    pub fn errors(&self) -> impl Iterator<Item = &ConfigIssue> {
        self.issues
            .iter()
            .filter(|issue| issue.severity == IssueSeverity::Error)
    }

    pub fn warnings(&self) -> impl Iterator<Item = &ConfigIssue> {
        self.issues
            .iter()
            .filter(|issue| issue.severity == IssueSeverity::Warning)
    }

    fn push(&mut self, issue: ConfigIssue) {
        self.issues.push(issue);
    }
}

/// `$TERM` values with terminfo entries that exist on virtually every system.
pub const WIDELY_AVAILABLE_TERMS: [&str; 9] = [
    "xterm-256color",
    "xterm",
    "screen-256color",
    "screen",
    "tmux-256color",
    "linux",
    "vt100",
    "alacritty",
    "foot",
];

/// Validates a configuration model.
pub fn validate(config: &Config, known_themes: &[&str]) -> ValidationReport {
    let mut report = ValidationReport::default();

    // Window
    if !(200..=16_384).contains(&config.window.width) {
        report.push(ConfigIssue::error(
            "window.width",
            format!(
                "must be between 200 and 16384 (found {})",
                config.window.width
            ),
        ));
    }
    if !(120..=16_384).contains(&config.window.height) {
        report.push(ConfigIssue::error(
            "window.height",
            format!(
                "must be between 120 and 16384 (found {})",
                config.window.height
            ),
        ));
    }
    if !(0.1..=1.0).contains(&config.window.opacity) {
        report.push(ConfigIssue::error(
            "window.opacity",
            format!(
                "must be between 0.1 and 1.0 (found {})",
                config.window.opacity
            ),
        ));
    } else if config.window.opacity < 0.5 {
        report.push(ConfigIssue::warning(
            "window.opacity",
            "values below 0.5 make text hard to read",
        ));
    }

    // Terminal
    if config.terminal.term.trim().is_empty() {
        report.push(ConfigIssue::error("terminal.term", "must not be empty"));
    } else if config.terminal.term.chars().any(char::is_whitespace) {
        report.push(ConfigIssue::error(
            "terminal.term",
            "must not contain whitespace",
        ));
    } else if !WIDELY_AVAILABLE_TERMS.contains(&config.terminal.term.as_str()) {
        report.push(ConfigIssue::warning(
            "terminal.term",
            format!(
                "`{}` is not a widely available terminfo entry; remote hosts may not recognise it",
                config.terminal.term
            ),
        ));
    }
    if !(100..=1_000_000).contains(&config.terminal.scrollback_lines) {
        report.push(ConfigIssue::error(
            "terminal.scrollback_lines",
            format!(
                "must be between 100 and 1000000 (found {})",
                config.terminal.scrollback_lines
            ),
        ));
    }
    if config.terminal.shell.trim().is_empty() {
        report.push(ConfigIssue::error(
            "terminal.shell",
            "must not be empty; use `auto`",
        ));
    } else if config.terminal.shell != "auto" && config.terminal.shell.contains('/') {
        let path = Path::new(&config.terminal.shell);
        if !path.exists() {
            report.push(ConfigIssue::error(
                "terminal.shell",
                format!("`{}` does not exist", config.terminal.shell),
            ));
        }
    }
    if let Some(problem) = working_directory_issue(&config.terminal.working_directory) {
        report.push(problem);
    }

    // Font
    if !(4.0..=96.0).contains(&config.font.size) {
        report.push(ConfigIssue::error(
            "font.size",
            format!("must be between 4 and 96 (found {})", config.font.size),
        ));
    }
    if !(0.8..=3.0).contains(&config.font.line_height) {
        report.push(ConfigIssue::error(
            "font.line_height",
            format!(
                "must be between 0.8 and 3.0 (found {})",
                config.font.line_height
            ),
        ));
    }
    if !(-5.0..=20.0).contains(&config.font.letter_spacing) {
        report.push(ConfigIssue::error(
            "font.letter_spacing",
            format!(
                "must be between -5 and 20 (found {})",
                config.font.letter_spacing
            ),
        ));
    }
    if !(100..=900).contains(&config.font.weight) || !config.font.weight.is_multiple_of(100) {
        report.push(ConfigIssue::error(
            "font.weight",
            format!(
                "must be a multiple of 100 between 100 and 900 (found {})",
                config.font.weight
            ),
        ));
    }
    if config.font.family.trim().is_empty() {
        report.push(ConfigIssue::error("font.family", "must not be empty"));
    }

    // Appearance
    if config.appearance.padding_x > 512 || config.appearance.padding_y > 512 {
        report.push(ConfigIssue::error(
            "appearance.padding_x",
            "padding must be at most 512px",
        ));
    } else if config.appearance.padding_x > 64 || config.appearance.padding_y > 64 {
        report.push(ConfigIssue::warning(
            "appearance.padding_x",
            "padding above 64px wastes workspace",
        ));
    }
    if !(120..=480).contains(&config.appearance.tab_strip_width) {
        report.push(ConfigIssue::error(
            "appearance.tab_strip_width",
            format!(
                "must be between 120 and 480 (found {})",
                config.appearance.tab_strip_width
            ),
        ));
    }
    if config.appearance.clock_format.trim().is_empty() {
        report.push(ConfigIssue::error(
            "appearance.clock_format",
            "must not be empty; use `%H:%M` for a 24-hour clock",
        ));
    } else if config.appearance.clock_format.chars().count() > 64 {
        report.push(ConfigIssue::error(
            "appearance.clock_format",
            "must be at most 64 characters",
        ));
    }
    if config.appearance.show_status_bar
        && !config.appearance.status_bar_cpu
        && !config.appearance.status_bar_ram
        && !config.appearance.status_bar_clock
    {
        report.push(ConfigIssue::warning(
            "appearance.show_status_bar",
            "the status bar has no right-hand segments enabled",
        ));
    }
    if !(1..=12).contains(&config.appearance.divider_thickness) {
        report.push(ConfigIssue::error(
            "appearance.divider_thickness",
            format!(
                "must be between 1 and 12 (found {})",
                config.appearance.divider_thickness
            ),
        ));
    }
    if config.appearance.pane_gap > 24 {
        report.push(ConfigIssue::error(
            "appearance.pane_gap",
            format!("must be at most 24 (found {})", config.appearance.pane_gap),
        ));
    }
    match Rgb::parse(&config.appearance.accent_color) {
        Ok(color) => {
            if color.luminance() < 0.05 {
                report.push(ConfigIssue::warning(
                    "appearance.accent_color",
                    "very dark accents are hard to see as focus indicators",
                ));
            }
        }
        Err(message) => report.push(ConfigIssue::error("appearance.accent_color", message)),
    }
    if !known_themes
        .iter()
        .any(|name| name.eq_ignore_ascii_case(&config.appearance.theme))
    {
        report.push(ConfigIssue::warning(
            "appearance.theme",
            format!(
                "theme `{}` not found; falling back to `midnight`",
                config.appearance.theme
            ),
        ));
    }
    if config.appearance.background_blur && config.window.opacity >= 1.0 {
        report.push(ConfigIssue::warning(
            "appearance.background_blur",
            "blur has no visible effect while the window is fully opaque",
        ));
    }

    // Performance
    if config.performance.max_fps_when_idle > 240 {
        report.push(ConfigIssue::error(
            "performance.max_fps_when_idle",
            format!(
                "must be at most 240 (found {})",
                config.performance.max_fps_when_idle
            ),
        ));
    }

    // Keyboard
    let keymap = Keymap::build(&config.keys);
    for (_, chord, message) in keymap.invalid() {
        report.push(ConfigIssue::error("keys", format!("`{chord}`: {message}")));
    }
    for (chord, first, second) in keymap.conflicts() {
        report.push(ConfigIssue::warning(
            "keys",
            format!("chord `{chord}` is bound to both `{first}` and `{second}`; `{first}` wins"),
        ));
    }

    // Profiles
    let mut seen: Vec<&str> = Vec::new();
    for profile in &config.profiles {
        if profile.name.trim().is_empty() {
            report.push(ConfigIssue::error(
                "profiles",
                "profile name must not be empty",
            ));
            continue;
        }
        if seen
            .iter()
            .any(|name| name.eq_ignore_ascii_case(&profile.name))
        {
            report.push(ConfigIssue::error(
                "profiles",
                format!("duplicate profile name `{}`", profile.name),
            ));
        }
        seen.push(&profile.name);
        if let Some(size) = profile.font_size
            && !(4.0..=96.0).contains(&size)
        {
            report.push(ConfigIssue::error(
                "profiles",
                format!(
                    "profile `{}` font_size must be between 4 and 96",
                    profile.name
                ),
            ));
        }
        if let Some(directory) = &profile.working_directory
            && let Some(problem) = working_directory_issue(directory)
        {
            report.push(ConfigIssue {
                field: format!("profiles.{}.working_directory", profile.name),
                ..problem
            });
        }
        if !profile.command.is_empty() && !profile.args.is_empty() {
            report.push(ConfigIssue::warning(
                "profiles",
                format!(
                    "profile `{}` sets both `command` and `args`; `command` is used",
                    profile.name
                ),
            ));
        }
        if profile.shell == "sh" && profile.args.iter().any(|arg| arg == "-c") {
            report.push(ConfigIssue::warning(
                "profiles",
                format!(
                    "profile `{}` passes `-c` to a shell; prefer `command` with an argument vector",
                    profile.name
                ),
            ));
        }
    }

    // Cursor styles are exhaustive enums, but keep the value set documented.
    let _ = CursorStyle::ALL;

    report
}

fn working_directory_issue(directory: &WorkingDirectory) -> Option<ConfigIssue> {
    match directory {
        WorkingDirectory::Policy(policy) => {
            const KNOWN: [&str; 4] = ["home", "current", "inherit", ""];
            if KNOWN.contains(&policy.as_str()) {
                None
            } else if policy.starts_with('/') {
                ConfigIssue::warning(
                    "working_directory",
                    format!("directory `{policy}` does not exist"),
                )
                .filter_if(!Path::new(policy).is_dir())
            } else {
                Some(ConfigIssue::error(
                    "working_directory",
                    format!("`{policy}` is neither a known policy nor an absolute path"),
                ))
            }
        }
        WorkingDirectory::Path(path) => {
            if !path.is_absolute() {
                Some(ConfigIssue::error(
                    "working_directory",
                    format!("`{}` must be an absolute path", path.display()),
                ))
            } else if !path.is_dir() {
                Some(ConfigIssue::error(
                    "working_directory",
                    format!("`{}` is not a directory", path.display()),
                ))
            } else {
                None
            }
        }
    }
}

/// Helper so a warning can be expressed conditionally without `if` nesting.
trait IssueExt {
    fn filter_if(self, keep: bool) -> Option<ConfigIssue>;
}

impl IssueExt for ConfigIssue {
    fn filter_if(self, keep: bool) -> Option<ConfigIssue> {
        if keep { Some(self) } else { None }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::Profile;

    fn themes() -> Vec<&'static str> {
        vec!["midnight", "dawn"]
    }

    #[test]
    fn default_configuration_is_valid() {
        let report = validate(&Config::default(), &themes());
        assert!(report.is_valid(), "issues: {:?}", report.issues);
    }

    #[test]
    fn rejects_out_of_range_font_size() {
        let mut config = Config::default();
        config.font.size = 200.0;
        let report = validate(&config, &themes());
        assert!(!report.is_valid());
        assert!(report.errors().any(|issue| issue.field == "font.size"));
    }

    #[test]
    fn rejects_non_multiple_font_weight() {
        let mut config = Config::default();
        config.font.weight = 450;
        let report = validate(&config, &themes());
        assert!(report.errors().any(|issue| issue.field == "font.weight"));
    }

    #[test]
    fn warns_about_non_portable_term() {
        let mut config = Config::default();
        config.terminal.term = "astraterm-256color".to_owned();
        let report = validate(&config, &themes());
        assert!(report.is_valid(), "custom TERM must not be a hard error");
        assert!(
            report
                .warnings()
                .any(|issue| issue.field == "terminal.term")
        );
    }

    #[test]
    fn rejects_invalid_accent_colour() {
        let mut config = Config::default();
        config.appearance.accent_color = "not-a-colour".to_owned();
        let report = validate(&config, &themes());
        assert!(
            report
                .errors()
                .any(|issue| issue.field == "appearance.accent_color")
        );
    }

    #[test]
    fn warns_about_unknown_theme_but_allows_it() {
        let mut config = Config::default();
        config.appearance.theme = "solar-flare".to_owned();
        let report = validate(&config, &themes());
        assert!(report.is_valid());
        assert!(
            report
                .warnings()
                .any(|issue| issue.field == "appearance.theme")
        );
    }

    #[test]
    fn rejects_relative_working_directory() {
        let mut config = Config::default();
        config.terminal.working_directory = WorkingDirectory::Path("code".into());
        let report = validate(&config, &themes());
        assert!(
            report
                .errors()
                .any(|issue| issue.field == "working_directory")
        );
    }

    #[test]
    fn rejects_missing_absolute_working_directory() {
        let mut config = Config::default();
        config.terminal.working_directory = WorkingDirectory::Path("/definitely/not/here".into());
        let report = validate(&config, &themes());
        assert!(
            report
                .errors()
                .any(|issue| issue.field == "working_directory")
        );
    }

    #[test]
    fn rejects_duplicate_profile_names() {
        let config = Config {
            profiles: vec![Profile::new("work"), Profile::new("Work")],
            ..Config::default()
        };
        let report = validate(&config, &themes());
        assert!(report.errors().any(|issue| issue.field == "profiles"));
    }

    #[test]
    fn reports_invalid_keybinding_chord() {
        let mut config = Config::default();
        config
            .keys
            .insert("new-tab".to_owned(), "ctrl+nope".to_owned());
        let report = validate(&config, &themes());
        assert!(report.errors().any(|issue| issue.field == "keys"));
    }

    #[test]
    fn warns_about_conflicting_chords() {
        let mut config = Config::default();
        config
            .keys
            .insert("paste".to_owned(), "ctrl+shift+t".to_owned());
        let report = validate(&config, &themes());
        assert!(report.is_valid());
        assert!(report.warnings().any(|issue| issue.field == "keys"));
    }

    #[test]
    fn rejects_an_unusable_tab_strip_width() {
        let mut config = Config::default();
        config.appearance.tab_strip_width = 80;
        let report = validate(&config, &themes());
        assert!(
            report
                .errors()
                .any(|issue| issue.field == "appearance.tab_strip_width")
        );
        config.appearance.tab_strip_width = 200;
        assert!(validate(&config, &themes()).is_valid());
    }

    #[test]
    fn rejects_an_empty_clock_format() {
        let mut config = Config::default();
        config.appearance.clock_format = "  ".to_owned();
        let report = validate(&config, &themes());
        assert!(
            report
                .errors()
                .any(|issue| issue.field == "appearance.clock_format")
        );
    }

    #[test]
    fn warns_when_the_status_bar_has_no_segments() {
        let mut config = Config::default();
        config.appearance.show_status_bar = true;
        config.appearance.status_bar_cpu = false;
        config.appearance.status_bar_ram = false;
        config.appearance.status_bar_clock = false;
        let report = validate(&config, &themes());
        assert!(report.is_valid());
        assert!(
            report
                .warnings()
                .any(|issue| issue.field == "appearance.show_status_bar")
        );
    }

    #[test]
    fn warns_when_blur_cannot_be_seen() {
        let mut config = Config::default();
        config.appearance.background_blur = true;
        let report = validate(&config, &themes());
        assert!(
            report
                .warnings()
                .any(|issue| issue.field == "appearance.background_blur")
        );
    }
}
