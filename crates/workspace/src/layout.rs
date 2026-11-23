//! Layout geometry: converts a split tree plus a rectangle into pixel rectangles.
//!
//! Rounding is handled here so that pane edges always tile the available area
//! exactly: there are no one-pixel gaps or overlaps, which would otherwise show up
//! as flickering seams in the renderer.

use astraterm_core::{PaneId, Rect, SplitAxis};

use crate::{LayoutConfig, tree::SplitNode};

/// Computes the two child rectangles of a split node inside `rect`.
///
/// Returns `None` when `node` is a leaf or when the rectangle is too small to
/// split, in which case callers should render the node as a single region.
pub fn split_children(rect: Rect, node: &SplitNode, config: LayoutConfig) -> Option<(Rect, Rect)> {
    let SplitNode::Split { axis, ratio, .. } = node else {
        return None;
    };
    let (axis, ratio) = (*axis, *ratio);
    match axis {
        SplitAxis::Vertical => {
            if rect.width <= config.divider + 1 {
                return None;
            }
            let available = rect.width - config.divider;
            let (first_w, second_w) = split_extent(available, ratio, config.min_pane_width);
            Some((
                Rect::new(rect.x, rect.y, first_w, rect.height),
                Rect::new(
                    rect.x + first_w as i32 + config.divider as i32,
                    rect.y,
                    second_w,
                    rect.height,
                ),
            ))
        }
        SplitAxis::Horizontal => {
            if rect.height <= config.divider + 1 {
                return None;
            }
            let available = rect.height - config.divider;
            let (first_h, second_h) = split_extent(available, ratio, config.min_pane_height);
            Some((
                Rect::new(rect.x, rect.y, rect.width, first_h),
                Rect::new(
                    rect.x,
                    rect.y + first_h as i32 + config.divider as i32,
                    rect.width,
                    second_h,
                ),
            ))
        }
    }
}

/// Splits a pixel extent into two parts that sum exactly to `available`.
///
/// The requested ratio is honoured unless it would make one side smaller than
/// `min`; then the split is clamped, and if the extent cannot hold two minimums
/// the split becomes even.
pub fn split_extent(available: u32, ratio: f32, min: u32) -> (u32, u32) {
    if available == 0 {
        return (0, 0);
    }
    let requested = (available as f32 * ratio.clamp(0.0, 1.0)).round() as u32;
    let min = min.min(available / 2);
    let first = requested.clamp(min, available - min);
    (first, available - first)
}

/// One pane placed on screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placement {
    pub pane: PaneId,
    pub rect: Rect,
}

/// Computes every pane rectangle for a tab layout.
///
/// `rect` is the terminal area, already excluding the tab strip, status bar and
/// window padding.
pub fn compute_layout(root: &SplitNode, rect: Rect, config: LayoutConfig) -> Vec<Placement> {
    let mut out = Vec::new();
    place(root, rect, config, &mut out);
    out
}

fn place(node: &SplitNode, rect: Rect, config: LayoutConfig, out: &mut Vec<Placement>) {
    match node {
        SplitNode::Pane(id) => {
            let inset = config.gap / 2;
            out.push(Placement {
                pane: *id,
                rect: rect.shrunk(inset),
            });
        }
        SplitNode::Split { .. } => {
            let Some((first, second)) = split_children(rect, node, config) else {
                // Degenerate rectangle: keep the first pane visible.
                if let SplitNode::Split { first, .. } = node {
                    place(first, rect, config, out);
                }
                return;
            };
            if let SplitNode::Split {
                first: first_node,
                second: second_node,
                ..
            } = node
            {
                place(first_node, first, config, out);
                place(second_node, second, config, out);
            }
        }
    }
}

/// Returns the divider rectangle for every split in the tree, for pointer hit
/// testing and for drawing.
pub fn compute_dividers(
    root: &SplitNode,
    rect: Rect,
    config: LayoutConfig,
) -> Vec<(crate::tree::SplitId, astraterm_core::SplitAxis, Rect)> {
    let mut out = Vec::new();
    collect_dividers(root, rect, config, &mut out);
    out
}

fn collect_dividers(
    node: &SplitNode,
    rect: Rect,
    config: LayoutConfig,
    out: &mut Vec<(crate::tree::SplitId, astraterm_core::SplitAxis, Rect)>,
) {
    let SplitNode::Split {
        id,
        axis,
        first,
        second,
        ..
    } = node
    else {
        return;
    };
    let Some((first_rect, second_rect)) = split_children(rect, node, config) else {
        return;
    };
    let divider = match axis {
        SplitAxis::Vertical => Rect::new(
            first_rect.right(),
            rect.y,
            config.divider.max(1),
            rect.height,
        ),
        SplitAxis::Horizontal => Rect::new(
            rect.x,
            first_rect.bottom(),
            rect.width,
            config.divider.max(1),
        ),
    };
    out.push((*id, *axis, divider));
    collect_dividers(first, first_rect, config, out);
    collect_dividers(second, second_rect, config, out);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> LayoutConfig {
        LayoutConfig {
            divider: 2,
            gap: 0,
            min_pane_width: 100,
            min_pane_height: 60,
        }
    }

    #[test]
    fn split_extent_tiles_exactly() {
        for ratio in [0.1, 0.33, 0.5, 0.66, 0.9] {
            let (a, b) = split_extent(1000, ratio, 50);
            assert_eq!(a + b, 1000, "ratio {ratio} must tile the extent");
        }
    }

    #[test]
    fn split_extent_honours_minimum_size() {
        let (first, second) = split_extent(1000, 0.05, 200);
        assert_eq!(first, 200);
        assert_eq!(second, 800);
    }

    #[test]
    fn split_extent_falls_back_to_even_split_when_too_small() {
        let (first, second) = split_extent(150, 0.5, 500);
        assert_eq!(first, 75);
        assert_eq!(second, 75);
    }

    #[test]
    fn single_pane_fills_the_whole_rect() {
        let pane = PaneId::next();
        let tree = SplitNode::pane(pane);
        let rect = Rect::new(0, 0, 800, 600);
        let layout = compute_layout(&tree, rect, config());
        assert_eq!(layout.len(), 1);
        assert_eq!(layout[0].rect, rect);
    }

    #[test]
    fn vertical_split_produces_side_by_side_panes() {
        let a = PaneId::next();
        let b = PaneId::next();
        let mut tree = SplitNode::pane(a);
        tree.split_at(a, b, SplitAxis::Vertical);
        let layout = compute_layout(&tree, Rect::new(0, 0, 802, 600), config());
        assert_eq!(layout.len(), 2);
        assert_eq!(layout[0].rect.width, 400);
        assert_eq!(layout[1].rect.x, 402);
        assert_eq!(layout[0].rect.y, layout[1].rect.y);
        assert_eq!(layout[0].rect.right() + 2, layout[1].rect.x);
    }

    #[test]
    fn horizontal_split_stacks_panes() {
        let a = PaneId::next();
        let b = PaneId::next();
        let mut tree = SplitNode::pane(a);
        tree.split_at(a, b, SplitAxis::Horizontal);
        let layout = compute_layout(&tree, Rect::new(0, 0, 800, 602), config());
        assert_eq!(layout[0].rect.height, 300);
        assert_eq!(layout[1].rect.y, 302);
    }

    #[test]
    fn nested_layout_covers_the_area_without_overlap() {
        let a = PaneId::next();
        let b = PaneId::next();
        let c = PaneId::next();
        let mut tree = SplitNode::pane(a);
        tree.split_at(a, b, SplitAxis::Vertical);
        tree.split_at(b, c, SplitAxis::Horizontal);
        let rect = Rect::new(10, 20, 1002, 602);
        let layout = compute_layout(&tree, rect, config());
        assert_eq!(layout.len(), 3);
        for pair in layout.iter().enumerate() {
            for other in layout.iter().skip(pair.0 + 1) {
                let overlap_x =
                    pair.1.rect.x < other.rect.right() && other.rect.x < pair.1.rect.right();
                let overlap_y =
                    pair.1.rect.y < other.rect.bottom() && other.rect.y < pair.1.rect.bottom();
                assert!(!(overlap_x && overlap_y), "panes must not overlap");
            }
        }
        for placement in &layout {
            assert!(placement.rect.x >= rect.x);
            assert!(placement.rect.right() <= rect.right());
            assert!(placement.rect.bottom() <= rect.bottom());
        }
    }

    #[test]
    fn gap_insets_each_pane() {
        let a = PaneId::next();
        let b = PaneId::next();
        let mut tree = SplitNode::pane(a);
        tree.split_at(a, b, SplitAxis::Vertical);
        let config = LayoutConfig {
            divider: 1,
            gap: 8,
            min_pane_width: 100,
            min_pane_height: 60,
        };
        let layout = compute_layout(&tree, Rect::new(0, 0, 801, 600), config);
        assert_eq!(layout[0].rect.x, 4);
        assert_eq!(
            layout[0].rect.width,
            400 - 8,
            "half the gap is removed on each side"
        );
    }

    #[test]
    fn degenerate_rect_does_not_panic() {
        let a = PaneId::next();
        let b = PaneId::next();
        let mut tree = SplitNode::pane(a);
        tree.split_at(a, b, SplitAxis::Vertical);
        let layout = compute_layout(&tree, Rect::new(0, 0, 1, 1), config());
        assert!(!layout.is_empty());
    }

    #[test]
    fn dividers_are_reported_for_every_split() {
        let a = PaneId::next();
        let b = PaneId::next();
        let c = PaneId::next();
        let mut tree = SplitNode::pane(a);
        tree.split_at(a, b, SplitAxis::Vertical);
        tree.split_at(b, c, SplitAxis::Horizontal);
        let dividers = compute_dividers(&tree, Rect::new(0, 0, 800, 600), config());
        assert_eq!(dividers.len(), 2);
        assert!(
            dividers
                .iter()
                .any(|(_, axis, _)| *axis == SplitAxis::Vertical)
        );
        assert!(
            dividers
                .iter()
                .any(|(_, axis, _)| *axis == SplitAxis::Horizontal)
        );
    }

    #[test]
    fn split_at_point_finds_the_divider() {
        let a = PaneId::next();
        let b = PaneId::next();
        let mut tree = SplitNode::pane(a);
        let split = tree.split_at(a, b, SplitAxis::Vertical).unwrap();
        let rect = Rect::new(0, 0, 802, 600);
        let found = tree.split_at_point(rect, config(), 400.0, 300.0, 4.0);
        assert_eq!(found.map(|(id, _)| id), Some(split));
        assert!(
            tree.split_at_point(rect, config(), 100.0, 300.0, 4.0)
                .is_none()
        );
    }
}
