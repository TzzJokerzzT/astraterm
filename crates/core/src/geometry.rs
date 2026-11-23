//! Geometry primitives shared by the workspace (layout math) and the renderer.
//!
//! All values are physical pixels unless a function name says otherwise. The
//! workspace computes pane rectangles in device pixels so that layout math stays
//! unit-testable without a window or a GPU.

/// A rectangle in device pixels, with the origin at the top-left of the surface.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    pub const fn is_empty(self) -> bool {
        self.width == 0 || self.height == 0
    }

    pub const fn right(self) -> i32 {
        self.x + self.width as i32
    }

    pub const fn bottom(self) -> i32 {
        self.y + self.height as i32
    }

    /// Shrinks the rectangle by `amount` on every side, clamping at zero.
    pub fn shrunk(self, amount: u32) -> Self {
        let dx = amount.min(self.width / 2);
        let dy = amount.min(self.height / 2);
        Self {
            x: self.x + dx as i32,
            y: self.y + dy as i32,
            width: self.width - 2 * dx,
            height: self.height - 2 * dy,
        }
    }

    /// Returns `true` when `(px, py)` is inside the rectangle.
    pub fn contains(self, px: f64, py: f64) -> bool {
        px >= self.x as f64
            && py >= self.y as f64
            && px < self.right() as f64
            && py < self.bottom() as f64
    }

    /// Converts a point inside the rectangle into a zero-based cell coordinate.
    pub fn to_cell(self, px: f64, py: f64, cell: CellMetrics) -> (i32, i32) {
        let col = ((px - self.x as f64) / cell.width.max(1.0) as f64).floor() as i32;
        let row = ((py - self.y as f64) / cell.height.max(1) as f64).floor() as i32;
        (col, row)
    }
}

/// A size in device pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Size {
    pub width: u32,
    pub height: u32,
}

impl Size {
    pub const fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }
}

/// Font-derived cell metrics, in device pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CellMetrics {
    /// Advance width of a single terminal cell.
    pub width: f32,
    /// Line height of a single terminal row.
    pub height: u32,
    /// Baseline offset from the top of the row, in device pixels.
    pub baseline: f32,
}

impl Default for CellMetrics {
    fn default() -> Self {
        Self {
            width: 8.0,
            height: 16,
            baseline: 12.0,
        }
    }
}

impl CellMetrics {
    /// Number of whole columns and rows that fit into `rect`.
    ///
    /// Always returns at least one column and one row so that a terminal is
    /// never asked to resize to a zero-sized grid.
    pub fn grid_size(self, rect: Rect) -> (u16, u16) {
        let cols = (rect.width as f32 / self.width.max(1.0)).floor().max(1.0) as u32;
        let rows = (rect.height as f32 / self.height.max(1) as f32)
            .floor()
            .max(1.0) as u32;
        (
            cols.clamp(1, u16::MAX as u32) as u16,
            rows.clamp(1, u16::MAX as u32) as u16,
        )
    }
}

/// Terminal padding in device pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Padding {
    pub x: u32,
    pub y: u32,
}

/// Orientation of a split between two panes.
///
/// `Horizontal` splits along the horizontal axis, producing two panes stacked
/// vertically (one above the other); `Vertical` produces two side-by-side panes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SplitAxis {
    Horizontal,
    Vertical,
}

impl SplitAxis {
    /// The opposite axis, used by rotate/swap style commands.
    pub fn flipped(self) -> Self {
        match self {
            SplitAxis::Horizontal => SplitAxis::Vertical,
            SplitAxis::Vertical => SplitAxis::Horizontal,
        }
    }

    /// Stable string form used in configuration and workspace metadata.
    pub fn as_str(self) -> &'static str {
        match self {
            SplitAxis::Horizontal => "horizontal",
            SplitAxis::Vertical => "vertical",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shrinking_never_produces_negative_dimensions() {
        let rect = Rect::new(10, 10, 4, 2).shrunk(8);
        assert_eq!(rect.width, 0);
        assert_eq!(rect.height, 0);
    }

    #[test]
    fn contains_uses_half_open_bounds() {
        let rect = Rect::new(0, 0, 10, 10);
        assert!(rect.contains(0.0, 0.0));
        assert!(rect.contains(9.9, 9.9));
        assert!(!rect.contains(10.0, 5.0));
        assert!(!rect.contains(5.0, 10.0));
    }

    #[test]
    fn grid_size_fits_whole_cells_only() {
        let metrics = CellMetrics {
            width: 10.0,
            height: 20,
            baseline: 14.0,
        };
        let rect = Rect::new(0, 0, 105, 45);
        assert_eq!(metrics.grid_size(rect), (10, 2));
    }

    #[test]
    fn grid_size_is_never_zero() {
        let metrics = CellMetrics {
            width: 10.0,
            height: 20,
            baseline: 14.0,
        };
        assert_eq!(metrics.grid_size(Rect::new(0, 0, 0, 0)), (1, 1));
    }

    #[test]
    fn to_cell_maps_points_to_zero_based_cells() {
        let metrics = CellMetrics {
            width: 10.0,
            height: 20,
            baseline: 14.0,
        };
        let rect = Rect::new(100, 50, 200, 200);
        assert_eq!(rect.to_cell(100.0, 50.0, metrics), (0, 0));
        assert_eq!(rect.to_cell(119.0, 69.0, metrics), (1, 0));
        assert_eq!(rect.to_cell(125.0, 75.0, metrics), (2, 1));
    }
}
