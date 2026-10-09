//! Finding URLs in terminal text: OSC 8 hyperlinks and plain URLs.

use std::ops::Range;

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::Term;
use alacritty_terminal::term::cell::{Cell, Flags};

use crate::image::image_cell_ref;

/// Longest wrapped line considered when looking for URLs, in rows.
const MAX_WRAPPED_ROWS: usize = 32;

/// A link on screen. Positions are viewport (column, line); `start` and
/// `end` are inclusive and may lie outside the viewport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub url: String,
    pub start: (usize, i32),
    pub end: (usize, i32),
}

/// The link under a grid point, if any. Points outside the grid have none.
pub(crate) fn link_at<T>(term: &Term<T>, mut point: Point) -> Option<Link> {
    let grid = term.grid();
    let inside = (grid.topmost_line()..=grid.bottommost_line()).contains(&point.line)
        && point.column <= term.last_column();
    if !inside {
        return None;
    }
    if grid[point].flags.contains(Flags::WIDE_CHAR_SPACER) && point.column.0 > 0 {
        point.column -= 1;
    }
    let last = term.last_column();
    let wraps = |line: Line| grid[line][last].flags.contains(Flags::WRAPLINE);

    // The logical line: rows joined by soft wraps.
    let (mut top, mut bottom) = (point.line, point.line);
    while top > grid.topmost_line()
        && wraps(Line(top.0 - 1))
        && (point.line.0 - top.0) < MAX_WRAPPED_ROWS as i32
    {
        top = Line(top.0 - 1);
    }
    while bottom < grid.bottommost_line()
        && wraps(bottom)
        && (bottom.0 - point.line.0) < MAX_WRAPPED_ROWS as i32
    {
        bottom = Line(bottom.0 + 1);
    }
    let points: Vec<Point> = (top.0..=bottom.0)
        .flat_map(|line| (0..=last.0).map(move |col| Point::new(Line(line), Column(col))))
        .filter(|p| {
            !grid[*p]
                .flags
                .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
        })
        .collect();
    let index = points.iter().position(|p| *p == point)?;
    let offset = grid.display_offset() as i32;
    let to_viewport = |p: Point| (p.column.0, p.line.0 + offset);

    // OSC 8 hyperlink: the run of cells carrying the same link.
    let cell: &Cell = &grid[point];
    if let Some(link) = cell.hyperlink().filter(|l| has_known_scheme(l.uri())) {
        let same = |p: &Point| grid[*p].hyperlink().as_ref() == Some(&link);
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

    // Text per cell: the base character and its combining marks; an image
    // cell is a gap, not text. `first[i]` is where cell `i` starts in
    // `chars`.
    let mut chars: Vec<char> = Vec::with_capacity(points.len());
    let mut first: Vec<usize> = Vec::with_capacity(points.len() + 1);
    for p in &points {
        first.push(chars.len());
        let cell = &grid[*p];
        if image_cell_ref(cell).is_some() {
            chars.push(' ');
        } else {
            chars.push(cell.c);
            chars.extend(cell.zerowidth().unwrap_or_default());
        }
    }
    first.push(chars.len());
    let cell_of = |char_index: usize| first.partition_point(|&f| f <= char_index) - 1;
    let range = find_urls(&chars)
        .into_iter()
        .find(|r| r.start < first[index + 1] && r.end > first[index])?;
    Some(Link {
        url: chars[range.clone()].iter().collect(),
        start: to_viewport(points[cell_of(range.start)]),
        end: to_viewport(points[cell_of(range.end - 1)]),
    })
}

const SCHEMES: [&str; 7] = [
    "https://", "http://", "file://", "ftp://", "ssh://", "git://", "mailto:",
];

/// Whether `uri` uses one of [`SCHEMES`]. OSC 8 links can carry any URI,
/// and opening e.g. `ms-msdt:` or `search-ms:` runs OS protocol handlers.
fn has_known_scheme(uri: &str) -> bool {
    let chars: Vec<char> = uri.chars().take(8).collect();
    SCHEMES.iter().any(|scheme| starts_with(&chars, scheme))
}

/// Characters that end a URL.
fn is_delimiter(c: char) -> bool {
    c.is_whitespace()
        || c.is_control()
        || matches!(
            c,
            '<' | '>' | '"' | '\'' | '`' | '{' | '}' | '|' | '\\' | '^'
        )
}

/// Ranges (in chars) of the URLs in `text`.
pub fn find_urls(text: &[char]) -> Vec<Range<usize>> {
    let mut urls = Vec::new();
    let mut i = 0;
    while i < text.len() {
        let scheme = SCHEMES.iter().find(|s| starts_with(&text[i..], s));
        let at_boundary = i == 0 || !text[i - 1].is_alphanumeric();
        let Some(scheme) = scheme.filter(|_| at_boundary) else {
            i += 1;
            continue;
        };
        let body_start = i + scheme.len();
        let mut end = body_start;
        while end < text.len() && !is_delimiter(text[end]) {
            end += 1;
        }
        end = trim_trailing(text, i, end);
        if end > body_start {
            urls.push(i..end);
            i = end;
        } else {
            i = body_start;
        }
    }
    urls
}

fn starts_with(text: &[char], prefix: &str) -> bool {
    let mut chars = text.iter();
    prefix
        .chars()
        .all(|p| chars.next().is_some_and(|c| c.eq_ignore_ascii_case(&p)))
}

/// Drop punctuation that ends a sentence rather than the URL, and closing
/// brackets without a matching opening one inside the URL.
fn trim_trailing(text: &[char], start: usize, mut end: usize) -> usize {
    let count = |c: char| text[start..end].iter().filter(|&&x| x == c).count();
    let (round_open, mut round_close) = (count('('), count(')'));
    let (square_open, mut square_close) = (count('['), count(']'));
    while let Some(&last) = text[start..end].last() {
        match last {
            '.' | ',' | ':' | ';' | '!' | '?' => {}
            ')' if round_open < round_close => round_close -= 1,
            ']' if square_open < square_close => square_close -= 1,
            _ => break,
        }
        end -= 1;
    }
    end
}

#[cfg(test)]
mod tests {
    use super::*;

    fn urls(text: &str) -> Vec<String> {
        let chars: Vec<char> = text.chars().collect();
        find_urls(&chars)
            .into_iter()
            .map(|r| chars[r].iter().collect())
            .collect()
    }

    #[test]
    fn plain_urls() {
        assert_eq!(
            urls("see https://example.com/a?b=1#c and http://x.org"),
            ["https://example.com/a?b=1#c", "http://x.org"]
        );
        assert_eq!(urls("file:///tmp/a b"), ["file:///tmp/a"]);
        assert_eq!(urls("mailto:me@example.com"), ["mailto:me@example.com"]);
    }

    #[test]
    fn surrounding_punctuation_is_trimmed() {
        assert_eq!(urls("Visit https://example.com."), ["https://example.com"]);
        assert_eq!(urls("(https://example.com)"), ["https://example.com"]);
        assert_eq!(urls("<https://example.com>"), ["https://example.com"]);
        assert_eq!(urls("\"https://example.com\""), ["https://example.com"]);
        assert_eq!(
            urls("https://en.wikipedia.org/wiki/Rust_(programming_language)"),
            ["https://en.wikipedia.org/wiki/Rust_(programming_language)"]
        );
        assert_eq!(
            urls(&format!("https://a{}", ")".repeat(30_000))),
            ["https://a"]
        );
        assert_eq!(urls("https://a/b_(c)))"), ["https://a/b_(c)"]);
    }

    #[test]
    fn only_known_schemes_are_links() {
        assert!(has_known_scheme("https://example.com"));
        assert!(has_known_scheme("MAILTO:me@example.com"));
        assert!(has_known_scheme("file:///tmp"));
        assert!(!has_known_scheme("ms-msdt:/id PCWDiagnostic"));
        assert!(!has_known_scheme("search-ms:query=x"));
        assert!(!has_known_scheme("javascript:alert(1)"));
        assert!(!has_known_scheme(""));
    }

    #[test]
    fn needs_a_boundary_and_a_body() {
        assert!(urls("xhttps://example.com").is_empty());
        assert!(urls("https:// nothing").is_empty());
        assert_eq!(urls("HTTPS://EXAMPLE.COM"), ["HTTPS://EXAMPLE.COM"]);
    }

    #[test]
    fn an_inline_image_ends_the_link() {
        use alacritty_terminal::event::VoidListener;
        use alacritty_terminal::term::Config;
        use alacritty_terminal::vte::ansi::{Color, NamedColor, Processor};

        let size = crate::TermSize {
            columns: 40,
            lines: 3,
            cell_width: 1,
            cell_height: 1,
        };
        let mut term = Term::new(Config::default(), &size, VoidListener);
        let mut parser: Processor = Processor::new();
        parser.advance(&mut term, b"https://x.org");
        term.grid_mut()[Point::new(Line(0), Column(13))] =
            crate::image::image_cell(1, 0, 0, Color::Named(NamedColor::Background));
        let link = link_at(&term, Point::new(Line(0), Column(3))).unwrap();
        assert_eq!(link.url, "https://x.org");
        assert_eq!(link.end, (12, 0));
    }
}
