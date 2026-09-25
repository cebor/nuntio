//! Drawing the editor with ratatui. All decisions live in `state`; this
//! only lays out what it reports.

use nuntio_config::Theme;
use nuntio_config::schema::Section;
use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Clear, List, ListItem, ListState, Paragraph, Wrap};
use unicode_width::UnicodeWidthChar;

use crate::state::{App, Focus, Mode, PickTarget, Row, ThemeSlot, Tone};
use crate::widgets::{Pick, TextInput};

const ACCENT: Color = Color::Cyan;

fn tone(tone: Tone) -> Style {
    match tone {
        Tone::Normal => Style::new(),
        Tone::Dim => Style::new().add_modifier(Modifier::DIM),
        Tone::Accent => Style::new().fg(ACCENT),
        Tone::Warn => Style::new().fg(Color::Yellow),
        Tone::Error => Style::new().fg(Color::Red),
    }
}

fn selected_style(focused: bool) -> Style {
    if focused {
        Style::new().add_modifier(Modifier::REVERSED)
    } else {
        Style::new().add_modifier(Modifier::BOLD)
    }
}

pub fn draw(frame: &mut Frame, app: &App) {
    let [header, body, details, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(5),
        Constraint::Length(7),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    draw_header(frame, header, app);
    let [sections, rows] =
        Layout::horizontal([Constraint::Length(17), Constraint::Min(20)]).areas(body);
    draw_sections(frame, sections, app);
    draw_rows(frame, rows, app);
    draw_details(frame, details, app);
    draw_footer(frame, footer, app);

    match &app.mode {
        Mode::Normal => {}
        Mode::Picker { picker, target, .. } => {
            let filter_line = u16::from(picker.filter.is_some());
            let area = popup(
                frame.area(),
                60,
                picker.visible.len() as u16 + 2 + filter_line,
            );
            frame.render_widget(Clear, area);
            let block = Block::bordered()
                .border_type(BorderType::Rounded)
                .title(format!(" {} ", picker.title))
                .border_style(Style::new().fg(ACCENT));
            let inner = block.inner(area);
            frame.render_widget(block, area);
            let list_area = match &picker.filter {
                Some(filter) => {
                    let [filter_area, list_area] =
                        Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(inner);
                    let line = Line::from(vec![Span::raw("› ").fg(ACCENT), Span::raw(filter)]);
                    frame.render_widget(Paragraph::new(line), filter_area);
                    frame.set_cursor_position(Position::new(
                        filter_area.x + 2 + filter.chars().count() as u16,
                        filter_area.y,
                    ));
                    list_area
                }
                None => inner,
            };
            let theme_target = matches!(target, PickTarget::Theme(_));
            let items: Vec<ListItem> = picker
                .visible
                .iter()
                .map(|&entry| {
                    let mut spans = Vec::new();
                    if theme_target
                        && let Some(i) = entry
                        && let Pick::Value(name) = &picker.choices[i].pick
                        && let Some(theme) = app.themes.get(name)
                    {
                        spans.extend(swatch(theme, 8));
                        spans.push(Span::raw(" "));
                    }
                    let label = picker.label(entry);
                    let style = match entry {
                        Some(i) if picker.choices[i].pick == Pick::Unset => tone(Tone::Dim),
                        None => tone(Tone::Accent),
                        _ => Style::new(),
                    };
                    spans.push(Span::styled(label, style));
                    ListItem::new(Line::from(spans))
                })
                .collect();
            let mut state = ListState::default().with_selected(Some(picker.selected));
            let list = List::new(items).highlight_style(selected_style(true));
            frame.render_stateful_widget(list, list_area, &mut state);
        }
        Mode::Input {
            input,
            title,
            problem,
            note,
            ..
        } => {
            let area = popup(frame.area(), 60, 5);
            frame.render_widget(Clear, area);
            let block = Block::bordered()
                .border_type(BorderType::Rounded)
                .title(format!(" {title} "))
                .border_style(Style::new().fg(ACCENT));
            let inner = block.inner(area);
            frame.render_widget(block, area);
            let [line_area, _, info_area] = Layout::vertical([
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .areas(inner);
            draw_input(frame, line_area, input);
            let info = match (problem, note) {
                (Some(problem), _) => Span::styled(problem.as_str(), tone(Tone::Error)),
                (None, Some(note)) if note.contains("already") => {
                    Span::styled(note.as_str(), tone(Tone::Warn))
                }
                (None, Some(note)) => Span::styled(note.as_str(), tone(Tone::Dim)),
                (None, None) => Span::styled("Enter to apply, Esc to cancel", tone(Tone::Dim)),
            };
            frame.render_widget(Paragraph::new(Line::from(info)), info_area);
        }
        Mode::Items { setting, selected } => {
            let entries = app.item_entries(setting);
            let area = popup(frame.area(), 40, entries.len() as u16 + 2);
            frame.render_widget(Clear, area);
            let block = Block::bordered()
                .border_type(BorderType::Rounded)
                .title(format!(" {} ", setting.label))
                .border_style(Style::new().fg(ACCENT));
            let items: Vec<ListItem> = entries
                .iter()
                .map(|(value, on)| {
                    let mark = if *on { "[x] " } else { "[ ] " };
                    let style = if *on { Style::new() } else { tone(Tone::Dim) };
                    ListItem::new(Line::styled(format!("{mark}{value}"), style))
                })
                .collect();
            let mut state = ListState::default().with_selected(Some(*selected));
            let list = List::new(items)
                .block(block)
                .highlight_style(selected_style(true));
            frame.render_stateful_widget(list, area, &mut state);
        }
        Mode::Search {
            input,
            results,
            selected,
        } => {
            let area = popup(frame.area(), 60, results.len() as u16 + 3);
            frame.render_widget(Clear, area);
            let block = Block::bordered()
                .border_type(BorderType::Rounded)
                .title(" Search ")
                .border_style(Style::new().fg(ACCENT));
            let inner = block.inner(area);
            frame.render_widget(block, area);
            let [line_area, list_area] =
                Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(inner);
            draw_input(frame, line_area, input);
            let items: Vec<ListItem> = results
                .iter()
                .map(|&(section, row)| {
                    let section = Section::ALL[section];
                    let row = app.rows(section)[row];
                    ListItem::new(Line::from(vec![
                        Span::styled(format!("{:<12}", section.label()), tone(Tone::Dim)),
                        Span::raw(app.row_label(row)),
                    ]))
                })
                .collect();
            let mut state = ListState::default().with_selected(Some(*selected));
            let list = List::new(items).highlight_style(selected_style(true));
            frame.render_stateful_widget(list, list_area, &mut state);
        }
    }
}

/// A centered area of the given size, clamped to `area`.
fn popup(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width.saturating_sub(4));
    let height = height.min(area.height.saturating_sub(2));
    let [area] = Layout::horizontal([Constraint::Length(width)])
        .flex(Flex::Center)
        .areas(area);
    let [area] = Layout::vertical([Constraint::Length(height)])
        .flex(Flex::Center)
        .areas(area);
    area
}

fn draw_input(frame: &mut Frame, area: Rect, input: &TextInput) {
    let line = Line::from(vec![Span::raw("› ").fg(ACCENT), Span::raw(&input.text)]);
    frame.render_widget(Paragraph::new(line), area);
    let cursor: usize = input
        .text
        .chars()
        .take(input.cursor)
        .map(|c| c.width().unwrap_or(0))
        .sum();
    frame.set_cursor_position(Position::new(area.x + 2 + cursor as u16, area.y));
}

fn draw_header(frame: &mut Frame, area: Rect, app: &App) {
    let state = if app.error.is_some() {
        Span::styled("  invalid ", tone(Tone::Error))
    } else if app.is_modified() {
        Span::styled("  changed · applied live ", tone(Tone::Accent))
    } else {
        Span::styled("  no changes ", tone(Tone::Dim))
    };
    let [left, right] = Layout::horizontal([
        Constraint::Min(10),
        Constraint::Length(state.width() as u16),
    ])
    .areas(area);
    let title = Line::from(vec![
        Span::raw(" nuntio-config ").bold().fg(ACCENT),
        Span::styled(app.path_label.as_str(), tone(Tone::Dim)),
    ]);
    frame.render_widget(Paragraph::new(title), left);
    frame.render_widget(Paragraph::new(Line::from(state)), right);
}

fn draw_sections(frame: &mut Frame, area: Rect, app: &App) {
    let focused = app.focus == Focus::Sections && matches!(app.mode, Mode::Normal);
    let items: Vec<ListItem> = Section::ALL
        .iter()
        .map(|s| ListItem::new(format!(" {}", s.label())))
        .collect();
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(if focused {
            Style::new().fg(ACCENT)
        } else {
            tone(Tone::Dim)
        });
    let mut state = ListState::default().with_selected(Some(app.section));
    let list = List::new(items)
        .block(block)
        .highlight_style(selected_style(focused));
    frame.render_stateful_widget(list, area, &mut state);
}

fn draw_rows(frame: &mut Frame, area: Rect, app: &App) {
    let focused = app.focus == Focus::Rows && matches!(app.mode, Mode::Normal);
    let section = app.current_section();
    let rows = app.rows(section);
    let label_width = rows
        .iter()
        .map(|&r| app.row_label(r).chars().count())
        .max()
        .unwrap_or(0)
        + 2;
    let items: Vec<ListItem> = rows
        .iter()
        .map(|&row| {
            let marker = match row {
                Row::Keybinding(i) if app.binding_problem(i).is_some() => {
                    Span::styled("⚠ ", tone(Tone::Warn))
                }
                _ if app.row_is_set(row) => Span::styled("● ", tone(Tone::Accent)),
                _ => Span::raw("  "),
            };
            let (value_tone, value) = app.row_value(row);
            let mut spans = vec![
                marker,
                Span::raw(format!("{:<label_width$}", app.row_label(row))),
                Span::styled(value, tone(value_tone)),
            ];
            if let Row::Theme(slot) = row
                && let Some(theme) = app.themes.get(&theme_name(app, slot))
            {
                spans.push(Span::raw("  "));
                spans.extend(swatch(theme, 16));
            }
            let mut item = ListItem::new(Line::from(spans));
            if app.row_is_foreign(row) {
                item = item.style(tone(Tone::Dim));
            }
            item
        })
        .collect();
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .title(format!(" {} ", section.label()))
        .border_style(if focused {
            Style::new().fg(ACCENT)
        } else {
            tone(Tone::Dim)
        });
    let mut state = ListState::default().with_selected(Some(app.row));
    let list = List::new(items)
        .block(block)
        .highlight_style(selected_style(focused));
    frame.render_stateful_widget(list, area, &mut state);
}

fn theme_name(app: &App, slot: ThemeSlot) -> String {
    app.row_value(Row::Theme(slot))
        .1
        .trim_end_matches(" (not found)")
        .to_owned()
}

/// Colored blocks for the theme's first `count` ANSI colors on its
/// background.
fn swatch(theme: &Theme, count: usize) -> Vec<Span<'static>> {
    let rgb = |c: nuntio_config::Color| Color::Rgb(c.r, c.g, c.b);
    let mut spans = vec![Span::styled(
        " Aa ",
        Style::new()
            .fg(rgb(theme.foreground))
            .bg(rgb(theme.background)),
    )];
    spans.extend(
        theme
            .ansi()
            .iter()
            .take(count)
            .map(|&c| Span::styled("▆", Style::new().fg(rgb(c)).bg(rgb(theme.background)))),
    );
    spans
}

fn draw_details(frame: &mut Frame, area: Rect, app: &App) {
    let mut lines: Vec<Line> = app
        .details()
        .into_iter()
        .map(|(t, text)| Line::styled(text, tone(t)))
        .collect();
    if let Some(error) = &app.error {
        lines.push(Line::styled(
            format!("The file is invalid, nuntio keeps the previous settings: {error}"),
            tone(Tone::Error),
        ));
    }
    for warning in &app.warnings {
        lines.push(Line::styled(
            format!("Warning: {warning}"),
            tone(Tone::Warn),
        ));
    }
    if let Some((t, message)) = &app.message {
        lines.push(Line::styled(message.as_str(), tone(*t)));
    }
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(tone(Tone::Dim));
    let paragraph = Paragraph::new(Text::from(lines))
        .block(block)
        .wrap(Wrap { trim: true });
    frame.render_widget(paragraph, area);
}

fn draw_footer(frame: &mut Frame, area: Rect, app: &App) {
    let hints: &[(&str, &str)] = match &app.mode {
        Mode::Normal if app.focus == Focus::Sections => &[
            ("↑↓", "section"),
            ("⏎/Tab", "settings"),
            ("/", "search"),
            ("u", "undo"),
            ("R", "restore"),
            ("q", "quit"),
        ],
        Mode::Normal if app.current_section() == Section::Keybindings => &[
            ("↑↓", "move"),
            ("⏎", "edit"),
            ("a", "add"),
            ("d", "delete"),
            ("Tab", "sections"),
            ("/", "search"),
            ("u", "undo"),
            ("q", "quit"),
        ],
        Mode::Normal => &[
            ("↑↓", "move"),
            ("←→", "change"),
            ("⏎", "edit"),
            ("d", "default"),
            ("Tab", "sections"),
            ("/", "search"),
            ("u", "undo"),
            ("R", "restore"),
            ("q", "quit"),
        ],
        Mode::Picker { picker, .. } if picker.filter.is_some() => &[
            ("type", "filter"),
            ("↑↓", "choose"),
            ("⏎", "apply"),
            ("Esc", "cancel"),
        ],
        Mode::Picker { .. } => &[("↑↓", "choose"), ("⏎", "apply"), ("Esc", "cancel")],
        Mode::Input { .. } => &[("⏎", "apply"), ("Esc", "cancel"), ("Ctrl+U", "clear")],
        Mode::Items { .. } => &[
            ("↑↓", "move"),
            ("Space", "show/hide"),
            ("Shift+↑↓", "reorder"),
            ("⏎", "done"),
        ],
        Mode::Search { .. } => &[
            ("type", "search"),
            ("↑↓", "choose"),
            ("⏎", "go"),
            ("Esc", "cancel"),
        ],
    };
    let mut spans = vec![Span::raw(" ")];
    for (key, what) in hints {
        spans.push(Span::raw(*key).fg(ACCENT));
        spans.push(Span::styled(format!(" {what}  "), tone(Tone::Dim)));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}
