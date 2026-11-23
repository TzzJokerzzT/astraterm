//! Turning configuration and profiles into a concrete PTY specification.
//!
//! The resolved specification never contains a shell string: either the shell is
//! started with an argument vector, or a profile's `command` vector is executed
//! directly. `sh -c` is never used to interpret configuration values.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use astraterm_config::{Config, Profile, WorkingDirectory};
use astraterm_core::ProfileId;
use astraterm_pty::PtySpec;

use astraterm_terminal::EngineOptions;

/// Everything needed to create one session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionSpec {
    pub pty: PtySpec,
    pub engine: EngineSpec,
    /// Profile that produced this session, if any.
    pub profile: Option<ProfileId>,
    /// Label to show in the tab strip before the application sets a title.
    pub label: Option<String>,
    /// Text typed into the PTY once it starts (never executed through a shell).
    pub startup_input: Option<String>,
}

/// The subset of engine options that depends on configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EngineSpec {
    pub scrollback: usize,
    pub allow_osc52: bool,
    /// Advertise the Kitty keyboard protocol (on unless configured otherwise).
    pub kitty_keyboard: bool,
}

impl SessionSpec {
    pub fn engine_options(&self, cols: u16, rows: u16) -> EngineOptions {
        EngineOptions {
            cols,
            rows,
            scrollback: self.engine.scrollback,
            hollow_cursor: false,
            allow_osc52: self.engine.allow_osc52,
            kitty_keyboard: self.engine.kitty_keyboard,
        }
    }
}

/// Resolves the shell program and its arguments.
///
/// `auto` follows `$SHELL` and falls back to `/bin/sh`; an explicit value is used
/// verbatim (it may be a name looked up in `PATH` or an absolute path).
pub fn resolve_shell(config: &Config, profile: Option<&Profile>) -> (String, Vec<String>) {
    if let Some(profile) = profile {
        if !profile.command.is_empty() {
            let mut command = profile.command.clone();
            let program = command.remove(0);
            return (program, command);
        }
        if !profile.shell.trim().is_empty() && profile.shell != "auto" {
            return (profile.shell.clone(), profile.args.clone());
        }
        if !profile.args.is_empty() {
            let (shell, _) = shell_from_config(config);
            return (shell, profile.args.clone());
        }
    }
    shell_from_config(config)
}

fn shell_from_config(config: &Config) -> (String, Vec<String>) {
    let configured = config.terminal.shell.trim();
    if !configured.is_empty() && configured != "auto" {
        return (configured.to_owned(), config.terminal.shell_args.clone());
    }
    let shell = std::env::var("SHELL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .filter(|value| Path::new(value).exists())
        .unwrap_or_else(|| "/bin/sh".to_owned());
    (shell, config.terminal.shell_args.clone())
}

/// Builds a session specification from the configuration.
pub fn session_spec(
    config: &Config,
    profile: Option<&Profile>,
    profile_id: Option<ProfileId>,
    cwd: Option<PathBuf>,
    cols: u16,
    rows: u16,
    cell: (u16, u16),
) -> SessionSpec {
    let (program, args) = resolve_shell(config, profile);

    let mut env: BTreeMap<String, String> = BTreeMap::new();
    env.insert("TERM".to_owned(), config.terminal.term.clone());
    env.insert("COLORTERM".to_owned(), "truecolor".to_owned());
    env.insert("ASTRATERM".to_owned(), "1".to_owned());
    env.insert(
        "ASTRATERM_VERSION".to_owned(),
        env!("CARGO_PKG_VERSION").to_owned(),
    );
    if let Some(profile) = profile {
        for (key, value) in &profile.env {
            env.insert(key.clone(), value.clone());
        }
    }

    let directory = profile
        .and_then(|profile| profile.working_directory.as_ref())
        .or(Some(&config.terminal.working_directory))
        .and_then(|policy| resolve_directory(policy, cwd.as_deref()))
        .or(cwd)
        .or_else(astraterm_config::schema::home_dir);

    let scrollback = config.terminal.scrollback_lines;
    let startup_input = profile.and_then(|profile| profile.startup_command.clone());

    SessionSpec {
        pty: PtySpec {
            program,
            args,
            cwd: directory,
            env,
            cols: cols.max(1),
            rows: rows.max(1),
            cell_width: cell.0,
            cell_height: cell.1,
        },
        engine: EngineSpec {
            scrollback,
            // Escape sequences are untrusted input: OSC 52 stays off.
            allow_osc52: false,
            kitty_keyboard: config.terminal.kitty_keyboard,
        },
        profile: profile_id,
        label: profile
            .and_then(|profile| profile.label.clone())
            .or_else(|| {
                profile
                    .map(|profile| profile.name.clone())
                    .filter(|name| !name.is_empty())
            }),
        startup_input,
    }
}

fn resolve_directory(policy: &WorkingDirectory, current: Option<&Path>) -> Option<PathBuf> {
    let resolved = policy.resolve(current)?;
    if resolved.is_dir() {
        Some(resolved)
    } else {
        // A configured directory may disappear between runs; fall back to home.
        astraterm_config::schema::home_dir().filter(|home| home.is_dir())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn auto_shell_follows_the_environment() {
        let config = Config::default();
        let (program, _) = resolve_shell(&config, None);
        assert!(
            program.starts_with('/'),
            "resolved shell must be absolute: {program}"
        );
    }

    #[test]
    fn explicit_shell_wins_over_the_environment() {
        let mut config = Config::default();
        config.terminal.shell = "/bin/sh".to_owned();
        assert_eq!(resolve_shell(&config, None).0, "/bin/sh");
    }

    #[test]
    fn profile_command_is_used_as_an_argument_vector() {
        let config = Config::default();
        let mut profile = Profile::new("ssh");
        profile.command = vec!["ssh".to_owned(), "production".to_owned()];
        let (program, args) = resolve_shell(&config, Some(&profile));
        assert_eq!(program, "ssh");
        assert_eq!(args, vec!["production".to_owned()]);
    }

    #[test]
    fn profile_shell_and_args_override_the_global_shell() {
        let config = Config::default();
        let mut profile = Profile::new("login");
        profile.shell = "/bin/zsh".to_owned();
        profile.args = vec!["-l".to_owned()];
        let (program, args) = resolve_shell(&config, Some(&profile));
        assert_eq!(program, "/bin/zsh");
        assert_eq!(args, vec!["-l".to_owned()]);
    }

    #[test]
    fn session_spec_sets_term_and_environment() {
        let mut config = Config::default();
        config.terminal.term = "screen-256color".to_owned();
        let mut env = BTreeMap::new();
        env.insert("PROJECT".to_owned(), "astraterm".to_owned());
        let mut profile = Profile::new("work");
        profile.env = env;

        let spec = session_spec(&config, Some(&profile), None, None, 80, 24, (8, 16));
        assert_eq!(
            spec.pty.env.get("TERM").map(String::as_str),
            Some("screen-256color")
        );
        assert_eq!(
            spec.pty.env.get("COLORTERM").map(String::as_str),
            Some("truecolor")
        );
        assert_eq!(
            spec.pty.env.get("PROJECT").map(String::as_str),
            Some("astraterm")
        );
        assert_eq!(spec.engine.scrollback, config.terminal.scrollback_lines);
        assert!(!spec.engine.allow_osc52, "OSC 52 stays disabled by default");
        assert!(
            spec.engine.kitty_keyboard,
            "the Kitty keyboard protocol is advertised by default"
        );
    }

    #[test]
    fn session_spec_prefers_the_profile_working_directory() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::default();
        let mut profile = Profile::new("work");
        profile.working_directory = Some(WorkingDirectory::Path(dir.path().to_path_buf()));
        let spec = session_spec(&config, Some(&profile), None, None, 80, 24, (8, 16));
        assert_eq!(spec.pty.cwd.as_deref(), Some(dir.path()));
    }

    #[test]
    fn a_missing_working_directory_falls_back_home() {
        let config = Config::default();
        let mut profile = Profile::new("work");
        profile.working_directory = Some(WorkingDirectory::Path("/definitely/gone".into()));
        let spec = session_spec(&config, Some(&profile), None, None, 80, 24, (8, 16));
        assert_ne!(
            spec.pty.cwd.as_deref(),
            Some(std::path::Path::new("/definitely/gone"))
        );
    }

    #[test]
    fn startup_command_is_carried_as_input_not_as_a_shell_string() {
        let config = Config::default();
        let mut profile = Profile::new("dev");
        profile.startup_command = Some("nvim .".to_owned());
        let spec = session_spec(&config, Some(&profile), None, None, 80, 24, (8, 16));
        assert_eq!(spec.startup_input.as_deref(), Some("nvim ."));
        assert!(
            spec.pty.args.is_empty(),
            "startup commands must not become argv"
        );
    }

    #[test]
    fn engine_options_use_requested_dimensions() {
        let config = Config::default();
        let spec = session_spec(&config, None, None, None, 132, 43, (8, 16));
        let options = spec.engine_options(132, 43);
        assert_eq!(options.cols, 132);
        assert_eq!(options.rows, 43);
        assert_eq!(options.scrollback, config.terminal.scrollback_lines);
        assert_eq!(options.kitty_keyboard, config.terminal.kitty_keyboard);
    }

    #[test]
    fn kitty_keyboard_follows_the_configuration() {
        let mut config = Config::default();
        config.terminal.kitty_keyboard = false;
        let spec = session_spec(&config, None, None, None, 80, 24, (8, 16));
        assert!(!spec.engine.kitty_keyboard);
        assert!(!spec.engine_options(80, 24).kitty_keyboard);
    }
}
