//! Scrollback search.
//!
//! Search walks the buffer line by line through the engine, so it covers both the
//! visible screen and the scrollback. Matching is plain substring matching with
//! optional case folding; regular expressions are intentionally not supported in
//! this milestone (documented in `docs/TERMINAL_COMPATIBILITY.md`).

use alacritty_terminal::index::Line;

use crate::engine::TerminalEngine;

/// Direction of search.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchDirection {
    Forward,
    Backward,
}

/// One match, in buffer coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SearchHit {
    pub line: i32,
    pub column: usize,
    /// Number of columns the match covers.
    pub length: usize,
}

/// Interactive search state kept per pane.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SearchState {
    pub query: String,
    pub case_sensitive: bool,
    pub current: Option<SearchHit>,
    /// 1-based index and total, for the status line (`3/17`).
    pub match_count: usize,
}

impl SearchState {
    pub fn is_active(&self) -> bool {
        !self.query.is_empty()
    }

    pub fn push_char(&mut self, ch: char) {
        self.query.push(ch);
        self.current = None;
    }

    pub fn pop_char(&mut self) {
        self.query.pop();
        self.current = None;
    }

    pub fn clear(&mut self) {
        self.query.clear();
        self.current = None;
        self.match_count = 0;
    }

    /// Human readable position indicator.
    pub fn indicator(&self) -> String {
        match self.current {
            Some(hit) if self.match_count > 0 => {
                format!("{} match(es), line {}", self.match_count, hit.line)
            }
            _ if self.query.is_empty() => "search".to_owned(),
            _ => format!("no match for `{}`", self.query),
        }
    }
}

/// Finds the next match starting at `from`, wrapping around once.
///
/// `from` is a buffer coordinate `(line, column)`. For forward search the match may
/// begin at `from.column`; for backward search it must end at or before it. Callers
/// advance past the current hit to find the next one.
pub fn find(
    engine: &TerminalEngine,
    state: &SearchState,
    from: (i32, usize),
    direction: SearchDirection,
) -> Option<SearchHit> {
    if state.query.is_empty() {
        return None;
    }
    let needle: Vec<char> = state.query.chars().collect();
    let (top, bottom) = engine.line_range();
    let (start_line, end_line) = (from.0.clamp(top, bottom), from.0.clamp(top, bottom));

    let order: Vec<i32> = match direction {
        SearchDirection::Forward => (start_line..=bottom).chain(top..start_line).collect(),
        SearchDirection::Backward => (top..=end_line)
            .rev()
            .chain(((end_line + 1)..=bottom).rev())
            .collect(),
    };

    for line in order {
        let chars: Vec<char> = engine.line_text(Line(line)).chars().collect();
        let is_origin = line == from.0;
        let hit = match direction {
            SearchDirection::Forward => {
                let start = if is_origin { from.1 } else { 0 };
                find_from(&chars, &needle, start, state.case_sensitive)
            }
            SearchDirection::Backward => {
                let limit = if is_origin {
                    from.1.min(chars.len())
                } else {
                    chars.len()
                };
                find_last(&chars[..limit], &needle, state.case_sensitive)
            }
        };
        if let Some((column, length)) = hit {
            if is_origin && direction == SearchDirection::Forward && column < from.1 {
                continue;
            }
            return Some(SearchHit {
                line,
                column,
                length,
            });
        }
    }
    None
}

/// First match at or after `start`, in character columns.
fn find_from(
    chars: &[char],
    needle: &[char],
    start: usize,
    case_sensitive: bool,
) -> Option<(usize, usize)> {
    if needle.is_empty() || needle.len() > chars.len() {
        return None;
    }
    for index in start..=(chars.len() - needle.len()) {
        if matches_at(chars, index, needle, case_sensitive) {
            return Some((index, needle.len()));
        }
    }
    None
}

/// Last match inside `chars`, in character columns.
fn find_last(chars: &[char], needle: &[char], case_sensitive: bool) -> Option<(usize, usize)> {
    if needle.is_empty() || needle.len() > chars.len() {
        return None;
    }
    for index in (0..=(chars.len() - needle.len())).rev() {
        if matches_at(chars, index, needle, case_sensitive) {
            return Some((index, needle.len()));
        }
    }
    None
}

fn matches_at(chars: &[char], index: usize, needle: &[char], case_sensitive: bool) -> bool {
    needle.iter().enumerate().all(|(offset, expected)| {
        let actual = chars[index + offset];
        if case_sensitive {
            actual == *expected
        } else {
            actual.eq_ignore_ascii_case(expected)
                || actual.to_lowercase().eq(expected.to_lowercase())
        }
    })
}

/// Counts matches across the whole buffer.
pub fn count_matches(engine: &TerminalEngine, state: &SearchState) -> usize {
    if state.query.is_empty() {
        return 0;
    }
    let needle: Vec<char> = state.query.chars().collect();
    let (top, bottom) = engine.line_range();
    let mut count = 0;
    for line in top..=bottom {
        let chars: Vec<char> = engine.line_text(Line(line)).chars().collect();
        if needle.len() > chars.len() {
            continue;
        }
        for index in 0..=(chars.len() - needle.len()) {
            if matches_at(&chars, index, &needle, state.case_sensitive) {
                count += 1;
            }
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::EngineOptions;

    fn engine_with(text: &str) -> TerminalEngine {
        let mut engine = TerminalEngine::new(EngineOptions {
            cols: 40,
            rows: 10,
            ..Default::default()
        });
        engine.process(text.as_bytes());
        engine
    }

    #[test]
    fn finds_text_on_the_visible_screen() {
        let engine = engine_with("alpha\r\nbeta needle here\r\ngamma\r\n");
        let state = SearchState {
            query: "needle".to_owned(),
            ..Default::default()
        };
        let hit = find(
            &engine,
            &state,
            (engine.line_range().0, 0),
            SearchDirection::Forward,
        )
        .unwrap();
        assert_eq!(hit.column, 5);
        assert_eq!(hit.length, 6);
    }

    #[test]
    fn search_is_case_insensitive_by_default() {
        let engine = engine_with("Needle\r\n");
        let state = SearchState {
            query: "needle".to_owned(),
            ..Default::default()
        };
        assert!(
            find(
                &engine,
                &state,
                (engine.line_range().0, 0),
                SearchDirection::Forward
            )
            .is_some()
        );
    }

    #[test]
    fn case_sensitive_search_can_miss() {
        let engine = engine_with("Needle\r\n");
        let state = SearchState {
            query: "needle".to_owned(),
            case_sensitive: true,
            ..Default::default()
        };
        assert!(
            find(
                &engine,
                &state,
                (engine.line_range().0, 0),
                SearchDirection::Forward
            )
            .is_none()
        );
    }

    #[test]
    fn empty_query_matches_nothing() {
        let engine = engine_with("anything\r\n");
        let state = SearchState::default();
        assert!(find(&engine, &state, (0, 0), SearchDirection::Forward).is_none());
        assert_eq!(count_matches(&engine, &state), 0);
    }

    #[test]
    fn search_wraps_once_forward() {
        let engine = engine_with("needle\r\nother\r\n");
        let state = SearchState {
            query: "needle".to_owned(),
            ..Default::default()
        };
        let (_, bottom) = engine.line_range();
        let hit = find(&engine, &state, (bottom, 0), SearchDirection::Forward);
        assert!(hit.is_some(), "search must wrap to the top");
    }

    #[test]
    fn backward_search_finds_the_previous_match() {
        let engine = engine_with("needle one\r\nmiddle\r\nneedle two\r\n");
        let state = SearchState {
            query: "needle".to_owned(),
            ..Default::default()
        };
        let (top, bottom) = engine.line_range();
        let last = find(&engine, &state, (bottom, 0), SearchDirection::Backward);
        assert!(last.is_some());
        let previous = find(
            &engine,
            &state,
            (last.unwrap().line, 0),
            SearchDirection::Backward,
        );
        assert!(previous.is_some());
        assert!(previous.unwrap().line < last.unwrap().line);
        assert!(top <= previous.unwrap().line);
    }

    #[test]
    fn counts_matches_across_lines() {
        let engine = engine_with("x x x\r\nno\r\nx\r\n");
        let state = SearchState {
            query: "x".to_owned(),
            ..Default::default()
        };
        assert_eq!(count_matches(&engine, &state), 4);
    }

    #[test]
    fn indicator_describes_state() {
        let mut state = SearchState {
            query: "abc".to_owned(),
            ..Default::default()
        };
        assert_eq!(state.indicator(), "no match for `abc`");
        state.current = Some(SearchHit {
            line: 3,
            column: 0,
            length: 3,
        });
        state.match_count = 2;
        assert!(state.indicator().contains("2 match"));
    }

    #[test]
    fn editing_the_query_resets_the_current_hit() {
        let mut state = SearchState::default();
        state.push_char('a');
        state.current = Some(SearchHit {
            line: 0,
            column: 0,
            length: 1,
        });
        state.push_char('b');
        assert!(state.current.is_none());
        state.pop_char();
        assert_eq!(state.query, "a");
        state.clear();
        assert!(!state.is_active());
    }
}
