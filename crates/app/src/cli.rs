//! Command-line interface.

use std::path::PathBuf;

use clap::Parser;

/// AstraTerm — a fast, native terminal emulator for Linux.
#[derive(Parser, Debug, Clone, Default)]
#[command(
    name = "astraterm",
    version,
    about = "AstraTerm is a native Rust terminal emulator with built-in tabs and splits",
    long_about = None,
    disable_help_subcommand = true
)]
pub struct Cli {
    /// Start the first session in this directory.
    #[arg(long, value_name = "DIR")]
    pub working_directory: Option<PathBuf>,

    /// Start with a named profile from the configuration.
    #[arg(long, value_name = "NAME")]
    pub profile: Option<String>,

    /// Run this command instead of the login shell, as an argument vector.
    ///
    /// The argument vector is passed straight to the PTY: no shell interprets it.
    #[arg(long, value_name = "ARGV", num_args = 1.., trailing_var_arg = true, allow_hyphen_values = true)]
    pub command: Vec<String>,

    /// Window title for the first tab.
    #[arg(long, value_name = "TEXT")]
    pub title: Option<String>,

    /// Use this configuration file instead of ~/.config/astraterm/config.toml.
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,

    /// Open a new window even when another one is already running.
    #[arg(long = "new-window")]
    pub new_window: bool,

    /// Enable verbose logging (equivalent to RUST_LOG=astraterm=debug).
    #[arg(long)]
    pub debug: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn parses_documented_invocations() {
        let cli = Cli::parse_from(["astraterm", "--working-directory", "/tmp"]);
        assert_eq!(
            cli.working_directory.as_deref(),
            Some(std::path::Path::new("/tmp"))
        );

        let cli = Cli::parse_from(["astraterm", "--profile", "work"]);
        assert_eq!(cli.profile.as_deref(), Some("work"));

        let cli = Cli::parse_from(["astraterm", "--title", "Backend"]);
        assert_eq!(cli.title.as_deref(), Some("Backend"));

        let cli = Cli::parse_from(["astraterm", "--new-window"]);
        assert!(cli.new_window);
    }

    #[test]
    fn command_is_kept_as_an_argument_vector() {
        let cli = Cli::parse_from(["astraterm", "--command", "ssh", "production"]);
        assert_eq!(cli.command, vec!["ssh".to_owned(), "production".to_owned()]);
    }

    #[test]
    fn command_forward_flags_to_the_program() {
        let cli = Cli::parse_from(["astraterm", "--command", "nvim", "-u", "NONE"]);
        assert_eq!(
            cli.command,
            vec!["nvim".to_owned(), "-u".to_owned(), "NONE".to_owned()]
        );
    }

    #[test]
    fn defaults_are_empty() {
        let cli = Cli::parse_from(["astraterm"]);
        assert!(cli.working_directory.is_none());
        assert!(cli.command.is_empty());
        assert!(!cli.debug);
    }
}
