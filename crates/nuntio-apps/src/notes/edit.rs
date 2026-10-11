//! Markdown-aware editing: list continuation, indenting list items and
//! toggling todos.

use std::ops::Range;

use super::buffer::{Buffer, EditKind, Pos};
use super::markdown::{Block, LineView, Task};

/// The start of a list item line: `  - [ ] text`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListPrefix {
    /// Leading spaces.
    pub indent: usize,
    /// `-`, `*`, `+` or `12.` / `12)`.
    pub marker: Range<usize>,
    /// Number and delimiter of an ordered item.
    pub ordered: Option<(u64, char)>,
    /// The `[ ]` box and whether it is checked.
    pub task: Option<(Range<usize>, bool)>,
    /// Byte offset of the content after the prefix.
    pub content: usize,
}

/// The list prefix of `line`:
/// `^( *)([-*+]|\d{1,9}[.)])( +|$)(\[[ xX]\]( +|$))?`.
pub fn list_prefix(line: &str) -> Option<ListPrefix> {
    let bytes = line.as_bytes();
    let indent = bytes.iter().take_while(|&&b| b == b' ').count();
    let mut i = indent;
    let ordered = match bytes.get(i)? {
        b'-' | b'*' | b'+' => {
            i += 1;
            None
        }
        b'0'..=b'9' => {
            let digits = bytes[i..].iter().take_while(|b| b.is_ascii_digit()).count();
            if digits > 9 {
                return None;
            }
            let number = line[i..i + digits].parse().ok()?;
            i += digits;
            let delimiter = match bytes.get(i)? {
                b'.' => '.',
                b')' => ')',
                _ => return None,
            };
            i += 1;
            Some((number, delimiter))
        }
        _ => return None,
    };
    let marker = indent..i;
    let spaces = |from: usize| bytes[from..].iter().take_while(|&&b| b == b' ').count();
    let gap = spaces(i);
    if gap == 0 && i < bytes.len() {
        return None;
    }
    i += gap;
    let mut task = None;
    if gap > 0
        && bytes.len() >= i + 3
        && bytes[i] == b'['
        && matches!(bytes[i + 1], b' ' | b'x' | b'X')
        && bytes[i + 2] == b']'
    {
        let after = spaces(i + 3);
        if after > 0 || i + 3 == bytes.len() {
            task = Some((i..i + 3, bytes[i + 1] != b' '));
            i += 3 + after;
        }
    }
    Some(ListPrefix {
        indent,
        marker,
        ordered,
        task,
        content: i,
    })
}

fn block(views: &[LineView], line: usize) -> Block {
    views.get(line).map_or(Block::Plain, |v| v.block)
}

/// The leading spaces and tabs of `text`.
fn leading_whitespace(text: &str) -> &str {
    &text[..text.len() - text.trim_start_matches([' ', '\t']).len()]
}

/// Byte offset after the last `>` of the quote markers at the start of
/// `line`.
fn quote_end(line: &str) -> Option<usize> {
    let bytes = line.as_bytes();
    let spaces = |from: usize| bytes[from..].iter().take_while(|&&b| b == b' ').count();
    let mut i = spaces(0);
    let mut end = None;
    while bytes.get(i) == Some(&b'>') {
        i += 1;
        end = Some(i);
        i += spaces(i);
    }
    end
}

/// Enter: continue the list or quote, or keep the indentation. One undo
/// step.
pub fn enter(buf: &mut Buffer, views: &[LineView]) {
    buf.begin(EditKind::Other);
    // The block of the cursor line is the same after the selection is gone.
    buf.delete_selection();
    let Pos { line, col } = buf.cursor();
    let text = buf.lines()[line].clone();
    let block = block(views, line);
    if block == Block::ListItem
        && let Some(prefix) = list_prefix(&text)
    {
        return list_break(buf, &text, &prefix, col);
    }
    if matches!(block, Block::Quote | Block::ListItem)
        && let Some(end) = quote_end(&text)
        && col >= end
    {
        if text[end..].trim().is_empty() {
            buf.set_line(line, String::new());
        } else {
            let prefix = format!("{} ", &text[..end]);
            buf.split_line_with(&prefix);
        }
        return;
    }
    plain_break(buf);
}

/// Break the line and copy the indentation before the cursor.
fn plain_break(buf: &mut Buffer) {
    let Pos { line, col } = buf.cursor();
    let indent = leading_whitespace(&buf.lines()[line][..col]).to_owned();
    buf.split_line_with(&indent);
}

fn list_break(buf: &mut Buffer, text: &str, prefix: &ListPrefix, col: usize) {
    let line = buf.cursor().line;
    if text[prefix.content..].trim().is_empty() {
        buf.set_line(line, String::new());
        return;
    }
    if col < prefix.content {
        // Before the content: open a line above and keep the cursor.
        buf.move_to(Pos { line, col: 0 }, false);
        buf.split_line_with("");
        buf.move_to(
            Pos {
                line: line + 1,
                col,
            },
            false,
        );
        return;
    }
    let marker = match prefix.ordered {
        Some((number, delimiter)) => format!("{}{delimiter}", number + 1),
        None => text[prefix.marker.clone()].to_owned(),
    };
    let task = if prefix.task.is_some() { "[ ] " } else { "" };
    let next = format!("{}{marker} {task}", " ".repeat(prefix.indent));
    buf.split_line_with(&next);
}

/// Spaces (or one tab) at the start of `line`, at most `max` spaces.
fn removable_indent(line: &str, max: usize) -> usize {
    if line.starts_with('\t') {
        return 1;
    }
    line.bytes().take_while(|&b| b == b' ').take(max).count()
}

/// Tab: nest the list item under the one above, or insert 4 spaces; with
/// several lines selected indent them all by 2. One undo step.
pub fn indent(buf: &mut Buffer, views: &[LineView]) {
    shift(buf, views, true);
}

/// Shift+Tab: the reverse of [`indent`].
pub fn outdent(buf: &mut Buffer, views: &[LineView]) {
    shift(buf, views, false);
}

fn shift(buf: &mut Buffer, views: &[LineView], deeper: bool) {
    buf.begin(EditKind::Other);
    if let Some((start, end)) = buf.selection()
        && start.line != end.line
    {
        let last = if end.col == 0 { end.line - 1 } else { end.line };
        for line in start.line..=last {
            if deeper {
                if !buf.lines()[line].is_empty() {
                    buf.replace_range(line, 0..0, "  ");
                }
            } else {
                let n = removable_indent(&buf.lines()[line], 2);
                buf.replace_range(line, 0..n, "");
            }
        }
        return;
    }
    let line = buf.cursor().line;
    let text = buf.lines()[line].clone();
    if block(views, line) == Block::ListItem
        && let Some(item) = list_prefix(&text)
    {
        // The items above in the same run of non-blank lines, nearest first.
        let mut above = (0..line)
            .rev()
            .take_while(|&i| !buf.lines()[i].trim().is_empty())
            .filter(|&i| block(views, i) == Block::ListItem)
            .filter_map(|i| list_prefix(&buf.lines()[i]));
        let indent = if deeper {
            match above.next() {
                // Under the content of the item above, but before its box.
                Some(prev) if prev.indent == item.indent => {
                    prev.task.map_or(prev.content, |(range, _)| range.start)
                }
                _ => item.indent + 2,
            }
        } else {
            above
                .find(|prev| prev.indent < item.indent)
                .map_or(item.indent.saturating_sub(2), |prev| prev.indent)
        };
        if indent != item.indent {
            buf.replace_range(line, 0..item.indent, &" ".repeat(indent));
        }
    } else if deeper {
        buf.insert("    ");
    } else {
        let n = removable_indent(&text, 4);
        buf.replace_range(line, 0..n, "");
    }
}

/// Ctrl+T on the cursor line: flip the checkbox, add one to a list item, or
/// make the line a todo. One undo step.
pub fn toggle_todo(buf: &mut Buffer, views: &[LineView]) {
    buf.begin(EditKind::Other);
    let line = buf.cursor().line;
    let block = block(views, line);
    if matches!(block, Block::Code | Block::Fence) {
        return;
    }
    if let Some(task) = views.get(line).and_then(|v| v.task.as_ref()) {
        let at = task.marker.start + 1;
        buf.replace_range(line, at..at + 1, if task.checked { " " } else { "x" });
        return;
    }
    let text = buf.lines()[line].clone();
    let (at, insert) = match list_prefix(&text) {
        Some(prefix) if block == Block::ListItem => {
            let spaced = text[..prefix.content].ends_with(' ');
            (prefix.content, if spaced { "[ ] " } else { " [ ] " })
        }
        _ => {
            let at = match quote_end(&text) {
                Some(end) if block == Block::Quote => {
                    end + usize::from(text[end..].starts_with(' '))
                }
                _ => text.len() - text.trim_start_matches(' ').len(),
            };
            (at, "- [ ] ")
        }
    };
    buf.replace_range(line, at..at, insert);
    if buf.anchor().is_none() && buf.cursor() == (Pos { line, col: at }) {
        buf.move_to(
            Pos {
                line,
                col: at + insert.len(),
            },
            false,
        );
    }
}

/// `line` with the checkbox of `task` flipped; unchanged when the marker
/// isn't a checkbox any more.
pub fn toggle_task_line(line: &str, task: &Task) -> String {
    let Some("[ ]" | "[x]" | "[X]") = line.get(task.marker.clone()) else {
        return line.to_owned();
    };
    let mut toggled = line.to_owned();
    toggled.replace_range(
        task.marker.clone(),
        if task.checked { "[ ]" } else { "[x]" },
    );
    toggled
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notes::markdown::render;

    #[test]
    fn list_prefixes() {
        let p = list_prefix("  - [x] milk").unwrap();
        assert_eq!((p.indent, p.marker, p.ordered), (2, 2..3, None));
        assert_eq!((p.task, p.content), (Some((4..7, true)), 8));
        let p = list_prefix("12) a").unwrap();
        assert_eq!((p.ordered, p.content, p.task), (Some((12, ')')), 4, None));
        assert_eq!(list_prefix("-").unwrap().content, 1);
        assert_eq!(list_prefix("- [ ]").unwrap().task, Some((2..5, false)));
        assert_eq!(list_prefix("- [ ]x").unwrap().task, None);
        assert!(list_prefix("-a").is_none());
        assert!(list_prefix("1234567890. a").is_none());
        assert!(list_prefix("text").is_none());
    }

    fn buffer(text: &str, line: usize, col: usize) -> Buffer {
        let mut buf = Buffer::new(text);
        buf.move_to(Pos { line, col }, false);
        buf
    }

    /// Run `edit` with views of the current text.
    fn run(buf: &mut Buffer, edit: fn(&mut Buffer, &[LineView])) {
        let views = render(buf.lines());
        edit(buf, &views);
    }

    #[test]
    fn enter_continues_a_task_list_with_an_open_box() {
        let mut buf = buffer("- [x] milk", 0, 10);
        run(&mut buf, enter);
        assert_eq!(buf.lines(), ["- [x] milk", "- [ ] "]);
        assert_eq!(buf.cursor(), Pos { line: 1, col: 6 });
    }

    #[test]
    fn enter_counts_up_ordered_lists() {
        let mut buf = buffer("3. a", 0, 4);
        run(&mut buf, enter);
        assert_eq!(buf.lines(), ["3. a", "4. "]);
        let mut buf = buffer("  9) a", 0, 6);
        run(&mut buf, enter);
        assert_eq!(buf.lines(), ["  9) a", "  10) "]);
    }

    #[test]
    fn enter_splits_the_content_at_the_cursor() {
        let mut buf = buffer("- ab", 0, 3);
        run(&mut buf, enter);
        assert_eq!(buf.lines(), ["- a", "- b"]);
        assert_eq!(buf.cursor(), Pos { line: 1, col: 2 });
    }

    #[test]
    fn enter_on_an_empty_item_ends_the_list() {
        let mut buf = buffer("- a\n- ", 1, 2);
        run(&mut buf, enter);
        assert_eq!(buf.lines(), ["- a", ""]);
        assert_eq!(buf.cursor(), Pos { line: 1, col: 0 });
        let mut buf = buffer("- [ ] ", 0, 6);
        run(&mut buf, enter);
        assert_eq!(buf.lines(), [""]);
    }

    #[test]
    fn enter_in_a_quote_continues_the_quote() {
        let mut buf = buffer("> a", 0, 3);
        run(&mut buf, enter);
        assert_eq!(buf.lines(), ["> a", "> "]);
        run(&mut buf, enter);
        assert_eq!(buf.lines(), ["> a", ""]);
    }

    #[test]
    fn enter_elsewhere_keeps_the_indentation_before_the_cursor() {
        let mut buf = buffer("  \tab", 0, 5);
        run(&mut buf, enter);
        assert_eq!(buf.lines(), ["  \tab", "  \t"]);
        let mut buf = buffer("    ab", 0, 2);
        run(&mut buf, enter);
        assert_eq!(buf.lines(), ["  ", "    ab"]);
        let mut buf = buffer("```\n- a\n```", 1, 3);
        run(&mut buf, enter);
        assert_eq!(buf.lines()[1..3], ["- a", ""]);
    }

    #[test]
    fn enter_replaces_a_selection() {
        let mut buf = buffer("- abc", 0, 3);
        buf.move_to(Pos { line: 0, col: 4 }, true);
        run(&mut buf, enter);
        assert_eq!(buf.lines(), ["- a", "- c"]);
    }

    #[test]
    fn tab_nests_a_list_item_and_shift_tab_undoes_it() {
        let mut buf = buffer("1. a\n2. b", 1, 4);
        run(&mut buf, indent);
        assert_eq!(buf.lines(), ["1. a", "   2. b"]);
        assert_eq!(buf.cursor(), Pos { line: 1, col: 7 });
        run(&mut buf, outdent);
        assert_eq!(buf.lines(), ["1. a", "2. b"]);
        assert_eq!(buf.cursor(), Pos { line: 1, col: 4 });
    }

    #[test]
    fn tab_under_a_task_nests_before_its_box() {
        let mut buf = buffer("- [ ] a\n- b", 1, 3);
        run(&mut buf, indent);
        assert_eq!(buf.lines(), ["- [ ] a", "  - b"]);
        // Nested already: 2 more spaces.
        run(&mut buf, indent);
        assert_eq!(buf.lines(), ["- [ ] a", "    - b"]);
        // Shift+Tab goes to the indent of the nearest less deep item above.
        run(&mut buf, outdent);
        assert_eq!(buf.lines(), ["- [ ] a", "- b"]);
    }

    #[test]
    fn shift_tab_goes_to_the_enclosing_item() {
        let mut buf = buffer("- a\n   - b\n   - c", 2, 5);
        run(&mut buf, outdent);
        assert_eq!(buf.lines()[2], "- c");
    }

    #[test]
    fn tab_on_plain_text_inserts_spaces() {
        let mut buf = buffer("ab", 0, 1);
        run(&mut buf, indent);
        assert_eq!(buf.lines(), ["a    b"]);
        let mut buf = buffer("      ab", 0, 6);
        run(&mut buf, outdent);
        assert_eq!(buf.lines(), ["  ab"]);
        assert_eq!(buf.cursor().col, 2);
    }

    #[test]
    fn tab_with_several_lines_selected_shifts_each_by_two() {
        let mut buf = buffer("a\n\nb\nc", 0, 0);
        buf.move_to(Pos { line: 3, col: 1 }, true);
        run(&mut buf, indent);
        assert_eq!(buf.lines(), ["  a", "", "  b", "  c"]);
        assert_eq!(buf.anchor(), Some(Pos { line: 0, col: 0 }));
        run(&mut buf, outdent);
        assert_eq!(buf.lines(), ["a", "", "b", "c"]);
    }

    #[test]
    fn a_selection_ending_at_a_line_start_leaves_that_line() {
        let mut buf = buffer("a\nb\nc", 0, 0);
        buf.move_to(Pos { line: 2, col: 0 }, true);
        run(&mut buf, indent);
        assert_eq!(buf.lines(), ["  a", "  b", "c"]);
    }

    #[test]
    fn toggling_is_one_undo_step() {
        let mut buf = buffer("- [ ] a\nb", 0, 3);
        run(&mut buf, toggle_todo);
        assert_eq!(buf.lines()[0], "- [x] a");
        assert_eq!(buf.cursor().col, 3);
        assert!(buf.undo());
        assert_eq!(buf.lines()[0], "- [ ] a");
    }

    #[test]
    fn ctrl_t_turns_a_line_into_a_todo_and_flips_it() {
        let mut buf = buffer("buy milk", 0, 3);
        run(&mut buf, toggle_todo);
        assert_eq!(buf.lines(), ["- [ ] buy milk"]);
        assert_eq!(buf.cursor().col, 9);
        run(&mut buf, toggle_todo);
        assert_eq!(buf.lines(), ["- [x] buy milk"]);
        run(&mut buf, toggle_todo);
        assert_eq!(buf.lines(), ["- [ ] buy milk"]);
    }

    #[test]
    fn ctrl_t_adds_a_box_to_list_items_and_blank_lines() {
        let mut buf = buffer("  - milk", 0, 8);
        run(&mut buf, toggle_todo);
        assert_eq!(buf.lines(), ["  - [ ] milk"]);
        let mut buf = buffer("- ", 0, 2);
        run(&mut buf, toggle_todo);
        assert_eq!(buf.lines(), ["- [ ] "]);
        assert_eq!(buf.cursor().col, 6);
        let mut buf = buffer("", 0, 0);
        run(&mut buf, toggle_todo);
        assert_eq!(buf.lines(), ["- [ ] "]);
        assert_eq!(buf.cursor().col, 6);
        let mut buf = buffer("> quote", 0, 0);
        run(&mut buf, toggle_todo);
        assert_eq!(buf.lines(), ["> - [ ] quote"]);
    }

    #[test]
    fn ctrl_t_treats_a_capital_x_as_checked_and_ignores_code() {
        let mut buf = buffer("- [X] a", 0, 0);
        run(&mut buf, toggle_todo);
        assert_eq!(buf.lines(), ["- [ ] a"]);
        let mut buf = buffer("```\nfoo\n```", 1, 0);
        let revision = buf.revision();
        run(&mut buf, toggle_todo);
        assert_eq!(buf.lines()[1], "foo");
        assert_eq!(buf.revision(), revision);
    }

    #[test]
    fn toggle_task_line_flips_the_marker() {
        let views = render(&["- [ ] a".to_owned(), "- [x] b [ ]".to_owned()]);
        let task = views[0].task.as_ref().unwrap();
        assert_eq!(toggle_task_line("- [ ] a", task), "- [x] a");
        let task = views[1].task.as_ref().unwrap();
        assert_eq!(toggle_task_line("- [x] b [ ]", task), "- [ ] b [ ]");
        // A line that changed under the marker stays as it is.
        assert_eq!(toggle_task_line("- text", task), "- text");
    }
}
