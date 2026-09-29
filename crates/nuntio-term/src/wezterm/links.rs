//! Links under the pointer on a wezterm screen: OSC 8 hyperlinks and
//! plain URLs, like `url::link_at` for alacritty.

use super::State;
use super::grid::{self, Col, Pos};
use crate::pane::GridPoint;
use crate::url::{Link, MAX_WRAPPED_ROWS, find_urls, has_known_scheme};

pub(crate) fn link_at(state: &mut State, point: GridPoint) -> Option<Link> {
    let top = state.viewport_top();
    let (mut pos, _) = state.to_pos(point);
    grid::with_grid(state.term.screen_mut(), |grid| {
        if !grid.contains_row(pos.row) || pos.column >= grid.cols {
            return None;
        }
        let rows = grid.logical_rows(pos.row, MAX_WRAPPED_ROWS);
        let cells: Vec<Vec<Col<'_>>> = rows.clone().map(|row| grid.row(row)).collect();
        let col = |p: Pos| cells[(p.row - rows.start) as usize][p.column];
        if col(pos).is_continuation() && pos.column > 0 {
            pos.column -= 1;
        }
        let points: Vec<Pos> = rows
            .clone()
            .flat_map(|row| (0..grid.cols).map(move |column| Pos::new(row, column)))
            .filter(|p| !col(*p).is_continuation())
            .collect();
        let index = points.iter().position(|p| *p == pos)?;
        let to_viewport = |p: Pos| (p.column, (p.row - top) as i32);

        // OSC 8 hyperlink: the run of cells carrying the same link.
        if let Some(link) = col(pos).hyperlink().filter(|l| has_known_scheme(l.uri())) {
            let same = |p: &Pos| col(*p).hyperlink().is_some_and(|l| l.uri() == link.uri());
            let start = points[..index]
                .iter()
                .rposition(|p| !same(p))
                .map_or(0, |i| i + 1);
            let end = points[index..]
                .iter()
                .position(|p| !same(p))
                .map_or(points.len(), |i| index + i);
            return Some(Link {
                url: link.uri().to_owned(),
                start: to_viewport(points[start]),
                end: to_viewport(points[end - 1]),
            });
        }

        let chars: Vec<char> = points.iter().map(|p| col(*p).first_char()).collect();
        let range = find_urls(&chars).into_iter().find(|r| r.contains(&index))?;
        Some(Link {
            url: chars[range.clone()].iter().collect(),
            start: to_viewport(points[range.start]),
            end: to_viewport(points[range.end - 1]),
        })
    })
}
