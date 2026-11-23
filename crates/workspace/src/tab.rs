//! Tabs: a workspace holding one split tree.

use astraterm_core::{FocusDirection, PaneId, Rect, SplitAxis, TabId};

use crate::{
    LayoutConfig, focus,
    layout::{self, Placement},
    tree::{SplitId, SplitNode},
};

/// A tab owns its own layout tree and its own focused pane.
#[derive(Clone, Debug, PartialEq)]
pub struct Tab {
    pub id: TabId,
    /// User-provided title. Empty means "derive from the active pane".
    pub title: String,
    pub root: SplitNode,
    pub active_pane: PaneId,
    /// Temporarily maximised pane; layout shows it alone while set.
    pub zoomed: Option<PaneId>,
}

impl Tab {
    /// Creates a tab with a single pane.
    pub fn new(title: impl Into<String>, first_pane: PaneId) -> Self {
        Self {
            id: TabId::next(),
            title: title.into(),
            root: SplitNode::pane(first_pane),
            active_pane: first_pane,
            zoomed: None,
        }
    }

    /// Splits the focused pane, returning the identifier of the new split.
    ///
    /// The new pane is created and focused; the caller is responsible for binding
    /// a session to it.
    pub fn split_focused(&mut self, axis: SplitAxis, new_pane: PaneId) -> Option<SplitId> {
        let target = self.zoomed.unwrap_or(self.active_pane);
        let id = self.root.split_at(target, new_pane, axis)?;
        self.zoomed = None;
        self.active_pane = new_pane;
        Some(id)
    }

    /// Closes a pane. Returns `false` when it was the last pane, in which case the
    /// caller should close the tab instead.
    pub fn close_pane(&mut self, pane: PaneId) -> bool {
        if self.root.pane_count() <= 1 {
            return false;
        }
        let previous_focus = self.active_pane;
        if !self.root.remove_pane(pane) {
            return false;
        }
        if self.zoomed == Some(pane) {
            self.zoomed = None;
        }
        if self.active_pane == pane {
            let panes = self.root.panes();
            self.active_pane = panes.first().copied().unwrap_or(previous_focus);
        }
        true
    }

    /// Focuses a pane that belongs to this tab.
    pub fn focus(&mut self, pane: PaneId) -> bool {
        if !self.root.contains(pane) {
            return false;
        }
        self.active_pane = pane;
        true
    }

    /// Moves focus to the pane adjacent in `direction`.
    pub fn focus_direction(
        &mut self,
        direction: FocusDirection,
        rect: Rect,
        config: LayoutConfig,
    ) -> bool {
        let placements = self.layout(rect, config);
        if let Some(next) = focus::nearest_in_direction(&placements, self.active_pane, direction) {
            self.active_pane = next;
            return true;
        }
        false
    }

    /// Cycles focus through the panes of this tab.
    pub fn focus_next(&mut self, rect: Rect, config: LayoutConfig) -> bool {
        let placements = self.layout(rect, config);
        if let Some(next) = focus::next_pane(&placements, self.active_pane) {
            self.active_pane = next;
            return true;
        }
        false
    }

    pub fn panes(&self) -> Vec<PaneId> {
        self.root.panes()
    }

    pub fn pane_count(&self) -> usize {
        self.root.pane_count()
    }

    /// Toggles zoom for the focused pane.
    pub fn toggle_zoom(&mut self) {
        self.zoomed = if self.zoomed.is_some() {
            None
        } else {
            Some(self.active_pane)
        };
    }

    /// Computes pane rectangles, honouring zoom.
    pub fn layout(&self, rect: Rect, config: LayoutConfig) -> Vec<Placement> {
        if let Some(zoomed) = self.zoomed
            && self.root.contains(zoomed)
        {
            return vec![Placement {
                pane: zoomed,
                rect: rect.shrunk(config.gap / 2),
            }];
        }
        layout::compute_layout(&self.root, rect, config)
    }

    /// Divider rectangles for pointer interaction and drawing.
    pub fn dividers(&self, rect: Rect, config: LayoutConfig) -> Vec<(SplitId, SplitAxis, Rect)> {
        if self.zoomed.is_some() {
            return Vec::new();
        }
        layout::compute_dividers(&self.root, rect, config)
    }

    /// Rect of one pane, if it is currently visible.
    pub fn pane_rect(&self, pane: PaneId, rect: Rect, config: LayoutConfig) -> Option<Rect> {
        self.layout(rect, config)
            .into_iter()
            .find(|placement| placement.pane == pane)
            .map(|placement| placement.rect)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tab() -> (Tab, PaneId) {
        let first = PaneId::next();
        (Tab::new("shell", first), first)
    }

    fn config() -> LayoutConfig {
        LayoutConfig {
            divider: 1,
            gap: 0,
            min_pane_width: 100,
            min_pane_height: 50,
        }
    }

    #[test]
    fn new_tab_has_a_single_focused_pane() {
        let (tab, first) = tab();
        assert_eq!(tab.active_pane, first);
        assert_eq!(tab.pane_count(), 1);
        assert!(tab.zoomed.is_none());
    }

    #[test]
    fn split_focuses_the_new_pane() {
        let (mut tab, first) = tab();
        let second = PaneId::next();
        assert!(tab.split_focused(SplitAxis::Vertical, second).is_some());
        assert_eq!(tab.active_pane, second);
        assert_eq!(tab.pane_count(), 2);
        assert!(tab.panes().contains(&first));
    }

    #[test]
    fn splitting_clears_zoom() {
        let (mut tab, _) = tab();
        tab.toggle_zoom();
        assert!(tab.zoomed.is_some());
        tab.split_focused(SplitAxis::Vertical, PaneId::next());
        assert!(tab.zoomed.is_none());
    }

    #[test]
    fn closing_a_pane_moves_focus_to_a_survivor() {
        let (mut tab, first) = tab();
        let second = PaneId::next();
        tab.split_focused(SplitAxis::Vertical, second);
        assert!(tab.close_pane(second));
        assert_eq!(tab.active_pane, first);
    }

    #[test]
    fn closing_the_last_pane_is_refused() {
        let (mut tab, first) = tab();
        assert!(!tab.close_pane(first));
        assert_eq!(tab.pane_count(), 1);
    }

    #[test]
    fn focus_direction_uses_pixel_layout() {
        let (mut tab, first) = tab();
        let second = PaneId::next();
        tab.split_focused(SplitAxis::Vertical, second);
        tab.active_pane = first;
        let rect = Rect::new(0, 0, 802, 600);
        assert!(tab.focus_direction(FocusDirection::Right, rect, config()));
        assert_eq!(tab.active_pane, second);
        assert!(tab.focus_direction(FocusDirection::Left, rect, config()));
        assert_eq!(tab.active_pane, first);
    }

    #[test]
    fn focus_outside_the_tab_is_refused() {
        let (mut tab, first) = tab();
        assert!(!tab.focus(PaneId::next()));
        assert_eq!(tab.active_pane, first);
    }

    #[test]
    fn zoom_shows_only_the_focused_pane() {
        let (mut tab, first) = tab();
        tab.split_focused(SplitAxis::Vertical, PaneId::next());
        tab.toggle_zoom();
        let layout = tab.layout(Rect::new(0, 0, 800, 600), config());
        assert_eq!(layout.len(), 1);
        assert_eq!(layout[0].pane, tab.active_pane);
        assert!(tab.dividers(Rect::new(0, 0, 800, 600), config()).is_empty());

        // Zoom persists while focus moves; unzooming restores the split layout.
        tab.focus(first);
        assert_eq!(tab.layout(Rect::new(0, 0, 800, 600), config()).len(), 1);
        tab.toggle_zoom();
        assert_eq!(tab.layout(Rect::new(0, 0, 800, 600), config()).len(), 2);
        assert_eq!(
            tab.pane_rect(first, Rect::new(0, 0, 800, 600), config())
                .unwrap()
                .width,
            400
        );
    }

    #[test]
    fn pane_rect_is_none_for_hidden_panes() {
        let (tab, first) = tab();
        assert!(
            tab.pane_rect(first, Rect::new(0, 0, 800, 600), config())
                .is_some()
        );
        assert!(
            tab.pane_rect(PaneId::next(), Rect::new(0, 0, 800, 600), config())
                .is_none()
        );
    }
}
