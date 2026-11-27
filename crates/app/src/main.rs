//! AstraTerm — a fast, native terminal emulator for Linux.
//!
//! The binary is a thin shell around [`Application`]: it parses the CLI, sets up
//! logging and hands control to the winit event loop.

mod app;
mod chrome;
mod cli;
mod ime;
mod keys;
mod paste;
mod prefs;
mod ui;

use anyhow::Context;
use clap::Parser;
use winit::event_loop::EventLoop;

use app::{Application, UserEvent};
use cli::Cli;

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    init_tracing(cli.debug);

    // A real event loop with a proxy: PTY threads use it to wake the application
    // instead of the UI polling for output.
    let event_loop = EventLoop::<UserEvent>::with_user_event()
        .build()
        .context("could not create the winit event loop")?;
    let proxy = event_loop.create_proxy();

    let mut application = Application::new(cli, proxy).context("could not start AstraTerm")?;
    event_loop
        .run_app(&mut application)
        .context("the event loop failed")?;

    tracing::info!("AstraTerm exited cleanly");
    Ok(())
}

/// Initialises structured logging.
///
/// `RUST_LOG` wins when set; `--debug` enables debug logging for this crate. No
/// terminal content, clipboard data or environment secrets are ever logged.
fn init_tracing(debug: bool) {
    let default = if debug {
        "astraterm=debug,astraterm_core=debug"
    } else {
        "astraterm=info,warn"
    };
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_writer(std::io::stderr)
        .try_init();
}
