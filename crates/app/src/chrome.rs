//! Window chrome geometry and status-bar content.
//!
//! Kept as free functions so layout rules (which strip goes where, how hit testing
//! works, how a label is cut) are unit-testable without a window, a GPU or a font.

use astraterm_config::schema::TabBarPosition;
use astraterm_core::{Rect, Size};
use astraterm_platform::system::SystemSnapshot;

/// Rectangle of the tab strip for the configured position.
pub fn strip_rect(
    position: TabBarPosition,
    size: Size,
    tab_height: u32,
    strip_width: u32,
    status_height: u32,
) -> Rect {
    match position {
        TabBarPosition::Hidden => Rect::new(0, 0, 0, 0),
        TabBarPosition::Top => Rect::new(0, 0, size.width, tab_height),
        TabBarPosition::Bottom => Rect::new(
            0,
            size.height.saturating_sub(tab_height) as i32,
            size.width,
            tab_height,
        ),
        TabBarPosition::Right => Rect::new(
            size.width.saturating_sub(strip_width) as i32,
            0,
            strip_width,
            size.height.saturating_sub(status_height),
        ),
    }
}

/// Rectangle available to the terminal panes.
pub fn terminal_rect(
    position: TabBarPosition,
    size: Size,
    tab_height: u32,
    strip_width: u32,
    status_height: u32,
) -> Rect {
    // Top and bottom strips both take height; only a hidden or right-side strip does
    // not.
    let horizontal = match position {
        TabBarPosition::Top | TabBarPosition::Bottom => tab_height,
        _ => 0,
    };
    let strip = match position {
        TabBarPosition::Right => strip_width,
        _ => 0,
    };
    let y = match position {
        TabBarPosition::Bottom => 0,
        _ => horizontal as i32,
    };
    Rect::new(
        0,
        y,
        size.width.saturating_sub(strip),
        size.height.saturating_sub(horizontal + status_height),
    )
}

/// Rectangle of the status bar, or an empty rectangle when it is hidden.
pub fn status_rect(visible: bool, size: Size, height: u32) -> Rect {
    if !visible {
        return Rect::new(0, 0, 0, 0);
    }
    Rect::new(
        0,
        size.height.saturating_sub(height) as i32,
        size.width,
        height,
    )
}

/// Hit test for the vertical tab strip.
pub fn vertical_tab_hit(strip: Rect, row_height: u32, count: usize, y: f64) -> Option<usize> {
    if !strip.contains(strip.x as f64, y) || row_height == 0 {
        return None;
    }
    let offset = (y - strip.y as f64).max(0.0);
    let index = (offset / row_height as f64).floor() as usize;
    (index < count).then_some(index)
}

/// Hit test for a horizontal tab strip, given each tab's width.
pub fn horizontal_tab_hit(strip: Rect, widths: &[u32], x: f64) -> Option<usize> {
    let mut offset = strip.x as f64 + 8.0;
    for (index, width) in widths.iter().enumerate() {
        if x >= offset && x < offset + *width as f64 {
            return Some(index);
        }
        offset += *width as f64;
    }
    None
}

/// Cuts a label so it fits into `max_width` device pixels, appending an ellipsis.
pub fn fit_label(label: &str, max_width: u32, cell_width: f32) -> String {
    let cell = cell_width.max(1.0);
    let capacity = (max_width as f32 / cell).floor().max(1.0) as usize;
    let count = label.chars().count();
    if count <= capacity {
        return label.to_owned();
    }
    if capacity <= 1 {
        return "…".to_owned();
    }
    let mut out: String = label.chars().take(capacity - 1).collect();
    out.push('…');
    out
}

/// Right-hand status bar text: CPU, RAM and clock segments.
pub fn status_segments(
    cpu: bool,
    ram: bool,
    clock: bool,
    clock_format: &str,
    system: &SystemSnapshot,
) -> String {
    let mut segments: Vec<String> = Vec::new();
    if cpu {
        segments.push(match system.cpu_percent {
            Some(percent) => format!("CPU {:>3.0}%", percent),
            None => "CPU  --".to_owned(),
        });
    }
    if ram && let Some(percent) = system.ram_percent {
        segments.push(format!("RAM {:>3.0}%", percent));
    }
    if clock && let Some(epoch) = system.clock_epoch {
        segments.push(astraterm_platform::format_clock(epoch, clock_format));
    }
    segments.join("  ·  ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn size() -> Size {
        Size::new(1200, 800)
    }

    #[test]
    fn top_strip_takes_the_full_width_and_pushes_the_terminal_down() {
        let strip = strip_rect(TabBarPosition::Top, size(), 20, 180, 18);
        assert_eq!(strip, Rect::new(0, 0, 1200, 20));
        let terminal = terminal_rect(TabBarPosition::Top, size(), 20, 180, 18);
        assert_eq!(terminal, Rect::new(0, 20, 1200, 762));
    }

    #[test]
    fn right_strip_takes_a_column_and_shrinks_the_terminal_width() {
        let strip = strip_rect(TabBarPosition::Right, size(), 20, 180, 18);
        assert_eq!(
            strip,
            Rect::new(1020, 0, 180, 782),
            "the strip stops above the status bar"
        );
        let terminal = terminal_rect(TabBarPosition::Right, size(), 20, 180, 18);
        assert_eq!(terminal, Rect::new(0, 0, 1020, 782));
    }

    #[test]
    fn bottom_strip_sits_at_the_bottom_and_keeps_the_terminal_at_the_top() {
        let strip = strip_rect(TabBarPosition::Bottom, size(), 20, 180, 18);
        assert_eq!(strip, Rect::new(0, 780, 1200, 20));
        let terminal = terminal_rect(TabBarPosition::Bottom, size(), 20, 180, 18);
        assert_eq!(terminal.y, 0);
        assert_eq!(terminal.height, 762);
    }

    #[test]
    fn hidden_strip_leaves_everything_to_the_terminal() {
        let strip = strip_rect(TabBarPosition::Hidden, size(), 20, 180, 18);
        assert!(strip.is_empty());
        let terminal = terminal_rect(TabBarPosition::Hidden, size(), 20, 180, 18);
        assert_eq!(terminal, Rect::new(0, 0, 1200, 782));
    }

    #[test]
    fn a_narrow_window_never_produces_a_negative_rectangle() {
        let tiny = Size::new(100, 50);
        let terminal = terminal_rect(TabBarPosition::Right, tiny, 20, 400, 18);
        assert!(terminal.width <= tiny.width);
        assert!(terminal.height <= tiny.height);
        let strip = strip_rect(TabBarPosition::Right, tiny, 20, 400, 18);
        assert!(strip.x >= 0);
    }

    #[test]
    fn status_bar_rect_follows_its_visibility() {
        assert!(status_rect(false, size(), 18).is_empty());
        assert_eq!(status_rect(true, size(), 18), Rect::new(0, 782, 1200, 18));
    }

    #[test]
    fn vertical_hit_testing_maps_rows_to_tabs() {
        let strip = Rect::new(1000, 0, 200, 600);
        assert_eq!(vertical_tab_hit(strip, 24, 3, 0.0), Some(0));
        assert_eq!(vertical_tab_hit(strip, 24, 3, 23.9), Some(0));
        assert_eq!(vertical_tab_hit(strip, 24, 3, 24.0), Some(1));
        assert_eq!(vertical_tab_hit(strip, 24, 3, 60.0), Some(2));
        assert_eq!(
            vertical_tab_hit(strip, 24, 3, 500.0),
            None,
            "past the last tab"
        );
    }

    #[test]
    fn vertical_hit_testing_ignores_a_zero_row_height() {
        let strip = Rect::new(1000, 0, 200, 600);
        assert_eq!(vertical_tab_hit(strip, 0, 3, 10.0), None);
    }

    #[test]
    fn horizontal_hit_testing_walks_the_widths() {
        let strip = Rect::new(0, 0, 1200, 20);
        let widths = [120, 80, 200];
        assert_eq!(horizontal_tab_hit(strip, &widths, 8.0), Some(0));
        assert_eq!(horizontal_tab_hit(strip, &widths, 127.0), Some(0));
        assert_eq!(horizontal_tab_hit(strip, &widths, 130.0), Some(1));
        assert_eq!(horizontal_tab_hit(strip, &widths, 215.0), Some(2));
        assert_eq!(horizontal_tab_hit(strip, &widths, 500.0), None);
    }

    #[test]
    fn labels_fit_or_get_an_ellipsis() {
        assert_eq!(fit_label("short", 100, 8.0), "short");
        // 80 px / 8 px per cell = 10 glyphs of room: nine characters plus the ellipsis.
        assert_eq!(fit_label("a-very-long-tab-title", 80, 8.0), "a-very-lo…");
        assert_eq!(fit_label("abc", 4, 8.0), "…", "no room for a single glyph");
        assert_eq!(
            fit_label("ñandú", 80, 8.0),
            "ñandú",
            "counting characters, not bytes"
        );
    }

    #[test]
    fn status_segments_respect_the_enabled_parts() {
        let system = SystemSnapshot {
            cpu_percent: Some(12.4),
            ram_percent: Some(41.6),
            clock_epoch: Some(1_700_000_000),
        };
        let all = status_segments(true, true, true, "%H:%M", &system);
        assert!(all.contains("CPU  12%"), "{all}");
        assert!(all.contains("RAM  42%"), "{all}");
        assert_eq!(all.matches('·').count(), 2);
        assert_eq!(
            status_segments(true, false, false, "%H:%M", &system),
            "CPU  12%"
        );
        assert_eq!(status_segments(false, false, false, "%H:%M", &system), "");
    }

    #[test]
    fn status_segments_show_a_placeholder_before_the_first_cpu_sample() {
        let system = SystemSnapshot {
            cpu_percent: None,
            ram_percent: None,
            clock_epoch: None,
        };
        assert_eq!(
            status_segments(true, true, true, "%H:%M", &system),
            "CPU  --"
        );
    }
}
