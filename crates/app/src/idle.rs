//! The idle frame budget: `[performance] max_fps_when_idle`, honoured (S10).
//!
//! The event loop is event-driven: output, a keystroke, a click or a resize asks for
//! a frame and gets one immediately. What is left is *idle* work — the status bar's
//! readings and the cursor blink — which nothing outside this module paces. The
//! budget caps how many frames that idle work may draw per second.
//!
//! Two rules make it honest:
//!
//! * a frame an event asked for is never delayed. [`IdleFrame::Draw`] is returned for
//!   any pending change regardless of the budget, because delaying output would be
//!   felt as a slow terminal;
//! * the cursor blink is *scheduled* against the budget instead of being deferred.
//!   Deferring a toggle would allow two toggles to collapse into one drawn frame and
//!   leave the cursor looking frozen, so [`blink_period`] stretches the blink's own
//!   period to at least the idle interval. At the default `1` fps that is a one-second
//!   toggle — still a blink — and above 2 fps the configured period is untouched.
//!
//! Everything is a decision on numbers, so the whole rule is testable without
//! sleeping: [`idle_frame`] takes the clock reading as an argument.

use std::time::{Duration, Instant};

/// Shortest gap between two frames the idle work may draw.
///
/// `None` means `max_fps_when_idle = 0`, which the configuration documents as "no
/// cap"; the schema rejects values above 240, so the interval never drops below
/// 4.1 ms.
pub fn interval(max_fps_when_idle: u32) -> Option<Duration> {
    (max_fps_when_idle > 0).then(|| Duration::from_micros(1_000_000 / max_fps_when_idle as u64))
}

/// What to do with the idle work that is due now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdleFrame {
    /// Draw now.
    Draw,
    /// Wait this long: the last frame is too recent for the budget.
    Wait(Duration),
}

/// Whether an idle frame may be drawn at `now`.
///
/// `last_draw` is when the previous frame reached the screen, or `None` before the
/// first one. `pending` is whether output, input or a resize changed something: that
/// is always drawn, whatever the budget says, so the cap can never make the terminal
/// feel laggy.
pub fn idle_frame(
    now: Instant,
    last_draw: Option<Instant>,
    max_fps_when_idle: u32,
    pending: bool,
) -> IdleFrame {
    if pending {
        return IdleFrame::Draw;
    }
    let Some(interval) = interval(max_fps_when_idle) else {
        // `0` is documented as no cap at all.
        return IdleFrame::Draw;
    };
    let Some(last_draw) = last_draw else {
        // Nothing has been drawn yet: the first frame is never a repeat.
        return IdleFrame::Draw;
    };
    let due = last_draw + interval;
    if now >= due {
        IdleFrame::Draw
    } else {
        IdleFrame::Wait(due - now)
    }
}

/// Period the cursor blink runs at under the idle budget.
///
/// The blink is the one redraw source that is pure idle work, so it is paced by
/// stretching its own period rather than by dropping toggles: the cursor keeps
/// blinking at `period`, or at the idle interval when that is longer.
pub fn blink_period(period: Duration, max_fps_when_idle: u32) -> Duration {
    match interval(max_fps_when_idle) {
        Some(interval) => period.max(interval),
        None => period,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BLINK: Duration = Duration::from_millis(530);
    const SECOND: Duration = Duration::from_secs(1);

    fn now() -> Instant {
        Instant::now()
    }

    #[test]
    fn the_interval_follows_the_configured_rate() {
        assert_eq!(interval(1), Some(SECOND));
        assert_eq!(interval(4), Some(Duration::from_millis(250)));
        assert_eq!(interval(240), Some(Duration::from_micros(4_166)));
        assert_eq!(
            interval(0),
            None,
            "0 is documented as no cap, not as no frames"
        );
    }

    #[test]
    fn with_the_cap_off_nothing_is_deferred() {
        let now = now();
        assert_eq!(
            idle_frame(now, Some(now), 0, false),
            IdleFrame::Draw,
            "0 means the terminal draws idle work as it always did"
        );
    }

    #[test]
    fn the_first_idle_frame_is_always_drawn() {
        assert_eq!(
            idle_frame(now(), None, 1, false),
            IdleFrame::Draw,
            "there is no previous frame to be too close to"
        );
    }

    #[test]
    fn an_event_driven_redraw_is_never_delayed_by_the_budget() {
        let now = now();
        // A frame was drawn this very microsecond and 1 fps is configured: output
        // still gets its frame now, because delaying it is what a terminal must not do.
        assert_eq!(idle_frame(now, Some(now), 1, true), IdleFrame::Draw);
        assert_eq!(
            idle_frame(now, Some(now), 240, true),
            IdleFrame::Draw,
            "the cap applies to idle work only, at every rate"
        );
    }

    #[test]
    fn an_idle_redraw_waits_until_the_budget_allows_it() {
        let now = now();
        let drawn = now - Duration::from_millis(250);
        match idle_frame(now, Some(drawn), 1, false) {
            IdleFrame::Wait(wait) => {
                assert_eq!(wait, SECOND - Duration::from_millis(250));
            }
            other => panic!("1 fps must wait after a frame 250 ms ago, got {other:?}"),
        }
        assert_eq!(
            idle_frame(now, Some(now - SECOND), 1, false),
            IdleFrame::Draw,
            "a full interval has passed"
        );
        assert_eq!(
            idle_frame(now, Some(now - Duration::from_secs(30)), 1, false),
            IdleFrame::Draw,
            "a long stall is not repaid with a burst of frames"
        );
    }

    #[test]
    fn a_higher_budget_waits_for_a_shorter_time() {
        let now = now();
        let drawn = now - Duration::from_millis(10);
        assert_eq!(
            idle_frame(now, Some(drawn), 100, false),
            IdleFrame::Draw,
            "10 ms after a frame at 100 fps the budget is already satisfied"
        );
        assert_eq!(
            idle_frame(now, Some(drawn), 50, false),
            IdleFrame::Wait(Duration::from_millis(10))
        );
    }

    #[test]
    fn the_blink_keeps_a_rate_that_still_looks_like_a_blink() {
        assert_eq!(
            blink_period(BLINK, 0),
            BLINK,
            "no cap: the configured blink is untouched"
        );
        assert_eq!(
            blink_period(BLINK, 240),
            BLINK,
            "a budget faster than the blink cannot slow it down"
        );
        let slowest = blink_period(BLINK, 1);
        assert_eq!(slowest, SECOND);
        assert!(
            slowest <= Duration::from_secs(2),
            "one toggle per second is a 2 s period, which is still a blink"
        );
        assert!(
            slowest >= BLINK,
            "the budget may only ever stretch the blink, never shorten it"
        );
    }
}
