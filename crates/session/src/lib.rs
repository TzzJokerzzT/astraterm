//! Native session multiplexer: owns PTY-backed terminal sessions.
//!
//! A session binds one PTY process to one terminal engine. The workspace owns
//! *where* panes are; this crate owns *what runs in them*. Nothing here depends on
//! the UI, the renderer or `winit`, and nothing here shells out to tmux.
//!
//! Output is pumped by the application event loop through
//! [`manager::SessionManager::pump`], which applies a per-session byte budget so a
//! single noisy process cannot starve the other panes or the input path.

pub mod lifecycle;
pub mod manager;

pub use lifecycle::{SessionSpec, resolve_shell, session_spec};
pub use manager::{PumpReport, Session, SessionError, SessionManager};
