//! Searching the scrollback.

use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Boundary, Column, Direction, Line, Point, Side};
use alacritty_terminal::term::Term;
use alacritty_terminal::term::search::{Match, RegexIter, RegexSearch};

/// A compiled search and the currently selected match.
pub struct Search {
    regex: RegexSearch,
    current: Option<Match>,
    /// Where to start when there is no current match yet.
    anchor: Option<Point>,
}

impl Search {
    /// Compile a search. Plain text is matched literally; either way the
    /// search ignores case unless the query contains uppercase letters.
    pub fn new(query: &str, regex: bool) -> Result<Self, String> {
        let pattern = if regex {
            query.to_owned()
        } else {
            escape(query)
        };
        let regex = RegexSearch::new(&pattern).map_err(|err| {
            let message = err.to_string();
            message.lines().last().unwrap_or(&message).trim().to_owned()
        })?;
        Ok(Self {
            regex,
            current: None,
            anchor: None,
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
        self
    }

    pub fn has_match(&self) -> bool {
        self.current.is_some()
    }

    pub(crate) fn current(&self) -> Option<&Match> {
        self.current.as_ref()
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
    search.current.is_some()
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
    use super::*;

    #[test]
    fn plain_text_is_escaped() {
        assert_eq!(escape("a.b(c)*"), "a\\.b\\(c\\)\\*");
        assert!(Search::new("foo(", false).is_ok());
    }

    #[test]
    fn invalid_regex_is_an_error() {
        let err = Search::new("foo(", true).err().unwrap();
        assert!(!err.is_empty());
    }
}
