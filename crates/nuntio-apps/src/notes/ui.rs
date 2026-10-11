//! Drawing the notes app with ratatui. Decisions live in `state`; this lays
//! out the areas and draws what `App` reports. Only ANSI colors and
//! modifiers, so the app follows the terminal's theme.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect, Size};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};
use unicode_width::UnicodeWidthStr;

use super::layout::Row;
use super::markdown::{self, PieceKind};
use super::state::{App, Focus, Prompt, TodoRow, Tone};
use super::widgets::TextInput;

const ACCENT: Color = Color::Cyan;
/// Width below which only the list or only the editor is shown.
const NARROW: u16 = 60;
/// Column of a todo's box in the overview, from the left of the body.
pub const TODO_BOX_X: u16 = 4;

/// Where things are on the screen; computed by `App::prepare`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Areas {
    pub header: Rect,
    pub body: Rect,
    pub footer: Rect,
    /// The note list, without its border.
    pub list: Option<Rect>,
    pub editor: Option<Rect>,
    /// The note's text in the editor, below the title.
    pub text: Option<Rect>,
    pub too_small: bool,
}

pub fn areas(size: Size, app: &App) -> Areas {
    let full = Rect::new(0, 0, size.width, size.height);
    if size.width < 40 || size.height < 8 {
        return Areas {
            too_small: true,
            ..Areas::default()
        };
    }
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .areas(full);
    let mut areas = Areas {
        header,
        body,
        footer,
        ..Areas::default()
    };
    if app.todos.is_some() || app.fatal.is_some() {
        return areas;
    }
    if size.width < NARROW {
        match app.focus {
            Focus::List => areas.list = Some(body),
            Focus::Editor => areas.editor = Some(body),
        }
    } else if app.sidebar {
        let width = (size.width * 3 / 10).clamp(22, 34);
        let [list, editor] =
            Layout::horizontal([Constraint::Length(width), Constraint::Min(1)]).areas(body);
        // The last column is the border.
        areas.list = Some(Rect {
            width: list.width - 1,
            ..list
        });
        areas.editor = Some(editor);
    } else {
        areas.editor = Some(body);
    }
    areas.text = areas.editor.map(|e| Rect {
        x: e.x + 2,
        y: e.y + 2,
        width: e.width.saturating_sub(3),
        height: e.height.saturating_sub(2),
    });
    areas
}

fn tone(tone: Tone) -> Style {
    match tone {
        Tone::Dim => Style::new().add_modifier(Modifier::DIM),
        Tone::Warn => Style::new().fg(Color::Yellow),
        Tone::Error => Style::new().fg(Color::Red),
    }
}

fn dim() -> Style {
    Style::new().add_modifier(Modifier::DIM)
}

fn center_line(frame: &mut Frame, area: Rect, lines: Vec<Line>) {
    let height = lines.len() as u16;
    let [_, middle, _] = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(height),
        Constraint::Fill(1),
    ])
    .areas(area);
    frame.render_widget(Paragraph::new(lines).centered(), middle);
}

/// `text` cut to `width` cells, with `…` if it was longer.
fn truncate(text: &str, width: usize) -> String {
    if text.width() <= width {
        return text.to_owned();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in text.chars() {
        let w = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if used + w + 1 > width {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push('…');
    out
}

pub fn draw(frame: &mut Frame, app: &App) {
    let areas = app.areas;
    if areas.too_small {
        center_line(frame, frame.area(), vec![Line::from("Terminal too small")]);
        return;
    }
    draw_header(frame, app);
    if let Some(message) = &app.fatal {
        center_line(
            frame,
            areas.body,
            vec![
                Line::styled(message.clone(), tone(Tone::Error)),
                Line::from(""),
                Line::styled("q quits", dim()),
            ],
        );
        return;
    }
    if app.todos.is_some() {
        draw_todos(frame, app);
    } else {
        if let Some(list) = areas.list {
            draw_list(frame, app, list);
            if areas.editor.is_some() {
                let border = Rect {
                    x: list.right(),
                    width: 1,
                    ..list
                };
                let bar = vec![Line::styled("│", dim()); border.height as usize];
                frame.render_widget(Paragraph::new(bar), border);
            }
        }
        if let Some(editor) = areas.editor {
            draw_editor(frame, app, editor);
        }
    }
    draw_footer(frame, app);
    if let Some(prompt) = &app.prompt {
        draw_prompt(frame, prompt, app);
    }
}

fn draw_header(frame: &mut Frame, app: &App) {
    let area = app.areas.header;
    let left = Line::from(vec![
        Span::styled(
            " Notes ",
            Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
        ),
        Span::styled(app.dir_label.clone(), dim()),
    ]);
    frame.render_widget(Paragraph::new(left), area);
    if let Some(open) = &app.open {
        let mut spans = vec![Span::raw(open.title.clone())];
        if open.dirty() {
            spans.push(Span::styled(" ●", Style::new().fg(Color::Yellow)));
        }
        spans.push(Span::raw(" "));
        let right = Line::from(spans).right_aligned();
        let width = right.width() as u16;
        let left_width = 7 + app.dir_label.width() as u16 + 2;
        if width + left_width <= area.width {
            frame.render_widget(Paragraph::new(right), area);
        }
    }
}

fn draw_list(frame: &mut Frame, app: &App, area: Rect) {
    let mut area = area;
    if let Some(filter) = &app.filter {
        let line = Line::from(vec![
            Span::styled("/ ", Style::new().fg(ACCENT)),
            Span::raw(filter.text().to_owned()),
        ]);
        frame.render_widget(Paragraph::new(line), Rect { height: 1, ..area });
        if app.filtering && app.prompt.is_none() {
            frame.set_cursor_position((area.x + 2 + filter.cursor_width(), area.y));
        }
        area.y += 1;
        area.height = area.height.saturating_sub(1);
    }
    let visible = app.visible();
    if app.notes.is_empty() {
        center_line(
            frame,
            area,
            vec![
                Line::styled("No notes yet", dim()),
                Line::styled("Ctrl+N creates one", dim()),
            ],
        );
        return;
    }
    if visible.is_empty() {
        center_line(frame, area, vec![Line::styled("No matches", dim())]);
        return;
    }
    let width = area.width as usize;
    for (row, &i) in visible
        .iter()
        .enumerate()
        .skip(app.list_scroll)
        .take(area.height as usize)
    {
        let entry = &app.notes[i];
        let progress = (entry.total > 0).then(|| format!("{}/{}", entry.done, entry.total));
        let progress_width = progress.as_ref().map_or(0, |p| p.len() + 1);
        let title = truncate(&entry.title, width.saturating_sub(progress_width + 2));
        let mut spans = vec![Span::raw(format!(" {title}"))];
        let used = 1 + title.width();
        if let Some(progress) = progress {
            let style = if entry.done == entry.total {
                Style::new().fg(Color::Green)
            } else {
                dim()
            };
            let gap = width.saturating_sub(used + progress.len() + 1);
            spans.push(Span::raw(" ".repeat(gap)));
            spans.push(Span::styled(progress, style));
            spans.push(Span::raw(" "));
        } else {
            spans.push(Span::raw(" ".repeat(width.saturating_sub(used))));
        }
        let mut line = Line::from(spans);
        if row == app.selected {
            line = line.style(if app.focus == Focus::List && app.todos.is_none() {
                Style::new().add_modifier(Modifier::REVERSED)
            } else {
                Style::new().add_modifier(Modifier::BOLD)
            });
        }
        let y = area.y + (row - app.list_scroll) as u16;
        frame.render_widget(
            Paragraph::new(line),
            Rect {
                y,
                height: 1,
                ..area
            },
        );
    }
}

fn draw_editor(frame: &mut Frame, app: &App, area: Rect) {
    let Some(open) = &app.open else {
        center_line(frame, area, vec![Line::styled("No note open", dim())]);
        return;
    };
    let title = Rect {
        x: area.x + 2,
        width: area.width.saturating_sub(3),
        height: 1,
        ..area
    };
    frame.render_widget(
        Paragraph::new(Line::styled(
            truncate(&open.title, title.width as usize),
            Style::new()
                .fg(ACCENT)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        )),
        title,
    );
    let Some(text) = app.areas.text else {
        return;
    };
    let lines = open.buffer.lines();
    let selection = open.buffer.selection();
    for (i, row) in app
        .rows
        .iter()
        .enumerate()
        .skip(app.scroll)
        .take(text.height as usize)
    {
        let y = text.y + (i - app.scroll) as u16;
        let line = &lines[row.line];
        let selected = selection.and_then(|(start, end)| {
            if row.line < start.line || row.line > end.line {
                return None;
            }
            let from = if row.line == start.line { start.col } else { 0 };
            let to = if row.line == end.line {
                end.col
            } else {
                usize::MAX
            };
            Some(from..to)
        });
        let spans = row_spans(row, line, text.width, selected);
        let area = Rect {
            x: text.x + row.x,
            y,
            width: text.width.saturating_sub(row.x),
            height: 1,
        };
        frame.render_widget(Paragraph::new(Line::from(spans)), area);
    }
    if let Some(at) = app.cursor_xy {
        frame.set_cursor_position(at);
    }
}

/// Source text as drawn: tabs as 4 spaces.
fn shown(text: &str) -> String {
    text.replace('\t', "    ")
}

fn row_spans(
    row: &Row,
    line: &str,
    width: u16,
    selected: Option<std::ops::Range<usize>>,
) -> Vec<Span<'static>> {
    let reversed = |style: Style| style.add_modifier(Modifier::REVERSED);
    let mut spans = Vec::new();
    for piece in &row.pieces {
        if piece.kind == PieceKind::Rule {
            spans.push(Span::styled("─".repeat(width as usize), piece.style));
            continue;
        }
        if let Some(deco) = piece.deco {
            let inside = selected
                .as_ref()
                .is_some_and(|s| s.start <= piece.src.start && piece.src.end <= s.end);
            let style = if inside {
                reversed(piece.style)
            } else {
                piece.style
            };
            spans.push(Span::styled(deco, style));
            continue;
        }
        let src = piece.src.clone();
        match &selected {
            Some(s) if s.start < src.end && src.start < s.end => {
                let a = s.start.clamp(src.start, src.end);
                let b = s.end.clamp(src.start, src.end);
                for (range, sel) in [(src.start..a, false), (a..b, true), (b..src.end, false)] {
                    if range.is_empty() {
                        continue;
                    }
                    let text = shown(&line[range]);
                    let style = if sel {
                        reversed(piece.style)
                    } else {
                        piece.style
                    };
                    spans.push(Span::styled(text, style));
                }
            }
            _ => spans.push(Span::styled(shown(&line[src]), piece.style)),
        }
    }
    spans
}

fn draw_todos(frame: &mut Frame, app: &App) {
    let Some(todos) = &app.todos else {
        return;
    };
    let body = app.areas.body;
    let header = Line::from(vec![
        Span::styled(
            " Todos ",
            Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("{} open", todos.open_count()), dim()),
        Span::styled(
            if todos.show_done {
                "  (done shown)"
            } else {
                ""
            },
            dim(),
        ),
    ]);
    frame.render_widget(Paragraph::new(header), Rect { height: 1, ..body });
    let list = Rect {
        y: body.y + 2,
        height: body.height.saturating_sub(2),
        ..body
    };
    if todos.items.is_empty() {
        center_line(frame, list, vec![Line::styled("No open todos", dim())]);
        return;
    }
    for (i, row) in todos
        .rows()
        .into_iter()
        .enumerate()
        .skip(todos.scroll)
        .take(list.height as usize)
    {
        let y = list.y + (i - todos.scroll) as u16;
        let area = Rect {
            y,
            height: 1,
            ..list
        };
        let line = match row {
            TodoRow::Note(item) => Line::styled(
                format!(" {}", todos.items[item].title),
                Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
            ),
            TodoRow::Item(item) => {
                let text = todos.items[item].text.trim_start().to_owned();
                let views = markdown::render(std::slice::from_ref(&text));
                let mut spans = vec![Span::raw(" ".repeat(TODO_BOX_X as usize))];
                let mut box_seen = false;
                for piece in &views[0].rendered {
                    if !box_seen && piece.kind != PieceKind::Checkbox {
                        // The bullet before the box is left out.
                        continue;
                    }
                    box_seen = true;
                    let shown = piece
                        .deco
                        .map_or_else(|| shown(&text[piece.src.clone()]), str::to_owned);
                    spans.push(Span::styled(shown, piece.style));
                }
                let mut line = Line::from(spans);
                if item == todos.selected {
                    line = line.style(Style::new().add_modifier(Modifier::REVERSED));
                }
                line
            }
        };
        frame.render_widget(Paragraph::new(line), area);
    }
}

fn hints(pairs: &[(&str, &str)]) -> Line<'static> {
    let mut spans = vec![Span::raw(" ")];
    for (i, (key, what)) in pairs.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" · ", dim()));
        }
        spans.push(Span::styled((*key).to_owned(), Style::new().fg(ACCENT)));
        spans.push(Span::styled(format!(" {what}"), dim()));
    }
    Line::from(spans)
}

fn draw_footer(frame: &mut Frame, app: &App) {
    let line = if let Some((t, message)) = &app.message {
        Line::styled(format!(" {message}"), tone(*t))
    } else if app.todos.is_some() {
        hints(&[
            ("Space", "toggle"),
            ("Enter", "open"),
            ("a", "done"),
            ("Esc", "back"),
        ])
    } else if app.focus == Focus::Editor && app.open.is_some() {
        hints(&[
            ("Esc", "list"),
            ("Ctrl+T", "todo"),
            ("Ctrl+O", "todos"),
            ("Ctrl+S", "save"),
            ("F1", "help"),
        ])
    } else {
        hints(&[
            ("Enter", "edit"),
            ("Ctrl+N", "new"),
            ("r", "rename"),
            ("d", "delete"),
            ("Ctrl+O", "todos"),
            ("F1", "help"),
            ("q", "quit"),
        ])
    };
    frame.render_widget(Paragraph::new(line), app.areas.footer);
}

/// A centered rect of at most `width`×`height` in `area`.
fn popup(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

/// A bordered popup in the middle of the screen, over what is below it.
/// Returns the area inside the border.
fn open_popup(frame: &mut Frame, width: u16, height: u16, title: &str) -> Rect {
    let area = popup(frame.area(), width, height);
    frame.render_widget(Clear, area);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .title(format!(" {title} "))
        .border_style(Style::new().fg(ACCENT));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    inner
}

fn draw_input(frame: &mut Frame, title: &str, input: &TextInput, error: Option<&str>) {
    let inner = open_popup(frame, 50, 4 + u16::from(error.is_some()), title);
    let mut lines = vec![Line::from(vec![
        Span::styled("› ", Style::new().fg(ACCENT)),
        Span::raw(input.text().to_owned()),
    ])];
    if let Some(error) = error {
        lines.push(Line::styled(error.to_owned(), tone(Tone::Error)));
    }
    lines.push(hints(&[("Enter", "ok"), ("Esc", "cancel")]));
    frame.render_widget(Paragraph::new(lines), inner);
    let x = (inner.x + 2 + input.cursor_width()).min(inner.right().saturating_sub(1));
    frame.set_cursor_position((x, inner.y));
}

fn draw_message(frame: &mut Frame, title: &str, lines: Vec<Line>) {
    let width = lines.iter().map(Line::width).max().unwrap_or(0) as u16 + 4;
    let inner = open_popup(frame, width.max(30), lines.len() as u16 + 2, title);
    frame.render_widget(
        Paragraph::new(lines),
        Rect {
            x: inner.x + 1,
            width: inner.width.saturating_sub(1),
            ..inner
        },
    );
}

const HELP: &[(&str, &str)] = &[
    ("Ctrl+N", "New note"),
    ("Ctrl+O", "Todo overview"),
    ("Ctrl+B", "Show or hide the list"),
    ("Ctrl+S", "Save now"),
    ("Ctrl+Q", "Quit"),
    ("", ""),
    ("List", ""),
    ("↑ ↓ PgUp PgDn", "Choose a note"),
    ("Enter", "Edit the note"),
    ("/", "Filter by title"),
    ("r  F2", "Rename"),
    ("d  Delete", "Delete (to .trash)"),
    ("q  Esc", "Quit"),
    ("", ""),
    ("Editor", ""),
    ("Ctrl+T", "Toggle a todo"),
    ("Tab  Shift+Tab", "Indent, outdent"),
    ("Shift+arrows", "Select"),
    ("Ctrl+A", "Select all"),
    ("Ctrl+C X V", "Copy, cut, paste"),
    ("Ctrl+Z Y", "Undo, redo"),
    ("Esc", "Back to the list"),
    ("", ""),
    ("Todos", ""),
    ("Space", "Toggle"),
    ("Enter", "Open the note there"),
    ("a", "Show or hide done todos"),
];

fn draw_prompt(frame: &mut Frame, prompt: &Prompt, app: &App) {
    match prompt {
        Prompt::NewNote(input, error) => draw_input(frame, "New note", input, *error),
        Prompt::Rename(input, error) => draw_input(frame, "Rename", input, *error),
        Prompt::Delete => {
            let visible = app.visible();
            let title = visible
                .get(app.selected)
                .map_or("", |&i| app.notes[i].title.as_str());
            draw_message(
                frame,
                "Delete",
                vec![
                    Line::from(format!("Move {title} to the trash?")),
                    hints(&[("y", "delete"), ("n", "keep")]),
                ],
            );
        }
        Prompt::Conflict => draw_message(
            frame,
            "Changed on disk",
            vec![
                Line::from("The note was changed by another program."),
                hints(&[("o", "overwrite"), ("r", "reload"), ("Esc", "keep editing")]),
            ],
        ),
        Prompt::QuitUnsaved(reason) => draw_message(
            frame,
            "Quit",
            vec![
                Line::styled(reason.clone(), tone(Tone::Error)),
                Line::from("Quit without saving?"),
                hints(&[("y", "quit"), ("n", "stay")]),
            ],
        ),
        Prompt::Help => {
            let lines = HELP
                .iter()
                .map(|(key, what)| {
                    if what.is_empty() {
                        Line::styled((*key).to_owned(), Style::new().add_modifier(Modifier::BOLD))
                    } else {
                        Line::from(vec![
                            Span::styled(format!("{key:<16}"), Style::new().fg(ACCENT)),
                            Span::raw((*what).to_owned()),
                        ])
                    }
                })
                .collect();
            draw_message(frame, "Keys", lines);
        }
    }
}
