//! Workspace metadata persistence.
//!
//! AstraTerm restores *layout metadata* — tabs, the split tree, the working
//! directory each pane was in, the profile it came from — and recreates fresh
//! sessions for it. Processes are never kept alive across application restarts;
//! that would require a session daemon, which is explicitly out of scope.

use std::collections::BTreeMap;
use std::path::PathBuf;

use astraterm_core::{PaneId, SplitAxis, WindowId};
use serde::{Deserialize, Serialize};

use crate::{Tab, WindowState, pane::Pane, tree::SplitNode};

/// Current snapshot schema version.
pub const SNAPSHOT_VERSION: u32 = 1;

/// Per-pane metadata that survives a restart.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneMeta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// One node of a persisted layout.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum NodeSnapshot {
    Pane(PaneMeta),
    Split {
        /// `horizontal` or `vertical`.
        axis: String,
        ratio: f32,
        first: Box<NodeSnapshot>,
        second: Box<NodeSnapshot>,
    },
}

/// A persisted tab.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TabSnapshot {
    #[serde(default)]
    pub title: String,
    /// Index of the focused pane within this tab's `layout`, in traversal order.
    #[serde(default)]
    pub active_pane: usize,
    pub layout: NodeSnapshot,
}

/// A persisted window.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct WindowSnapshot {
    #[serde(default = "default_width")]
    pub width: u32,
    #[serde(default = "default_height")]
    pub height: u32,
    #[serde(default)]
    pub maximized: bool,
    #[serde(default)]
    pub active_tab: usize,
    #[serde(default)]
    pub tabs: Vec<TabSnapshot>,
}

fn default_width() -> u32 {
    1280
}

fn default_height() -> u32 {
    800
}

/// Everything persisted between runs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceSnapshot {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub windows: Vec<WindowSnapshot>,
}

impl Default for WorkspaceSnapshot {
    fn default() -> Self {
        Self {
            version: SNAPSHOT_VERSION,
            windows: Vec::new(),
        }
    }
}

/// Captures a window's layout.
pub fn snapshot_window(window: &WindowState, panes: &BTreeMap<PaneId, Pane>) -> WindowSnapshot {
    let tabs = window
        .tabs
        .iter()
        .map(|tab| snapshot_tab(tab, panes))
        .collect();
    WindowSnapshot {
        width: 0,
        height: 0,
        maximized: false,
        active_tab: window.active_index().unwrap_or(0),
        tabs,
    }
}

fn snapshot_tab(tab: &Tab, panes: &BTreeMap<PaneId, Pane>) -> TabSnapshot {
    let order = tab.root.panes();
    let active_pane = order
        .iter()
        .position(|pane| *pane == tab.active_pane)
        .unwrap_or(0);
    TabSnapshot {
        title: tab.title.clone(),
        active_pane,
        layout: snapshot_node(&tab.root, panes),
    }
}

fn snapshot_node(node: &SplitNode, panes: &BTreeMap<PaneId, Pane>) -> NodeSnapshot {
    match node {
        SplitNode::Pane(id) => {
            let pane = panes.get(id);
            NodeSnapshot::Pane(PaneMeta {
                cwd: pane.and_then(|pane| pane.cwd.clone()),
                profile: pane.and_then(|pane| pane.profile.map(|profile| profile.to_string())),
                label: None,
            })
        }
        SplitNode::Split {
            axis,
            ratio,
            first,
            second,
            ..
        } => NodeSnapshot::Split {
            axis: axis.as_str().to_owned(),
            ratio: *ratio,
            first: Box::new(snapshot_node(first, panes)),
            second: Box::new(snapshot_node(second, panes)),
        },
    }
}

/// Result of restoring a persisted layout.
pub struct RestoredWorkspace {
    pub window_id: WindowId,
    pub tabs: Vec<Tab>,
    pub active_index: usize,
    /// Metadata for every recreated pane, keyed by its fresh identifier.
    pub panes: BTreeMap<PaneId, PaneMeta>,
}

/// Rebuilds a window from a snapshot, allocating fresh pane identifiers through
/// `next_pane`. Sessions are created later, by the caller, from `panes`.
pub fn restore_window(
    snapshot: &WindowSnapshot,
    mut next_pane: impl FnMut() -> PaneId,
) -> Option<RestoredWorkspace> {
    let mut panes = BTreeMap::new();
    let mut tabs = Vec::new();

    for tab_snapshot in &snapshot.tabs {
        let Some(root) = restore_node(&tab_snapshot.layout, &mut next_pane, &mut panes) else {
            continue;
        };
        let order = root.panes();
        if order.is_empty() {
            continue;
        }
        let active_pane = order
            .get(tab_snapshot.active_pane.min(order.len().saturating_sub(1)))
            .copied()
            .unwrap_or(order[0]);
        tabs.push(Tab {
            id: astraterm_core::TabId::next(),
            title: tab_snapshot.title.clone(),
            root,
            active_pane,
            zoomed: None,
        });
    }

    if tabs.is_empty() {
        return None;
    }

    Some(RestoredWorkspace {
        window_id: WindowId::next(),
        active_index: snapshot.active_tab.min(tabs.len() - 1),
        tabs,
        panes,
    })
}

fn restore_node(
    snapshot: &NodeSnapshot,
    next_pane: &mut impl FnMut() -> PaneId,
    panes: &mut BTreeMap<PaneId, PaneMeta>,
) -> Option<SplitNode> {
    match snapshot {
        NodeSnapshot::Pane(meta) => {
            let id = next_pane();
            panes.insert(id, meta.clone());
            Some(SplitNode::Pane(id))
        }
        NodeSnapshot::Split {
            axis,
            ratio,
            first,
            second,
        } => {
            let first_node = restore_node(first, next_pane, panes)?;
            let second_node = restore_node(second, next_pane, panes)?;
            let axis = if axis == "horizontal" {
                SplitAxis::Horizontal
            } else {
                SplitAxis::Vertical
            };
            let mut node = SplitNode::Split {
                id: crate::tree::SplitId::fresh(),
                axis,
                ratio: ratio.clamp(crate::tree::MIN_RATIO, crate::tree::MAX_RATIO),
                first: Box::new(first_node),
                second: Box::new(second_node),
            };
            // Normalise: guarantee the id is unique for this node.
            if let SplitNode::Split { id, .. } = &mut node {
                *id = crate::tree::SplitId::fresh();
            }
            Some(node)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane(id: u64) -> PaneId {
        PaneId::from_raw(id)
    }

    #[test]
    fn snapshot_and_restore_round_trip_the_layout() {
        let mut window_panes = BTreeMap::new();
        let first = PaneId::next();
        let second = PaneId::next();
        window_panes.insert(first, Pane::new(first));
        window_panes.insert(second, Pane::new(second));

        let mut tab = Tab::new("work", first);
        tab.split_focused(SplitAxis::Horizontal, second);
        let window = WindowState::new(WindowId::next(), tab);

        let snapshot = snapshot_window(&window, &window_panes);
        assert_eq!(snapshot.tabs.len(), 1);
        assert_eq!(snapshot.tabs[0].title, "work");

        let restored = restore_window(&snapshot, PaneId::next).unwrap();
        assert_eq!(restored.tabs.len(), 1);
        assert_eq!(restored.tabs[0].title, "work");
        assert_eq!(restored.tabs[0].pane_count(), 2);
        assert_eq!(restored.panes.len(), 2);
        match restored.tabs[0].root {
            SplitNode::Split { axis, .. } => assert_eq!(axis, SplitAxis::Horizontal),
            _ => panic!("expected a split"),
        }
    }

    #[test]
    fn restore_allocates_fresh_pane_identifiers() {
        let snapshot = WindowSnapshot {
            width: 800,
            height: 600,
            maximized: false,
            active_tab: 0,
            tabs: vec![TabSnapshot {
                title: "one".to_owned(),
                active_pane: 0,
                layout: NodeSnapshot::Pane(PaneMeta {
                    cwd: Some("/tmp".into()),
                    profile: None,
                    label: None,
                }),
            }],
        };
        let restored = restore_window(&snapshot, PaneId::next).unwrap();
        let id = restored.tabs[0].active_pane;
        assert_ne!(id, pane(1));
        assert_eq!(restored.panes[&id].cwd, Some(PathBuf::from("/tmp")));
    }

    #[test]
    fn empty_snapshot_restores_nothing() {
        let snapshot = WindowSnapshot {
            tabs: Vec::new(),
            ..Default::default()
        };
        assert!(restore_window(&snapshot, PaneId::next).is_none());
    }

    #[test]
    fn snapshot_serializes_to_toml() {
        let snapshot = WorkspaceSnapshot {
            version: SNAPSHOT_VERSION,
            windows: vec![WindowSnapshot {
                width: 1024,
                height: 768,
                maximized: true,
                active_tab: 0,
                tabs: vec![TabSnapshot {
                    title: "t".to_owned(),
                    active_pane: 0,
                    layout: NodeSnapshot::Pane(PaneMeta::default()),
                }],
            }],
        };
        let text = toml::to_string_pretty(&snapshot).unwrap();
        let parsed: WorkspaceSnapshot = toml::from_str(&text).unwrap();
        assert_eq!(parsed, snapshot);
    }

    #[test]
    fn active_pane_index_is_clamped_on_restore() {
        let snapshot = WindowSnapshot {
            active_tab: 5,
            tabs: vec![TabSnapshot {
                title: "t".to_owned(),
                active_pane: 9,
                layout: NodeSnapshot::Pane(PaneMeta::default()),
            }],
            ..Default::default()
        };
        let restored = restore_window(&snapshot, PaneId::next).unwrap();
        assert_eq!(restored.active_index, 0);
        assert_eq!(restored.tabs[0].active_pane, restored.tabs[0].panes()[0]);
    }
}
