//! Focus traversal across panes, computed from the pixel layout.
//!
//! Geometric traversal (rather than tree traversal) is what users expect: pressing
//! `Alt+Left` moves to the pane that is visually to the left, regardless of how
//! deeply nested it is in the split tree.

use astraterm_core::{FocusDirection, PaneId, Rect};

use crate::layout::Placement;

/// Returns the pane adjacent to `from` in `direction`, if any.
///
/// Candidates must be strictly in the requested direction on the primary axis.
/// Among them, the closest primary gap wins; ties are broken by the smallest
/// perpendicular offset, then by traversal order for determinism.
pub fn nearest_in_direction(
    placements: &[Placement],
    from: PaneId,
    direction: FocusDirection,
) -> Option<PaneId> {
    let origin = placements
        .iter()
        .find(|placement| placement.pane == from)?
        .rect;

    let mut best: Option<(i64, i64, PaneId)> = None;
    for candidate in placements {
        if candidate.pane == from {
            continue;
        }
        let rect = candidate.rect;
        let (primary, perpendicular) = match direction {
            FocusDirection::Left => {
                if rect.right() > origin.x {
                    continue;
                }
                (
                    (origin.x - rect.right()) as i64,
                    center_distance(origin, rect, true),
                )
            }
            FocusDirection::Right => {
                if rect.x < origin.right() {
                    continue;
                }
                (
                    (rect.x - origin.right()) as i64,
                    center_distance(origin, rect, true),
                )
            }
            FocusDirection::Up => {
                if rect.bottom() > origin.y {
                    continue;
                }
                (
                    (origin.y - rect.bottom()) as i64,
                    center_distance(origin, rect, false),
                )
            }
            FocusDirection::Down => {
                if rect.y < origin.bottom() {
                    continue;
                }
                (
                    (rect.y - origin.bottom()) as i64,
                    center_distance(origin, rect, false),
                )
            }
        };

        let score = (primary, perpendicular);
        best = match best {
            Some((best_primary, best_perpendicular, _))
                if (best_primary, best_perpendicular) <= score =>
            {
                best
            }
            _ => Some((score.0, score.1, candidate.pane)),
        };
    }

    best.map(|(_, _, pane)| pane)
}

/// Distance between the perpendicular centres of two rectangles.
fn center_distance(a: Rect, b: Rect, vertical_axis: bool) -> i64 {
    if vertical_axis {
        let a_center = a.y as i64 * 2 + a.height as i64;
        let b_center = b.y as i64 * 2 + b.height as i64;
        (a_center - b_center).abs()
    } else {
        let a_center = a.x as i64 * 2 + a.width as i64;
        let b_center = b.x as i64 * 2 + b.width as i64;
        (a_center - b_center).abs()
    }
}

/// Cycles to the next pane in traversal order, wrapping around.
pub fn next_pane(placements: &[Placement], from: PaneId) -> Option<PaneId> {
    cycle_pane(placements, from, true)
}

/// Cycles to the previous pane in traversal order, wrapping around.
pub fn previous_pane(placements: &[Placement], from: PaneId) -> Option<PaneId> {
    cycle_pane(placements, from, false)
}

fn cycle_pane(placements: &[Placement], from: PaneId, forward: bool) -> Option<PaneId> {
    if placements.len() < 2 {
        return None;
    }
    let index = placements
        .iter()
        .position(|placement| placement.pane == from)?;
    let len = placements.len();
    let next = if forward {
        (index + 1) % len
    } else {
        (index + len - 1) % len
    };
    Some(placements[next].pane)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn placements() -> Vec<Placement> {
        // Layout:
        //   +--------+--------+
        //   |   a    |   b    |
        //   +--------+--------+
        //   |        c        |
        //   +-----------------+
        let a = PaneId::from_raw(1);
        let b = PaneId::from_raw(2);
        let c = PaneId::from_raw(3);
        vec![
            Placement {
                pane: a,
                rect: Rect::new(0, 0, 400, 300),
            },
            Placement {
                pane: b,
                rect: Rect::new(400, 0, 400, 300),
            },
            Placement {
                pane: c,
                rect: Rect::new(0, 300, 800, 300),
            },
        ]
    }

    #[test]
    fn moves_right_between_siblings() {
        let layout = placements();
        assert_eq!(
            nearest_in_direction(&layout, PaneId::from_raw(1), FocusDirection::Right),
            Some(PaneId::from_raw(2))
        );
    }

    #[test]
    fn moves_down_to_the_full_width_pane() {
        let layout = placements();
        assert_eq!(
            nearest_in_direction(&layout, PaneId::from_raw(1), FocusDirection::Down),
            Some(PaneId::from_raw(3))
        );
        assert_eq!(
            nearest_in_direction(&layout, PaneId::from_raw(2), FocusDirection::Down),
            Some(PaneId::from_raw(3))
        );
    }

    #[test]
    fn moves_up_from_the_full_width_pane() {
        let layout = placements();
        let up = nearest_in_direction(&layout, PaneId::from_raw(3), FocusDirection::Up);
        assert_eq!(
            up,
            Some(PaneId::from_raw(1)),
            "centred panes pick the left one deterministically"
        );
    }

    #[test]
    fn returns_none_at_the_edges() {
        let layout = placements();
        assert_eq!(
            nearest_in_direction(&layout, PaneId::from_raw(1), FocusDirection::Left),
            None
        );
        assert_eq!(
            nearest_in_direction(&layout, PaneId::from_raw(1), FocusDirection::Up),
            None
        );
        assert_eq!(
            nearest_in_direction(&layout, PaneId::from_raw(3), FocusDirection::Down),
            None
        );
    }

    #[test]
    fn unknown_pane_has_no_neighbour() {
        let layout = placements();
        assert_eq!(
            nearest_in_direction(&layout, PaneId::from_raw(99), FocusDirection::Right),
            None
        );
    }

    #[test]
    fn cycling_wraps_in_both_directions() {
        let layout = placements();
        assert_eq!(
            next_pane(&layout, PaneId::from_raw(3)),
            Some(PaneId::from_raw(1))
        );
        assert_eq!(
            previous_pane(&layout, PaneId::from_raw(1)),
            Some(PaneId::from_raw(3))
        );
    }

    #[test]
    fn cycling_a_single_pane_is_a_noop() {
        let layout = vec![Placement {
            pane: PaneId::from_raw(1),
            rect: Rect::new(0, 0, 10, 10),
        }];
        assert_eq!(next_pane(&layout, PaneId::from_raw(1)), None);
    }
}
