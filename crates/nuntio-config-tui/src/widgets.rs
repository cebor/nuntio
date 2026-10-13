//! State of the editing widgets: a filterable choice list, and how keys edit
//! a text input.

use nuntio_tui::text_input::{Edit, TextInput};

use crate::state::Key;

/// What choosing an entry sets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pick {
    /// Remove the key, so the default applies.
    Unset,
    Value(String),
}

#[derive(Debug, Clone)]
pub struct Choice {
    pub pick: Pick,
    pub label: String,
    pub help: String,
}

/// A list of values to choose from. With a filter, typing narrows it down;
/// with `free_text`, the typed text itself is offered too.
#[derive(Debug, Clone)]
pub struct Picker {
    pub title: String,
    pub choices: Vec<Choice>,
    pub filter: Option<String>,
    pub free_text: bool,
    /// Indices into `choices`; `None` is the free-text entry.
    pub visible: Vec<Option<usize>>,
    pub selected: usize,
}

impl Picker {
    pub fn new(title: impl Into<String>, choices: Vec<Choice>, current: &Pick) -> Self {
        let mut picker = Self {
            title: title.into(),
            choices,
            filter: None,
            free_text: false,
            visible: Vec::new(),
            selected: 0,
        };
        picker.refilter();
        picker.selected = picker
            .choices
            .iter()
            .position(|c| same_pick(&c.pick, current))
            .unwrap_or(0);
        picker
    }

    /// Let typing filter the list.
    pub fn filterable(mut self, free_text: bool) -> Self {
        self.filter = Some(String::new());
        self.free_text = free_text;
        self
    }

    /// The highlighted entry.
    pub fn current(&self) -> Option<Pick> {
        match self.visible.get(self.selected)? {
            Some(i) => Some(self.choices[*i].pick.clone()),
            None => Some(Pick::Value(self.filter.clone()?.trim().to_owned())),
        }
    }

    pub fn label(&self, entry: Option<usize>) -> String {
        match entry {
            Some(i) => self.choices[i].label.clone(),
            None => format!("Use \"{}\"", self.filter.as_deref().unwrap_or("").trim()),
        }
    }

    /// Handle a key; returns whether the highlighted entry changed.
    pub fn key(&mut self, key: Key) -> bool {
        let before = self.current();
        let last = self.visible.len().saturating_sub(1);
        match key {
            Key::Up => self.selected = self.selected.saturating_sub(1),
            Key::Down => self.selected = (self.selected + 1).min(last),
            Key::PageUp => self.selected = self.selected.saturating_sub(10),
            Key::PageDown => self.selected = (self.selected + 10).min(last),
            Key::Home => self.selected = 0,
            Key::End => self.selected = last,
            Key::Char(c) if self.filter.is_some() => {
                self.filter.as_mut().unwrap().push(c);
                self.refilter();
                self.selected = 0;
            }
            Key::Backspace if self.filter.is_some() => {
                self.filter.as_mut().unwrap().pop();
                self.refilter();
                self.selected = 0;
            }
            _ => {}
        }
        self.current() != before
    }

    fn refilter(&mut self) {
        let query = self.filter.as_deref().unwrap_or("").trim().to_lowercase();
        let mut matches: Vec<(usize, usize)> = self
            .choices
            .iter()
            .enumerate()
            .filter_map(|(i, c)| score(&c.label.to_lowercase(), &query).map(|s| (s, i)))
            .collect();
        // Stable: equally good matches keep their order.
        matches.sort_by_key(|&(score, _)| score);
        self.visible = matches.into_iter().map(|(_, i)| Some(i)).collect();
        let exact = self.choices.iter().any(|c| c.label.to_lowercase() == query);
        if self.free_text && !query.is_empty() && !exact {
            self.visible.push(None);
        }
    }
}

fn same_pick(a: &Pick, b: &Pick) -> bool {
    match (a, b) {
        (Pick::Unset, Pick::Unset) => true,
        (Pick::Value(a), Pick::Value(b)) => a.eq_ignore_ascii_case(b),
        _ => false,
    }
}

/// Lower is better; `None` if `query` isn't a subsequence of `text`.
fn score(text: &str, query: &str) -> Option<usize> {
    if query.is_empty() {
        return Some(0);
    }
    if text.starts_with(query) {
        return Some(0);
    }
    if let Some(pos) = text.find(query) {
        return Some(1 + pos);
    }
    let mut chars = text.chars();
    query.chars().all(|q| chars.any(|c| c == q)).then_some(1000)
}

/// Feed `key` to `input`; returns whether the text changed.
pub fn edit_input(input: &mut TextInput, key: Key) -> bool {
    let edit = match key {
        Key::Char(c) => Edit::Insert(c),
        Key::Backspace => Edit::Backspace,
        Key::Delete => Edit::Delete,
        Key::Ctrl('u') => Edit::Clear,
        Key::Left => Edit::Left,
        Key::Right => Edit::Right,
        Key::Home | Key::Ctrl('a') => Edit::Home,
        Key::End | Key::Ctrl('e') => Edit::End,
        _ => return false,
    };
    input.apply(edit)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn choices(labels: &[&str]) -> Vec<Choice> {
        labels
            .iter()
            .map(|l| Choice {
                pick: Pick::Value(l.to_string()),
                label: l.to_string(),
                help: String::new(),
            })
            .collect()
    }

    fn type_text(picker: &mut Picker, text: &str) {
        for c in text.chars() {
            picker.key(Key::Char(c));
        }
    }

    #[test]
    fn starts_at_the_current_value() {
        let picker = Picker::new("t", choices(&["a", "b", "c"]), &Pick::Value("B".into()));
        assert_eq!(picker.current(), Some(Pick::Value("b".into())));
    }

    #[test]
    fn filters_and_ranks() {
        let mut picker = Picker::new(
            "t",
            choices(&[
                "Solarized Dark",
                "Dracula",
                "Tokyo Night",
                "Solarized Light",
            ]),
            &Pick::Unset,
        )
        .filterable(false);
        type_text(&mut picker, "dar");
        let labels: Vec<_> = picker.visible.iter().map(|&e| picker.label(e)).collect();
        assert_eq!(labels, ["Solarized Dark"]);
        picker.key(Key::Backspace);
        picker.key(Key::Backspace);
        let labels: Vec<_> = picker.visible.iter().map(|&e| picker.label(e)).collect();
        assert_eq!(labels, ["Dracula", "Solarized Dark", "Solarized Light"]);
        type_text(&mut picker, "zzz");
        assert!(picker.visible.is_empty());
        assert_eq!(picker.current(), None);
    }

    #[test]
    fn subsequence_matches_come_last() {
        let mut picker = Picker::new("t", choices(&["Hack Nerd Font", "Hasklig"]), &Pick::Unset)
            .filterable(false);
        type_text(&mut picker, "hnf");
        assert_eq!(picker.current(), Some(Pick::Value("Hack Nerd Font".into())));
    }

    #[test]
    fn free_text_is_offered_unless_it_matches_exactly() {
        let mut picker = Picker::new("t", choices(&["Hack"]), &Pick::Unset).filterable(true);
        type_text(&mut picker, "Ha");
        assert_eq!(picker.visible, [Some(0), None]);
        type_text(&mut picker, "ck");
        assert_eq!(picker.visible, [Some(0)]);
        type_text(&mut picker, " Mono");
        assert_eq!(picker.visible, [None]);
        assert_eq!(picker.current(), Some(Pick::Value("Hack Mono".into())));
    }

    #[test]
    fn navigation_reports_changes() {
        let mut picker = Picker::new("t", choices(&["a", "b"]), &Pick::Unset);
        assert!(!picker.key(Key::Up));
        assert!(picker.key(Key::Down));
        assert!(!picker.key(Key::Down));
        assert!(!picker.key(Key::Char('x')), "not filterable");
    }

    #[test]
    fn keys_edit_a_text_input() {
        let mut input = TextInput::new("%H:%M");
        edit_input(&mut input, Key::Home);
        assert!(edit_input(&mut input, Key::Char('ä')));
        edit_input(&mut input, Key::Ctrl('e'));
        assert!(edit_input(&mut input, Key::Backspace));
        assert!(!edit_input(&mut input, Key::Ctrl('a')));
        assert!(!edit_input(&mut input, Key::Tab));
        assert!(edit_input(&mut input, Key::Ctrl('u')));
        assert_eq!(input.text(), "");
    }
}
