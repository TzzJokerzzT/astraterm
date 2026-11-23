//! Stable identifiers, geometry and inter-subsystem event types shared by every
//! AstraTerm crate.
//!
//! This crate must stay dependency-light: it is the vocabulary the workspace,
//! session, terminal, renderer and configuration layers all speak.

pub mod events;
pub mod geometry;
pub mod ids;

pub use events::{AppCommand, AppEvent, ExitReason, FocusDirection, SessionEvent};
pub use geometry::{CellMetrics, Padding, Rect, Size, SplitAxis};
pub use ids::{PaneId, ProfileId, SessionId, TabId, WindowId};
