//! The editable fields exposed by the settings panel.
//!
//! Each field knows how to read itself from a [`Config`], how to apply a change,
//! and which kind of widget it needs. Adding a setting is a matter of adding one
//! variant here and one row in its section, which keeps the panel and the TOML
//! schema in step (they share the same typed model).

use astraterm_config::schema::CursorStyle;
use astraterm_config::{Config, Palette, Rgb, TabBarPosition};

/// Which palette colour the theme editor is changing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaletteEntry {
    Background,
    Foreground,
    Accent,
    Cursor,
    CursorText,
    SelectionBackground,
    PaneBorder,
    TabBarBackground,
}

impl PaletteEntry {
    pub const ALL: [PaletteEntry; 8] = [
        PaletteEntry::Background,
        PaletteEntry::Foreground,
        PaletteEntry::Accent,
        PaletteEntry::Cursor,
        PaletteEntry::CursorText,
        PaletteEntry::SelectionBackground,
        PaletteEntry::PaneBorder,
        PaletteEntry::TabBarBackground,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            PaletteEntry::Background => "Background",
            PaletteEntry::Foreground => "Foreground",
            PaletteEntry::Accent => "Accent",
            PaletteEntry::Cursor => "Cursor",
            PaletteEntry::CursorText => "Cursor text",
            PaletteEntry::SelectionBackground => "Selection",
            PaletteEntry::PaneBorder => "Focused border",
            PaletteEntry::TabBarBackground => "Tab bar",
        }
    }

    pub fn get(self, palette: &Palette) -> Rgb {
        match self {
            PaletteEntry::Background => palette.background,
            PaletteEntry::Foreground => palette.foreground,
            PaletteEntry::Accent => palette.accent,
            PaletteEntry::Cursor => palette.cursor,
            PaletteEntry::CursorText => palette.cursor_text,
            PaletteEntry::SelectionBackground => palette.selection_background,
            PaletteEntry::PaneBorder => palette.pane_border_focused,
            PaletteEntry::TabBarBackground => palette.tab_bar_background,
        }
    }

    pub fn set(self, palette: &mut Palette, color: Rgb) {
        match self {
            PaletteEntry::Background => palette.background = color,
            PaletteEntry::Foreground => palette.foreground = color,
            PaletteEntry::Accent => palette.accent = color,
            PaletteEntry::Cursor => palette.cursor = color,
            PaletteEntry::CursorText => palette.cursor_text = color,
            PaletteEntry::SelectionBackground => palette.selection_background = color,
            PaletteEntry::PaneBorder => palette.pane_border_focused = color,
            PaletteEntry::TabBarBackground => palette.tab_bar_background = color,
        }
    }
}

/// How a field is edited.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldKind {
    /// `Enter`/`Space` flips the value.
    Toggle,
    /// `Left`/`Right` step through the allowed values.
    Choice,
    /// `Left`/`Right` step by a fixed increment.
    Number,
    /// `Enter` starts text entry.
    Text,
    /// A colour in `#rrggbb` form, edited as text.
    Color,
    /// Informational row; not editable.
    Info,
    /// Runs an action when activated (for example "save theme as").
    Action,
}

/// Every editable setting.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Field {
    // Appearance
    Theme,
    CursorStyle,
    CursorBlink,
    ShowStatusBar,
    BackgroundBlur,
    TabBarPosition,
    TabStripWidth,
    StatusBarCpu,
    StatusBarRam,
    StatusBarClock,
    ClockFormat,
    CompactTabs,
    AccentColor,
    PaletteEntry,
    PaletteColor,
    SaveTheme,
    PaddingX,
    PaddingY,
    PaneGap,
    DividerThickness,
    // Fonts
    FontFamily,
    FontSize,
    FontWeight,
    LineHeight,
    LetterSpacing,
    // Terminal
    Shell,
    Term,
    ScrollbackLines,
    WorkingDirectory,
    ConfirmOnClose,
    CopyOnSelect,
    OpenUrlsOnClick,
    WordDelimiters,
    // Window
    WindowWidth,
    WindowHeight,
    WindowOpacity,
    StartupMaximized,
    RememberGeometry,
    // Workspace
    RestoreOnStartup,
    ClosePaneWhenShellExits,
    // Performance
    GpuAcceleration,
    LowPowerMode,
    MaxFpsWhenIdle,
    // About
    AboutVersion,
    AboutConfigPath,
    AboutThemes,
}

impl Field {
    /// Stable identifier used in tests and diagnostics.
    pub const fn id(self) -> &'static str {
        match self {
            Field::Theme => "theme",
            Field::CursorStyle => "cursor-style",
            Field::CursorBlink => "cursor-blink",
            Field::ShowStatusBar => "show-status-bar",
            Field::BackgroundBlur => "background-blur",
            Field::TabBarPosition => "tab-bar-position",
            Field::TabStripWidth => "tab-strip-width",
            Field::StatusBarCpu => "status-bar-cpu",
            Field::StatusBarRam => "status-bar-ram",
            Field::StatusBarClock => "status-bar-clock",
            Field::ClockFormat => "clock-format",
            Field::CompactTabs => "compact-tabs",
            Field::AccentColor => "accent-color",
            Field::PaletteEntry => "palette-entry",
            Field::PaletteColor => "palette-color",
            Field::SaveTheme => "save-theme",
            Field::PaddingX => "padding-x",
            Field::PaddingY => "padding-y",
            Field::PaneGap => "pane-gap",
            Field::DividerThickness => "divider-thickness",
            Field::FontFamily => "font-family",
            Field::FontSize => "font-size",
            Field::FontWeight => "font-weight",
            Field::LineHeight => "line-height",
            Field::LetterSpacing => "letter-spacing",
            Field::Shell => "shell",
            Field::Term => "term",
            Field::ScrollbackLines => "scrollback-lines",
            Field::WorkingDirectory => "working-directory",
            Field::ConfirmOnClose => "confirm-on-close",
            Field::CopyOnSelect => "copy-on-select",
            Field::OpenUrlsOnClick => "open-urls-on-click",
            Field::WordDelimiters => "word-delimiters",
            Field::WindowWidth => "window-width",
            Field::WindowHeight => "window-height",
            Field::WindowOpacity => "window-opacity",
            Field::StartupMaximized => "startup-maximized",
            Field::RememberGeometry => "remember-geometry",
            Field::RestoreOnStartup => "restore-on-startup",
            Field::ClosePaneWhenShellExits => "close-pane-when-shell-exits",
            Field::GpuAcceleration => "gpu-acceleration",
            Field::LowPowerMode => "low-power-mode",
            Field::MaxFpsWhenIdle => "max-fps-when-idle",
            Field::AboutVersion => "about-version",
            Field::AboutConfigPath => "about-config-path",
            Field::AboutThemes => "about-themes",
        }
    }

    /// Human readable row label.
    pub const fn label(self) -> &'static str {
        match self {
            Field::Theme => "Theme",
            Field::CursorStyle => "Cursor shape",
            Field::CursorBlink => "Cursor blinks",
            Field::ShowStatusBar => "Status bar",
            Field::BackgroundBlur => "Background blur",
            Field::TabBarPosition => "Tab bar position",
            Field::TabStripWidth => "Tab strip width",
            Field::StatusBarCpu => "Status bar: CPU",
            Field::StatusBarRam => "Status bar: RAM",
            Field::StatusBarClock => "Status bar: clock",
            Field::ClockFormat => "Clock format",
            Field::CompactTabs => "Compact tabs",
            Field::AccentColor => "Accent colour",
            Field::PaletteEntry => "Edit theme colour",
            Field::PaletteColor => "Colour value",
            Field::SaveTheme => "Save theme as…",
            Field::PaddingX => "Horizontal padding",
            Field::PaddingY => "Vertical padding",
            Field::PaneGap => "Pane gap",
            Field::DividerThickness => "Divider thickness",
            Field::FontFamily => "Font family",
            Field::FontSize => "Font size",
            Field::FontWeight => "Font weight",
            Field::LineHeight => "Line height",
            Field::LetterSpacing => "Letter spacing",
            Field::Shell => "Shell",
            Field::Term => "$TERM",
            Field::ScrollbackLines => "Scrollback lines",
            Field::WorkingDirectory => "Working directory",
            Field::ConfirmOnClose => "Confirm before closing",
            Field::CopyOnSelect => "Copy on select",
            Field::OpenUrlsOnClick => "Open URLs on click",
            Field::WordDelimiters => "Word delimiters",
            Field::WindowWidth => "Window width",
            Field::WindowHeight => "Window height",
            Field::WindowOpacity => "Window opacity",
            Field::StartupMaximized => "Start maximized",
            Field::RememberGeometry => "Remember window geometry",
            Field::RestoreOnStartup => "Restore layout on startup",
            Field::ClosePaneWhenShellExits => "Close pane when shell exits",
            Field::GpuAcceleration => "GPU acceleration",
            Field::LowPowerMode => "Low power mode",
            Field::MaxFpsWhenIdle => "Max idle FPS",
            Field::AboutVersion => "Version",
            Field::AboutConfigPath => "Configuration file",
            Field::AboutThemes => "Theme directory",
        }
    }

    /// The TOML path this field maps to, shown next to the label.
    pub const fn path(self) -> &'static str {
        match self {
            Field::Theme
            | Field::CursorStyle
            | Field::CursorBlink
            | Field::ShowStatusBar
            | Field::StatusBarCpu
            | Field::StatusBarRam
            | Field::StatusBarClock
            | Field::BackgroundBlur
            | Field::TabBarPosition
            | Field::TabStripWidth
            | Field::ClockFormat
            | Field::CompactTabs
            | Field::AccentColor
            | Field::PaletteEntry
            | Field::PaletteColor
            | Field::SaveTheme
            | Field::PaddingX
            | Field::PaddingY
            | Field::PaneGap
            | Field::DividerThickness => "appearance",
            Field::FontFamily
            | Field::FontSize
            | Field::FontWeight
            | Field::LineHeight
            | Field::LetterSpacing => "font",
            Field::Shell
            | Field::Term
            | Field::ScrollbackLines
            | Field::WorkingDirectory
            | Field::ConfirmOnClose
            | Field::CopyOnSelect
            | Field::OpenUrlsOnClick
            | Field::WordDelimiters => "terminal",
            Field::WindowWidth
            | Field::WindowHeight
            | Field::WindowOpacity
            | Field::StartupMaximized
            | Field::RememberGeometry => "window",
            Field::RestoreOnStartup | Field::ClosePaneWhenShellExits => "workspace",
            Field::GpuAcceleration | Field::LowPowerMode | Field::MaxFpsWhenIdle => "performance",
            Field::AboutVersion | Field::AboutConfigPath | Field::AboutThemes => "about",
        }
    }

    pub const fn kind(self) -> FieldKind {
        match self {
            Field::CursorBlink
            | Field::ShowStatusBar
            | Field::StatusBarCpu
            | Field::StatusBarRam
            | Field::StatusBarClock
            | Field::BackgroundBlur
            | Field::CompactTabs
            | Field::ConfirmOnClose
            | Field::CopyOnSelect
            | Field::OpenUrlsOnClick
            | Field::StartupMaximized
            | Field::RememberGeometry
            | Field::RestoreOnStartup
            | Field::ClosePaneWhenShellExits
            | Field::GpuAcceleration
            | Field::LowPowerMode => FieldKind::Toggle,
            Field::Theme | Field::CursorStyle | Field::TabBarPosition | Field::PaletteEntry => {
                FieldKind::Choice
            }
            Field::PaddingX
            | Field::PaddingY
            | Field::PaneGap
            | Field::DividerThickness
            | Field::FontSize
            | Field::FontWeight
            | Field::LineHeight
            | Field::LetterSpacing
            | Field::TabStripWidth
            | Field::ScrollbackLines
            | Field::WindowWidth
            | Field::WindowHeight
            | Field::WindowOpacity
            | Field::MaxFpsWhenIdle => FieldKind::Number,
            Field::FontFamily
            | Field::Shell
            | Field::Term
            | Field::WorkingDirectory
            | Field::WordDelimiters
            | Field::ClockFormat => FieldKind::Text,
            Field::AccentColor => FieldKind::Color,
            Field::PaletteColor => FieldKind::Color,
            Field::SaveTheme => FieldKind::Action,
            Field::AboutVersion | Field::AboutConfigPath | Field::AboutThemes => FieldKind::Info,
        }
    }

    /// Current value rendered for the row.
    pub fn value(self, config: &Config) -> String {
        match self {
            Field::Theme => config.appearance.theme.clone(),
            Field::CursorStyle => config.appearance.cursor.label().to_owned(),
            Field::CursorBlink => yes_no(config.appearance.cursor_blink),
            Field::ShowStatusBar => yes_no(config.appearance.show_status_bar),
            Field::BackgroundBlur => yes_no(config.appearance.background_blur),
            Field::TabBarPosition => config.appearance.tab_bar_position.label().to_owned(),
            Field::CompactTabs => yes_no(config.appearance.compact_tabs),
            Field::TabStripWidth => config.appearance.tab_strip_width.to_string(),
            Field::StatusBarCpu => yes_no(config.appearance.status_bar_cpu),
            Field::StatusBarRam => yes_no(config.appearance.status_bar_ram),
            Field::StatusBarClock => yes_no(config.appearance.status_bar_clock),
            Field::ClockFormat => config.appearance.clock_format.clone(),
            Field::AccentColor => config.appearance.accent_color.clone(),
            Field::PaletteEntry | Field::PaletteColor | Field::SaveTheme => String::new(),
            Field::PaddingX => config.appearance.padding_x.to_string(),
            Field::PaddingY => config.appearance.padding_y.to_string(),
            Field::PaneGap => config.appearance.pane_gap.to_string(),
            Field::DividerThickness => config.appearance.divider_thickness.to_string(),
            Field::FontFamily => config.font.family.clone(),
            Field::FontSize => format!("{:.1}", config.font.size),
            Field::FontWeight => config.font.weight.to_string(),
            Field::LineHeight => format!("{:.2}", config.font.line_height),
            Field::LetterSpacing => format!("{:.1}", config.font.letter_spacing),
            Field::Shell => config.terminal.shell.clone(),
            Field::Term => config.terminal.term.clone(),
            Field::ScrollbackLines => config.terminal.scrollback_lines.to_string(),
            Field::WorkingDirectory => config.terminal.working_directory.label(),
            Field::ConfirmOnClose => yes_no(config.terminal.confirm_on_close),
            Field::CopyOnSelect => yes_no(config.terminal.copy_on_select),
            Field::OpenUrlsOnClick => yes_no(config.terminal.open_urls_on_click),
            Field::WordDelimiters => config.terminal.word_delimiters.clone(),
            Field::WindowWidth => config.window.width.to_string(),
            Field::WindowHeight => config.window.height.to_string(),
            Field::WindowOpacity => format!("{:.2}", config.window.opacity),
            Field::StartupMaximized => yes_no(config.window.startup_maximized),
            Field::RememberGeometry => yes_no(config.window.remember_geometry),
            Field::RestoreOnStartup => yes_no(config.workspace.restore_on_startup),
            Field::ClosePaneWhenShellExits => yes_no(config.workspace.close_pane_when_shell_exits),
            Field::GpuAcceleration => yes_no(config.performance.gpu_acceleration),
            Field::LowPowerMode => yes_no(config.performance.low_power_mode),
            Field::MaxFpsWhenIdle => config.performance.max_fps_when_idle.to_string(),
            Field::AboutVersion => env!("CARGO_PKG_VERSION").to_owned(),
            Field::AboutConfigPath => String::new(),
            Field::AboutThemes => String::new(),
        }
    }

    /// Applies a step to the field. `direction` is -1 or +1 for number/choice
    /// fields; toggles ignore it.
    pub fn step(self, config: &mut Config, direction: i32, themes: &[String]) {
        let up = direction >= 0;
        match self {
            Field::Theme => {
                if themes.is_empty() {
                    return;
                }
                let current = themes
                    .iter()
                    .position(|theme| theme.eq_ignore_ascii_case(&config.appearance.theme))
                    .unwrap_or(0);
                let next = if up {
                    (current + 1) % themes.len()
                } else {
                    (current + themes.len() - 1) % themes.len()
                };
                config.appearance.theme = themes[next].clone();
            }
            Field::CursorStyle => {
                let current = CursorStyle::ALL
                    .iter()
                    .position(|style| *style == config.appearance.cursor)
                    .unwrap_or(0);
                let next = step_index(current, CursorStyle::ALL.len(), up);
                config.appearance.cursor = CursorStyle::ALL[next];
            }
            Field::TabBarPosition => {
                let current = TabBarPosition::ALL
                    .iter()
                    .position(|position| *position == config.appearance.tab_bar_position)
                    .unwrap_or(0);
                let next = step_index(current, TabBarPosition::ALL.len(), up);
                config.appearance.tab_bar_position = TabBarPosition::ALL[next];
            }
            Field::CursorBlink => config.appearance.cursor_blink = !config.appearance.cursor_blink,
            Field::ShowStatusBar => {
                config.appearance.show_status_bar = !config.appearance.show_status_bar;
            }
            Field::BackgroundBlur => {
                config.appearance.background_blur = !config.appearance.background_blur;
            }
            Field::CompactTabs => config.appearance.compact_tabs = !config.appearance.compact_tabs,
            Field::StatusBarCpu => {
                config.appearance.status_bar_cpu = !config.appearance.status_bar_cpu
            }
            Field::StatusBarRam => {
                config.appearance.status_bar_ram = !config.appearance.status_bar_ram
            }
            Field::StatusBarClock => {
                config.appearance.status_bar_clock = !config.appearance.status_bar_clock;
            }
            Field::TabStripWidth => {
                config.appearance.tab_strip_width =
                    step_u32(config.appearance.tab_strip_width, direction, 20, 120, 480);
            }
            Field::ConfirmOnClose => {
                config.terminal.confirm_on_close = !config.terminal.confirm_on_close;
            }
            Field::CopyOnSelect => config.terminal.copy_on_select = !config.terminal.copy_on_select,
            Field::OpenUrlsOnClick => {
                config.terminal.open_urls_on_click = !config.terminal.open_urls_on_click;
            }
            Field::StartupMaximized => {
                config.window.startup_maximized = !config.window.startup_maximized
            }
            Field::RememberGeometry => {
                config.window.remember_geometry = !config.window.remember_geometry;
            }
            Field::RestoreOnStartup => {
                config.workspace.restore_on_startup = !config.workspace.restore_on_startup;
            }
            Field::ClosePaneWhenShellExits => {
                config.workspace.close_pane_when_shell_exits =
                    !config.workspace.close_pane_when_shell_exits;
            }
            Field::GpuAcceleration => {
                config.performance.gpu_acceleration = !config.performance.gpu_acceleration;
            }
            Field::LowPowerMode => {
                config.performance.low_power_mode = !config.performance.low_power_mode
            }
            Field::PaddingX => {
                config.appearance.padding_x =
                    step_u32(config.appearance.padding_x, direction, 2, 0, 256);
            }
            Field::PaddingY => {
                config.appearance.padding_y =
                    step_u32(config.appearance.padding_y, direction, 2, 0, 256);
            }
            Field::PaneGap => {
                config.appearance.pane_gap =
                    step_u32(config.appearance.pane_gap, direction, 1, 0, 24);
            }
            Field::DividerThickness => {
                config.appearance.divider_thickness =
                    step_u32(config.appearance.divider_thickness, direction, 1, 1, 12);
            }
            Field::FontSize => {
                config.font.size = step_f32(config.font.size, direction, 0.5, 4.0, 96.0);
            }
            Field::FontWeight => {
                let weight = step_i32(config.font.weight as i32, direction, 100, 100, 900);
                config.font.weight = weight as u16;
            }
            Field::LineHeight => {
                config.font.line_height =
                    step_f32(config.font.line_height, direction, 0.05, 0.8, 3.0);
            }
            Field::LetterSpacing => {
                config.font.letter_spacing =
                    step_f32(config.font.letter_spacing, direction, 0.5, -5.0, 20.0);
            }
            Field::ScrollbackLines => {
                let lines = step_i32(
                    config.terminal.scrollback_lines as i32,
                    direction,
                    1000,
                    100,
                    1_000_000,
                );
                config.terminal.scrollback_lines = lines as usize;
            }
            Field::WindowWidth => {
                config.window.width = step_u32(config.window.width, direction, 40, 200, 16_384);
            }
            Field::WindowHeight => {
                config.window.height = step_u32(config.window.height, direction, 40, 120, 16_384);
            }
            Field::WindowOpacity => {
                config.window.opacity = step_f32(config.window.opacity, direction, 0.05, 0.1, 1.0);
            }
            Field::MaxFpsWhenIdle => {
                config.performance.max_fps_when_idle =
                    step_u32(config.performance.max_fps_when_idle, direction, 1, 1, 240);
            }
            // Text and colour fields are committed from `set_text`; palette rows are
            // owned by the panel because they edit a theme, not the configuration.
            Field::PaletteEntry | Field::PaletteColor | Field::SaveTheme | Field::ClockFormat => {}
            Field::FontFamily
            | Field::Shell
            | Field::Term
            | Field::WorkingDirectory
            | Field::WordDelimiters
            | Field::AccentColor
            | Field::AboutVersion
            | Field::AboutConfigPath
            | Field::AboutThemes => {}
        }
    }

    /// Applies committed text entry to a text or colour field.
    pub fn set_text(self, config: &mut Config, text: &str) {
        let text = text.trim();
        match self {
            Field::FontFamily => config.font.family = text.to_owned(),
            Field::Shell => config.terminal.shell = text.to_owned(),
            Field::Term => config.terminal.term = text.to_owned(),
            Field::WordDelimiters => config.terminal.word_delimiters = text.to_owned(),
            Field::AccentColor => config.appearance.accent_color = text.to_owned(),
            Field::ClockFormat => config.appearance.clock_format = text.to_owned(),
            Field::WorkingDirectory => {
                config.terminal.working_directory =
                    if text.is_empty() || text == "home" || text == "current" {
                        astraterm_config::WorkingDirectory::Policy(if text.is_empty() {
                            "home".to_owned()
                        } else {
                            text.to_owned()
                        })
                    } else {
                        astraterm_config::WorkingDirectory::Path(std::path::PathBuf::from(text))
                    };
            }
            _ => {}
        }
    }

    /// Restores the field to its built-in default.
    pub fn reset(self, config: &mut Config) {
        let defaults = Config::default();
        match self {
            Field::Theme => config.appearance.theme = defaults.appearance.theme,
            Field::CursorStyle => config.appearance.cursor = defaults.appearance.cursor,
            Field::CursorBlink => config.appearance.cursor_blink = defaults.appearance.cursor_blink,
            Field::ShowStatusBar => {
                config.appearance.show_status_bar = defaults.appearance.show_status_bar
            }
            Field::BackgroundBlur => {
                config.appearance.background_blur = defaults.appearance.background_blur
            }
            Field::TabBarPosition => {
                config.appearance.tab_bar_position = defaults.appearance.tab_bar_position
            }
            Field::CompactTabs => config.appearance.compact_tabs = defaults.appearance.compact_tabs,
            Field::TabStripWidth => {
                config.appearance.tab_strip_width = defaults.appearance.tab_strip_width;
            }
            Field::StatusBarCpu => {
                config.appearance.status_bar_cpu = defaults.appearance.status_bar_cpu
            }
            Field::StatusBarRam => {
                config.appearance.status_bar_ram = defaults.appearance.status_bar_ram
            }
            Field::StatusBarClock => {
                config.appearance.status_bar_clock = defaults.appearance.status_bar_clock;
            }
            Field::ClockFormat => config.appearance.clock_format = defaults.appearance.clock_format,
            Field::AccentColor => config.appearance.accent_color = defaults.appearance.accent_color,
            Field::PaddingX => config.appearance.padding_x = defaults.appearance.padding_x,
            Field::PaddingY => config.appearance.padding_y = defaults.appearance.padding_y,
            Field::PaneGap => config.appearance.pane_gap = defaults.appearance.pane_gap,
            Field::DividerThickness => {
                config.appearance.divider_thickness = defaults.appearance.divider_thickness;
            }
            Field::FontFamily => config.font.family = defaults.font.family,
            Field::FontSize => config.font.size = defaults.font.size,
            Field::FontWeight => config.font.weight = defaults.font.weight,
            Field::LineHeight => config.font.line_height = defaults.font.line_height,
            Field::LetterSpacing => config.font.letter_spacing = defaults.font.letter_spacing,
            Field::Shell => config.terminal.shell = defaults.terminal.shell,
            Field::Term => config.terminal.term = defaults.terminal.term,
            Field::ScrollbackLines => {
                config.terminal.scrollback_lines = defaults.terminal.scrollback_lines;
            }
            Field::WorkingDirectory => {
                config.terminal.working_directory = defaults.terminal.working_directory;
            }
            Field::ConfirmOnClose => {
                config.terminal.confirm_on_close = defaults.terminal.confirm_on_close
            }
            Field::CopyOnSelect => {
                config.terminal.copy_on_select = defaults.terminal.copy_on_select
            }
            Field::OpenUrlsOnClick => {
                config.terminal.open_urls_on_click = defaults.terminal.open_urls_on_click
            }
            Field::WordDelimiters => {
                config.terminal.word_delimiters = defaults.terminal.word_delimiters
            }
            Field::WindowWidth => config.window.width = defaults.window.width,
            Field::WindowHeight => config.window.height = defaults.window.height,
            Field::WindowOpacity => config.window.opacity = defaults.window.opacity,
            Field::StartupMaximized => {
                config.window.startup_maximized = defaults.window.startup_maximized
            }
            Field::RememberGeometry => {
                config.window.remember_geometry = defaults.window.remember_geometry
            }
            Field::RestoreOnStartup => {
                config.workspace.restore_on_startup = defaults.workspace.restore_on_startup
            }
            Field::ClosePaneWhenShellExits => {
                config.workspace.close_pane_when_shell_exits =
                    defaults.workspace.close_pane_when_shell_exits;
            }
            Field::GpuAcceleration => {
                config.performance.gpu_acceleration = defaults.performance.gpu_acceleration;
            }
            Field::LowPowerMode => {
                config.performance.low_power_mode = defaults.performance.low_power_mode
            }
            Field::MaxFpsWhenIdle => {
                config.performance.max_fps_when_idle = defaults.performance.max_fps_when_idle;
            }
            Field::PaletteEntry
            | Field::PaletteColor
            | Field::SaveTheme
            | Field::AboutVersion
            | Field::AboutConfigPath
            | Field::AboutThemes => {}
        }
    }

    /// Whether the field needs a new session/window to take effect.
    pub const fn requires_restart(self) -> bool {
        matches!(
            self,
            Field::Term
                | Field::ScrollbackLines
                | Field::WindowWidth
                | Field::WindowHeight
                | Field::StartupMaximized
                | Field::GpuAcceleration
                | Field::WorkingDirectory
                | Field::Shell
        )
    }
}

fn yes_no(value: bool) -> String {
    if value {
        "on".to_owned()
    } else {
        "off".to_owned()
    }
}

fn step_index(current: usize, len: usize, up: bool) -> usize {
    if len == 0 {
        return 0;
    }
    if up {
        (current + 1) % len
    } else {
        (current + len - 1) % len
    }
}

fn step_u32(value: u32, direction: i32, step: u32, min: u32, max: u32) -> u32 {
    let direction = direction.signum() as i64;
    let next = value as i64 + direction * step as i64;
    next.clamp(min as i64, max as i64) as u32
}

fn step_i32(value: i32, direction: i32, step: i32, min: i32, max: i32) -> i32 {
    let direction = direction.signum();
    (value + direction * step).clamp(min, max)
}

fn step_f32(value: f32, direction: i32, step: f32, min: f32, max: f32) -> f32 {
    let direction = direction.signum() as f32;
    let next = value + direction * step;
    // Round to the step grid to avoid accumulating float noise.
    let rounded = (next / step).round() * step;
    rounded.clamp(min, max)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn themes() -> Vec<String> {
        vec!["midnight".to_owned(), "dawn".to_owned()]
    }

    #[test]
    fn every_field_reports_id_and_label() {
        for field in [
            Field::Theme,
            Field::FontSize,
            Field::Shell,
            Field::WindowOpacity,
            Field::MaxFpsWhenIdle,
            Field::AboutVersion,
        ] {
            assert!(!field.id().is_empty());
            assert!(!field.label().is_empty());
            assert!(!field.path().is_empty());
        }
    }

    #[test]
    fn toggles_flip() {
        let mut config = Config::default();
        let before = config.appearance.show_status_bar;
        Field::ShowStatusBar.step(&mut config, 1, &themes());
        assert_ne!(config.appearance.show_status_bar, before);
        Field::ShowStatusBar.step(&mut config, 1, &themes());
        assert_eq!(config.appearance.show_status_bar, before);
    }

    #[test]
    fn number_fields_step_and_clamp() {
        let mut config = Config::default();
        Field::FontSize.step(&mut config, 1, &themes());
        assert!(config.font.size > Config::default().font.size);
        for _ in 0..500 {
            Field::FontSize.step(&mut config, 1, &themes());
        }
        assert_eq!(config.font.size, 96.0);
        for _ in 0..500 {
            Field::FontSize.step(&mut config, -1, &themes());
        }
        assert_eq!(config.font.size, 4.0);
    }

    #[test]
    fn choice_fields_cycle_through_their_values() {
        let mut config = Config::default();
        let start = config.appearance.cursor;
        Field::CursorStyle.step(&mut config, 1, &themes());
        assert_ne!(config.appearance.cursor, start);
        for _ in 0..CursorStyle::ALL.len() - 1 {
            Field::CursorStyle.step(&mut config, 1, &themes());
        }
        assert_eq!(config.appearance.cursor, start);
    }

    #[test]
    fn theme_field_cycles_through_available_themes() {
        let mut config = Config::default();
        Field::Theme.step(&mut config, 1, &themes());
        assert_eq!(config.appearance.theme, "dawn");
        Field::Theme.step(&mut config, 1, &themes());
        assert_eq!(config.appearance.theme, "midnight");
    }

    #[test]
    fn text_fields_commit_trimmed_values() {
        let mut config = Config::default();
        Field::FontFamily.set_text(&mut config, "  Iosevka  ");
        assert_eq!(config.font.family, "Iosevka");
        Field::AccentColor.set_text(&mut config, "#ff8800");
        assert_eq!(config.appearance.accent_color, "#ff8800");
    }

    #[test]
    fn working_directory_accepts_policies_and_paths() {
        let mut config = Config::default();
        Field::WorkingDirectory.set_text(&mut config, "current");
        assert_eq!(config.terminal.working_directory.label(), "current");
        Field::WorkingDirectory.set_text(&mut config, "/tmp");
        assert_eq!(config.terminal.working_directory.label(), "/tmp");
        Field::WorkingDirectory.set_text(&mut config, "");
        assert_eq!(config.terminal.working_directory.label(), "home");
    }

    #[test]
    fn reset_restores_defaults() {
        let mut config = Config::default();
        Field::FontSize.step(&mut config, 1, &themes());
        Field::Theme.step(&mut config, 1, &themes());
        Field::FontSize.reset(&mut config);
        Field::Theme.reset(&mut config);
        assert_eq!(config.font.size, Config::default().font.size);
        assert_eq!(config.appearance.theme, Config::default().appearance.theme);
    }

    #[test]
    fn tab_strip_width_steps_within_bounds_and_resets() {
        let mut config = Config::default();
        let default = config.appearance.tab_strip_width;
        Field::TabStripWidth.step(&mut config, 1, &themes());
        assert!(config.appearance.tab_strip_width > default);
        for _ in 0..40 {
            Field::TabStripWidth.step(&mut config, 1, &themes());
        }
        assert_eq!(config.appearance.tab_strip_width, 480);
        for _ in 0..40 {
            Field::TabStripWidth.step(&mut config, -1, &themes());
        }
        assert_eq!(config.appearance.tab_strip_width, 120);
        Field::TabStripWidth.reset(&mut config);
        assert_eq!(config.appearance.tab_strip_width, default);
    }

    #[test]
    fn status_bar_segments_toggle_independently() {
        let mut config = Config::default();
        assert!(config.appearance.status_bar_cpu);
        Field::StatusBarCpu.step(&mut config, 1, &themes());
        assert!(!config.appearance.status_bar_cpu);
        assert!(
            config.appearance.status_bar_ram,
            "the other segments are untouched"
        );
        assert!(config.appearance.status_bar_clock);
    }

    #[test]
    fn the_clock_format_is_editable_text() {
        let mut config = Config::default();
        Field::ClockFormat.set_text(&mut config, "%H:%M:%S");
        assert_eq!(config.appearance.clock_format, "%H:%M:%S");
        Field::ClockFormat.reset(&mut config);
        assert_eq!(config.appearance.clock_format, "%H:%M");
    }

    #[test]
    fn every_new_row_maps_to_the_appearance_section() {
        for field in [
            Field::TabStripWidth,
            Field::StatusBarCpu,
            Field::StatusBarRam,
            Field::StatusBarClock,
            Field::ClockFormat,
        ] {
            assert_eq!(field.path(), "appearance", "{}", field.id());
        }
    }

    #[test]
    fn fields_that_need_a_new_session_are_declared() {
        assert!(Field::Term.requires_restart());
        assert!(Field::ScrollbackLines.requires_restart());
        assert!(!Field::FontSize.requires_restart());
        assert!(!Field::ShowStatusBar.requires_restart());
    }

    #[test]
    fn every_value_is_readable() {
        let config = Config::default();
        for field in [
            Field::Theme,
            Field::FontSize,
            Field::FontWeight,
            Field::LineHeight,
            Field::LetterSpacing,
            Field::ScrollbackLines,
            Field::WindowOpacity,
            Field::MaxFpsWhenIdle,
            Field::AboutVersion,
        ] {
            assert!(
                !field.value(&config).is_empty(),
                "{} has no value",
                field.id()
            );
        }
    }
}
