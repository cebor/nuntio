//! Regex search through a wezterm screen, scrollback included, with the
//! behavior of the alacritty engine's search.

use super::grid::{self, Grid, LogicalLine, Pos};
use crate::search::{MAX_COUNTED, MatchPosition, Search};

/// A match, from its first to its last cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WezMatch {
    pub(crate) start: Pos,
    pub(crate) end: Pos,
}

impl WezMatch {
    pub(crate) fn contains(&self, pos: Pos) -> bool {
        self.start <= pos && pos <= self.end
    }
}

/// The part of a [`Search`] the wezterm engine keeps.
#[derive(Debug, Clone, Default)]
pub(crate) struct State {
    pub(crate) current: Option<WezMatch>,
    /// Where to start when there is no current match yet.
    pub(crate) anchor: Option<Pos>,
}

/// The matches in one logical line.
fn line_matches(
    regex: &regex::Regex,
    line: &LogicalLine,
    mut f: impl FnMut(WezMatch) -> bool,
) -> bool {
    for m in regex.find_iter(&line.text) {
        if m.is_empty() {
            continue;
        }
        let found = WezMatch {
            start: line.cell_at(m.start()),
            end: line.cell_at(m.end() - 1),
        };
        if !f(found) {
            return false;
        }
    }
    true
}

/// Every match from the top of the scrollback on, until `f` returns
/// `false`.
fn each_match(grid: &Grid<'_>, regex: &regex::Regex, mut f: impl FnMut(WezMatch) -> bool) {
    grid.logical_lines(grid.first()..grid.end(), |line| {
        line_matches(regex, line, &mut f)
    });
}

/// Forget positions whose rows are gone.
fn drop_stale(grid: &Grid<'_>, search: &mut Search) {
    let wez = &mut search.wez;
    if wez
        .current
        .is_some_and(|m| !grid.contains_row(m.start.row) || !grid.contains_row(m.end.row))
    {
        wez.current = None;
    }
    if wez.anchor.is_some_and(|p| !grid.contains_row(p.row)) {
        wez.anchor = None;
    }
}

/// Move to the next match upwards (older output) or downwards and scroll
/// it into view. Wraps around the whole scrollback.
pub(crate) fn find(state: &mut super::State, search: &mut Search, up: bool) -> bool {
    let top = state.viewport_top();
    let rows = state.term.screen().physical_rows as isize;
    let current = grid::with_grid(state.term.screen_mut(), |grid| {
        drop_stale(grid, search);
        let cols = grid.cols;
        // The cell before or after the current match; may lie outside the
        // grid, which makes the search wrap.
        let origin = match search.wez.current {
            Some(m) if up => match m.start.column {
                0 => Pos::new(m.start.row - 1, cols - 1),
                column => Pos::new(m.start.row, column - 1),
            },
            Some(m) if m.end.column + 1 == cols => Pos::new(m.end.row + 1, 0),
            Some(m) => Pos::new(m.end.row, m.end.column + 1),
            None => search.wez.anchor.unwrap_or(if up {
                Pos::new(top + rows - 1, cols - 1)
            } else {
                Pos::new(top, 0)
            }),
        };
        // The nearest match in the direction, and the one to wrap around
        // to: the newest going up, the oldest going down.
        let mut hit = None;
        let mut wrap = None;
        each_match(grid, &search.wez_regex, |m| {
            if up {
                // The last match that ends at or before the origin.
                if m.end <= origin {
                    hit = Some(m);
                }
                wrap = Some(m);
                true
            } else if m.start >= origin {
                // The first match that starts at or after it.
                hit = Some(m);
                false
            } else {
                wrap.get_or_insert(m);
                true
            }
        });
        hit.or(wrap)
    });
    search.wez.current = current;

    if let Some(m) = current {
        state.scroll_to_row(m.start.row);
    }
    search.position = current.map(|m| count(state, search, m));
    current.is_some()
}

/// Count the matches, up to `MAX_COUNTED`, and find `current` among them.
fn count(state: &mut super::State, search: &Search, current: WezMatch) -> MatchPosition {
    let mut total = 0;
    let mut index = None;
    let mut more = false;
    grid::with_grid(state.term.screen_mut(), |grid| {
        each_match(grid, &search.wez_regex, |m| {
            if total == MAX_COUNTED {
                more = true;
                return false;
            }
            total += 1;
            if m.start == current.start {
                index = Some(total);
            }
            true
        });
    });
    MatchPosition { index, total, more }
}

/// The matches that show in the viewport, in grid order, for highlighting.
pub(crate) fn visible_matches(state: &mut super::State, search: &mut Search) -> Vec<WezMatch> {
    let top = state.viewport_top();
    let rows = state.term.screen().physical_rows as isize;
    let first = Pos::new(top, 0);
    let last = Pos::new(top + rows - 1, usize::MAX);
    let mut matches = Vec::new();
    grid::with_grid(state.term.screen_mut(), |grid| {
        drop_stale(grid, search);
        grid.logical_lines(top..top + rows, |line| {
            line_matches(&search.wez_regex, line, |m| {
                if m.end >= first && m.start <= last {
                    matches.push(m);
                }
                matches.len() < 1000
            })
        });
    });
    matches
}
