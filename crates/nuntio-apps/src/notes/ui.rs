//! Drawing the notes app with ratatui. Decisions live in `state`; this lays
//! out the areas and draws what `App` reports. Only ANSI colors and
//! modifiers, so the app follows the terminal's theme.

use nuntio_config::schema::NOTES_SIDEBAR_WIDTH;
use nuntio_tui::popup;
use nuntio_tui::style::{ACCENT, Tone, border, center, heading, hints, panel, too_small};
use nuntio_tui::text_input::TextInput;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect, Size};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

use super::failed::Failed;
use super::layout::Row;
use super::markdown::{self, PieceKind};
use super::state::{App, Focus, Prompt, TodoRow};

/// Width below which only the list or only the editor is shown.
const NARROW: u16 = 60;
/// Columns the editor keeps at least next to the list.
const MIN_EDITOR: u16 = 30;
/// Column of a todo's box in the overview, from the left of the body.
pub const TODO_BOX_X: u16 = 5;

/// Where things are on the screen; computed by `App::prepare`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Areas {
    pub header: Rect,
    /// Below the header, frame included.
    pub body: Rect,
    pub footer: Rect,
    /// The note list, without its border.
    pub list: Option<Rect>,
    pub editor: Option<Rect>,
    /// The note's text in the editor.
    pub text: Option<Rect>,
    pub too_small: bool,
}

/// The widest the list can be in a body `body_width` columns wide,
/// frame included.
pub fn max_list_width(body_width: u16) -> u16 {
    NOTES_SIDEBAR_WIDTH
        .1
        .min(body_width.saturating_sub(2 + 1 + MIN_EDITOR))
}

/// Header, body and footer of a screen of `size`; `None` if it is too small.
fn split(size: Size) -> Option<[Rect; 3]> {
    if size.width < 40 || size.height < 8 {
        return None;
    }
    Some(
        Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .areas(Rect::new(0, 0, size.width, size.height)),
    )
}

pub fn areas(size: Size, app: &App) -> Areas {
    let Some([header, body, footer]) = split(size) else {
        return Areas {
            too_small: true,
            ..Areas::default()
        };
    };
    let mut areas = Areas {
        header,
        body,
        footer,
        ..Areas::default()
    };
    if app.todos.is_some() {
        return areas;
    }
    // Inside the frame; the body is at least 40×6.
    let inner = Rect {
        x: body.x + 1,
        y: body.y + 1,
        width: body.width - 2,
        height: body.height - 2,
    };
    if size.width < NARROW {
        match app.focus {
            Focus::List => areas.list = Some(inner),
            Focus::Editor => areas.editor = Some(inner),
        }
    } else if app.sidebar {
        let width = app.sidebar_width.min(max_list_width(body.width));
        areas.list = Some(Rect { width, ..inner });
        // The column after the list is the divider.
        areas.editor = Some(Rect {
            x: inner.x + width + 1,
            width: inner.width - width - 1,
            ..inner
        });
    } else {
        areas.editor = Some(inner);
    }
    areas.text = areas.editor.map(|e| Rect {
        x: e.x + 1,
        y: e.y,
        width: e.width.saturating_sub(2),
        height: e.height,
    });
    areas
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

/// The screen of a notes app that cannot start: the reason, centered.
pub fn draw_failed(frame: &mut Frame, failed: &Failed) {
    let Some([header, body, _]) = split(frame.area().as_size()) else {
        too_small(frame);
        return;
    };
    draw_title(frame, header, &failed.dir_label);
    center(
        frame,
        body,
        vec![
            Line::styled(failed.message.clone(), Tone::Error.style()),
            Line::from(""),
            Line::styled("q quits", Tone::Dim.style()),
        ],
    );
}

pub fn draw(frame: &mut Frame, app: &App) {
    let areas = app.areas;
    if areas.too_small {
        too_small(frame);
        return;
    }
    draw_header(frame, app);
    if app.todos.is_some() {
        draw_todos(frame, app);
    } else {
        let body = areas.body;
        frame.render_widget(panel(false), body);
        if let (Some(list), Some(_)) = (areas.list, areas.editor) {
            let mut bar = vec![Line::raw("┬")];
            bar.extend((list.y..list.bottom()).map(|_| Line::raw("│")));
            bar.push(Line::raw("┴"));
            let divider = Rect {
                x: list.right(),
                width: 1,
                ..body
            };
            frame.render_widget(Paragraph::new(bar).style(border(app.resizing)), divider);
        }
        // The list has no title of its own: the ` Notes ` pill is right above it.
        if let Some(list) = areas.list {
            draw_list(frame, app, list);
        }
        if let Some(editor) = areas.editor {
            draw_editor(frame, app, editor);
            if let Some(open) = &app.open {
                // At least one `─` stays before the corner.
                let max = editor.width.saturating_sub(1);
                let style = title_style(app.focus == Focus::Editor);
                let title = truncate(&open.title, usize::from(max.saturating_sub(5)));
                let mut spans = vec![Span::styled(" ", style), Span::styled(title, style)];
                if open.dirty() {
                    spans.push(Span::styled(" ●", Style::new().fg(Color::Yellow)));
                }
                spans.push(Span::styled(" ", style));
                border_title(frame, editor.x, body.y, max, spans);
            }
        }
    }
    draw_footer(frame, app);
    if let Some(prompt) = &app.prompt {
        draw_prompt(frame, prompt, app);
    }
}

/// A panel's title in the frame: in the accent color while it has the focus.
fn title_style(focused: bool) -> Style {
    if focused {
        heading()
    } else {
        Tone::Dim.style()
    }
}

/// A title in the top border at `x`: `spans` cut to `max` cells.
fn border_title(frame: &mut Frame, x: u16, y: u16, max: u16, spans: Vec<Span<'static>>) {
    let line = Line::from(spans);
    let width = max.min(line.width() as u16);
    frame.render_widget(
        Paragraph::new(line),
        Rect {
            x,
            y,
            width,
            height: 1,
        },
    );
}

/// The ` Notes ` pill and the notes folder.
fn draw_title(frame: &mut Frame, area: Rect, dir_label: &str) {
    let left = Line::from(vec![
        Span::styled(
            " Notes ",
            Style::new()
                .fg(ACCENT)
                .add_modifier(Modifier::BOLD | Modifier::REVERSED),
        ),
        Span::raw(" "),
        Span::styled(dir_label.to_owned(), Tone::Dim.style()),
    ]);
    frame.render_widget(Paragraph::new(left), area);
}

fn draw_header(frame: &mut Frame, app: &App) {
    let area = app.areas.header;
    draw_title(frame, area, &app.dir_label);
    let count = match app.notes.len() {
        1 => "1 note ".to_owned(),
        n => format!("{n} notes "),
    };
    if 7 + 1 + app.dir_label.width() + 1 + count.width() <= usize::from(area.width) {
        frame.render_widget(
            Paragraph::new(Line::styled(count, Tone::Dim.style()).right_aligned()),
            area,
        );
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
        center(
            frame,
            area,
            vec![
                Line::styled("No notes yet", Tone::Dim.style()),
                Line::styled("Ctrl+N creates one", Tone::Dim.style()),
            ],
        );
        return;
    }
    if visible.is_empty() {
        center(
            frame,
            area,
            vec![Line::styled("No matches", Tone::Dim.style())],
        );
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
        let (marker, name_style) = match (
            row == app.selected,
            app.focus == Focus::List && app.todos.is_none(),
        ) {
            (false, _) => (Span::raw(" "), Style::new()),
            (true, true) => (Span::styled("▌", Style::new().fg(ACCENT)), heading()),
            (true, false) => (
                Span::styled("▌", Tone::Dim.style()),
                Style::new().add_modifier(Modifier::BOLD),
            ),
        };
        let used = 1 + title.width();
        let mut spans = vec![marker, Span::styled(title, name_style)];
        if let Some(progress) = progress {
            let style = if entry.done == entry.total {
                Style::new().fg(Color::Green)
            } else {
                Tone::Dim.style()
            };
            let gap = width.saturating_sub(used + progress.len() + 1);
            spans.push(Span::raw(" ".repeat(gap)));
            spans.push(Span::styled(progress, style));
            spans.push(Span::raw(" "));
        } else {
            spans.push(Span::raw(" ".repeat(width.saturating_sub(used))));
        }
        let line = Line::from(spans);
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
        center(
            frame,
            area,
            vec![Line::styled("No note open", Tone::Dim.style())],
        );
        return;
    };
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

/// Source text as drawn: tabs as `TAB_WIDTH` spaces.
fn shown(text: &str) -> String {
    text.replace('\t', &" ".repeat(usize::from(markdown::TAB_WIDTH)))
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
    frame.render_widget(panel(false), body);
    let mut title = vec![
        Span::styled(" Todos ", heading()),
        Span::styled(format!("{} open ", todos.open_count()), Tone::Dim.style()),
    ];
    if todos.show_done {
        title.push(Span::styled("· done shown ", Tone::Dim.style()));
    }
    border_title(
        frame,
        body.x + 1,
        body.y,
        body.width.saturating_sub(2),
        title,
    );
    let list = Rect {
        x: body.x + 1,
        y: body.y + 1,
        width: body.width.saturating_sub(2),
        height: body.height.saturating_sub(2),
    };
    if todos.items.is_empty() {
        center(
            frame,
            list,
            vec![Line::styled("No open todos", Tone::Dim.style())],
        );
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
                Style::new().add_modifier(Modifier::BOLD),
            ),
            TodoRow::Item(item) => {
                let text = todos.items[item].text.trim_start().to_owned();
                let views = markdown::render(std::slice::from_ref(&text));
                let selected = item == todos.selected;
                // The box lands at `TODO_BOX_X` from the left of the body.
                let lead = usize::from(TODO_BOX_X - 1);
                let mut spans = if selected {
                    vec![
                        Span::styled("▌", Style::new().fg(ACCENT)),
                        Span::raw(" ".repeat(lead - 1)),
                    ]
                } else {
                    vec![Span::raw(" ".repeat(lead))]
                };
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
                if selected {
                    line = line.style(Style::new().add_modifier(Modifier::BOLD));
                }
                line
            }
        };
        frame.render_widget(Paragraph::new(line), area);
    }
}

fn draw_footer(frame: &mut Frame, app: &App) {
    let line = if let Some((t, message)) = &app.message {
        Line::styled(format!(" {message}"), t.style())
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

fn draw_input(frame: &mut Frame, title: &str, input: &TextInput, error: Option<&str>) {
    let inner = popup::open(frame, 50, 4 + u16::from(error.is_some()), title);
    let mut lines = vec![Line::from(vec![
        Span::styled("› ", Style::new().fg(ACCENT)),
        Span::raw(input.text().to_owned()),
    ])];
    if let Some(error) = error {
        lines.push(Line::styled(error.to_owned(), Tone::Error.style()));
    }
    lines.push(hints(&[("Enter", "ok"), ("Esc", "cancel")]));
    frame.render_widget(Paragraph::new(lines), inner);
    let x = (inner.x + 2 + input.cursor_width()).min(inner.right().saturating_sub(1));
    frame.set_cursor_position((x, inner.y));
}

fn draw_message(frame: &mut Frame, title: &str, lines: Vec<Line>) {
    let width = lines.iter().map(Line::width).max().unwrap_or(0) as u16 + 4;
    let inner = popup::open(frame, width.max(30), lines.len() as u16 + 2, title);
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
    ("Ctrl+T", "Create or tick a todo"),
    ("Ctrl+D", "Delete the line"),
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
                Line::styled(reason.clone(), Tone::Error.style()),
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
