//! Drawing the editor with ratatui. All decisions live in `state`; this
//! only lays out what it reports.

use nuntio_config::Theme;
use nuntio_config::schema::Section;
use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Clear, List, ListItem, ListState, Paragraph, Wrap};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::state::{App, Entry, Focus, Mode, PickTarget, Row, Tone, bar_preview};
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

/// Drawing state that outlives a frame.
#[derive(Default)]
pub struct View {
    /// Scroll position of the settings list.
    rows: ListState,
}

pub fn draw(frame: &mut Frame, app: &App, view: &mut View) {
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
    draw_rows(frame, rows, app, &mut view.rows);
    draw_details(frame, details, app);
    draw_footer(frame, footer, app);

    match &app.mode {
        Mode::Normal => {}
        Mode::Picker { picker, target, .. } => {
            let filter_line = u16::from(picker.filter.is_some());
            // At least one line, for "No matches".
            let area = popup(
                frame.area(),
                60,
                picker.visible.len().max(1) as u16 + 2 + filter_line,
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
                        filter_area.x + 2 + filter.width() as u16,
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
                .enumerate()
                .map(|(index, &entry)| {
                    let mut spans = Vec::new();
                    if theme_target
                        && let Some(i) = entry
                        && let Pick::Value(name) = &picker.choices[i].pick
                        && let Some(theme) = app.themes.get(name)
                    {
                        spans.extend(swatch(theme, 8, index == picker.selected));
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
            if items.is_empty() {
                let empty = Line::styled("No matches", tone(Tone::Dim));
                frame.render_widget(Paragraph::new(empty), list_area);
            }
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
                (None, Some((t, note))) => Span::styled(note.as_str(), tone(*t)),
                (None, None) => Span::styled("Enter to apply, Esc to cancel", tone(Tone::Dim)),
            };
            frame.render_widget(Paragraph::new(Line::from(info)), info_area);
        }
        Mode::Items { setting, selected } => {
            let entries = app.item_entries(setting);
            // Preview and a rule above the list.
            let area = popup(frame.area(), 44, entries.len() as u16 + 4);
            frame.render_widget(Clear, area);
            let block = Block::bordered()
                .border_type(BorderType::Rounded)
                .title(format!(" {} ", setting.label))
                .border_style(Style::new().fg(ACCENT));
            let inner = block.inner(area);
            frame.render_widget(block, area);
            let [preview_area, rule_area, list_area] = Layout::vertical([
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Min(1),
            ])
            .areas(inner);
            let width = inner.width as usize;
            let preview = bar_preview(&entries, width.saturating_sub(2));
            frame.render_widget(
                Paragraph::new(Line::from(format!(" {preview}"))).style(tone(Tone::Accent)),
                preview_area,
            );
            frame.render_widget(
                Paragraph::new("─".repeat(width)).style(tone(Tone::Dim)),
                rule_area,
            );
            let items: Vec<ListItem> = entries
                .iter()
                .map(|entry| match *entry {
                    Entry::Item { value, on } => {
                        let mark = if on { "[x] " } else { "[ ] " };
                        let style = if on { Style::new() } else { tone(Tone::Dim) };
                        ListItem::new(Line::styled(format!("{mark}{value}"), style))
                    }
                    Entry::Spring { implicit } => {
                        let label = if implicit {
                            " spring (automatic) "
                        } else {
                            " spring "
                        };
                        ListItem::new(Line::styled(spring_line(label, width), {
                            let style = tone(Tone::Accent);
                            if implicit {
                                style.add_modifier(Modifier::DIM)
                            } else {
                                style
                            }
                        }))
                    }
                })
                .collect();
            let mut state = ListState::default().with_selected(Some(*selected));
            let list = List::new(items).highlight_style(selected_style(true));
            frame.render_stateful_widget(list, list_area, &mut state);
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
/// ` ⟷ ──── spring ──────`, `width` columns wide.
fn spring_line(label: &str, width: usize) -> String {
    let rest = width.saturating_sub(label.width() + 4);
    let left = rest / 3;
    format!(" ⟷ {}{label}{}", "─".repeat(left), "─".repeat(rest - left))
}

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
    let width = usize::from(area.width.saturating_sub(2));
    let (text, cursor) = scrolled(&input.text, input.cursor, width);
    let line = Line::from(vec![Span::raw("› ").fg(ACCENT), Span::raw(text)]);
    frame.render_widget(Paragraph::new(line), area);
    frame.set_cursor_position(Position::new(area.x + 2 + cursor as u16, area.y));
}

/// The part of `text` to show in `width` columns so that the cursor (a
/// char index) stays visible, and the cursor's column in it.
fn scrolled(text: &str, cursor: usize, width: usize) -> (&str, usize) {
    let column = |s: &str| -> usize { s.chars().map(|c| c.width().unwrap_or(0)).sum() };
    let cursor_byte = text
        .char_indices()
        .nth(cursor)
        .map_or(text.len(), |(i, _)| i);
    // Drop leading characters until the cursor fits, with room for it.
    let mut start = 0;
    while column(&text[start..cursor_byte]) >= width.max(1) {
        start += text[start..].chars().next().map_or(1, char::len_utf8);
    }
    (&text[start..], column(&text[start..cursor_byte]))
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
        .enumerate()
        .map(|(index, s)| {
            if index != app.section {
                return ListItem::new(format!("  {}", s.label()));
            }
            let style = Style::new().fg(ACCENT).add_modifier(Modifier::BOLD);
            let style = if focused {
                style.add_modifier(Modifier::REVERSED)
            } else {
                style
            };
            ListItem::new(format!("▌ {}", s.label())).style(style)
        })
        .collect();
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(if focused {
            Style::new().fg(ACCENT)
        } else {
            tone(Tone::Dim)
        });
    // Selected only so the list scrolls to it; the item styles itself.
    let mut state = ListState::default().with_selected(Some(app.section));
    frame.render_stateful_widget(List::new(items).block(block), area, &mut state);
}

/// All sections' rows in one list, each under a heading.
fn draw_rows(frame: &mut Frame, area: Rect, app: &App, state: &mut ListState) {
    let focused = app.focus == Focus::Rows && matches!(app.mode, Mode::Normal);
    let mut items: Vec<ListItem> = Vec::new();
    let mut heading = 0;
    for (section_index, &section) in Section::ALL.iter().enumerate() {
        if section_index > 0 {
            items.push(ListItem::new(""));
        }
        if section_index == app.section {
            heading = items.len();
        }
        items.push(ListItem::new(Line::styled(
            section.label(),
            Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
        )));
        let rows = app.rows(section);
        let label_width = rows
            .iter()
            .map(|&r| app.row_label(r).chars().count())
            .max()
            .unwrap_or(0)
            + 2;
        items.extend(rows.iter().enumerate().map(|(index, &row)| {
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
                && let Some(theme) = app.themes.get(&app.theme_name(slot))
            {
                let highlighted = focused && section_index == app.section && index == app.row;
                spans.push(Span::raw("  "));
                spans.extend(swatch(theme, 16, highlighted));
            }
            let mut item = ListItem::new(Line::from(spans));
            if app.row_is_foreign(row) {
                item = item.style(tone(Tone::Dim));
            }
            item
        }));
    }
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .title(" Settings ")
        .border_style(if focused {
            Style::new().fg(ACCENT)
        } else {
            tone(Tone::Dim)
        });
    let selected = heading + 1 + app.row;
    let on_sections = app.focus == Focus::Sections;
    *state.offset_mut() = rows_offset(state.offset(), heading, selected, on_sections);
    state.select(Some(selected));
    let list = List::new(items)
        .block(block)
        .scroll_padding(1)
        .highlight_style(selected_style(focused));
    frame.render_stateful_widget(list, area, state);
}

/// Where the settings list starts before ratatui scrolls the selection
/// into view. Choosing a section in the sidebar puts its heading at the
/// top; entering a section's first row keeps its heading visible.
fn rows_offset(previous: usize, heading: usize, selected: usize, on_sections: bool) -> usize {
    if on_sections {
        heading
    } else if selected == heading + 1 {
        previous.min(heading)
    } else {
        previous
    }
}

/// Colored blocks for the theme's first `count` ANSI colors on its
/// background. In a row highlighted with [`Modifier::REVERSED`], pass
/// `highlighted` so the colors are swapped in advance and the highlight
/// swaps them back; otherwise the blocks turn into the background color.
fn swatch(theme: &Theme, count: usize, highlighted: bool) -> Vec<Span<'static>> {
    let rgb = |c: nuntio_config::Color| Color::Rgb(c.r, c.g, c.b);
    let style = |fg, bg| {
        let (fg, bg) = if highlighted { (bg, fg) } else { (fg, bg) };
        Style::new().fg(rgb(fg)).bg(rgb(bg))
    };
    let mut spans = vec![Span::styled(
        " Aa ",
        style(theme.foreground, theme.background),
    )];
    spans.extend(
        theme
            .ansi()
            .iter()
            .take(count)
            .map(|&c| Span::styled("▆", style(c, theme.background))),
    );
    spans
}

fn draw_details(frame: &mut Frame, area: Rect, app: &App) {
    // Problems and messages first: the pane is short, and help text is
    // what may be cut off.
    let mut lines = Vec::new();
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
    lines.extend(
        app.details()
            .into_iter()
            .map(|(t, text)| Line::styled(text, tone(t))),
    );
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
            ("e", "editor"),
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
            ("e", "editor"),
            ("u", "undo"),
            ("R", "restore"),
            ("q", "quit"),
        ],
        Mode::Normal => &[
            ("↑↓", "move"),
            ("←→", "change"),
            ("⏎", "edit"),
            ("d", "default"),
            ("Tab", "sections"),
            ("/", "search"),
            ("e", "editor"),
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
            ("s", "add spring"),
            ("d", "remove spring"),
            ("⏎/Esc", "done"),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_offset_keeps_headings_in_view() {
        // The sidebar puts the section's heading at the top.
        assert_eq!(rows_offset(0, 20, 21, true), 20);
        assert_eq!(rows_offset(40, 20, 23, true), 20);
        // A first row pulls its heading into view.
        assert_eq!(rows_offset(25, 20, 21, false), 20);
        assert_eq!(rows_offset(10, 20, 21, false), 10);
        // Elsewhere the list keeps its position.
        assert_eq!(rows_offset(10, 20, 23, false), 10);
    }

    #[test]
    fn input_scrolls_to_keep_the_cursor_visible() {
        assert_eq!(scrolled("hello", 5, 10), ("hello", 5));
        assert_eq!(scrolled("hello world", 11, 5), ("orld", 4));
        assert_eq!(scrolled("hello world", 2, 5), ("hello world", 2));
        // Wide characters count two columns.
        assert_eq!(scrolled("日本語", 3, 4), ("語", 2));
    }
}
