//! Searching the scrollback.

use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Boundary, Column, Direction, Line, Point, Side};
use alacritty_terminal::term::Term;
use alacritty_terminal::term::search::{Match, RegexIter, RegexSearch};

/// Matches counted at most; beyond that the count shows as "999+".
const MAX_COUNTED: usize = 999;

/// The query is not a valid regular expression.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct SearchError(pub String);

/// A compiled search and the currently selected match.
pub struct Search {
    regex: RegexSearch,
    current: Option<Match>,
    /// Where to start when there is no current match yet.
    anchor: Option<Point>,
    /// Scrollback size when `current` and `anchor` were set. Grid lines
    /// count from the top of the screen, so new output that grows the
    /// scrollback moves the text they point at upwards.
    history: usize,
    /// Where the current match is among all matches, as of the last `find`.
    position: Option<MatchPosition>,
}

/// The current match's place among all matches in the scrollback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MatchPosition {
    /// 1-based index of the current match, if it is among those counted.
    pub index: Option<usize>,
    pub total: usize,
    /// There are more than `total` matches; counting stopped.
    pub more: bool,
}

impl Search {
    /// Compile a search. Plain text is matched literally; either way the
    /// search ignores case unless the query contains uppercase letters.
    pub fn new(query: &str, regex: bool) -> Result<Self, SearchError> {
        let pattern = if regex {
            query.to_owned()
        } else {
            escape(query)
        };
        let regex = RegexSearch::new(&pattern).map_err(|err| {
            // The regex crate's message spans several lines, with the
            // pattern drawn above the reason; keep only the reason.
            let message = err.to_string();
            SearchError(message.lines().last().unwrap_or(&message).trim().to_owned())
        })?;
        Ok(Self {
            regex,
            current: None,
            anchor: None,
            history: 0,
            position: None,
        })
    }

    /// Keep the position of `previous` when the query is edited, so typing
    /// refines the match in place instead of jumping around.
    pub fn continue_from(mut self, previous: &Search) -> Self {
        self.anchor = previous
            .current
            .as_ref()
            .map(|m| *m.end())
            .or(previous.anchor);
        self.history = previous.history;
        self
    }

    /// Move the stored positions along with output that scrolled the
    /// screen since. Once the scrollback is full, old lines drop out
    /// without it growing, and that isn't detected.
    fn follow_output<T>(&mut self, term: &Term<T>) {
        let history = term.grid().history_size();
        let delta = history as i32 - self.history as i32;
        self.history = history;
        if delta == 0 {
            return;
        }
        let top = term.topmost_line();
        let shift = |p: Point| Some(Point::new(p.line - delta, p.column)).filter(|p| p.line >= top);
        self.current = self.current.take().and_then(|m| {
            let (start, end) = (shift(*m.start())?, shift(*m.end())?);
            Some(start..=end)
        });
        self.anchor = self.anchor.and_then(shift);
    }

    /// The selected match, after following new output.
    pub(crate) fn current_in<T>(&mut self, term: &Term<T>) -> Option<&Match> {
        self.follow_output(term);
        self.current.as_ref()
    }

    pub fn has_match(&self) -> bool {
        self.current.is_some()
    }

    /// Where the current match is among all matches, if there is one.
    pub fn position(&self) -> Option<MatchPosition> {
        self.position.filter(|_| self.current.is_some())
    }
}

/// Escape regex syntax so `text` matches literally.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if "\\.+*?()|[]{}^$#&-~".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Move to the next match upwards (older output) or downwards and scroll
/// it into view. Wraps around the whole scrollback.
pub(crate) fn find<T: EventListener>(term: &mut Term<T>, search: &mut Search, up: bool) -> bool {
    search.follow_output(term);
    let offset = term.grid().display_offset() as i32;
    let (direction, side) = if up {
        (Direction::Left, Side::Left)
    } else {
        (Direction::Right, Side::Right)
    };
    // Points go stale when the scrollback shrinks (`clear`, resize); an
    // anchor outside the grid would index out of bounds.
    let in_grid = |p: &Point| p.line >= term.topmost_line() && p.line <= term.bottommost_line();
    let origin = match &search.current {
        Some(m) if up => m.start().sub(term, Boundary::None, 1),
        Some(m) => m.end().add(term, Boundary::None, 1),
        None => search.anchor.filter(in_grid).unwrap_or_else(|| {
            if up {
                Point::new(
                    Line(term.screen_lines() as i32 - 1 - offset),
                    term.last_column(),
                )
            } else {
                Point::new(Line(-offset), Column(0))
            }
        }),
    };
    search.current = term.search_next(&mut search.regex, origin, direction, side, None);
    if let Some(m) = &search.current {
        term.scroll_to_point(*m.start());
    }
    search.position = search.current.is_some().then(|| count(term, search));
    search.current.is_some()
}

/// Count the matches in the whole scrollback, up to `MAX_COUNTED`, and
/// find the current one among them.
fn count<T>(term: &Term<T>, search: &mut Search) -> MatchPosition {
    let start = Point::new(term.topmost_line(), Column(0));
    let end = Point::new(term.bottommost_line(), term.last_column());
    let current = search.current.as_ref().map(|m| *m.start());
    let mut total = 0;
    let mut index = None;
    let mut more = false;
    for m in RegexIter::new(start, end, Direction::Right, term, &mut search.regex) {
        if total == MAX_COUNTED {
            more = true;
            break;
        }
        total += 1;
        if Some(*m.start()) == current {
            index = Some(total);
        }
    }
    MatchPosition { index, total, more }
}

/// All matches in the visible part of the screen, for highlighting.
pub(crate) fn visible_matches<T>(term: &Term<T>, search: &mut Search) -> Vec<Match> {
    let offset = term.grid().display_offset() as i32;
    let start = Point::new(Line(-offset), Column(0));
    let end = Point::new(
        Line(term.screen_lines() as i32 - 1 - offset),
        term.last_column(),
    );
    RegexIter::new(start, end, Direction::Right, term, &mut search.regex)
        .take(1000)
        .collect()
}

#[cfg(test)]
mod tests {
    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::term::Config;
    use alacritty_terminal::vte::ansi::Processor;

    use super::*;

    #[test]
    fn plain_text_is_escaped() {
        assert_eq!(escape("a.b(c)*"), "a\\.b\\(c\\)\\*");
        assert!(Search::new("foo(", false).is_ok());
    }

    fn term(lines: usize) -> Term<VoidListener> {
        let size = crate::TermSize {
            columns: 20,
            lines: lines as u16,
            cell_width: 1,
            cell_height: 1,
        };
        Term::new(Config::default(), &size, VoidListener)
    }

    fn feed(term: &mut Term<VoidListener>, text: &str) {
        let mut parser: Processor = Processor::new();
        parser.advance(term, text.as_bytes());
    }

    fn text_at<T>(term: &Term<T>, m: &Match) -> String {
        term.bounds_to_string(*m.start(), *m.end())
    }

    #[test]
    fn the_current_match_follows_scrolling_output() {
        let mut term = term(4);
        feed(&mut term, "one\r\ntwo\r\nneedle\r\n");
        let mut search = Search::new("needle", false).unwrap();
        assert!(find(&mut term, &mut search, true));
        // More output pushes the match up into the scrollback.
        feed(&mut term, "x\r\ny\r\nz\r\n");
        let current = search.current_in(&term).cloned().unwrap();
        assert_eq!(text_at(&term, &current), "needle");
        // Searching on starts from there, not from where it used to be.
        feed(&mut term, "needle\r\n");
        assert!(find(&mut term, &mut search, false));
        let next = search.current_in(&term).cloned().unwrap();
        assert!(next.start().line > current.start().line);
    }

    #[test]
    fn matches_are_counted_across_the_scrollback() {
        let mut term = term(3);
        feed(&mut term, "a1\r\nb\r\na2\r\nc\r\nd\r\na3\r\ne");
        let mut search = Search::new("a", false).unwrap();
        assert!(find(&mut term, &mut search, true));
        let position = search.position().unwrap();
        assert_eq!(
            (position.index, position.total, position.more),
            (Some(3), 3, false)
        );
        assert!(find(&mut term, &mut search, true));
        assert_eq!(search.position().unwrap().index, Some(2));
    }

    #[test]
    fn invalid_regex_is_an_error() {
        let err = Search::new("foo(", true).err().unwrap();
        assert!(!err.0.is_empty());
    }
}
