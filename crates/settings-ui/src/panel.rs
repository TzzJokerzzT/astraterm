//! The settings panel model and its text projection.
//!
//! The panel is keyboard-first: arrows move, `Left`/`Right` change values, `Enter`
//! edits, `Tab` switches section, `/` filters, `Ctrl+R` resets a section and `Esc`
//! closes. The application draws [`SettingsPanel::lines`] with the same renderer as
//! the terminal, so the settings experience is native, compact and consistent with
//! the dark, minimal workspace.

use astraterm_config::{Config, ConfigIssue, IssueSeverity, Palette, Rgb, Theme};

use crate::fields::{Field, FieldKind, PaletteEntry};

/// Settings sections, in the order they are shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Section {
    Appearance,
    Fonts,
    Colors,
    Terminal,
    Keyboard,
    Workspace,
    Performance,
    Profiles,
    About,
}

impl Section {
    pub const ALL: [Section; 9] = [
        Section::Appearance,
        Section::Fonts,
        Section::Colors,
        Section::Terminal,
        Section::Keyboard,
        Section::Workspace,
        Section::Performance,
        Section::Profiles,
        Section::About,
    ];

    pub const fn title(self) -> &'static str {
        match self {
            Section::Appearance => "Appearance",
            Section::Fonts => "Fonts",
            Section::Colors => "Colours & themes",
            Section::Terminal => "Terminal",
            Section::Keyboard => "Keyboard",
            Section::Workspace => "Workspace",
            Section::Performance => "Performance",
            Section::Profiles => "Profiles",
            Section::About => "About",
        }
    }

    /// Editable fields of this section.
    pub fn fields(self) -> Vec<Field> {
        match self {
            Section::Appearance => vec![
                Field::Theme,
                Field::CursorStyle,
                Field::CursorBlink,
                Field::ShowStatusBar,
                Field::StatusBarCpu,
                Field::StatusBarRam,
                Field::StatusBarClock,
                Field::ClockFormat,
                Field::TabBarPosition,
                Field::TabStripWidth,
                Field::CompactTabs,
                Field::BackgroundBlur,
                Field::PaddingX,
                Field::PaddingY,
                Field::PaneGap,
                Field::DividerThickness,
                Field::WindowWidth,
                Field::WindowHeight,
                Field::WindowOpacity,
                Field::StartupMaximized,
                Field::RememberGeometry,
            ],
            Section::Fonts => vec![
                Field::FontFamily,
                Field::FontSize,
                Field::FontWeight,
                Field::LineHeight,
                Field::LetterSpacing,
            ],
            Section::Colors => vec![
                Field::Theme,
                Field::PaletteEntry,
                Field::PaletteColor,
                Field::SaveTheme,
                Field::AccentColor,
            ],
            Section::Terminal => vec![
                Field::Shell,
                Field::Term,
                Field::ScrollbackLines,
                Field::WorkingDirectory,
                Field::ConfirmOnClose,
                Field::CopyOnSelect,
                Field::Bell,
                Field::OpenUrlsOnClick,
                Field::WordDelimiters,
            ],
            Section::Keyboard => Vec::new(),
            Section::Workspace => {
                vec![Field::RestoreOnStartup, Field::ClosePaneWhenShellExits]
            }
            Section::Performance => {
                vec![
                    Field::GpuAcceleration,
                    Field::LowPowerMode,
                    Field::MaxFpsWhenIdle,
                ]
            }
            Section::Profiles => Vec::new(),
            Section::About => vec![
                Field::AboutVersion,
                Field::AboutConfigPath,
                Field::AboutThemes,
            ],
        }
    }
}

/// A key press understood by the panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanelKey {
    Up,
    Down,
    Left,
    Right,
    PageUp,
    PageDown,
    Home,
    End,
    Enter,
    Escape,
    Tab,
    Backspace,
    Char(char),
    ResetSection,
}

/// What the panel wants the application to do after a key press.
#[derive(Clone, Debug, PartialEq)]
pub enum PanelAction {
    /// Nothing happened; redraw is optional.
    Consumed,
    /// The panel wants to close.
    Close,
    /// The user confirmed: persist this configuration.
    Save(Box<Config>),
    /// Show a message (for example a validation failure).
    Message(String),
    /// Revert the draft to the last saved configuration.
    Revert,
    /// Show a candidate palette immediately, without persisting anything.
    PreviewPalette(Box<Palette>),
    /// Save the edited palette as a named theme and select it.
    SaveTheme { name: String, palette: Box<Palette> },
}

/// Visual role of one projected line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanelStyle {
    /// Section strip.
    Tabs,
    /// Row label, not focused.
    Row,
    /// Row label, focused.
    Focused,
    /// Row value.
    Value,
    /// Focused row value.
    FocusedValue,
    /// Help/footer text.
    Hint,
    /// Validation error.
    Error,
    /// Validation warning.
    Warning,
    /// Editable text buffer.
    Editing,
}

/// One line of the projected panel.
///
/// A settings row carries its label in `text` and its current value in `value`, so
/// the renderer can align values in a column instead of stacking two lines per row.
#[derive(Clone, Debug, PartialEq)]
pub struct PanelLine {
    pub text: String,
    pub value: Option<String>,
    pub style: PanelStyle,
}

impl PanelLine {
    fn plain(text: impl Into<String>, style: PanelStyle) -> Self {
        Self {
            text: text.into(),
            value: None,
            style,
        }
    }

    fn row(label: impl Into<String>, value: impl Into<String>, style: PanelStyle) -> Self {
        Self {
            text: label.into(),
            value: Some(value.into()),
            style,
        }
    }
}

/// The settings panel state.
#[derive(Clone, Debug)]
pub struct SettingsPanel {
    pub section: Section,
    pub focus: usize,
    /// Draft configuration edited by the panel.
    draft: Config,
    /// The configuration as last persisted, used by revert.
    baseline: Config,
    /// Inline text entry buffer, when editing a text/colour field.
    edit_buffer: Option<String>,
    /// Search filter text.
    filter: String,
    /// Whether the search box has focus.
    filtering: bool,
    /// Validation issues to display.
    issues: Vec<ConfigIssue>,
    /// Themes available for the picker and for seeding the editor.
    themes: Vec<Theme>,
    /// Palette being edited; seeded from the selected theme.
    palette: Palette,
    /// Which palette entry the editor is changing.
    palette_entry: PaletteEntry,
    /// True while the inline buffer holds a new theme name.
    editing_theme_name: bool,
    /// Reminder of which settings need a new session.
    restart_needed: bool,
    message: Option<String>,
    /// Lines scrolled away from the top of the current section.
    scroll: usize,
    /// Number of visible rows the renderer will show.
    visible_rows: usize,
    /// Paths shown by the About section: (config file, themes directory).
    about_paths: (String, String),
}

impl SettingsPanel {
    /// Creates a panel editing a copy of `config`.
    pub fn new(config: &Config, themes: Vec<Theme>, issues: Vec<ConfigIssue>) -> Self {
        let palette = themes
            .iter()
            .find(|theme| theme.name.eq_ignore_ascii_case(&config.appearance.theme))
            .map(|theme| theme.palette.clone())
            .unwrap_or_else(Palette::midnight);
        Self {
            section: Section::Appearance,
            focus: 0,
            draft: config.clone(),
            baseline: config.clone(),
            edit_buffer: None,
            filter: String::new(),
            filtering: false,
            issues,
            themes,
            palette,
            palette_entry: PaletteEntry::Background,
            editing_theme_name: false,
            restart_needed: false,
            message: None,
            scroll: 0,
            visible_rows: 12,
            about_paths: (String::new(), String::new()),
        }
    }

    /// Refreshes the panel after the configuration changed underneath it.
    pub fn resync(&mut self, config: &Config, issues: Vec<ConfigIssue>) {
        self.draft = config.clone();
        self.baseline = config.clone();
        self.issues = issues;
        self.restart_needed = false;
    }

    pub fn draft(&self) -> &Config {
        &self.draft
    }

    pub fn issues(&self) -> &[ConfigIssue] {
        &self.issues
    }

    pub fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }

    pub fn set_themes(&mut self, themes: Vec<Theme>) {
        self.themes = themes;
    }

    /// Theme names in picker order.
    pub fn theme_names(&self) -> Vec<String> {
        self.themes.iter().map(|theme| theme.name.clone()).collect()
    }

    /// Palette being edited.
    pub fn palette(&self) -> &Palette {
        &self.palette
    }

    /// Which colour the editor is changing.
    pub fn palette_entry(&self) -> PaletteEntry {
        self.palette_entry
    }

    /// Value shown for a row, including the panel-owned palette rows.
    fn field_value(&self, field: Field) -> String {
        match field {
            Field::PaletteEntry => self.palette_entry.label().to_owned(),
            Field::PaletteColor => self.palette_entry.get(&self.palette).to_hex(),
            Field::SaveTheme => "enter to name and save".to_owned(),
            other => other.value(&self.draft),
        }
    }

    /// Tells the panel how many rows fit on screen so scrolling stays correct.
    pub fn set_visible_rows(&mut self, rows: usize) {
        self.visible_rows = rows.max(1);
    }

    /// Whether the draft differs from the persisted configuration.
    pub fn is_dirty(&self) -> bool {
        self.draft != self.baseline
    }

    /// Whether any edited setting requires a new session to take effect.
    pub fn needs_restart(&self) -> bool {
        self.restart_needed
    }

    /// Rows of the current section after filtering.
    pub fn rows(&self) -> Vec<Field> {
        let all = self.section.fields();
        if self.filter.is_empty() {
            return all;
        }
        let needle = self.filter.to_lowercase();
        all.into_iter()
            .filter(|field| {
                field.label().to_lowercase().contains(&needle)
                    || field.id().contains(&needle)
                    || self.field_value(*field).to_lowercase().contains(&needle)
            })
            .collect()
    }

    /// Currently focused field.
    pub fn focused_field(&self) -> Option<Field> {
        self.rows().get(self.focus).copied()
    }

    /// Handles a key press.
    pub fn handle_key(&mut self, key: PanelKey) -> PanelAction {
        self.message = None;

        // Inline text entry consumes most keys.
        if self.edit_buffer.is_some() {
            return self.handle_edit_key(key);
        }
        if self.filtering {
            return self.handle_filter_key(key);
        }

        match key {
            PanelKey::Escape => {
                if self.is_dirty() {
                    PanelAction::Revert
                } else {
                    PanelAction::Close
                }
            }
            PanelKey::Up => {
                self.move_focus(-1);
                PanelAction::Consumed
            }
            PanelKey::Down => {
                self.move_focus(1);
                PanelAction::Consumed
            }
            PanelKey::PageUp => {
                self.move_focus(-(self.visible_rows as i32));
                PanelAction::Consumed
            }
            PanelKey::PageDown => {
                self.move_focus(self.visible_rows as i32);
                PanelAction::Consumed
            }
            PanelKey::Home => {
                self.focus = 0;
                self.ensure_visible();
                PanelAction::Consumed
            }
            PanelKey::End => {
                self.focus = self.rows().len().saturating_sub(1);
                self.ensure_visible();
                PanelAction::Consumed
            }
            PanelKey::Tab => {
                self.next_section(1);
                PanelAction::Consumed
            }
            PanelKey::Enter => self.activate(),
            PanelKey::Left => self.step_focused(-1).unwrap_or(PanelAction::Consumed),
            PanelKey::Right => self.step_focused(1).unwrap_or(PanelAction::Consumed),
            PanelKey::ResetSection => {
                self.reset_section();
                PanelAction::Consumed
            }
            PanelKey::Char('/') => {
                self.filtering = true;
                PanelAction::Consumed
            }
            PanelKey::Char('s') if self.is_dirty() => self.confirm(),
            PanelKey::Char('r') => {
                self.reset_section();
                PanelAction::Consumed
            }
            PanelKey::Char('q') => PanelAction::Close,
            PanelKey::Backspace | PanelKey::Char(_) => PanelAction::Consumed,
        }
    }

    fn handle_edit_key(&mut self, key: PanelKey) -> PanelAction {
        let Some(buffer) = self.edit_buffer.as_mut() else {
            return PanelAction::Consumed;
        };
        match key {
            PanelKey::Enter => {
                let text = buffer.clone();
                self.edit_buffer = None;

                if self.editing_theme_name {
                    self.editing_theme_name = false;
                    return PanelAction::SaveTheme {
                        name: text,
                        palette: Box::new(self.palette.clone()),
                    };
                }

                let Some(field) = self.focused_field() else {
                    return PanelAction::Consumed;
                };
                if field == Field::PaletteColor {
                    return match Rgb::parse(&text) {
                        Ok(color) => {
                            self.palette_entry.set(&mut self.palette, color);
                            PanelAction::PreviewPalette(Box::new(self.palette.clone()))
                        }
                        Err(message) => PanelAction::Message(message),
                    };
                }

                field.set_text(&mut self.draft, &text);
                if field.requires_restart() && self.draft != self.baseline {
                    self.restart_needed = true;
                }
                self.confirm()
            }
            PanelKey::Escape => {
                self.edit_buffer = None;
                self.editing_theme_name = false;
                PanelAction::Consumed
            }
            PanelKey::Backspace => {
                buffer.pop();
                PanelAction::Consumed
            }
            PanelKey::Char(ch) => {
                buffer.push(ch);
                PanelAction::Consumed
            }
            _ => PanelAction::Consumed,
        }
    }

    fn handle_filter_key(&mut self, key: PanelKey) -> PanelAction {
        match key {
            PanelKey::Enter => {
                self.filtering = false;
                self.focus = 0;
            }
            PanelKey::Escape => {
                self.filtering = false;
                self.filter.clear();
                self.focus = 0;
            }
            PanelKey::Backspace => {
                self.filter.pop();
                self.focus = 0;
            }
            PanelKey::Char(ch) => {
                self.filter.push(ch);
                self.focus = 0;
            }
            _ => {}
        }
        PanelAction::Consumed
    }

    fn activate(&mut self) -> PanelAction {
        let Some(field) = self.focused_field() else {
            return PanelAction::Consumed;
        };
        match field.kind() {
            FieldKind::Toggle | FieldKind::Choice | FieldKind::Number => {
                self.step_focused(1);
                PanelAction::Consumed
            }
            FieldKind::Text => {
                self.editing_theme_name = false;
                self.edit_buffer = Some(field.value(&self.draft));
                PanelAction::Consumed
            }
            FieldKind::Color => {
                self.editing_theme_name = false;
                self.edit_buffer = Some(self.field_value(field));
                PanelAction::Consumed
            }
            FieldKind::Action => {
                // "Save theme as…": start with a suggestion derived from the base
                // theme so the user only has to confirm or rename it.
                if field == Field::SaveTheme {
                    let base = self.draft.appearance.theme.clone();
                    self.editing_theme_name = true;
                    self.edit_buffer = Some(format!("{base}-custom"));
                }
                PanelAction::Consumed
            }
            FieldKind::Info => PanelAction::Consumed,
        }
    }

    fn step_focused(&mut self, direction: i32) -> Option<PanelAction> {
        let field = self.focused_field()?;
        match field {
            Field::PaletteEntry => {
                let current = PaletteEntry::ALL
                    .iter()
                    .position(|entry| *entry == self.palette_entry)
                    .unwrap_or(0);
                let len = PaletteEntry::ALL.len();
                let next = if direction >= 0 {
                    (current + 1) % len
                } else {
                    (current + len - 1) % len
                };
                self.palette_entry = PaletteEntry::ALL[next];
                None
            }
            _ => {
                if field.kind() == FieldKind::Info || field.kind() == FieldKind::Action {
                    return None;
                }
                let names = self.theme_names();
                field.step(&mut self.draft, direction, &names);
                if field.requires_restart() && self.draft != self.baseline {
                    self.restart_needed = true;
                }
                if field == Field::Theme {
                    // Selecting a theme seeds the editor with that palette and previews
                    // it, so "edit → save as" starts from what is on screen.
                    let selected = names
                        .iter()
                        .position(|name| name.eq_ignore_ascii_case(&self.draft.appearance.theme))
                        .and_then(|index| self.themes.get(index))
                        .map(|theme| theme.palette.clone());
                    if let Some(palette) = selected {
                        self.palette = palette;
                        return Some(PanelAction::PreviewPalette(Box::new(self.palette.clone())));
                    }
                }
                None
            }
        }
    }

    fn reset_section(&mut self) {
        let fields = self.section.fields();
        for field in fields {
            field.reset(&mut self.draft);
        }
        self.restart_needed = true;
    }

    fn confirm(&mut self) -> PanelAction {
        if self.draft == self.baseline {
            return PanelAction::Consumed;
        }
        PanelAction::Save(Box::new(self.draft.clone()))
    }

    fn next_section(&mut self, direction: i32) {
        let index = Section::ALL
            .iter()
            .position(|section| *section == self.section)
            .unwrap_or(0);
        let len = Section::ALL.len();
        let next = if direction >= 0 {
            (index + 1) % len
        } else {
            (index + len - 1) % len
        };
        self.section = Section::ALL[next];
        self.focus = 0;
        self.scroll = 0;
    }

    fn move_focus(&mut self, delta: i32) {
        let len = self.rows().len();
        if len == 0 {
            self.focus = 0;
            return;
        }
        let next = self.focus as i32 + delta;
        self.focus = next.clamp(0, len as i32 - 1) as usize;
        self.ensure_visible();
    }

    fn ensure_visible(&mut self) {
        if self.focus < self.scroll {
            self.scroll = self.focus;
        } else if self.focus >= self.scroll + self.visible_rows {
            self.scroll = self.focus + 1 - self.visible_rows;
        }
    }

    /// Accepts a configuration that was persisted successfully, clearing dirty
    /// state and refreshing validation issues.
    pub fn on_saved(&mut self, config: &Config, issues: Vec<ConfigIssue>) {
        self.draft = config.clone();
        self.baseline = config.clone();
        self.issues = issues;
        self.restart_needed = false;
    }

    /// Reverts the draft to the last persisted configuration.
    pub fn revert(&mut self) {
        self.draft = self.baseline.clone();
        self.restart_needed = false;
        self.edit_buffer = None;
    }

    /// Sets the informational paths shown by the About section.
    pub fn set_paths(&mut self, config_path: String, themes_dir: String) {
        self.about_paths = (config_path, themes_dir);
    }

    /// Projects the panel into lines for the renderer.
    ///
    /// The projection is deliberately plain text: the application maps
    /// [`PanelStyle`] onto colours and draws it in the overlay, with values aligned
    /// in a column.
    pub fn lines(&self) -> Vec<PanelLine> {
        let mut lines = Vec::new();

        // Section strip with the active section marked.
        let tabs: Vec<String> = Section::ALL
            .iter()
            .map(|section| {
                if *section == self.section {
                    format!("[{}]", section.title())
                } else {
                    section.title().to_owned()
                }
            })
            .collect();
        lines.push(PanelLine::plain(
            format!(" {}", tabs.join("   ")),
            PanelStyle::Tabs,
        ));

        if self.filtering || !self.filter.is_empty() {
            lines.push(PanelLine::plain(
                format!(
                    " filter: {}{}",
                    self.filter,
                    if self.filtering { "▏" } else { "" }
                ),
                PanelStyle::Editing,
            ));
        }

        let rows = self.rows();
        if rows.is_empty() {
            let note = match self.section {
                Section::Keyboard => {
                    "  Shortcuts are edited in [keys] of the TOML file; see docs/CONFIGURATION.md."
                }
                Section::Profiles => "  Profiles are edited in [[profiles]] of the TOML file.",
                _ => "  No settings match the filter.",
            };
            lines.push(PanelLine::plain(note, PanelStyle::Hint));
        }

        let end = (self.scroll + self.visible_rows).min(rows.len());
        for (index, field) in rows.iter().enumerate().take(end).skip(self.scroll) {
            let focused = index == self.focus;
            let editing = focused && self.edit_buffer.is_some();
            let value = if editing {
                format!("{}▏", self.edit_buffer.clone().unwrap_or_default())
            } else if let Some(text) = self.about_value(*field) {
                text
            } else {
                self.field_value(*field)
            };
            let suffix = if field.requires_restart() {
                "  · new session"
            } else {
                ""
            };
            lines.push(PanelLine::row(
                format!(" {}", field.label()),
                format!("{value}{suffix}"),
                if focused {
                    PanelStyle::Focused
                } else {
                    PanelStyle::Row
                },
            ));
        }

        if self.needs_restart() {
            lines.push(PanelLine::plain(
                "  Some changes apply to new sessions only",
                PanelStyle::Hint,
            ));
        }

        for issue in self.issues.iter().take(3) {
            lines.push(PanelLine::plain(
                format!("  {}: {}", issue.field, issue.message),
                match issue.severity {
                    IssueSeverity::Error => PanelStyle::Error,
                    IssueSeverity::Warning => PanelStyle::Warning,
                },
            ));
        }

        if let Some(message) = &self.message {
            lines.push(PanelLine::plain(format!("  {message}"), PanelStyle::Error));
        }

        lines.push(PanelLine::plain(
            "  ↑↓ move · ←→ change · enter edit · tab section · / filter · r reset · s save · esc close",
            PanelStyle::Hint,
        ));
        lines
    }

    fn about_value(&self, field: Field) -> Option<String> {
        match field {
            Field::AboutConfigPath => Some(self.about_paths.0.clone()),
            Field::AboutThemes => Some(self.about_paths.1.clone()),
            _ => None,
        }
    }

    /// Shows a message in the panel footer.
    pub fn set_message(&mut self, message: impl Into<String>) {
        self.message = Some(message.into());
    }

    /// Injects validation issues (for example after a failed save).
    pub fn set_issues(&mut self, issues: Vec<ConfigIssue>) {
        self.issues = issues;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn panel() -> SettingsPanel {
        SettingsPanel::new(
            &Config::default(),
            astraterm_config::builtin_themes(),
            Vec::new(),
        )
    }

    #[test]
    fn starts_in_the_appearance_section_with_a_focused_row() {
        let panel = panel();
        assert_eq!(panel.section, Section::Appearance);
        assert_eq!(panel.focus, 0);
        assert_eq!(panel.focused_field(), Some(Field::Theme));
    }

    #[test]
    fn sections_are_all_reachable_with_tab() {
        let mut panel = panel();
        let mut seen = vec![panel.section];
        for _ in 0..Section::ALL.len() - 1 {
            panel.handle_key(PanelKey::Tab);
            seen.push(panel.section);
        }
        assert_eq!(seen.len(), Section::ALL.len());
        panel.handle_key(PanelKey::Tab);
        assert_eq!(panel.section, Section::Appearance, "tab wraps around");
    }

    #[test]
    fn navigation_is_clamped_to_the_row_list() {
        let mut panel = panel();
        panel.handle_key(PanelKey::Up);
        assert_eq!(panel.focus, 0);
        for _ in 0..100 {
            panel.handle_key(PanelKey::Down);
        }
        assert_eq!(panel.focus, panel.rows().len() - 1);
    }

    #[test]
    fn right_changes_the_focused_value_and_marks_dirty() {
        let mut panel = panel();
        panel.handle_key(PanelKey::Right);
        assert!(panel.is_dirty());
        assert_ne!(
            panel.draft().appearance.theme,
            Config::default().appearance.theme
        );
    }

    #[test]
    fn toggling_requires_confirm_to_save() {
        let mut panel = panel();
        let actions = panel.handle_key(PanelKey::Char('s'));
        assert!(
            matches!(actions, PanelAction::Consumed),
            "nothing to save yet"
        );
        panel.handle_key(PanelKey::Right);
        assert!(matches!(
            panel.handle_key(PanelKey::Char('s')),
            PanelAction::Save(_)
        ));
    }

    #[test]
    fn escape_reverts_a_dirty_draft() {
        let mut panel = panel();
        panel.handle_key(PanelKey::Right);
        let action = panel.handle_key(PanelKey::Escape);
        assert_eq!(action, PanelAction::Revert);
        panel.revert();
        assert!(!panel.is_dirty());
        assert_eq!(panel.handle_key(PanelKey::Escape), PanelAction::Close);
    }

    #[test]
    fn text_fields_open_an_inline_editor() {
        let mut panel = panel();
        panel.section = Section::Fonts;
        panel.focus = 0;
        assert_eq!(panel.focused_field(), Some(Field::FontFamily));
        panel.handle_key(PanelKey::Enter);
        // The editor starts prefilled with the current value, which is replaced
        // here by clearing it first.
        for _ in 0..64 {
            panel.handle_key(PanelKey::Backspace);
        }
        for ch in "Iosevka".chars() {
            panel.handle_key(PanelKey::Char(ch));
        }
        panel.handle_key(PanelKey::Enter);
        assert_eq!(panel.draft().font.family, "Iosevka");
    }

    #[test]
    fn escaping_the_editor_discards_the_edit() {
        let mut panel = panel();
        panel.section = Section::Fonts;
        panel.focus = 0;
        panel.handle_key(PanelKey::Enter);
        panel.handle_key(PanelKey::Char('X'));
        panel.handle_key(PanelKey::Escape);
        assert_eq!(panel.draft().font.family, Config::default().font.family);
    }

    #[test]
    fn filter_narrows_the_rows() {
        let mut panel = panel();
        panel.handle_key(PanelKey::Char('/'));
        for ch in "cursor".chars() {
            panel.handle_key(PanelKey::Char(ch));
        }
        panel.handle_key(PanelKey::Enter);
        let labels: Vec<&str> = panel.rows().iter().map(|field| field.label()).collect();
        assert!(
            labels
                .iter()
                .all(|label| label.to_lowercase().contains("cursor")),
            "{labels:?}"
        );
    }

    #[test]
    fn resetting_a_section_restores_defaults() {
        let mut panel = panel();
        panel.handle_key(PanelKey::Right);
        panel.handle_key(PanelKey::ResetSection);
        assert_eq!(
            panel.draft().appearance.theme,
            Config::default().appearance.theme
        );
    }

    #[test]
    fn projection_contains_tabs_rows_and_help() {
        let panel = panel();
        let lines = panel.lines();
        assert!(lines[0].text.contains("[Appearance]"));
        assert!(lines.iter().any(|line| line.text.contains("Theme")));
        assert!(lines.last().unwrap().text.contains("esc close"));
    }

    #[test]
    fn projection_shows_validation_issues() {
        let mut panel = SettingsPanel::new(
            &Config::default(),
            Vec::new(),
            vec![ConfigIssue::error("font.size", "must be between 4 and 96")],
        );
        let lines = panel.lines();
        assert!(lines.iter().any(|line| line.style == PanelStyle::Error));
        panel.set_issues(Vec::new());
        assert!(
            !panel
                .lines()
                .iter()
                .any(|line| line.style == PanelStyle::Error)
        );
    }

    #[test]
    fn keyboard_and_profiles_sections_explain_where_to_edit() {
        let mut panel = panel();
        panel.section = Section::Keyboard;
        assert!(
            panel
                .lines()
                .iter()
                .any(|line| line.text.contains("[keys]"))
        );
        panel.section = Section::Profiles;
        assert!(
            panel
                .lines()
                .iter()
                .any(|line| line.text.contains("[[profiles]]"))
        );
    }

    #[test]
    fn scrolling_keeps_the_focused_row_visible() {
        let mut panel = panel();
        panel.set_visible_rows(3);
        for _ in 0..6 {
            panel.handle_key(PanelKey::Down);
        }
        let lines = panel.lines();
        assert!(lines.iter().any(|line| line.style == PanelStyle::Focused));
    }

    #[test]
    fn saved_configuration_clears_dirty_state() {
        let mut panel = panel();
        panel.handle_key(PanelKey::Right);
        assert!(panel.is_dirty());
        let draft = panel.draft().clone();
        panel.on_saved(&draft, Vec::new());
        assert!(!panel.is_dirty());
    }

    #[test]
    fn the_bell_row_lives_in_the_terminal_section_and_applies_live() {
        let mut panel = panel();
        panel.section = Section::Terminal;
        while panel.focused_field() != Some(Field::Bell) {
            panel.handle_key(PanelKey::Down);
        }
        let row = panel
            .lines()
            .into_iter()
            .find(|line| line.text.trim_start() == "Bell")
            .expect("the Terminal section shows a Bell row");
        assert_eq!(
            row.value.as_deref(),
            Some("visual"),
            "the default keeps today's visible answer to a BEL"
        );

        panel.handle_key(PanelKey::Right);
        assert_eq!(panel.draft().terminal.bell, astraterm_config::BellMode::Audible);
        assert!(
            !panel.needs_restart(),
            "the bell is read when a BEL arrives, so no new session is needed"
        );
    }

    #[test]
    fn restart_requirement_is_tracked() {
        let mut panel = panel();
        panel.section = Section::Terminal;
        while panel.focused_field() != Some(Field::ScrollbackLines) {
            panel.handle_key(PanelKey::Down);
        }
        panel.handle_key(PanelKey::Right);
        assert!(panel.needs_restart());
        panel.revert();
        assert!(!panel.needs_restart());
    }
}

#[cfg(test)]
mod theme_editor_tests {
    use super::*;
    use crate::fields::PaletteEntry;

    fn panel() -> SettingsPanel {
        let mut config = Config::default();
        config.appearance.theme = "midnight".to_owned();
        SettingsPanel::new(&config, astraterm_config::builtin_themes(), Vec::new())
    }

    fn focus(panel: &mut SettingsPanel, field: Field) {
        panel.section = Section::Colors;
        let rows = panel.rows();
        panel.focus = rows
            .iter()
            .position(|row| *row == field)
            .expect("field in section");
    }

    #[test]
    fn the_editor_starts_from_the_selected_theme() {
        let panel = panel();
        assert_eq!(panel.palette().background, Palette::midnight().background);
        assert_eq!(palette_entry_label(&panel), "Background");
    }

    fn palette_entry_label(panel: &SettingsPanel) -> &'static str {
        panel.palette_entry().label()
    }

    #[test]
    fn stepping_the_entry_cycles_through_the_palette() {
        let mut panel = panel();
        focus(&mut panel, Field::PaletteEntry);
        assert_eq!(panel.palette_entry(), PaletteEntry::Background);
        panel.handle_key(PanelKey::Right);
        assert_eq!(panel.palette_entry(), PaletteEntry::Foreground);
        panel.handle_key(PanelKey::Left);
        assert_eq!(panel.palette_entry(), PaletteEntry::Background);
    }

    #[test]
    fn editing_a_colour_previews_it_without_saving_the_config() {
        let mut panel = panel();
        focus(&mut panel, Field::PaletteColor);
        panel.handle_key(PanelKey::Enter);
        for _ in 0..16 {
            panel.handle_key(PanelKey::Backspace);
        }
        for ch in "#102030".chars() {
            panel.handle_key(PanelKey::Char(ch));
        }
        let action = panel.handle_key(PanelKey::Enter);
        match action {
            PanelAction::PreviewPalette(palette) => {
                assert_eq!(palette.background, Rgb::new(0x10, 0x20, 0x30));
            }
            other => panic!("expected a preview, got {other:?}"),
        }
        assert_eq!(panel.palette().background, Rgb::new(0x10, 0x20, 0x30));
        assert!(
            !panel.is_dirty(),
            "editing a theme does not dirty the configuration"
        );
    }

    #[test]
    fn an_invalid_colour_is_reported_instead_of_applied() {
        let mut panel = panel();
        focus(&mut panel, Field::PaletteColor);
        panel.handle_key(PanelKey::Enter);
        for _ in 0..16 {
            panel.handle_key(PanelKey::Backspace);
        }
        for ch in "nope".chars() {
            panel.handle_key(PanelKey::Char(ch));
        }
        let action = panel.handle_key(PanelKey::Enter);
        assert!(matches!(action, PanelAction::Message(_)), "got {action:?}");
        assert_eq!(panel.palette().background, Palette::midnight().background);
    }

    #[test]
    fn saving_asks_for_a_name_and_emits_the_palette() {
        let mut panel = panel();
        focus(&mut panel, Field::SaveTheme);
        assert!(matches!(
            panel.handle_key(PanelKey::Enter),
            PanelAction::Consumed
        ));
        // The suggestion is pre-filled from the base theme.
        let action = panel.handle_key(PanelKey::Enter);
        match action {
            PanelAction::SaveTheme { name, palette } => {
                assert!(name.contains("midnight"), "suggested name: {name}");
                assert_eq!(*palette, *panel.palette());
            }
            other => panic!("expected SaveTheme, got {other:?}"),
        }
    }

    #[test]
    fn choosing_another_theme_reseeds_the_editor_and_previews_it() {
        let mut panel = panel();
        focus(&mut panel, Field::Theme);
        let action = panel.handle_key(PanelKey::Right);
        assert!(
            matches!(action, PanelAction::PreviewPalette(_)),
            "got {action:?}"
        );
        assert_eq!(panel.palette().background, Palette::dawn().background);
    }

    #[test]
    fn palette_rows_show_their_values_in_the_projection() {
        let mut panel = panel();
        focus(&mut panel, Field::PaletteColor);
        let lines = panel.lines();
        assert!(
            lines
                .iter()
                .any(|line| line.value.as_deref() == Some("#1c1f26")),
            "the background hex is shown: {lines:?}"
        );
    }
}
