//! The recursive split tree.
//!
//! A tab's layout is a binary tree: leaves are panes, inner nodes are splits with
//! an axis and a ratio. This supports arbitrary nested layouts (unlike a fixed
//! grid) with only a handful of operations.

use astraterm_core::{PaneId, SplitAxis};

/// Identifier of a split node, stable across layout changes so a divider drag can
/// keep addressing the same node.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SplitId(u64);

impl SplitId {
    fn next() -> Self {
        Self::fresh()
    }

    /// Numeric value, used by the UI to address a divider.
    pub fn raw(self) -> u64 {
        self.0
    }

    /// Allocates a fresh split identifier (used when restoring a layout).
    pub fn fresh() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(1);
        Self(COUNTER.fetch_add(1, Ordering::Relaxed))
    }
}

/// Minimum and maximum fraction a divider may take, so panes never disappear.
pub const MIN_RATIO: f32 = 0.1;
pub const MAX_RATIO: f32 = 0.9;

/// One node of a tab layout.
#[derive(Clone, Debug, PartialEq)]
pub enum SplitNode {
    Pane(PaneId),
    Split {
        id: SplitId,
        axis: SplitAxis,
        ratio: f32,
        first: Box<SplitNode>,
        second: Box<SplitNode>,
    },
}

impl SplitNode {
    pub fn pane(id: PaneId) -> Self {
        SplitNode::Pane(id)
    }

    /// Splits the node containing `target`, inserting `new_pane` as the second
    /// child at ratio 0.5. Returns the new split identifier when the target was
    /// found.
    pub fn split_at(
        &mut self,
        target: PaneId,
        new_pane: PaneId,
        axis: SplitAxis,
    ) -> Option<SplitId> {
        match self {
            SplitNode::Pane(id) if *id == target => {
                let id = SplitId::next();
                let old = SplitNode::Pane(target);
                *self = SplitNode::Split {
                    id,
                    axis,
                    ratio: 0.5,
                    first: Box::new(old),
                    second: Box::new(SplitNode::Pane(new_pane)),
                };
                Some(id)
            }
            SplitNode::Pane(_) => None,
            SplitNode::Split { first, second, .. } => first
                .split_at(target, new_pane, axis)
                .or_else(|| second.split_at(target, new_pane, axis)),
        }
    }

    /// Removes the leaf for `target`, collapsing single-child splits.
    ///
    /// Returns `true` when the pane was found. Removing the last pane leaves the
    /// tree unchanged and returns `false`; the caller decides whether to close
    /// the tab.
    pub fn remove_pane(&mut self, target: PaneId) -> bool {
        if let SplitNode::Pane(_) = self {
            // Leaves are removed by their parent, which then collapses itself.
            return false;
        }
        let SplitNode::Split { first, second, .. } = self else {
            return false;
        };

        if let SplitNode::Pane(id) = **first
            && id == target
        {
            let replacement = std::mem::replace(&mut **second, SplitNode::Pane(id));
            *self = replacement;
            return true;
        }
        if let SplitNode::Pane(id) = **second
            && id == target
        {
            let replacement = std::mem::replace(&mut **first, SplitNode::Pane(id));
            *self = replacement;
            return true;
        }

        first.remove_pane(target) || second.remove_pane(target)
    }

    pub fn contains(&self, target: PaneId) -> bool {
        match self {
            SplitNode::Pane(id) => *id == target,
            SplitNode::Split { first, second, .. } => {
                first.contains(target) || second.contains(target)
            }
        }
    }

    /// Number of leaves (panes) in this tree.
    pub fn pane_count(&self) -> usize {
        match self {
            SplitNode::Pane(_) => 1,
            SplitNode::Split { first, second, .. } => first.pane_count() + second.pane_count(),
        }
    }

    /// Pane identifiers in traversal order (left-to-right, top-to-bottom).
    pub fn panes(&self) -> Vec<PaneId> {
        let mut out = Vec::new();
        self.collect_panes(&mut out);
        out
    }

    fn collect_panes(&self, out: &mut Vec<PaneId>) {
        match self {
            SplitNode::Pane(id) => out.push(*id),
            SplitNode::Split { first, second, .. } => {
                first.collect_panes(out);
                second.collect_panes(out);
            }
        }
    }

    /// Finds the split node with `id` and applies `f` to its ratio.
    pub fn adjust_ratio(&mut self, split: SplitId, delta: f32) -> bool {
        if let SplitNode::Split { id, ratio, .. } = self
            && *id == split
        {
            *ratio = (*ratio + delta).clamp(MIN_RATIO, MAX_RATIO);
            return true;
        }
        match self {
            SplitNode::Pane(_) => false,
            SplitNode::Split { first, second, .. } => {
                first.adjust_ratio(split, delta) || second.adjust_ratio(split, delta)
            }
        }
    }

    /// Sets every split ratio in the tree back to 0.5.
    pub fn equalize(&mut self) {
        match self {
            SplitNode::Pane(_) => {}
            SplitNode::Split {
                ratio,
                first,
                second,
                ..
            } => {
                *ratio = 0.5;
                first.equalize();
                second.equalize();
            }
        }
    }

    /// Exchanges the positions of two panes.
    pub fn swap_panes(&mut self, a: PaneId, b: PaneId) -> bool {
        let mut first_slot = None;
        let mut second_slot = None;
        self.visit_slots(&mut |slot| match slot {
            SplitNode::Pane(id) if *id == a => first_slot = Some(*id),
            SplitNode::Pane(id) if *id == b => second_slot = Some(*id),
            _ => {}
        });
        if first_slot.is_none() || second_slot.is_none() || a == b {
            return false;
        }
        self.visit_slots_mut(&mut |slot| match slot {
            SplitNode::Pane(id) if *id == a => *id = b,
            SplitNode::Pane(id) if *id == b => *id = a,
            _ => {}
        });
        true
    }

    fn visit_slots(&self, f: &mut impl FnMut(&SplitNode)) {
        f(self);
        if let SplitNode::Split { first, second, .. } = self {
            first.visit_slots(f);
            second.visit_slots(f);
        }
    }

    fn visit_slots_mut(&mut self, f: &mut impl FnMut(&mut SplitNode)) {
        f(self);
        if let SplitNode::Split { first, second, .. } = self {
            first.visit_slots_mut(f);
            second.visit_slots_mut(f);
        }
    }

    /// Finds the split node whose divider is closest to `(x, y)` inside `rect`,
    /// so a pointer drag can grab the correct divider.
    pub fn split_at_point(
        &self,
        rect: astraterm_core::Rect,
        config: crate::LayoutConfig,
        x: f64,
        y: f64,
        tolerance: f64,
    ) -> Option<(SplitId, astraterm_core::Rect)> {
        let (first_rect, second_rect) = crate::layout::split_children(rect, self, config)?;
        let SplitNode::Split {
            id,
            axis,
            first,
            second,
            ..
        } = self
        else {
            return None;
        };

        match axis {
            SplitAxis::Horizontal => {
                let divider_y = first_rect.bottom() as f64;
                let hit = (y - divider_y).abs() <= tolerance
                    && x >= rect.x as f64
                    && x <= rect.right() as f64;
                if hit {
                    return Some((*id, rect));
                }
            }
            SplitAxis::Vertical => {
                let divider_x = first_rect.right() as f64;
                let hit = (x - divider_x).abs() <= tolerance
                    && y >= rect.y as f64
                    && y <= rect.bottom() as f64;
                if hit {
                    return Some((*id, rect));
                }
            }
        }

        first
            .split_at_point(first_rect, config, x, y, tolerance)
            .or_else(|| second.split_at_point(second_rect, config, x, y, tolerance))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane() -> PaneId {
        PaneId::next()
    }

    #[test]
    fn split_creates_two_leaves_with_a_fresh_id() {
        let a = pane();
        let b = pane();
        let mut tree = SplitNode::pane(a);
        let split = tree.split_at(a, b, SplitAxis::Vertical).unwrap();
        assert!(tree.contains(a));
        assert!(tree.contains(b));
        assert_eq!(tree.pane_count(), 2);
        assert_eq!(split, split);
        match tree {
            SplitNode::Split {
                id, ratio, axis, ..
            } => {
                assert_eq!(id, split);
                assert!((ratio - 0.5).abs() < f32::EPSILON);
                assert_eq!(axis, SplitAxis::Vertical);
            }
            _ => panic!("expected a split node"),
        }
    }

    #[test]
    fn splitting_an_unknown_pane_is_refused() {
        let a = pane();
        let b = pane();
        let mut tree = SplitNode::pane(a);
        assert!(tree.split_at(b, pane(), SplitAxis::Vertical).is_none());
        assert_eq!(tree.pane_count(), 1);
    }

    #[test]
    fn a_leaf_that_survived_a_split_can_be_split_again() {
        let a = pane();
        let b = pane();
        let c = pane();
        let mut tree = SplitNode::pane(a);
        assert!(tree.split_at(a, b, SplitAxis::Vertical).is_some());
        assert!(tree.split_at(a, c, SplitAxis::Horizontal).is_some());
        assert_eq!(tree.pane_count(), 3);
    }

    #[test]
    fn nested_splits_are_supported() {
        let a = pane();
        let b = pane();
        let c = pane();
        let mut tree = SplitNode::pane(a);
        tree.split_at(a, b, SplitAxis::Vertical);
        assert!(tree.split_at(b, c, SplitAxis::Horizontal).is_some());
        assert_eq!(tree.pane_count(), 3);
    }

    #[test]
    fn removing_a_leaf_collapses_its_parent() {
        let a = pane();
        let b = pane();
        let mut tree = SplitNode::pane(a);
        tree.split_at(a, b, SplitAxis::Vertical);
        assert!(tree.remove_pane(b));
        assert_eq!(tree, SplitNode::Pane(a));
    }

    #[test]
    fn removing_a_nested_leaf_collapses_correctly() {
        let a = pane();
        let b = pane();
        let c = pane();
        let mut tree = SplitNode::pane(a);
        tree.split_at(a, b, SplitAxis::Vertical);
        tree.split_at(b, c, SplitAxis::Horizontal);
        assert!(tree.remove_pane(b));
        assert_eq!(tree.pane_count(), 2);
        assert!(tree.contains(a));
        assert!(tree.contains(c));
        assert!(!tree.contains(b));
    }

    #[test]
    fn removing_the_last_pane_reports_failure() {
        let a = pane();
        let mut tree = SplitNode::pane(a);
        assert!(!tree.remove_pane(a));
        assert_eq!(tree, SplitNode::Pane(a));
    }

    #[test]
    fn removing_an_unknown_pane_is_a_noop() {
        let a = pane();
        let b = pane();
        let mut tree = SplitNode::pane(a);
        tree.split_at(a, b, SplitAxis::Vertical);
        assert!(!tree.remove_pane(pane()));
        assert_eq!(tree.pane_count(), 2);
    }

    #[test]
    fn ratio_adjustment_is_clamped() {
        let a = pane();
        let b = pane();
        let mut tree = SplitNode::pane(a);
        let split = tree.split_at(a, b, SplitAxis::Vertical).unwrap();
        assert!(tree.adjust_ratio(split, 0.9));
        match tree {
            SplitNode::Split { ratio, .. } => assert!((ratio - MAX_RATIO).abs() < f32::EPSILON),
            _ => panic!(),
        }
        assert!(tree.adjust_ratio(split, -5.0));
        match tree {
            SplitNode::Split { ratio, .. } => assert!((ratio - MIN_RATIO).abs() < f32::EPSILON),
            _ => panic!(),
        }
    }

    #[test]
    fn unknown_split_id_is_ignored() {
        let a = pane();
        let b = pane();
        let mut tree = SplitNode::pane(a);
        tree.split_at(a, b, SplitAxis::Vertical);
        let other = {
            let mut other = SplitNode::pane(pane());
            other
                .split_at(
                    match other {
                        SplitNode::Pane(id) => id,
                        _ => unreachable!(),
                    },
                    pane(),
                    SplitAxis::Vertical,
                )
                .unwrap()
        };
        assert!(!tree.adjust_ratio(other, 0.1));
    }

    #[test]
    fn equalize_resets_every_ratio() {
        let a = pane();
        let b = pane();
        let c = pane();
        let mut tree = SplitNode::pane(a);
        let outer = tree.split_at(a, b, SplitAxis::Vertical).unwrap();
        let inner = tree.split_at(b, c, SplitAxis::Horizontal).unwrap();
        tree.adjust_ratio(outer, 0.3);
        tree.adjust_ratio(inner, -0.3);
        tree.equalize();
        let mut ratios = Vec::new();
        tree.visit_slots(&mut |node| {
            if let SplitNode::Split { ratio, .. } = node {
                ratios.push(*ratio);
            }
        });
        assert!(
            ratios
                .iter()
                .all(|ratio| (ratio - 0.5).abs() < f32::EPSILON)
        );
    }

    #[test]
    fn swap_panes_exchanges_leaf_positions() {
        let a = pane();
        let b = pane();
        let mut tree = SplitNode::pane(a);
        tree.split_at(a, b, SplitAxis::Vertical);
        assert!(tree.swap_panes(a, b));
        assert_eq!(tree.panes(), vec![b, a]);
        assert!(
            !tree.swap_panes(a, pane()),
            "unknown pane cannot be swapped"
        );
    }

    #[test]
    fn panes_are_reported_in_layout_order() {
        let a = pane();
        let b = pane();
        let c = pane();
        let mut tree = SplitNode::pane(a);
        tree.split_at(a, b, SplitAxis::Vertical);
        tree.split_at(b, c, SplitAxis::Horizontal);
        assert_eq!(tree.panes(), vec![a, b, c]);
    }
}
