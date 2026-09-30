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

use crate::state::{
    App, Entry, Focus, InputMode, ItemsMode, Mode, PickTarget, PickerMode, Row, SearchMode, Tone,
    bar_preview,
};
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
    let area = frame.area();
    if area.height < 14 || area.width < 37 {
        let text = Paragraph::new("Terminal too small").centered();
        let [_, middle, _] = Layout::vertical([
            Constraint::Fill(1),
            Constraint::Length(1),
            Constraint::Fill(1),
        ])
        .areas(area);
        frame.render_widget(text, middle);
        return;
    }
    let [header, body, details, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(5),
        Constraint::Length(7),
        Constraint::Length(1),
    ])
    .areas(area);

    draw_header(frame, header, app);
    let [sections, rows] =
        Layout::horizontal([Constraint::Length(17), Constraint::Min(20)]).areas(body);
    draw_sections(frame, sections, app);
    draw_rows(frame, rows, app, &mut view.rows);
    draw_details(frame, details, app);
    draw_footer(frame, footer, app);

    match &app.mode {
        Mode::Normal => {}
        Mode::Picker(mode) => draw_picker(frame, app, mode),
        Mode::Input(mode) => draw_input_popup(frame, mode),
        Mode::Items(mode) => draw_items(frame, app, mode),
        Mode::Search(mode) => draw_search(frame, app, mode),
    }
}

/// A bordered popup of about `width`×`height` in the middle of the screen,
/// over what is below it. Returns the area inside the border.
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

/// A list with its `selected` line highlighted.
fn draw_list(frame: &mut Frame, area: Rect, items: Vec<ListItem>, selected: usize) {
    let mut state = ListState::default().with_selected(Some(selected));
    let list = List::new(items).highlight_style(selected_style(true));
    frame.render_stateful_widget(list, area, &mut state);
}

fn draw_picker(frame: &mut Frame, app: &App, mode: &PickerMode) {
    let picker = &mode.picker;
    // A stable size while filtering, and at least one line for "No matches".
    let height = (picker.choices.len() + usize::from(picker.filter.is_some())).max(1) as u16 + 2;
    let inner = open_popup(frame, 60, height, &picker.title);
    let list_area = match &picker.filter {
        Some(filter) => {
            let [filter_area, list_area] =
                Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(inner);
            draw_prompt(frame, filter_area, filter, filter.chars().count());
            list_area
        }
        None => inner,
    };
    let theme_target = matches!(mode.target, PickTarget::Theme(_));
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
    draw_list(frame, list_area, items, picker.selected);
}

fn draw_input_popup(frame: &mut Frame, mode: &InputMode) {
    let inner = open_popup(frame, 60, 5, &mode.title);
    let [line_area, _, info_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(inner);
    draw_input(frame, line_area, &mode.input);
    let info = match (&mode.problem, &mode.note) {
        (Some(problem), _) => Span::styled(problem.as_str(), tone(Tone::Error)),
        (None, Some((t, note))) => Span::styled(note.as_str(), tone(*t)),
        (None, None) => Span::styled("Enter to apply, Esc to cancel", tone(Tone::Dim)),
    };
    frame.render_widget(Paragraph::new(Line::from(info)), info_area);
}

fn draw_items(frame: &mut Frame, app: &App, mode: &ItemsMode) {
    let entries = app.item_entries(mode.setting);
    // Preview and a rule above the list.
    let inner = open_popup(frame, 44, entries.len() as u16 + 4, mode.setting.label);
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
    let items = entries
        .iter()
        .map(|entry| item_line(app, *entry, width))
        .collect();
    draw_list(frame, list_area, items, mode.selected);
}

/// A line in the editor of an ordered set: a checkbox or a spring.
fn item_line(app: &App, entry: Entry, width: usize) -> ListItem<'static> {
    let dim_unless = |style: Style, on: bool| {
        if on {
            style
        } else {
            style.add_modifier(Modifier::DIM)
        }
    };
    match entry {
        Entry::Item { value, on } => {
            let mark = if on { "[x] " } else { "[ ] " };
            if app.item_disabled(value) {
                // Switched off elsewhere, but placed here as usual.
                let text = format!("{mark}{value} (off under Updates)");
                return ListItem::new(Line::styled(text, dim_unless(tone(Tone::Warn), on)));
            }
            ListItem::new(Line::styled(
                format!("{mark}{value}"),
                dim_unless(Style::new(), on),
            ))
        }
        Entry::Spring { implicit } => {
            let label = if implicit {
                " spring (automatic) "
            } else {
                " spring "
            };
            let style = dim_unless(tone(Tone::Accent), !implicit);
            ListItem::new(Line::styled(spring_line(label, width), style))
        }
    }
}

fn draw_search(frame: &mut Frame, app: &App, mode: &SearchMode) {
    let height = mode.results.len().max(1) as u16 + 3;
    let inner = open_popup(frame, 60, height, "Search");
    let [line_area, list_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(inner);
    draw_input(frame, line_area, &mode.input);
    let items: Vec<ListItem> = mode
        .results
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
    if items.is_empty() {
        let empty = Line::styled("No matches", tone(Tone::Dim));
        frame.render_widget(Paragraph::new(empty), list_area);
    }
    draw_list(frame, list_area, items, mode.selected);
}

/// ` ⟷ ──── spring ──────`, `width` columns wide.
fn spring_line(label: &str, width: usize) -> String {
    let rest = width.saturating_sub(label.width() + 4);
    let left = rest / 3;
    format!(" ⟷ {}{label}{}", "─".repeat(left), "─".repeat(rest - left))
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
    draw_prompt(frame, area, &input.text, input.cursor);
}

/// `› ` and `text`, scrolled to keep the cursor (a char index) visible.
fn draw_prompt(frame: &mut Frame, area: Rect, text: &str, cursor: usize) {
    let width = usize::from(area.width.saturating_sub(2));
    let (text, cursor) = scrolled(text, cursor, width);
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
        Mode::Picker(PickerMode { picker, .. }) if picker.filter.is_some() => &[
            ("type", "filter"),
            ("↑↓", "choose"),
            ("⏎", "apply"),
            ("Esc", "cancel"),
        ],
        Mode::Picker(_) => &[("↑↓", "choose"), ("⏎", "apply"), ("Esc", "cancel")],
        Mode::Input(_) => &[("⏎", "apply"), ("Esc", "cancel"), ("Ctrl+U", "clear")],
        Mode::Items(_) => &[
            ("↑↓", "move"),
            ("Space", "show/hide"),
            ("Shift+↑↓", "reorder"),
            ("s", "add spring"),
            ("d", "remove spring"),
            ("⏎/Esc", "done"),
        ],
        Mode::Search(_) => &[
            ("type", "search"),
            ("↑↓", "choose"),
            ("⏎", "go"),
            ("Esc", "cancel"),
        ],
    };
    let editor = editor_hint(hints, &app.editor_name, area.width);
    let hints = fit_hints(hints, editor, area.width);
    let mut spans = vec![Span::raw(" ")];
    for &(key, what) in &hints {
        let what = if key == "e" { editor } else { what };
        spans.push(Span::raw(key).fg(ACCENT));
        spans.push(Span::styled(format!(" {what}  "), tone(Tone::Dim)));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// The columns the footer takes with `editor` as the text of the `e` hint:
/// a leading space, and two spaces between hints.
fn footer_width(hints: &[(&str, &str)], editor: &str) -> usize {
    hints
        .iter()
        .map(|&(key, what)| {
            let what = if key == "e" { editor } else { what };
            key.width() + 1 + what.width() + 2
        })
        .sum::<usize>()
        .saturating_sub(1)
}

/// `hints` without the least important ones until the footer fits in
/// `width`, so `q quit` and `/ search` stay.
fn fit_hints<'a>(
    hints: &[(&'a str, &'a str)],
    editor: &str,
    width: u16,
) -> Vec<(&'a str, &'a str)> {
    let mut hints = hints.to_vec();
    while footer_width(&hints, editor) > usize::from(width) {
        let droppable = ["R", "u", "Tab", "←→"]
            .iter()
            .find_map(|key| hints.iter().position(|&(k, _)| k == *key));
        let Some(index) = droppable else { break };
        hints.remove(index);
    }
    hints
}

/// The text of the `e` hint: the editor's name, unless the footer doesn't
/// fit in `width` with it and the generic `editor` is shorter.
fn editor_hint<'a>(hints: &[(&str, &'a str)], name: &'a str, width: u16) -> &'a str {
    let needed = footer_width(hints, name);
    match hints.iter().find(|&&(key, _)| key == "e") {
        Some(&(_, generic)) if needed > usize::from(width) && generic.width() < name.width() => {
            generic
        }
        _ => name,
    }
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
    fn editor_name_only_when_the_footer_fits() {
        let hints = [("/", "search"), ("e", "editor"), ("q", "quit")];
        // " / search  e editor  q quit" is 27 columns.
        assert_eq!(editor_hint(&hints, "notepad", 28), "notepad");
        assert_eq!(editor_hint(&hints, "notepad", 27), "editor");
        // Short names never become the longer `editor`.
        assert_eq!(editor_hint(&hints, "vim", 10), "vim");
        assert_eq!(editor_hint(&[("q", "quit")], "vim", 1), "vim");
    }

    #[test]
    fn footer_drops_secondary_hints_before_quit() {
        let hints = [
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
        ];
        let fitted = fit_hints(&hints, "editor", 80);
        assert!(fitted.contains(&("q", "quit")));
        assert!(fitted.contains(&("/", "search")));
        assert!(!fitted.contains(&("R", "restore")));
        assert!(footer_width(&fitted, "editor") <= 80);
        // Nothing to drop when it fits.
        assert_eq!(fit_hints(&hints, "editor", 200), hints);
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
