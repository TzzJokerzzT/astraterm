//! Native workspace model: windows, tabs, the recursive split tree, focus and
//! layout geometry.
//!
//! Everything in this crate is pure data plus pure functions. No window, no GPU,
//! no PTY: layout math and focus traversal are unit-testable, which is what makes
//! the multiplexer trustworthy.

pub mod focus;
pub mod layout;
pub mod pane;
pub mod snapshot;
pub mod tab;
pub mod tree;
pub mod window;

pub use astraterm_core::FocusDirection;
pub use pane::{Pane, PaneRect};
pub use snapshot::{
    NodeSnapshot, PaneMeta, RestoredWorkspace, TabSnapshot, WindowSnapshot, WorkspaceSnapshot,
    restore_window, snapshot_window,
};
pub use tab::Tab;
pub use tree::SplitNode;
pub use window::WindowState;

pub use astraterm_core::SplitAxis;

/// Configuration that affects layout geometry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LayoutConfig {
    /// Thickness of the divider between two panes, in device pixels.
    pub divider: u32,
    /// Gap added around each pane, in device pixels.
    pub gap: u32,
    /// Smallest pane a split may produce, in device pixels.
    pub min_pane_width: u32,
    pub min_pane_height: u32,
}

impl Default for LayoutConfig {
    fn default() -> Self {
        Self {
            divider: 1,
            gap: 2,
            min_pane_width: 120,
            min_pane_height: 80,
        }
    }
}
