//! Live-preview rendering: every line of a note in its rendered form
//! (markers hidden or replaced) and its raw form (all source visible, the
//! markers dimmed), for the cursor line.

use std::ops::Range;

use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use ratatui::style::{Color, Modifier, Style};
use unicode_width::UnicodeWidthChar;

/// The kind of block a line belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Block {
    Plain,
    Heading(u8),
    ListItem,
    Quote,
    Code,
    /// The opening or closing line of a fenced code block.
    Fence,
    Rule,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PieceKind {
    Text,
    Checkbox,
    /// A horizontal rule: the UI fills the row with `─`.
    Rule,
}

/// A run of a line with one style. Shown as `deco` if set, else as
/// `&line[src]`.
#[derive(Debug, Clone, PartialEq)]
pub struct Piece {
    pub src: Range<usize>,
    pub deco: Option<&'static str>,
    pub style: Style,
    pub kind: PieceKind,
}

/// A task list item on a line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Task {
    /// The 3 bytes `[ ]` or `[x]`.
    pub marker: Range<usize>,
    pub checked: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LineView {
    pub block: Block,
    pub rendered: Vec<Piece>,
    pub raw: Vec<Piece>,
    /// Indent of wrapped continuation rows, in cells.
    pub hang: u16,
    pub raw_hang: u16,
    pub task: Option<Task>,
}

/// Display width of a tab, in cells.
const TAB_WIDTH: u16 = 4;

const BULLETS: [&str; 3] = ["•", "◦", "▪"];

/// A replacement of a source range by other text.
struct Deco {
    range: Range<usize>,
    text: &'static str,
    style: Style,
    kind: PieceKind,
}

/// Per-byte styling of the whole document, filled while parsing.
struct Doc<'a> {
    text: &'a str,
    starts: Vec<usize>,
    style: Vec<Style>,
    hidden: Vec<bool>,
    blocks: Vec<Block>,
    tasks: Vec<Option<Task>>,
    quote_done: Vec<bool>,
    decos: Vec<Deco>,
}

impl Doc<'_> {
    fn line_of(&self, pos: usize) -> usize {
        self.starts.partition_point(|&s| s <= pos).saturating_sub(1)
    }

    fn line_end(&self, line: usize) -> usize {
        match self.starts.get(line + 1) {
            Some(next) => next - 1,
            None => self.text.len(),
        }
    }

    /// The lines a source range touches (a trailing newline belongs to the
    /// line before it).
    fn lines_of(&self, r: &Range<usize>) -> Range<usize> {
        self.line_of(r.start)..self.line_of(r.end.saturating_sub(1).max(r.start)) + 1
    }

    fn hide(&mut self, r: Range<usize>) {
        self.hidden[r].fill(true);
    }

    fn show(&mut self, r: Range<usize>, style: Style) {
        self.hidden[r.clone()].fill(false);
        self.style[r].fill(style);
    }

    fn mark(&mut self, line: usize, block: Block) {
        self.blocks[line] = block;
    }

    /// Turns the leading `>` of `line` into bars, once per line; the first
    /// line of a quote inside a list item starts scanning at `from`.
    fn quote_markers(&mut self, line: usize, from: usize) {
        if std::mem::replace(&mut self.quote_done[line], true) {
            return;
        }
        let end = self.line_end(line);
        let bytes = self.text.as_bytes();
        let mut p = from.max(self.starts[line]);
        loop {
            let mut q = p;
            while q < end && bytes[q] == b' ' {
                q += 1;
            }
            if q < end && bytes[q] == b'>' {
                self.decos.push(Deco {
                    range: q..q + 1,
                    text: "▎",
                    style: Style::new().fg(Color::Blue),
                    kind: PieceKind::Text,
                });
                p = q + 1;
            } else {
                break;
            }
        }
    }

    fn parse(&mut self) {
        let text = self.text;
        let mut stack: Vec<Style> = Vec::new();
        // Checked task item: dim and crossed out until the item ends.
        let mut done = false;
        let mut depth = 0usize;
        // Per open item: marker start and the index of its bullet decoration.
        let mut items: Vec<(usize, Option<usize>)> = Vec::new();
        let options = Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
        for (event, r) in Parser::new_ext(text, options).into_offset_iter() {
            let current = |stack: &[Style], done: bool| {
                let style = stack.iter().fold(Style::new(), |a, &s| a.patch(s));
                if done {
                    style.patch(Style::new().add_modifier(Modifier::DIM | Modifier::CROSSED_OUT))
                } else {
                    style
                }
            };
            match event {
                Event::Start(tag) => match tag {
                    Tag::Heading { level, .. } => {
                        let level = level as u8;
                        self.hide(r.clone());
                        stack.push(heading_style(level));
                        for line in self.lines_of(&r) {
                            self.mark(line, Block::Heading(level));
                        }
                    }
                    Tag::Strong => {
                        self.hide(r);
                        stack.push(Style::new().add_modifier(Modifier::BOLD));
                    }
                    Tag::Emphasis => {
                        self.hide(r);
                        stack.push(Style::new().add_modifier(Modifier::ITALIC));
                    }
                    Tag::Strikethrough => {
                        self.hide(r);
                        stack.push(Style::new().add_modifier(Modifier::CROSSED_OUT));
                    }
                    Tag::Link { .. } | Tag::Image { .. } => {
                        self.hide(r);
                        stack.push(
                            Style::new()
                                .fg(Color::Blue)
                                .add_modifier(Modifier::UNDERLINED),
                        );
                    }
                    Tag::CodeBlock(kind) => {
                        stack.push(Style::new().fg(Color::Yellow));
                        let lines = self.lines_of(&r);
                        for line in lines.clone() {
                            self.mark(line, Block::Code);
                        }
                        if matches!(kind, CodeBlockKind::Fenced(_)) {
                            let fence = |line: usize| {
                                let s = &text[self.starts[line]..self.line_end(line)];
                                let s = s.trim_start_matches(' ');
                                s.starts_with("```") || s.starts_with("~~~")
                            };
                            let (first, last) = (lines.start, lines.end - 1);
                            let (first_fence, last_fence) = (fence(first), fence(last));
                            if first_fence {
                                self.mark(first, Block::Fence);
                            }
                            if last_fence {
                                self.mark(last, Block::Fence);
                            }
                        }
                    }
                    Tag::BlockQuote(_) => {
                        stack.push(Style::new().add_modifier(Modifier::ITALIC));
                        let lines = self.lines_of(&r);
                        for line in lines {
                            if self.blocks[line] == Block::Plain {
                                self.mark(line, Block::Quote);
                            }
                            self.quote_markers(line, r.start);
                        }
                    }
                    Tag::List(_) => {
                        depth += 1;
                        done = false;
                    }
                    Tag::Item => {
                        let line = self.line_of(r.start);
                        self.mark(line, Block::ListItem);
                        let bullet = match text.as_bytes()[r.start] {
                            b'-' | b'*' | b'+' => {
                                self.decos.push(Deco {
                                    range: r.start..r.start + 1,
                                    text: BULLETS[depth.saturating_sub(1) % BULLETS.len()],
                                    style: Style::new().fg(Color::Cyan),
                                    kind: PieceKind::Text,
                                });
                                Some(self.decos.len() - 1)
                            }
                            _ => {
                                let bytes = text.as_bytes();
                                let mut end = r.start;
                                while end < bytes.len() && bytes[end].is_ascii_digit() {
                                    end += 1;
                                }
                                end = (end + 1).min(bytes.len());
                                self.style[r.start..end].fill(Style::new().fg(Color::Cyan));
                                None
                            }
                        };
                        items.push((r.start, bullet));
                    }
                    _ => {}
                },
                Event::End(end) => match end {
                    TagEnd::Heading(_)
                    | TagEnd::Strong
                    | TagEnd::Emphasis
                    | TagEnd::Strikethrough
                    | TagEnd::Link
                    | TagEnd::Image
                    | TagEnd::CodeBlock
                    | TagEnd::BlockQuote(_) => {
                        stack.pop();
                    }
                    TagEnd::List(_) => depth = depth.saturating_sub(1),
                    TagEnd::Item => {
                        items.pop();
                        done = false;
                    }
                    TagEnd::Paragraph => done = false,
                    _ => {}
                },
                Event::Text(_) => {
                    let style = current(&stack, done);
                    self.show(r, style);
                }
                Event::Html(_) | Event::InlineHtml(_) => {
                    let style = current(&stack, done);
                    self.show(r, style);
                }
                Event::Code(_) => {
                    self.hide(r.clone());
                    let s = &text[r.clone()];
                    let lead = s.bytes().take_while(|&b| b == b'`').count();
                    let trail = s.bytes().rev().take_while(|&b| b == b'`').count();
                    if lead + trail < s.len() {
                        let style = current(&stack, done).patch(Style::new().fg(Color::Yellow));
                        self.show(r.start + lead..r.end - trail, style);
                    }
                }
                Event::Rule => {
                    let line = self.line_of(r.start);
                    self.mark(line, Block::Rule);
                }
                Event::TaskListMarker(checked) => {
                    let Some(&(item_start, bullet)) = items.last() else {
                        continue;
                    };
                    let exact = &text[r.clone()];
                    let marker = if matches!(exact, "[ ]" | "[x]" | "[X]") {
                        r.clone()
                    } else {
                        let line = self.line_of(item_start);
                        let end = self.line_end(line);
                        match text[item_start..end].find('[') {
                            Some(p) if end >= item_start + p + 3 => {
                                item_start + p..item_start + p + 3
                            }
                            _ => continue,
                        }
                    };
                    let (glyph, color) = if checked {
                        ("☑", Color::Green)
                    } else {
                        ("☐", Color::Cyan)
                    };
                    let deco = |start: usize| Deco {
                        range: start..marker.end,
                        text: glyph,
                        style: Style::new().fg(color),
                        kind: PieceKind::Checkbox,
                    };
                    match bullet {
                        Some(i) => self.decos[i] = deco(self.decos[i].range.start),
                        None => self.decos.push(deco(marker.start)),
                    }
                    let line = self.line_of(marker.start);
                    let ls = self.starts[line];
                    self.tasks[line] = Some(Task {
                        marker: marker.start - ls..marker.end - ls,
                        checked,
                    });
                    if checked {
                        done = true;
                    }
                }
                _ => {}
            }
        }
    }

    fn view(&self, i: usize, line: &str) -> LineView {
        let block = self.blocks[i];
        let task = self.tasks[i].clone();
        let mut view = LineView {
            block,
            rendered: Vec::new(),
            raw: Vec::new(),
            hang: 0,
            raw_hang: 0,
            task,
        };
        if line.is_empty() {
            return view;
        }
        let whole = |kind: PieceKind| Piece {
            src: 0..line.len(),
            deco: None,
            style: Style::new().add_modifier(Modifier::DIM),
            kind,
        };
        match block {
            Block::Rule => {
                view.rendered = vec![whole(PieceKind::Rule)];
                view.raw = vec![whole(PieceKind::Text)];
                return view;
            }
            Block::Fence => {
                view.rendered = vec![whole(PieceKind::Text)];
                view.raw = vec![whole(PieceKind::Text)];
                return view;
            }
            _ => {}
        }
        let ls = self.starts[i];
        let le = ls + line.len();
        let mut decos: Vec<&Deco> = self
            .decos
            .iter()
            .filter(|d| d.range.start >= ls && d.range.start < le)
            .collect();
        decos.sort_by_key(|d| d.range.start);

        // Rendered: hidden runs dropped, decorations in place of their range.
        let mut p = ls;
        for d in decos.iter().filter(|d| d.range.start >= ls) {
            if d.range.start < p {
                continue;
            }
            self.runs(ls, p..d.range.start, &mut view.rendered);
            let end = d.range.end.min(le);
            view.rendered.push(Piece {
                src: d.range.start - ls..end - ls,
                deco: Some(d.text),
                style: d.style,
                kind: d.kind,
            });
            p = end;
        }
        self.runs(ls, p..le, &mut view.rendered);

        // Raw: every byte, markers dimmed.
        let dim = Modifier::DIM;
        let mut styles: Vec<Style> = (ls..le)
            .map(|b| {
                if self.hidden[b] {
                    self.style[b].add_modifier(dim)
                } else {
                    self.style[b]
                }
            })
            .collect();
        for d in &decos {
            styles[d.range.start - ls..d.range.end.min(le) - ls].fill(d.style.add_modifier(dim));
        }
        let mut b = 0;
        while b < styles.len() {
            let style = styles[b];
            let len = styles[b..].iter().take_while(|&&s| s == style).count();
            view.raw.push(Piece {
                src: b..b + len,
                deco: None,
                style,
                kind: PieceKind::Text,
            });
            b += len;
        }

        let limit = match block {
            Block::ListItem => crate::notes::edit::list_prefix(line).map(|p| p.content),
            Block::Quote => Some(quote_content(line)),
            _ => None,
        };
        if let Some(limit) = limit {
            view.hang = width_before(&view.rendered, line, limit);
            view.raw_hang = width_before(&view.raw, line, limit);
        }
        view
    }

    /// Visible runs of the source range `r` (document offsets) as pieces of
    /// the line starting at `ls`.
    fn runs(&self, ls: usize, r: Range<usize>, out: &mut Vec<Piece>) {
        let mut b = r.start;
        while b < r.end {
            let key = (self.hidden[b], self.style[b]);
            let mut e = b + 1;
            while e < r.end && (self.hidden[e], self.style[e]) == key {
                e += 1;
            }
            if !key.0 {
                out.push(Piece {
                    src: b - ls..e - ls,
                    deco: None,
                    style: key.1,
                    kind: PieceKind::Text,
                });
            }
            b = e;
        }
    }
}

fn heading_style(level: u8) -> Style {
    let bold = Style::new().add_modifier(Modifier::BOLD);
    match level {
        1 => bold.fg(Color::Cyan).add_modifier(Modifier::UNDERLINED),
        2 => bold.fg(Color::Cyan),
        3 => bold.fg(Color::Blue),
        _ => bold,
    }
}

/// Byte offset after the `>` markers of a quote line and one space.
fn quote_content(line: &str) -> usize {
    let bytes = line.as_bytes();
    let mut end = 0;
    let mut p = 0;
    loop {
        while p < bytes.len() && bytes[p] == b' ' {
            p += 1;
        }
        if p < bytes.len() && bytes[p] == b'>' {
            p += 1;
            end = p;
        } else {
            break;
        }
    }
    if bytes.get(end) == Some(&b' ') {
        end += 1;
    }
    end
}

/// Display width of `s`; a tab counts [`TAB_WIDTH`] cells.
fn text_width(s: &str) -> u16 {
    s.chars()
        .map(|c| match c {
            '\t' => usize::from(TAB_WIDTH),
            c => c.width().unwrap_or(0),
        })
        .sum::<usize>()
        .min(usize::from(u16::MAX)) as u16
}

/// Width of the pieces' text before source byte `limit`.
fn width_before(pieces: &[Piece], line: &str, limit: usize) -> u16 {
    let mut width = 0u16;
    for piece in pieces {
        if piece.src.start >= limit {
            break;
        }
        width = width.saturating_add(match piece.deco {
            Some(deco) => text_width(deco),
            None => text_width(&line[piece.src.start..piece.src.end.min(limit)]),
        });
    }
    width
}

/// Renders every line of a note, one [`LineView`] per input line.
pub fn render(lines: &[String]) -> Vec<LineView> {
    let text = lines.join("\n");
    let mut starts = Vec::with_capacity(lines.len());
    let mut at = 0;
    for line in lines {
        starts.push(at);
        at += line.len() + 1;
    }
    let mut doc = Doc {
        text: &text,
        starts,
        style: vec![Style::new(); text.len()],
        hidden: vec![false; text.len()],
        blocks: vec![Block::Plain; lines.len()],
        tasks: vec![None; lines.len()],
        quote_done: vec![false; lines.len()],
        decos: Vec::new(),
    };
    doc.parse();
    lines
        .iter()
        .enumerate()
        .map(|(i, line)| doc.view(i, line))
        .collect()
}

/// `(done, total)` task items.
pub fn counts(views: &[LineView]) -> (usize, usize) {
    let tasks = views.iter().filter_map(|v| v.task.as_ref());
    (tasks.clone().filter(|t| t.checked).count(), tasks.count())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn views(text: &str) -> Vec<LineView> {
        let lines: Vec<String> = text.split('\n').map(str::to_owned).collect();
        render(&lines)
    }

    fn shown(line: &str, pieces: &[Piece]) -> String {
        pieces
            .iter()
            .map(|p| p.deco.unwrap_or(&line[p.src.clone()]))
            .collect()
    }

    fn has(style: Style, m: Modifier) -> bool {
        style.add_modifier.contains(m)
    }

    #[test]
    fn bold_hides_its_markers() {
        let line = "a **bold** b";
        let v = &views(line)[0];
        assert_eq!(shown(line, &v.rendered), "a bold b");
        let bold: Vec<_> = v
            .rendered
            .iter()
            .filter(|p| has(p.style, Modifier::BOLD))
            .collect();
        assert_eq!(bold.len(), 1);
        assert_eq!(&line[bold[0].src.clone()], "bold");
        assert_eq!(shown(line, &v.raw), line);
        let stars: Vec<_> = v
            .raw
            .iter()
            .filter(|p| line[p.src.clone()].contains('*'))
            .collect();
        assert_eq!(stars.len(), 2);
        assert!(
            stars
                .iter()
                .all(|p| has(p.style, Modifier::DIM) && !has(p.style, Modifier::BOLD))
        );
        let word = v
            .raw
            .iter()
            .find(|p| &line[p.src.clone()] == "bold")
            .unwrap();
        assert!(has(word.style, Modifier::BOLD));
    }

    #[test]
    fn heading() {
        let v = &views("# Title")[0];
        assert_eq!(shown("# Title", &v.rendered), "Title");
        assert_eq!(v.block, Block::Heading(1));
        assert_eq!(v.rendered[0].style.fg, Some(Color::Cyan));
    }

    #[test]
    fn open_task() {
        let line = "- [ ] milk";
        let v = &views(line)[0];
        assert_eq!(v.block, Block::ListItem);
        assert_eq!(v.rendered[0].deco, Some("☐"));
        assert_eq!(v.rendered[0].kind, PieceKind::Checkbox);
        assert_eq!(shown(line, &v.rendered), "☐ milk");
        assert_eq!(
            v.task,
            Some(Task {
                marker: 2..5,
                checked: false
            })
        );
        assert_eq!(v.hang, 2);
        assert_eq!(v.raw_hang, 6);
    }

    #[test]
    fn checked_task() {
        let line = "- [x] done";
        let v = &views(line)[0];
        assert_eq!(v.rendered[0].deco, Some("☑"));
        assert!(v.task.as_ref().unwrap().checked);
        let text = v.rendered.last().unwrap();
        assert_eq!(&line[text.src.clone()].trim(), &"done");
        assert!(has(text.style, Modifier::CROSSED_OUT));
    }

    #[test]
    fn nested_bullet_and_ordered() {
        let v = views("- a\n  - b\n    - c\n1. d");
        let bullet = |v: &LineView| v.rendered.iter().find_map(|p| p.deco);
        assert_eq!(bullet(&v[0]), Some("•"));
        assert_eq!(bullet(&v[1]), Some("◦"));
        assert_eq!(bullet(&v[2]), Some("▪"));
        assert_eq!((v[1].hang, v[2].hang), (4, 6));
        assert_eq!(shown("1. d", &v[3].rendered), "1. d");
        assert_eq!(v[3].hang, 3);
    }

    #[test]
    fn inline_code() {
        let line = "a `x` b";
        let v = &views(line)[0];
        assert_eq!(shown(line, &v.rendered), "a x b");
        let code = v
            .rendered
            .iter()
            .find(|p| &line[p.src.clone()] == "x")
            .unwrap();
        assert_eq!(code.style.fg, Some(Color::Yellow));
    }

    #[test]
    fn code_block() {
        let v = views("```rust\nlet x = 1;\n```");
        let blocks: Vec<_> = v.iter().map(|v| v.block).collect();
        assert_eq!(blocks, [Block::Fence, Block::Code, Block::Fence]);
        assert_eq!(v[1].rendered[0].style.fg, Some(Color::Yellow));
    }

    #[test]
    fn no_task_in_code_block() {
        let v = views("```\n- [ ] a\n```");
        assert!(v[1].task.is_none());
        assert_eq!(shown("- [ ] a", &v[1].rendered), "- [ ] a");
    }

    #[test]
    fn rule() {
        let v = views("a\n\n---\n");
        assert_eq!(v[2].block, Block::Rule);
        assert_eq!(v[2].rendered.len(), 1);
        assert_eq!(v[2].rendered[0].kind, PieceKind::Rule);
        assert!(v[3].rendered.is_empty());
    }

    #[test]
    fn quote_and_link() {
        let v = views("> quoted\n\n[a link](http://x)");
        assert_eq!(v[0].block, Block::Quote);
        assert_eq!(shown("> quoted", &v[0].rendered), "▎ quoted");
        assert_eq!(v[0].hang, 2);
        assert_eq!(shown("[a link](http://x)", &v[2].rendered), "a link");
    }

    #[test]
    fn counts_tasks() {
        let v = views("- [x] a\n- [ ] b\n- [X] c\ntext");
        assert_eq!(counts(&v), (2, 3));
    }

    #[test]
    fn raw_pieces_cover_every_line() {
        let doc = "# Head **b**\n\nText with *em*, `code` and [l](u).\n\n> quote\n> > deep\n\n- [ ] a\n  - [x] **b** c\n1. one\n\n```rust\nlet x = 1;\n```\n---\n\n\ttabbed\n<b>html</b>\nSetext\n======\n- a\n  more\n";
        let lines: Vec<String> = doc.split('\n').map(str::to_owned).collect();
        let v = render(&lines);
        assert_eq!(v.len(), lines.len());
        for (line, view) in lines.iter().zip(&v) {
            let mut at = 0;
            for p in &view.raw {
                assert_eq!(p.src.start, at, "{line:?}");
                at = p.src.end;
            }
            assert_eq!(at, line.len(), "{line:?}");
            let mut at = 0;
            for p in &view.rendered {
                assert!(p.src.start >= at && p.src.end <= line.len(), "{line:?}");
                at = p.src.end;
            }
            assert_eq!(view.raw.is_empty(), line.is_empty());
        }
    }
}
