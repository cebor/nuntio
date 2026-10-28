//! Finding links in terminal text: OSC 8 hyperlinks, plain URLs, absolute
//! paths and relative path candidates.

use std::ops::{Range, RangeInclusive};

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::Term;
use alacritty_terminal::term::cell::{Cell, Flags};

use crate::image::image_cell_ref;
use crate::snapshot::{contains, is_spacer};

/// Longest wrapped line considered when looking for URLs, in rows.
const MAX_WRAPPED_ROWS: usize = 32;

/// A link on screen. Positions are viewport (column, line); `start` and
/// `end` are inclusive and may lie outside the viewport. They may lie on
/// different rows, joined by soft wraps or by line breaks a program wrote
/// itself to wrap a long word; the cells between them then include the
/// padding and frame at those joints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    /// The URL, or for [`LinkKind::Path`] and [`LinkKind::Relative`] the
    /// path as the text shows it.
    pub url: String,
    pub kind: LinkKind,
    pub start: (usize, i32),
    pub end: (usize, i32),
}

/// What a link's `url` holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkKind {
    /// A URL (OSC 8 or plain text with a known scheme).
    Url,
    /// An absolute path as the text shows it: `/x`, `~/x`, `C:\x`, `C:/x`;
    /// also the content of quotes that starts like one (`'/tmp/a b.txt'`).
    Path,
    /// A word, or the content of quotes, that may name a file relative to the
    /// pane's working directory: it is a link only if that file exists.
    Relative,
}

/// The link under a grid point, if any. Points outside the grid have none.
pub(crate) fn link_at<T>(term: &Term<T>, mut point: Point) -> Option<Link> {
    let grid = term.grid();
    if !contains(term, point) {
        return None;
    }
    if grid[point].flags.contains(Flags::WIDE_CHAR_SPACER) && point.column.0 > 0 {
        point.column -= 1;
    }
    let offset = grid.display_offset() as i32;
    let to_viewport = |p: Point| (p.column.0, p.line.0 + offset);

    // OSC 8 hyperlink: the run of cells carrying a link to the same URI.
    // Programs reopen the link on each row they write, and alacritty gives
    // every link opened without an id its own, so ids are not compared.
    let cell: &Cell = &grid[point];
    if let Some(link) = cell.hyperlink().filter(|l| has_known_scheme(l.uri())) {
        let uri = link.uri();
        let points = logical_line(term, point, |l, cols| link_wraps(term, l, cols, uri));
        let index = points.iter().position(|p| *p == point)?;
        let same = |p: &Point| grid[*p].hyperlink().is_some_and(|l| l.uri() == uri);
        let start = points[..index]
            .iter()
            .rposition(|p| !same(p))
            .map_or(0, |i| i + 1);
        let end = points[index..]
            .iter()
            .position(|p| !same(p))
            .map_or(points.len(), |i| index + i);
        return Some(Link {
            url: uri.to_owned(),
            kind: LinkKind::Url,
            start: to_viewport(points[start]),
            end: to_viewport(points[end - 1]),
        });
    }

    let points = logical_line(term, point, |l, cols| hard_wraps(term, l, cols));
    let index = points.iter().position(|p| *p == point)?;

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
    let (range, kind) = find_links(&chars)
        .into_iter()
        .find(|(r, _)| r.start < first[index + 1] && r.end > first[index])
        .or_else(|| {
            relative_at(&chars, first[index]).map(|r| {
                // Quoted content that starts like an absolute path (`'/tmp/a b.txt'`).
                let absolute = find_path(&chars, r.start).is_some();
                (
                    r,
                    if absolute {
                        LinkKind::Path
                    } else {
                        LinkKind::Relative
                    },
                )
            })
        })?;
    Some(Link {
        url: chars[range.clone()].iter().collect(),
        kind,
        start: to_viewport(points[cell_of(range.start)]),
        end: to_viewport(points[cell_of(range.end - 1)]),
    })
}

/// Most cells a program may leave between the text of a row it broke
/// itself and the right edge of its panel: its right padding and a frame
/// border.
const MAX_WRAP_MARGIN: usize = 4;

/// Frame borders, pane dividers and scrollbars: box-drawing characters
/// and block elements (U+2500..=U+259F).
fn is_separator(c: char) -> bool {
    ('\u{2500}'..='\u{259f}').contains(&c)
}

/// Padding, indentation and frame borders around the text of a row:
/// blanks, wide-character spacers and separators ([`is_separator`]).
fn is_gutter(cell: &Cell) -> bool {
    if image_cell_ref(cell).is_some() {
        return false;
    }
    is_spacer(cell.flags) || cell.c == ' ' || is_separator(cell.c)
}

/// Columns of the panel around `point`: the cells between the nearest
/// separators left and right of it on its row (a frame, a multiplexer's
/// sidebar or pane border, a scrollbar), or the row's edges.
fn panel<T>(term: &Term<T>, point: Point) -> RangeInclusive<usize> {
    let row = &term.grid()[point.line];
    let last = term.last_column().0;
    let col = point.column.0;
    let lo = (0..col)
        .rev()
        .find(|&c| is_separator(row[Column(c)].c))
        .map_or(0, |c| c + 1);
    let hi = (col + 1..=last)
        .find(|&c| is_separator(row[Column(c)].c))
        .map_or(last, |c| c - 1);
    lo..=hi
}

/// Columns of the first and last non-gutter cell of `line` within `cols`;
/// `None` if there is only gutter.
fn text_span<T>(
    term: &Term<T>,
    line: Line,
    cols: &RangeInclusive<usize>,
) -> Option<(usize, usize)> {
    let row = &term.grid()[line];
    let first = cols.clone().find(|&c| !is_gutter(&row[Column(c)]))?;
    let last = cols.clone().rev().find(|&c| !is_gutter(&row[Column(c)]))?;
    Some((first, last))
}

/// Text spans of `line` and the row below it, if `line` may go on there
/// through a hard wrap: it is not the last row, does not soft-wrap, and
/// both rows hold text.
fn hard_wrap_spans<T>(
    term: &Term<T>,
    line: Line,
    cols: &RangeInclusive<usize>,
) -> Option<((usize, usize), (usize, usize))> {
    let grid = term.grid();
    if line >= grid.bottommost_line()
        || grid[line][term.last_column()]
            .flags
            .contains(Flags::WRAPLINE)
    {
        return None;
    }
    Some((
        text_span(term, line, cols)?,
        text_span(term, Line(line.0 + 1), cols)?,
    ))
}

/// Whether a program broke a word at the end of `line` and continued it
/// on the next row itself (no soft wrap): the row holds a single word
/// that reaches the right margin of its panel `cols`, and the next row's
/// text in `cols` starts at the same column.
fn hard_wraps<T>(term: &Term<T>, line: Line, cols: &RangeInclusive<usize>) -> bool {
    let Some(((first, end), (next_first, _))) = hard_wrap_spans(term, line, cols) else {
        return false;
    };
    let grid = term.grid();
    let is_word = |cell: &Cell| image_cell_ref(cell).is_none() && !is_delimiter(cell.c);
    *cols.end() - end <= MAX_WRAP_MARGIN
        && next_first == first
        && (first..=end).all(|c| {
            let cell = &grid[line][Column(c)];
            is_spacer(cell.flags) || is_word(cell)
        })
        && is_word(&grid[Line(line.0 + 1)][Column(next_first)])
}

/// Whether the OSC 8 link to `uri` goes on from the end of `line` to the
/// start of the next row (programs reopen the link on each row they write).
fn link_wraps<T>(term: &Term<T>, line: Line, cols: &RangeInclusive<usize>, uri: &str) -> bool {
    let Some(((_, end), (next_first, _))) = hard_wrap_spans(term, line, cols) else {
        return false;
    };
    let grid = term.grid();
    let links_to = |p: Point| grid[p].hyperlink().is_some_and(|l| l.uri() == uri);
    links_to(Point::new(line, Column(end)))
        && links_to(Point::new(Line(line.0 + 1), Column(next_first)))
}

/// The cells of the line through `point`: rows joined by soft wraps, and by
/// hard wraps where `hard` says a row goes on. Hard wraps are judged inside
/// the [`panel`] around `point`. At a hard wrap the gutter between the rows
/// (right margin, next row's indent) is left out. Wide-character spacers
/// are left out everywhere.
fn logical_line<T>(
    term: &Term<T>,
    point: Point,
    hard: impl Fn(Line, &RangeInclusive<usize>) -> bool,
) -> Vec<Point> {
    let cols = panel(term, point);
    let line = point.line;
    let grid = term.grid();
    let last = term.last_column();
    let soft = |l: Line| grid[l][last].flags.contains(Flags::WRAPLINE);
    let joined = |l: Line| soft(l) || hard(l, &cols);
    let limit = MAX_WRAPPED_ROWS as i32;

    let (mut top, mut bottom) = (line, line);
    while top > grid.topmost_line() && joined(Line(top.0 - 1)) && (line.0 - top.0) < limit {
        top = Line(top.0 - 1);
    }
    while bottom < grid.bottommost_line() && joined(bottom) && (bottom.0 - line.0) < limit {
        bottom = Line(bottom.0 + 1);
    }

    let span = |l: Line| text_span(term, l, &cols).expect("a hard-wrapped row holds text");
    let mut points = Vec::new();
    for l in (top.0..=bottom.0).map(Line) {
        let from = if l > top && !soft(Line(l.0 - 1)) {
            span(l).0
        } else {
            0
        };
        let to = if l < bottom && !soft(l) {
            span(l).1
        } else {
            last.0
        };
        points.extend(
            (from..=to)
                .map(|c| Point::new(l, Column(c)))
                .filter(|p| !is_spacer(grid[*p].flags)),
        );
    }
    points
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
        || is_separator(c)
}

/// Ranges (in chars) of the URLs and absolute paths in `text`.
pub(crate) fn find_links(text: &[char]) -> Vec<(Range<usize>, LinkKind)> {
    let mut links = Vec::new();
    let mut i = 0;
    while i < text.len() {
        let scheme = SCHEMES.iter().find(|s| starts_with(&text[i..], s));
        let at_boundary = i == 0 || !text[i - 1].is_alphanumeric();
        if let Some(scheme) = scheme.filter(|_| at_boundary) {
            let body_start = i + scheme.len();
            let mut end = body_start;
            while end < text.len() && !is_delimiter(text[end]) {
                end += 1;
            }
            end = trim_trailing(text, i, end);
            if end > body_start {
                links.push((i..end, LinkKind::Url));
                i = end;
            } else {
                i = body_start;
            }
        } else if let Some(range) = find_path(text, i) {
            i = range.end;
            links.push((range, LinkKind::Path));
        } else {
            i += 1;
        }
    }
    links
}

/// Whether a path may start right after `c`. Not after `<`, so that
/// `</div>` is no path.
fn path_may_follow(c: char) -> bool {
    c.is_whitespace()
        || is_separator(c)
        || matches!(
            c,
            '"' | '\'' | '`' | '(' | '[' | '{' | '=' | ',' | ';' | ':' | '>' | '|'
        )
}

/// Characters of a path; `windows` paths also take `\`. A `:` ends every
/// path, so a line suffix (`/x/a.rs:42:7`) is not part of it and the
/// entries of `PATH=/usr/bin:/bin` are paths of their own.
fn is_path_char(c: char, windows: bool) -> bool {
    c != ':' && (!is_delimiter(c) || (windows && c == '\\'))
}

/// The absolute path starting at `start` in `text`, if any: `/x`, `~/x`,
/// `C:\x` or `C:/x` at a boundary ([`path_may_follow`]), ending before
/// the first character that is no [`is_path_char`] and without trailing
/// punctuation.
fn find_path(text: &[char], start: usize) -> Option<Range<usize>> {
    if start > 0 && !path_may_follow(text[start - 1]) {
        return None;
    }
    let (prefix, windows) = match text[start..] {
        ['/', ..] => (1, false),
        ['~', '/', ..] => (2, false),
        [letter, ':', '\\' | '/', ..] if letter.is_ascii_alphabetic() => (3, true),
        _ => return None,
    };
    let first = *text.get(start + prefix)?;
    if !is_path_char(first, windows) || matches!(first, '/' | '\\') {
        return None;
    }
    let mut end = start + prefix;
    while end < text.len() && is_path_char(text[end], windows) {
        end += 1;
    }
    let end = trim_trailing(text, start, end);
    text[start + prefix..end]
        .iter()
        .any(|c| c.is_alphanumeric())
        .then_some(start..end)
}

/// The relative path candidate at char `at` of `text`: the content of quotes
/// around it, else the word under it. Only the file system can tell
/// whether it names a file.
pub(crate) fn relative_at(text: &[char], at: usize) -> Option<Range<usize>> {
    quoted_at(text, at).or_else(|| word_at(text, at))
}

/// The content of `'…'` or `"…"` around `at`, as GNU ls quotes names with
/// spaces or quotes. A quote counts only at a word boundary: no letter or
/// digit right before the opening one or right after the closing one. Shell
/// escapes such as `'it'\''s'` are not decoded.
fn quoted_at(text: &[char], at: usize) -> Option<Range<usize>> {
    ['\'', '"'].into_iter().find_map(|q| {
        if text[at] == q {
            return None;
        }
        let open = text[..at].iter().rposition(|&c| c == q)?;
        let close = at + text[at..].iter().position(|&c| c == q)?;
        // Quotes at word boundaries only: the apostrophes of `can't` and `it's` quote nothing.
        let is_text = |c: Option<&char>| c.is_some_and(|c| c.is_alphanumeric());
        if is_text(open.checked_sub(1).map(|i| &text[i])) || is_text(text.get(close + 1)) {
            return None;
        }
        let content = &text[open + 1..close];
        let (first, last) = (content.first()?, content.last()?);
        let usable = !first.is_whitespace()
            && !last.is_whitespace()
            && !content.iter().any(|&c| c.is_control() || is_separator(c))
            && content.iter().any(|c| c.is_alphanumeric());
        usable.then_some(open + 1..close)
    })
}

/// The word under `at`: path characters (`\` too on Windows), stopping on
/// the left at `(`, `[`, `=`, `,` and `;`, without trailing punctuation; it
/// holds a letter or digit and doesn't start with `-` (an option).
fn word_at(text: &[char], at: usize) -> Option<Range<usize>> {
    let is_word =
        |c: char| is_path_char(c, cfg!(windows)) && !matches!(c, '(' | '[' | '=' | ',' | ';');
    if !is_word(text[at]) {
        return None;
    }
    let start = text[..at]
        .iter()
        .rposition(|&c| !is_word(c))
        .map_or(0, |i| i + 1);
    let end = at
        + text[at..]
            .iter()
            .position(|&c| !is_word(c))
            .unwrap_or(text.len() - at);
    let end = trim_trailing(text, start, end);
    (at < end && text[start] != '-' && text[start..end].iter().any(|c| c.is_alphanumeric()))
        .then_some(start..end)
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

    fn links(text: &str, kind: LinkKind) -> Vec<String> {
        let chars: Vec<char> = text.chars().collect();
        find_links(&chars)
            .into_iter()
            .filter(|(_, k)| *k == kind)
            .map(|(r, _)| chars[r].iter().collect())
            .collect()
    }

    fn urls(text: &str) -> Vec<String> {
        links(text, LinkKind::Url)
    }

    fn paths(text: &str) -> Vec<String> {
        links(text, LinkKind::Path)
    }

    #[test]
    fn absolute_paths() {
        assert_eq!(
            paths("see /etc/hosts and ~/notes.md"),
            ["/etc/hosts", "~/notes.md"]
        );
        assert_eq!(
            paths(r"C:\Users\me\a.txt, D:/x"),
            [r"C:\Users\me\a.txt", "D:/x"]
        );
        assert_eq!(paths("src/main.rs:4 /x/main.rs:42:7"), ["/x/main.rs"]);
        assert_eq!(paths("PATH=/usr/bin:/bin"), ["/usr/bin", "/bin"]);
        assert_eq!(paths("(/tmp/x)."), ["/tmp/x"]);
        assert_eq!(paths("│/tmp/x▕"), ["/tmp/x"]);
    }

    #[test]
    fn not_absolute_paths() {
        assert!(paths("a/b ./x ../y km/h 1/2 // c //host/x / ~ ~/ /... ").is_empty());
        assert!(paths(r"C:\ xC:\y").is_empty());
        assert!(paths("</div>").is_empty());
    }

    #[test]
    fn paths_inside_urls_are_not_paths() {
        let text = "file:///tmp/a https://x.org/a/b";
        assert!(paths(text).is_empty());
        assert_eq!(urls(text), ["file:///tmp/a", "https://x.org/a/b"]);
    }

    #[test]
    fn path_on_the_grid() {
        let link = click(&term(b"open /tmp/a.txt now"), 8, 0).unwrap();
        assert_eq!(link.url, "/tmp/a.txt");
        assert_eq!(link.kind, LinkKind::Path);
        assert_eq!((link.start, link.end), ((5, 0), (14, 0)));
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
        assert_eq!(urls("│https://example.com▕"), ["https://example.com"]);
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

    fn term(bytes: &[u8]) -> Term<alacritty_terminal::event::VoidListener> {
        sized_term(40, bytes)
    }

    fn sized_term(columns: u16, bytes: &[u8]) -> Term<alacritty_terminal::event::VoidListener> {
        let mut term = crate::test_support::term(columns, 3);
        crate::test_support::feed(&mut term, bytes);
        term
    }

    fn click<T>(term: &Term<T>, column: usize, line: i32) -> Option<Link> {
        link_at(term, Point::new(Line(line), Column(column)))
    }

    fn long_url() -> String {
        format!("https://example.com/{}", "a".repeat(30))
    }

    #[test]
    fn an_inline_image_ends_the_link() {
        use alacritty_terminal::vte::ansi::{Color, NamedColor};

        let mut term = term(b"https://x.org");
        term.grid_mut()[Point::new(Line(0), Column(13))] =
            crate::image::image_cell(1, 0, 0, Color::Named(NamedColor::Background));
        let link = click(&term, 3, 0).unwrap();
        assert_eq!(link.url, "https://x.org");
        assert_eq!(link.end, (12, 0));
    }

    #[test]
    fn hard_wrapped_url_is_one_link() {
        let url = long_url();
        let term = term(format!(" {} \r\n {} ok", &url[..38], &url[38..]).as_bytes());
        for (column, line) in [(5, 0), (3, 1)] {
            let link = click(&term, column, line).unwrap();
            assert_eq!(link.url, url);
            assert_eq!((link.start, link.end), ((1, 0), (12, 1)));
        }
        assert_eq!(
            click(&term, 15, 1).map(|l| (l.url, l.kind)),
            Some(("ok".to_owned(), LinkKind::Relative))
        );
    }

    #[test]
    fn framed_hard_wrapped_url_is_one_link() {
        let url = long_url();
        let term = term(format!("│ {} │\r\n│ {}", &url[..36], &url[36..]).as_bytes());
        let link = click(&term, 3, 1).unwrap();
        assert_eq!(link.url, url);
        assert_eq!((link.start, link.end), ((2, 0), (15, 1)));
    }

    #[test]
    fn url_wrapped_in_a_herdr_pane_is_one_link() {
        // herdr's sidebar with its border on the left, a scrollbar in the
        // pane's last column, and omp's padding of one cell inside the pane.
        let url = long_url();
        let row = |sidebar: &str, text: &str| format!("{sidebar:<9}│{text:<49}▕");
        let rows = format!(
            "{}\r\n{}",
            row("ws nuntio", &format!(" {} ", &url[..47])),
            row(" 2 agents", &format!(" {} ok", &url[47..])),
        );
        let term = sized_term(60, rows.as_bytes());
        for (column, line) in [(30, 0), (12, 1)] {
            let link = click(&term, column, line).unwrap();
            assert_eq!(link.url, url);
            assert_eq!((link.start, link.end), ((11, 0), (13, 1)));
        }
        assert_eq!(
            click(&term, 16, 1).map(|l| (l.url, l.kind)),
            Some(("ok".to_owned(), LinkKind::Relative))
        );
    }

    fn relative(text: &str, at: usize) -> Option<String> {
        let chars: Vec<char> = text.chars().collect();
        relative_at(&chars, at).map(|r| chars[r].iter().collect())
    }

    #[test]
    fn relative_candidates() {
        let some = |s: &str| Some(s.to_owned());
        assert_eq!(
            relative("Cargo.toml  crates  README.md", 13),
            some("crates")
        );
        assert_eq!(relative("src/main.rs:42:7", 5), some("src/main.rs"));
        assert_eq!(relative("src/main.rs:42:7", 12), some("42"));
        assert_eq!(relative("(src/x.rs).", 2), some("src/x.rs"));
        assert_eq!(relative("--out=dist/a.js", 8), some("dist/a.js"));
        assert_eq!(relative("'my file.txt'  b.txt", 4), some("my file.txt"));
        assert_eq!(relative("\"it's here.txt\"", 8), some("it's here.txt"));
        assert_eq!(relative("'a' and 'b'", 5), some("and"));
        // Content starting with a space is not quoted.
        assert_eq!(relative("' a'", 2), some("a"));
        assert_eq!(
            relative("\"it's.txt\"  'my file.txt'", 13),
            some("my file.txt")
        );
        assert_eq!(relative("can't find a.txt, it's", 11), some("a.txt"));
    }

    #[test]
    fn no_relative_candidate() {
        assert_eq!(relative("-rw-r--r-- 1 me", 3), None);
        assert_eq!(relative("a  b", 1), None);
        assert_eq!(relative("'my file.txt'", 0), None);
    }

    #[test]
    fn relative_word_on_the_grid() {
        let link = click(&term(b"Cargo.toml  src"), 13, 0).unwrap();
        assert_eq!(link.url, "src");
        assert_eq!(link.kind, LinkKind::Relative);
        assert_eq!((link.start, link.end), ((12, 0), (14, 0)));
        // An absolute path still wins.
        let term = term(b"open /tmp/a.txt now");
        assert_eq!(click(&term, 8, 0).unwrap().kind, LinkKind::Path);
        // So does quoted content that starts like one.
        let link = click(&self::term(b"'/tmp/a b.txt'"), 8, 0).unwrap();
        assert_eq!(link.url, "/tmp/a b.txt");
        assert_eq!(link.kind, LinkKind::Path);
    }

    #[test]
    fn rows_are_not_joined_without_a_hard_wrap() {
        let short = term(b" https://example.com/x\r\n aaa");
        assert_eq!(click(&short, 5, 0).unwrap().url, "https://example.com/x");

        let prose = term(b" see https://example.com/aaaaaaaaaaaaaa \r\n bbb");
        assert_eq!(
            click(&prose, 10, 0).unwrap().url,
            "https://example.com/aaaaaaaaaaaaaa"
        );

        let url = long_url();
        let indented = term(format!(" {} \r\n   {}", &url[..38], &url[38..]).as_bytes());
        assert_eq!(click(&indented, 5, 0).unwrap().url, &url[..38]);
    }

    #[test]
    fn osc8_link_reopened_on_the_next_row() {
        let term = term(
            b" \x1b]8;;https://nuntio.dev/docs\x1b\\see the\x1b]8;;\x1b\\\r\n \
              \x1b]8;;https://nuntio.dev/docs\x1b\\docs\x1b]8;;\x1b\\ after",
        );
        let link = click(&term, 2, 1).unwrap();
        assert_eq!(link.url, "https://nuntio.dev/docs");
        assert_eq!((link.start, link.end), ((1, 0), (4, 1)));
    }

    #[test]
    fn url_wrapped_by_omp_is_one_link() {
        // A user message in omp at 88 columns: one cell of padding on both
        // sides, the URL on a row of its own, the rest after it.
        let url = format!("https://example.com/hard-wrap-check/{}", "a".repeat(130));
        let rows = format!(" {} \r\n {} see", &url[..86], &url[86..]);
        let term = sized_term(88, rows.as_bytes());
        let link = click(&term, 40, 1).unwrap();
        assert_eq!(link.url, url);
        assert_eq!((link.start, link.end), ((1, 0), (80, 1)));
    }
}
