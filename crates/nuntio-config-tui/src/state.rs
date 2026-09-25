//! Everything the editor does, without drawing: navigation, the editors and
//! applying changes to the config file.
//!
//! Every valid change is written right away, so nuntio's hot reload shows
//! it. Changes that would make a valid config invalid are refused.

use std::io;

use unicode_width::UnicodeWidthStr;

use nuntio_config::schema::{Kind, SETTINGS, SPRING, Section, Setting};
use nuntio_config::toml_edit::{self, InlineTable};
use nuntio_config::{ACTIONS, Config, ConfigDoc, KeyCombo, Keybinding, ThemeSelection, ThemeSet};

use crate::args;
use crate::detect::Found;
use crate::widgets::{Choice, Pick, Picker, TextInput};

/// Where the config lives. A trait so tests can run without files.
pub trait Store {
    /// Current contents; `None` if the file doesn't exist yet.
    fn read(&self) -> io::Result<Option<String>>;
    fn write(&mut self, contents: &str) -> io::Result<()>;
}

/// What is installed, for the pickers. Each is asked once, when first
/// needed; tests pass fixed lists.
pub struct Sources {
    /// Monospace font families.
    pub fonts: Box<dyn Fn() -> Vec<String>>,
    pub shells: Box<dyn Fn() -> Vec<Found>>,
    pub wsl_distributions: Box<dyn Fn() -> Vec<Found>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Ctrl(char),
    Up,
    Down,
    Left,
    Right,
    ShiftUp,
    ShiftDown,
    PageUp,
    PageDown,
    Home,
    End,
    Enter,
    Esc,
    Tab,
    BackTab,
    Backspace,
    Delete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeSlot {
    Single,
    Light,
    Dark,
}

/// A line in the editor of an ordered set like the status bar items.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Entry {
    Item {
        value: &'static str,
        on: bool,
    },
    /// A flexible gap. `implicit` is the one nuntio assumes before the last
    /// item when the list has none; it can be moved but not removed.
    Spring {
        implicit: bool,
    },
}

impl Entry {
    /// Part of the list that is written, as opposed to a hidden item.
    pub fn is_chosen(&self) -> bool {
        !matches!(self, Entry::Item { on: false, .. })
    }
}

/// One line sketching the bar with the chosen `entries`: item names a space
/// apart, springs as dotted gaps sharing the free space. Items that don't
/// fit are dropped as in the bar: from the end, the last one last.
pub fn bar_preview(entries: &[Entry], width: usize) -> String {
    let chosen: Vec<Option<&str>> = entries
        .iter()
        .take_while(|e| e.is_chosen())
        .map(|e| match e {
            Entry::Item { value, .. } => Some(*value),
            Entry::Spring { .. } => None,
        })
        .collect();
    let names: Vec<&str> = chosen.iter().flatten().copied().collect();
    let mut keep = vec![true; names.len()];
    let needed = |keep: &[bool]| {
        let kept = names.iter().zip(keep).filter(|(_, k)| **k);
        kept.map(|(n, _)| n.width() + 1)
            .sum::<usize>()
            .saturating_sub(1)
    };
    while needed(&keep) > width {
        match keep[..names.len().saturating_sub(1)]
            .iter()
            .rposition(|k| *k)
        {
            Some(i) => keep[i] = false,
            None => break,
        }
    }
    let springs = chosen.iter().filter(|c| c.is_none()).count();
    let free = width.saturating_sub(needed(&keep));
    // A run of `len` columns before, between or after items; dotted where
    // springs are, with a space left next to each item.
    let run = |len: usize, dotted: bool, left: bool, right: bool| {
        let pad = usize::from(left) + usize::from(right);
        if !dotted || len <= pad {
            return " ".repeat(len);
        }
        let dots = "·".repeat(len - pad);
        format!(
            "{}{dots}{}",
            if left { " " } else { "" },
            if right { " " } else { "" }
        )
    };
    let mut out = String::new();
    let (mut spring, mut pending, mut dotted, mut placed) = (0, 0, false, false);
    let mut keep = keep.into_iter();
    for c in &chosen {
        match c {
            None => {
                pending += free / springs + usize::from(spring < free % springs);
                spring += 1;
                dotted = true;
            }
            Some(name) => {
                if keep.next() != Some(true) {
                    continue;
                }
                out += &run(pending + usize::from(placed), dotted, placed, true);
                out += name;
                (pending, dotted, placed) = (0, false, true);
            }
        }
    }
    out += &run(pending, dotted, placed, false);
    out
}

/// A line in the settings list.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Row {
    Setting(&'static Setting),
    /// Whether the theme follows the OS appearance.
    FollowOs,
    Theme(ThemeSlot),
    Keybinding(usize),
    AddKeybinding,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Sections,
    Rows,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Normal,
    Dim,
    Accent,
    Warn,
    Error,
}

/// What a picker sets.
#[derive(Debug, Clone)]
pub enum PickTarget {
    Setting(&'static Setting),
    Theme(ThemeSlot),
    /// The second step of editing a keybinding, after its key.
    Action {
        index: Option<usize>,
        key: String,
    },
}

#[derive(Debug, Clone)]
pub enum InputTarget {
    Setting(&'static Setting),
    /// The first step of editing (`Some`) or adding a keybinding.
    BindingKey {
        index: Option<usize>,
    },
}

#[derive(Debug, Clone)]
pub enum Mode {
    Normal,
    Picker {
        picker: Picker,
        target: PickTarget,
        /// File contents when the picker opened, for Esc and undo.
        before: String,
    },
    Input {
        input: TextInput,
        target: InputTarget,
        title: String,
        /// Problem with the current text, shown while typing.
        problem: Option<String>,
        /// Hint that doesn't block Enter.
        note: Option<(Tone, String)>,
    },
    /// Checklist for an ordered set like the status bar items.
    Items {
        setting: &'static Setting,
        selected: usize,
    },
    Search {
        input: TextInput,
        results: Vec<(usize, usize)>,
        selected: usize,
    },
}

pub struct App {
    store: Box<dyn Store>,
    pub path_label: String,
    doc: ConfigDoc,
    /// The file as last read or written.
    source: String,
    original: String,
    undo: Vec<String>,
    /// The config in effect: the last valid one (nuntio keeps it too).
    pub config: Config,
    values: toml::Value,
    defaults: toml::Value,
    /// Why the file is invalid, if it is.
    pub error: Option<String>,
    pub warnings: Vec<String>,
    /// Broken theme files, shown along with the config's warnings.
    theme_warnings: Vec<String>,
    pub message: Option<(Tone, String)>,
    pub themes: ThemeSet,
    sources: Sources,
    fonts: Option<Vec<String>>,
    shells: Option<Vec<Found>>,
    wsl_distributions: Option<Vec<Found>>,
    pub section: usize,
    pub row: usize,
    pub focus: Focus,
    pub mode: Mode,
    pub quit: bool,
}

fn serialize(config: &Config) -> toml::Value {
    toml::Value::try_from(config).expect("the config serializes")
}

fn value_at<'a>(root: &'a toml::Value, path: &str) -> Option<&'a toml::Value> {
    path.split('.').try_fold(root, |value, key| value.get(key))
}

/// A number without float noise from `f32`: `0.15`, `13`.
fn format_float(value: f64) -> String {
    let text = format!("{value:.4}");
    text.trim_end_matches('0').trim_end_matches('.').to_owned()
}

fn format_value(value: &toml::Value) -> String {
    match value {
        toml::Value::String(s) => s.clone(),
        toml::Value::Integer(i) => i.to_string(),
        toml::Value::Float(f) => format_float(*f),
        toml::Value::Boolean(b) => if *b { "on" } else { "off" }.into(),
        toml::Value::Array(items) => {
            let items: Vec<String> = items.iter().map(format_value).collect();
            if items.is_empty() {
                "(none)".into()
            } else {
                items.join(", ")
            }
        }
        other => other.to_string(),
    }
}

/// Round to the precision of `step`, so stepping 0.15 by 0.05 gives 0.2.
fn round_to_step(value: f64, step: f64) -> f64 {
    let decimals = format_float(step)
        .split_once('.')
        .map_or(0, |(_, fraction)| fraction.len());
    let factor = 10f64.powi(decimals as i32);
    (value * factor).round() / factor
}

fn theme_value(selection: &ThemeSelection) -> toml_edit::Value {
    match selection {
        ThemeSelection::Single(name) => name.as_str().into(),
        ThemeSelection::Auto { light, dark } => {
            let mut table = InlineTable::new();
            table.insert("light", light.as_str().into());
            table.insert("dark", dark.as_str().into());
            toml_edit::Value::InlineTable(table)
        }
    }
}

impl App {
    pub fn new(
        store: Box<dyn Store>,
        path_label: String,
        themes: ThemeSet,
        sources: Sources,
    ) -> Result<Self, String> {
        let source = store
            .read()
            .map_err(|e| format!("{path_label}: {e}"))?
            .unwrap_or_default();
        let doc = ConfigDoc::parse(&source).map_err(|e| format!("{path_label}: {e}"))?;
        let defaults = serialize(&Config::default());
        let mut app = Self {
            store,
            path_label,
            doc,
            original: source.clone(),
            source,
            undo: Vec::new(),
            config: Config::default(),
            values: defaults.clone(),
            defaults,
            error: None,
            warnings: Vec::new(),
            theme_warnings: Vec::new(),
            message: None,
            themes,
            sources,
            fonts: None,
            shells: None,
            wsl_distributions: None,
            section: 0,
            row: 0,
            focus: Focus::Rows,
            mode: Mode::Normal,
            quit: false,
        };
        app.revalidate();
        Ok(app)
    }

    /// Show problems with the theme files, e.g. why a theme is missing.
    pub fn set_theme_warnings(&mut self, warnings: Vec<String>) {
        self.theme_warnings = warnings;
        self.revalidate();
    }

    fn revalidate(&mut self) {
        match nuntio_config::parse(&self.source) {
            Ok(loaded) => {
                self.values = serialize(&loaded.config);
                self.config = loaded.config;
                self.warnings = loaded.warnings;
                self.error = None;
            }
            Err(err) => {
                self.error = Some(err);
                self.warnings.clear();
            }
        }
        self.warnings.extend(self.theme_warnings.iter().cloned());
        // Rows can disappear, e.g. the light/dark theme rows.
        self.clamp_row();
    }

    pub fn is_modified(&self) -> bool {
        self.source != self.original
    }

    // ----- Applying changes -------------------------------------------------

    /// Reload if the file was changed by someone else. Returns `false` then,
    /// so the change the user just made isn't applied to stale contents.
    fn in_sync(&mut self) -> bool {
        let current = match self.store.read() {
            Ok(current) => current.unwrap_or_default(),
            Err(err) => {
                self.message = Some((Tone::Error, format!("can't read the file: {err}")));
                return false;
            }
        };
        if current == self.source {
            return true;
        }
        match ConfigDoc::parse(&current) {
            Ok(doc) => {
                self.doc = doc;
                self.source = current;
                self.revalidate();
                self.message = Some((
                    Tone::Warn,
                    "The file was changed elsewhere and has been reloaded.".into(),
                ));
            }
            Err(err) => self.message = Some((Tone::Error, err)),
        }
        false
    }

    /// Check an edit without applying it.
    fn check(&self, edit: impl FnOnce(&mut ConfigDoc)) -> Result<(), String> {
        let mut doc = self.doc.clone();
        edit(&mut doc);
        match nuntio_config::parse(&doc.to_string()) {
            // Once the file is broken anyway, don't block fixing it bit by bit.
            Err(err) if self.error.is_none() => Err(err),
            _ => Ok(()),
        }
    }

    /// Apply an edit and write the file. Returns whether it was applied.
    fn apply(&mut self, edit: impl FnOnce(&mut ConfigDoc), record_undo: bool) -> bool {
        if !self.in_sync() {
            return false;
        }
        let mut doc = self.doc.clone();
        edit(&mut doc);
        let text = doc.to_string();
        if text == self.source {
            return true;
        }
        if let Err(err) = nuntio_config::parse(&text)
            && self.error.is_none()
        {
            self.message = Some((Tone::Error, err));
            return false;
        }
        if let Err(err) = self.store.write(&text) {
            self.message = Some((Tone::Error, format!("can't write the file: {err}")));
            return false;
        }
        if record_undo {
            self.undo.push(std::mem::replace(&mut self.source, text));
        } else {
            self.source = text;
        }
        self.doc = doc;
        self.revalidate();
        self.message = None;
        true
    }

    fn commit(&mut self, edit: impl FnOnce(&mut ConfigDoc)) -> bool {
        self.apply(edit, true)
    }

    /// Replace the whole file, e.g. for undo. Validity isn't checked: the
    /// text was in the file before.
    fn restore_text(&mut self, text: String) -> bool {
        let doc = match ConfigDoc::parse(&text) {
            Ok(doc) => doc,
            Err(err) => {
                self.message = Some((Tone::Error, err));
                return false;
            }
        };
        if let Err(err) = self.store.write(&text) {
            self.message = Some((Tone::Error, format!("can't write the file: {err}")));
            return false;
        }
        self.doc = doc;
        self.source = text;
        self.revalidate();
        true
    }

    pub fn undo(&mut self) {
        if !self.in_sync() {
            return;
        }
        match self.undo.pop() {
            Some(text) => {
                if self.restore_text(text) {
                    self.message = Some((Tone::Normal, "Undone.".into()));
                }
            }
            None => self.message = Some((Tone::Dim, "Nothing to undo.".into())),
        }
    }

    /// Back to the file as it was when the editor started (undoable).
    pub fn restore_original(&mut self) {
        if !self.in_sync() {
            return;
        }
        if !self.is_modified() {
            self.message = Some((Tone::Dim, "No changes to restore.".into()));
            return;
        }
        let current = self.source.clone();
        if self.restore_text(self.original.clone()) {
            self.undo.push(current);
            self.message = Some((Tone::Normal, "Restored the original file.".into()));
        }
    }

    // ----- Rows ------------------------------------------------------------

    pub fn current_section(&self) -> Section {
        Section::ALL[self.section]
    }

    pub fn rows(&self, section: Section) -> Vec<Row> {
        match section {
            Section::Theme => {
                let mut rows = vec![Row::FollowOs];
                match self.config.theme {
                    ThemeSelection::Single(_) => rows.push(Row::Theme(ThemeSlot::Single)),
                    ThemeSelection::Auto { .. } => {
                        rows.extend([Row::Theme(ThemeSlot::Light), Row::Theme(ThemeSlot::Dark)])
                    }
                }
                rows
            }
            Section::Keybindings => {
                let count = self.doc.keybindings().len();
                (0..count)
                    .map(Row::Keybinding)
                    .chain([Row::AddKeybinding])
                    .collect()
            }
            _ => SETTINGS
                .iter()
                .filter(|s| s.section == section && s.kind != Kind::Theme)
                // A choice with a single value offers nothing to choose.
                .filter(|s| !matches!(s.kind, Kind::Choice(values) if values.len() < 2))
                .map(Row::Setting)
                .collect(),
        }
    }

    pub fn current_row(&self) -> Option<Row> {
        self.rows(self.current_section()).get(self.row).copied()
    }

    fn clamp_row(&mut self) {
        let count = self.rows(self.current_section()).len();
        self.row = self.row.min(count.saturating_sub(1));
    }

    pub fn row_label(&self, row: Row) -> String {
        match row {
            Row::Setting(s) => match s.platform {
                Some(platform) => format!("{} ({})", s.label, platform.label()),
                None => s.label.into(),
            },
            Row::FollowOs => "Follow OS appearance".into(),
            Row::Theme(ThemeSlot::Single) => "Theme".into(),
            Row::Theme(ThemeSlot::Light) => "Light theme".into(),
            Row::Theme(ThemeSlot::Dark) => "Dark theme".into(),
            Row::Keybinding(i) => {
                let key = &self.doc.keybindings()[i].key;
                match KeyCombo::parse(key) {
                    Ok(combo) => combo.to_string(),
                    Err(_) => key.clone(),
                }
            }
            Row::AddKeybinding => "+ Add keybinding".into(),
        }
    }

    /// The value column, with the tone to show it in.
    pub fn row_value(&self, row: Row) -> (Tone, String) {
        match row {
            Row::Setting(s) => match value_at(&self.values, s.path) {
                Some(value) if s.kind == Kind::StringList => {
                    let items: Vec<String> = value
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|v| v.as_str().map(str::to_owned))
                        .collect();
                    (Tone::Normal, args::join(&items))
                }
                Some(value) => (Tone::Normal, format_value(value)),
                None => (Tone::Dim, s.unset.unwrap_or("").into()),
            },
            Row::FollowOs => {
                let on = matches!(self.config.theme, ThemeSelection::Auto { .. });
                (Tone::Normal, if on { "on" } else { "off" }.into())
            }
            Row::Theme(slot) => {
                let name = self.theme_name(slot);
                match self.themes.get(&name) {
                    Some(_) => (Tone::Normal, name),
                    None => (Tone::Warn, format!("{name} (not found)")),
                }
            }
            Row::Keybinding(i) => {
                let action = self.doc.keybindings()[i].action.clone();
                (Tone::Normal, action)
            }
            Row::AddKeybinding => (Tone::Dim, String::new()),
        }
    }

    /// Whether the row's value comes from the file rather than a default.
    pub fn row_is_set(&self, row: Row) -> bool {
        match row {
            Row::Setting(s) => self.doc.is_set(s.path),
            Row::FollowOs | Row::Theme(_) => self.doc.is_set("theme"),
            Row::Keybinding(_) => true,
            Row::AddKeybinding => false,
        }
    }

    /// Settings for another OS are shown dimmed.
    pub fn row_is_foreign(&self, row: Row) -> bool {
        matches!(row, Row::Setting(Setting { platform: Some(p), .. }) if !p.is_current())
    }

    /// A problem with a keybinding entry.
    pub fn binding_problem(&self, index: usize) -> Option<String> {
        let bindings = self.doc.keybindings();
        let binding = bindings.get(index)?;
        let combo = match KeyCombo::parse(&binding.key) {
            Ok(combo) => combo,
            Err(err) => return Some(format!("key \"{}\": {err}", binding.key)),
        };
        if !ACTIONS.iter().any(|a| a.value == binding.action) {
            return Some(format!("unknown action `{}`", binding.action));
        }
        let earlier = bindings[..index]
            .iter()
            .position(|b| KeyCombo::parse(&b.key) == Ok(combo));
        earlier.map(|i| {
            format!(
                "{combo} is also bound by entry {}; the first entry wins",
                i + 1
            )
        })
    }

    pub fn theme_name(&self, slot: ThemeSlot) -> String {
        match (&self.config.theme, slot) {
            (ThemeSelection::Single(name), _) => name.clone(),
            (ThemeSelection::Auto { light, .. }, ThemeSlot::Light) => light.clone(),
            (ThemeSelection::Auto { dark, .. }, _) => dark.clone(),
        }
    }

    /// The text below the list: help, default, range, problems.
    pub fn details(&self) -> Vec<(Tone, String)> {
        let mut lines = Vec::new();
        match &self.mode {
            Mode::Picker { picker, .. } => {
                if let Some(Some(i)) = picker.visible.get(picker.selected) {
                    let help = &picker.choices[*i].help;
                    if !help.is_empty() {
                        lines.push((Tone::Normal, help.clone()));
                    }
                }
                return lines;
            }
            Mode::Items { setting, selected } => {
                if let Kind::OrderedSet(variants) = setting.kind {
                    let entry = self.item_entries(setting)[*selected];
                    let value = match entry {
                        Entry::Item { value, .. } => value,
                        Entry::Spring { .. } => SPRING,
                    };
                    if let Some(v) = variants.iter().find(|v| v.value == value) {
                        lines.push((Tone::Normal, v.help.into()));
                    }
                    if entry == (Entry::Spring { implicit: true }) {
                        lines.push((
                            Tone::Dim,
                            "Without a spring, the last item sits at the right edge. \
                             Put a spring at the end to align everything left."
                                .into(),
                        ));
                    }
                }
                return lines;
            }
            _ => {}
        }
        let Some(row) = self.current_row() else {
            return lines;
        };
        match row {
            Row::Setting(s) => {
                lines.push((Tone::Normal, s.help.into()));
                if let Kind::Choice(variants) = s.kind
                    && let Some(current) = value_at(&self.values, s.path).and_then(|v| v.as_str())
                    && let Some(v) = variants.iter().find(|v| v.value == current)
                {
                    lines.push((Tone::Accent, format!("{}: {}", v.value, v.help)));
                }
                match s.kind {
                    Kind::Int { min, max, .. } => {
                        lines.push((Tone::Dim, format!("Range: {min} to {max}")))
                    }
                    Kind::Float { min, max, .. } => lines.push((
                        Tone::Dim,
                        format!("Range: {} to {}", format_float(min), format_float(max)),
                    )),
                    _ => {}
                }
                let default = match value_at(&self.defaults, s.path) {
                    Some(value) => format_value(value),
                    None => s.unset.unwrap_or("").to_owned(),
                };
                lines.push((Tone::Dim, format!("Default: {default}")));
                if let Some(platform) = s.platform {
                    lines.push((
                        Tone::Dim,
                        format!("Only has an effect on {}.", platform.label()),
                    ));
                }
            }
            Row::FollowOs => lines.push((
                Tone::Normal,
                "Use a light and a dark theme that follow the OS appearance.".into(),
            )),
            Row::Theme(_) => lines.push((
                Tone::Normal,
                "Custom themes (.toml or .itermcolors) go in the themes/ directory next to \
                 the config file."
                    .into(),
            )),
            Row::Keybinding(i) => {
                let binding = &self.doc.keybindings()[i];
                if let Some(action) = ACTIONS.iter().find(|a| a.value == binding.action) {
                    lines.push((Tone::Normal, action.help.into()));
                }
                if let Some(problem) = self.binding_problem(i) {
                    lines.push((Tone::Warn, problem));
                }
                lines.push((
                    Tone::Dim,
                    "Your bindings take precedence over the default shortcuts.".into(),
                ));
            }
            Row::AddKeybinding => lines.push((
                Tone::Normal,
                "Bind a key combination to an action, or to `none` to pass it to the terminal."
                    .into(),
            )),
        }
        lines
    }

    // ----- Keys ------------------------------------------------------------

    pub fn key(&mut self, key: Key) {
        if key == Key::Ctrl('c') {
            self.quit = true;
            return;
        }
        match std::mem::replace(&mut self.mode, Mode::Normal) {
            Mode::Normal => self.normal_key(key),
            Mode::Picker {
                picker,
                target,
                before,
            } => self.picker_key(key, picker, target, before),
            Mode::Input {
                input,
                target,
                title,
                problem,
                note,
            } => self.input_key(key, input, target, title, problem, note),
            Mode::Items { setting, selected } => self.items_key(key, setting, selected),
            Mode::Search {
                input,
                results,
                selected,
            } => self.search_key(key, input, results, selected),
        }
    }

    fn normal_key(&mut self, key: Key) {
        self.message = None;
        match key {
            Key::Char('q') => self.quit = true,
            Key::Char('u') => self.undo(),
            Key::Char('R') => self.restore_original(),
            Key::Char('/') => {
                self.mode = Mode::Search {
                    input: TextInput::default(),
                    results: self.search(""),
                    selected: 0,
                }
            }
            Key::Tab | Key::BackTab => {
                self.focus = match self.focus {
                    Focus::Sections => Focus::Rows,
                    Focus::Rows => Focus::Sections,
                }
            }
            _ => match self.focus {
                Focus::Sections => self.sections_key(key),
                Focus::Rows => self.rows_key(key),
            },
        }
    }

    fn sections_key(&mut self, key: Key) {
        let last = Section::ALL.len() - 1;
        match key {
            Key::Up | Key::Char('k') => self.select_section(self.section.saturating_sub(1)),
            Key::Down | Key::Char('j') => self.select_section((self.section + 1).min(last)),
            Key::Home => self.select_section(0),
            Key::End => self.select_section(last),
            Key::Enter | Key::Right | Key::Char('l') => self.focus = Focus::Rows,
            _ => {}
        }
    }

    fn select_section(&mut self, section: usize) {
        if section != self.section {
            self.section = section;
            self.row = 0;
        }
    }

    fn rows_key(&mut self, key: Key) {
        let count = self.rows(self.current_section()).len();
        let last = count.saturating_sub(1);
        match key {
            Key::Up | Key::Char('k') => {
                if self.row == 0 {
                    // Wrap into the previous section's last row.
                    if self.section > 0 {
                        self.section -= 1;
                        self.row = self.rows(self.current_section()).len().saturating_sub(1);
                    }
                } else {
                    self.row -= 1;
                }
            }
            Key::Down | Key::Char('j') => {
                if self.row >= last {
                    if self.section + 1 < Section::ALL.len() {
                        self.section += 1;
                        self.row = 0;
                    }
                } else {
                    self.row += 1;
                }
            }
            Key::Home => self.row = 0,
            Key::End => self.row = last,
            Key::Esc | Key::Char('h') => self.focus = Focus::Sections,
            Key::Enter => self.activate(),
            Key::Char(' ') => self.adjust(0),
            Key::Left => self.adjust(-1),
            Key::Right | Key::Char('l') => self.adjust(1),
            Key::Char('d') | Key::Delete | Key::Char('x') => self.reset(),
            Key::Char('a') if self.current_section() == Section::Keybindings => {
                self.edit_binding_key(None)
            }
            _ => {}
        }
    }

    /// Change the value in place: toggle, step or cycle. `0` is Space,
    /// which only toggles.
    fn adjust(&mut self, direction: i64) {
        let Some(row) = self.current_row() else {
            return;
        };
        match row {
            Row::Setting(s) => {
                let current = value_at(&self.values, s.path);
                match s.kind {
                    Kind::Bool => {
                        let on = current.and_then(|v| v.as_bool()).unwrap_or(false);
                        self.commit(|doc| doc.set(s.path, !on));
                    }
                    Kind::Int { min, max, step } if direction != 0 => {
                        let value = current.and_then(|v| v.as_integer()).unwrap_or(min);
                        let new = (value + direction * step).clamp(min, max);
                        self.commit(|doc| doc.set(s.path, new));
                    }
                    Kind::Float { min, max, step } if direction != 0 => {
                        let value = current.and_then(|v| v.as_float()).unwrap_or(min);
                        let new = round_to_step(value + direction as f64 * step, step);
                        let new = new.clamp(min, max);
                        self.commit(|doc| doc.set(s.path, new));
                    }
                    Kind::Choice(variants) if direction != 0 => {
                        let current = current.and_then(|v| v.as_str());
                        let index = variants.iter().position(|v| Some(v.value) == current);
                        let len = variants.len() as i64;
                        let next = match index {
                            Some(i) => (i as i64 + direction).rem_euclid(len),
                            None if direction > 0 => 0,
                            None => len - 1,
                        };
                        let value = variants[next as usize].value;
                        self.commit(|doc| doc.set(s.path, value));
                    }
                    _ => {}
                }
            }
            Row::FollowOs => {
                let new = match self.config.theme.clone() {
                    ThemeSelection::Single(name) => ThemeSelection::Auto {
                        light: name.clone(),
                        dark: name,
                    },
                    ThemeSelection::Auto { dark, .. } => ThemeSelection::Single(dark),
                };
                self.commit(|doc| doc.set("theme", theme_value(&new)));
                self.clamp_row();
            }
            _ => {}
        }
    }

    /// Enter on a row: open its editor.
    fn activate(&mut self) {
        let Some(row) = self.current_row() else {
            return;
        };
        match row {
            Row::Setting(s) => self.edit_setting(s),
            Row::FollowOs => self.adjust(1),
            Row::Theme(slot) => self.open_theme_picker(slot),
            Row::Keybinding(i) => self.edit_binding_key(Some(i)),
            Row::AddKeybinding => self.edit_binding_key(None),
        }
    }

    fn reset(&mut self) {
        let Some(row) = self.current_row() else {
            return;
        };
        match row {
            Row::Setting(s) => {
                self.commit(|doc| doc.unset(s.path));
            }
            Row::FollowOs | Row::Theme(_) => {
                self.commit(|doc| doc.unset("theme"));
                self.clamp_row();
            }
            Row::Keybinding(i) => {
                self.commit(|doc| doc.remove_keybinding(i));
                self.clamp_row();
            }
            Row::AddKeybinding => {}
        }
    }

    // ----- Editors ---------------------------------------------------------

    fn edit_setting(&mut self, setting: &'static Setting) {
        let current = value_at(&self.values, setting.path);
        match setting.kind {
            Kind::Bool => self.adjust(0),
            Kind::Int { .. } | Kind::Float { .. } | Kind::Text => {
                let text = current.map(format_value).unwrap_or_default();
                self.open_input(InputTarget::Setting(setting), setting.label, &text);
            }
            Kind::StringList => {
                let text = self.row_value(Row::Setting(setting));
                let text = if current.is_some() {
                    text.1
                } else {
                    String::new()
                };
                self.open_input(InputTarget::Setting(setting), setting.label, &text);
            }
            Kind::Choice(variants) => {
                let mut choices = Vec::new();
                if let Some(unset) = setting.unset {
                    choices.push(Choice {
                        pick: Pick::Unset,
                        label: format!("(default: {unset})"),
                        help: "Leave the setting out of the file.".into(),
                    });
                }
                choices.extend(variants.iter().map(|v| Choice {
                    pick: Pick::Value(v.value.into()),
                    label: v.value.into(),
                    help: v.help.into(),
                }));
                let current = self.current_pick(setting);
                let picker = Picker::new(setting.label, choices, &current);
                self.open_picker(picker, PickTarget::Setting(setting));
            }
            Kind::Font => {
                let fonts = self
                    .fonts
                    .get_or_insert_with(|| (self.sources.fonts)())
                    .clone();
                let mut choices = vec![Choice {
                    pick: Pick::Unset,
                    label: "(system monospace)".into(),
                    help: "The system's monospace font.".into(),
                }];
                let current = self.current_pick(setting);
                // A name typed earlier, or a font that isn't installed here.
                if let Pick::Value(family) = &current
                    && !fonts.iter().any(|f| f.eq_ignore_ascii_case(family))
                {
                    choices.push(Choice {
                        pick: current.clone(),
                        label: family.clone(),
                        help: "Not found among the installed monospace fonts.".into(),
                    });
                }
                choices.extend(fonts.into_iter().map(|family| Choice {
                    pick: Pick::Value(family.clone()),
                    label: family,
                    help: String::new(),
                }));
                let picker = Picker::new(
                    "Font family (type to filter, or enter any name)",
                    choices,
                    &current,
                )
                .filterable(true);
                self.open_picker(picker, PickTarget::Setting(setting));
            }
            Kind::Program => {
                let shells = self
                    .shells
                    .get_or_insert_with(|| (self.sources.shells)())
                    .clone();
                self.open_detected_picker(setting, shells, "Program");
            }
            Kind::WslDistribution => {
                // To tell host shells in `shell.program` apart.
                self.shells.get_or_insert_with(|| (self.sources.shells)());
                let distributions = self
                    .wsl_distributions
                    .get_or_insert_with(|| (self.sources.wsl_distributions)())
                    .clone();
                self.open_detected_picker(setting, distributions, "Distribution");
            }
            Kind::OrderedSet(_) => {
                self.mode = Mode::Items {
                    setting,
                    selected: 0,
                }
            }
            Kind::Theme => self.open_theme_picker(ThemeSlot::Single),
        }
    }

    /// A picker of what was found on this system; any other value can be
    /// typed in.
    fn open_detected_picker(&mut self, setting: &'static Setting, found: Vec<Found>, what: &str) {
        let mut choices = vec![Choice {
            pick: Pick::Unset,
            label: format!("(default: {})", setting.unset.unwrap_or("none")),
            help: "Leave the setting out of the file.".into(),
        }];
        let current = self.current_pick(setting);
        if let Pick::Value(value) = &current
            && !found.iter().any(|f| f.value.eq_ignore_ascii_case(value))
        {
            choices.push(Choice {
                pick: current.clone(),
                label: value.clone(),
                help: "Not found on this system.".into(),
            });
        }
        choices.extend(found.into_iter().map(|f| Choice {
            pick: Pick::Value(f.value.clone()),
            label: f.value,
            help: f.help,
        }));
        let title = format!("{what} (type to filter, or enter any name)");
        let picker = Picker::new(title, choices, &current).filterable(true);
        self.open_picker(picker, PickTarget::Setting(setting));
    }

    fn current_pick(&self, setting: &Setting) -> Pick {
        match self
            .doc
            .get(setting.path)
            .and_then(|item| item.as_str())
            .or_else(|| {
                // Not in the file: the default, unless the setting is optional.
                setting
                    .unset
                    .is_none()
                    .then(|| value_at(&self.values, setting.path)?.as_str())
                    .flatten()
            }) {
            Some(value) => Pick::Value(value.into()),
            None => Pick::Unset,
        }
    }

    fn open_theme_picker(&mut self, slot: ThemeSlot) {
        let choices = self
            .themes
            .names()
            .map(|name| {
                let dark = self.themes.get(name).is_some_and(|t| t.is_dark());
                Choice {
                    pick: Pick::Value(name.into()),
                    label: name.into(),
                    help: if dark { "Dark theme" } else { "Light theme" }.into(),
                }
            })
            .collect();
        let title = match slot {
            ThemeSlot::Single => "Theme",
            ThemeSlot::Light => "Light theme",
            ThemeSlot::Dark => "Dark theme",
        };
        let current = Pick::Value(self.theme_name(slot));
        let title = format!("{title} (type to filter)");
        let picker = Picker::new(title, choices, &current).filterable(false);
        self.open_picker(picker, PickTarget::Theme(slot));
    }

    fn open_picker(&mut self, picker: Picker, target: PickTarget) {
        self.mode = Mode::Picker {
            picker,
            target,
            before: self.source.clone(),
        };
    }

    /// The edit that choosing `pick` makes.
    /// Whether `program` is one of the shells found on this system.
    fn is_detected_shell(&self, program: &str) -> bool {
        self.shells
            .iter()
            .flatten()
            .any(|f| f.value.eq_ignore_ascii_case(program))
    }

    fn pick_edit(&self, target: &PickTarget, pick: Pick) -> Box<dyn FnOnce(&mut ConfigDoc)> {
        match (target, pick) {
            (&PickTarget::Setting(s), Pick::Unset) => Box::new(|doc| doc.unset(s.path)),
            // A detected shell runs on the host, so it leaves WSL; a program
            // inside WSL would be looked up in the distribution instead.
            (&PickTarget::Setting(s), Pick::Value(value))
                if s.kind == Kind::Program && self.is_detected_shell(&value) =>
            {
                Box::new(move |doc| {
                    doc.unset("shell.wsl");
                    doc.unset("shell.wsl_user");
                    doc.set(s.path, value);
                })
            }
            // Likewise, a distribution drops a host shell: it isn't
            // installed in there.
            (&PickTarget::Setting(s), Pick::Value(value))
                if s.kind == Kind::WslDistribution
                    && self
                        .doc
                        .get("shell.program")
                        .and_then(|item| item.as_str())
                        .is_some_and(|program| self.is_detected_shell(program)) =>
            {
                Box::new(move |doc| {
                    doc.unset("shell.program");
                    doc.unset("shell.args");
                    doc.set(s.path, value);
                })
            }
            (&PickTarget::Setting(s), Pick::Value(value)) => {
                Box::new(move |doc| doc.set(s.path, value))
            }
            (PickTarget::Theme(_), Pick::Unset) => Box::new(|doc| doc.unset("theme")),
            (PickTarget::Theme(slot), Pick::Value(name)) => {
                let selection = match (self.config.theme.clone(), slot) {
                    (ThemeSelection::Auto { dark, .. }, ThemeSlot::Light) => {
                        ThemeSelection::Auto { light: name, dark }
                    }
                    (ThemeSelection::Auto { light, .. }, ThemeSlot::Dark) => {
                        ThemeSelection::Auto { light, dark: name }
                    }
                    _ => ThemeSelection::Single(name),
                };
                Box::new(move |doc| doc.set("theme", theme_value(&selection)))
            }
            (PickTarget::Action { index, key }, pick) => {
                let action = match pick {
                    Pick::Value(action) => action,
                    Pick::Unset => "none".into(),
                };
                let binding = Keybinding {
                    key: key.clone(),
                    action,
                };
                let index = *index;
                Box::new(move |doc| match index {
                    Some(i) => doc.set_keybinding(i, &binding),
                    None => doc.push_keybinding(&binding),
                })
            }
        }
    }

    fn picker_key(&mut self, key: Key, mut picker: Picker, target: PickTarget, before: String) {
        let live = !matches!(target, PickTarget::Action { .. });
        match key {
            Key::Esc => {
                // Unless the file was changed elsewhere meanwhile: then
                // that change wins and is reloaded instead.
                if live && self.source != before && self.in_sync() {
                    self.restore_text(before);
                }
                return;
            }
            Key::Enter => {
                let Some(pick) = picker.current() else {
                    self.mode = Mode::Picker {
                        picker,
                        target,
                        before,
                    };
                    return;
                };
                let edit = self.pick_edit(&target, pick);
                if self.apply(edit, false) {
                    if self.source != before {
                        self.undo.push(before);
                    }
                    if let PickTarget::Action { index: None, .. } = target {
                        // Select the new entry.
                        self.row = self.doc.keybindings().len() - 1;
                    }
                    return;
                }
            }
            key => {
                if picker.key(key)
                    && live
                    && let Some(pick) = picker.current()
                {
                    // Live preview; Enter keeps it, Esc goes back.
                    let edit = self.pick_edit(&target, pick);
                    self.apply(edit, false);
                }
            }
        }
        self.mode = Mode::Picker {
            picker,
            target,
            before,
        };
    }

    fn open_input(&mut self, target: InputTarget, title: &str, text: &str) {
        let mut mode = Mode::Input {
            input: TextInput::new(text),
            target,
            title: title.into(),
            problem: None,
            note: None,
        };
        if let Mode::Input {
            input,
            target,
            problem,
            note,
            ..
        } = &mut mode
        {
            (*problem, *note) = self.check_input(target, &input.text);
        }
        self.mode = mode;
    }

    /// The value a setting's text input stands for; `None` unsets it.
    fn parse_input(setting: &Setting, text: &str) -> Result<Option<toml_edit::Value>, String> {
        let trimmed = text.trim();
        if trimmed.is_empty() && setting.unset.is_some() {
            return Ok(None);
        }
        let value = match setting.kind {
            Kind::Int { min, max, .. } => {
                let n: i64 = trimmed
                    .parse()
                    .map_err(|_| "not a whole number".to_owned())?;
                if !(min..=max).contains(&n) {
                    return Err(format!("must be between {min} and {max}"));
                }
                n.into()
            }
            Kind::Float { min, max, .. } => {
                let n: f64 = trimmed.parse().map_err(|_| "not a number".to_owned())?;
                if !(min..=max).contains(&n) {
                    return Err(format!(
                        "must be between {} and {}",
                        format_float(min),
                        format_float(max)
                    ));
                }
                n.into()
            }
            Kind::StringList => {
                let items = args::split(text)?;
                toml_edit::Value::Array(items.iter().map(String::as_str).collect())
            }
            _ => text.into(),
        };
        Ok(Some(value))
    }

    fn input_edit(
        setting: &'static Setting,
        value: Option<toml_edit::Value>,
    ) -> impl FnOnce(&mut ConfigDoc) {
        move |doc| match value {
            Some(value) => doc.set(setting.path, value),
            None => doc.unset(setting.path),
        }
    }

    /// Problem and note for the text typed so far.
    fn check_input(
        &self,
        target: &InputTarget,
        text: &str,
    ) -> (Option<String>, Option<(Tone, String)>) {
        match target {
            InputTarget::Setting(setting) => {
                let problem = Self::parse_input(setting, text)
                    .and_then(|value| self.check(Self::input_edit(setting, value)))
                    .err();
                (problem, None)
            }
            InputTarget::BindingKey { index } => match KeyCombo::parse(text) {
                Err(err) => (Some(err.to_string()), None),
                Ok(combo) => {
                    let clash = self
                        .doc
                        .keybindings()
                        .iter()
                        .enumerate()
                        .find(|(i, b)| Some(*i) != *index && KeyCombo::parse(&b.key) == Ok(combo))
                        .map(|(_, b)| {
                            let text = format!("{combo} is already bound to `{}`", b.action);
                            (Tone::Warn, text)
                        });
                    let recognized = || (Tone::Dim, format!("Recognized as {combo}"));
                    (None, Some(clash.unwrap_or_else(recognized)))
                }
            },
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn input_key(
        &mut self,
        key: Key,
        mut input: TextInput,
        target: InputTarget,
        title: String,
        mut problem: Option<String>,
        mut note: Option<(Tone, String)>,
    ) {
        match key {
            Key::Esc => return,
            Key::Enter if problem.is_none() => match &target {
                InputTarget::Setting(setting) => {
                    if let Ok(value) = Self::parse_input(setting, &input.text)
                        && self.commit(Self::input_edit(setting, value))
                    {
                        return;
                    }
                }
                InputTarget::BindingKey { index } => {
                    self.open_action_picker(*index, input.text.trim().to_owned());
                    return;
                }
            },
            key => {
                if input.key(key) {
                    (problem, note) = self.check_input(&target, &input.text);
                }
            }
        }
        self.mode = Mode::Input {
            input,
            target,
            title,
            problem,
            note,
        };
    }

    fn edit_binding_key(&mut self, index: Option<usize>) {
        let key = index
            .and_then(|i| self.doc.keybindings().get(i).map(|b| b.key.clone()))
            .unwrap_or_default();
        let title = if index.is_some() {
            "Key combination"
        } else {
            "New keybinding: key combination"
        };
        self.open_input(InputTarget::BindingKey { index }, title, &key);
    }

    fn open_action_picker(&mut self, index: Option<usize>, key: String) {
        let current = index
            .and_then(|i| self.doc.keybindings().get(i).map(|b| b.action.clone()))
            .map_or(Pick::Unset, Pick::Value);
        let choices = ACTIONS
            .iter()
            .map(|a| Choice {
                pick: Pick::Value(a.value.into()),
                label: a.value.into(),
                help: a.help.into(),
            })
            .collect();
        let picker = Picker::new(format!("Action for {key}"), choices, &current).filterable(false);
        self.open_picker(picker, PickTarget::Action { index, key });
    }

    /// The entries of an ordered set in display order: the chosen ones
    /// first, in their order and with their springs, then the rest.
    pub fn item_entries(&self, setting: &Setting) -> Vec<Entry> {
        let Kind::OrderedSet(variants) = setting.kind else {
            return Vec::new();
        };
        let chosen: Vec<&str> = value_at(&self.values, setting.path)
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str())
            .collect();
        let mut entries: Vec<Entry> = chosen
            .iter()
            .filter_map(|c| variants.iter().find(|v| v.value == *c))
            .map(|v| match v.value {
                SPRING => Entry::Spring { implicit: false },
                value => Entry::Item { value, on: true },
            })
            .collect();
        // Same rule as `nuntio_config::arranged`: without a spring, the
        // last item is pushed to the right edge.
        let springs = variants.iter().any(|v| v.value == SPRING);
        if springs && !entries.is_empty() && !chosen.contains(&SPRING) {
            entries.insert(entries.len() - 1, Entry::Spring { implicit: true });
        }
        entries.extend(
            variants
                .iter()
                .filter(|v| v.value != SPRING && !chosen.contains(&v.value))
                .map(|v| Entry::Item {
                    value: v.value,
                    on: false,
                }),
        );
        entries
    }

    fn items_key(&mut self, key: Key, setting: &'static Setting, mut selected: usize) {
        let mut entries = self.item_entries(setting);
        let last = entries.len().saturating_sub(1);
        let chosen = entries.iter().take_while(|e| e.is_chosen()).count();
        let mut changed = false;
        match key {
            Key::Esc | Key::Enter | Key::Char('q') => return,
            Key::Up | Key::Char('k') => selected = selected.saturating_sub(1),
            Key::Down | Key::Char('j') => selected = (selected + 1).min(last),
            Key::Char(' ') | Key::Char('d') | Key::Delete => match &mut entries[selected] {
                Entry::Item { on, .. } if key == Key::Char(' ') => {
                    *on = !*on;
                    changed = true;
                }
                Entry::Spring { implicit: false } => {
                    entries.remove(selected);
                    // Onto the previous entry if it was the last chosen one.
                    selected = selected.min(chosen.saturating_sub(2));
                    changed = true;
                }
                // Removing it would bring it back.
                Entry::Spring { implicit: true } | Entry::Item { .. } => {}
            },
            Key::Char('s') => {
                selected = (selected + 1).min(chosen);
                entries.insert(selected, Entry::Spring { implicit: false });
                changed = true;
            }
            Key::ShiftUp | Key::Char('K') if selected > 0 && entries[selected].is_chosen() => {
                entries.swap(selected, selected - 1);
                selected -= 1;
                changed = true;
            }
            Key::ShiftDown | Key::Char('J')
                if selected < last && entries[selected + 1].is_chosen() =>
            {
                entries.swap(selected, selected + 1);
                selected += 1;
                changed = true;
            }
            _ => {}
        }
        if changed {
            // Every spring shown is written, so the bar looks as listed.
            let chosen: toml_edit::Array = entries
                .iter()
                .filter_map(|e| match *e {
                    Entry::Item { value, on: true } => Some(value),
                    Entry::Item { on: false, .. } => None,
                    Entry::Spring { .. } => Some(SPRING),
                })
                .collect();
            self.commit(|doc| doc.set(setting.path, chosen));
            // Keep the cursor on the same item after it moved in the order.
            // Springs keep their place: the chosen part is written as is.
            let entries_now = self.item_entries(setting);
            if let Entry::Item { value: moved, .. } = entries[selected]
                && let Some(i) = entries_now
                    .iter()
                    .position(|e| matches!(e, Entry::Item { value, .. } if *value == moved))
            {
                selected = i;
            }
            selected = selected.min(entries_now.len().saturating_sub(1));
        }
        self.mode = Mode::Items { setting, selected };
    }

    // ----- Search ----------------------------------------------------------

    /// Rows matching `query` in label, key path or help, as (section, row).
    pub fn search(&self, query: &str) -> Vec<(usize, usize)> {
        let query = query.trim().to_lowercase();
        let mut results = Vec::new();
        for (s, section) in Section::ALL.iter().enumerate() {
            for (r, row) in self.rows(*section).into_iter().enumerate() {
                let haystack = match row {
                    Row::Setting(setting) => {
                        format!("{} {} {}", setting.label, setting.path, setting.help)
                    }
                    _ => format!("{} {}", self.row_label(row), section.label()),
                };
                if haystack.to_lowercase().contains(&query) {
                    results.push((s, r));
                }
            }
        }
        results
    }

    fn search_key(
        &mut self,
        key: Key,
        mut input: TextInput,
        mut results: Vec<(usize, usize)>,
        mut selected: usize,
    ) {
        match key {
            Key::Esc => return,
            Key::Enter => {
                if let Some(&(section, row)) = results.get(selected) {
                    self.section = section;
                    self.row = row;
                    self.focus = Focus::Rows;
                }
                return;
            }
            Key::Up => selected = selected.saturating_sub(1),
            Key::Down => selected = (selected + 1).min(results.len().saturating_sub(1)),
            key => {
                if input.key(key) {
                    results = self.search(&input.text);
                    selected = 0;
                }
            }
        }
        self.mode = Mode::Search {
            input,
            results,
            selected,
        };
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use super::*;

    #[derive(Clone, Default)]
    struct Memory(Rc<RefCell<Option<String>>>);

    impl Store for Memory {
        fn read(&self) -> io::Result<Option<String>> {
            Ok(self.0.borrow().clone())
        }

        fn write(&mut self, contents: &str) -> io::Result<()> {
            *self.0.borrow_mut() = Some(contents.to_owned());
            Ok(())
        }
    }

    impl Memory {
        fn text(&self) -> String {
            self.0.borrow().clone().unwrap_or_default()
        }

        fn set(&self, text: &str) {
            *self.0.borrow_mut() = Some(text.into());
        }
    }

    fn app(source: Option<&str>) -> (App, Memory) {
        let memory = Memory(Rc::new(RefCell::new(source.map(str::to_owned))));
        let (themes, _) = ThemeSet::load(None);
        let app = App::new(
            Box::new(memory.clone()),
            "config.toml".into(),
            themes,
            Sources {
                fonts: Box::new(|| vec!["Hack".into(), "JetBrains Mono".into()]),
                shells: Box::new(|| {
                    vec![
                        Found {
                            value: "pwsh".into(),
                            help: "PowerShell 7".into(),
                        },
                        Found {
                            value: "cmd".into(),
                            help: "Command Prompt".into(),
                        },
                    ]
                }),
                wsl_distributions: Box::new(|| {
                    vec![
                        Found {
                            value: "Ubuntu".into(),
                            help: "The default distribution.".into(),
                        },
                        Found {
                            value: "Debian".into(),
                            help: String::new(),
                        },
                    ]
                }),
            },
        )
        .unwrap();
        (app, memory)
    }

    fn go_to(app: &mut App, path: &str) {
        let setting = SETTINGS.iter().find(|s| s.path == path).unwrap();
        app.section = Section::ALL
            .iter()
            .position(|s| *s == setting.section)
            .unwrap();
        app.row = app
            .rows(setting.section)
            .iter()
            .position(|r| *r == Row::Setting(setting))
            .unwrap();
        app.focus = Focus::Rows;
    }

    fn keys(app: &mut App, keys: &[Key]) {
        for key in keys {
            app.key(*key);
        }
    }

    fn type_text(app: &mut App, text: &str) {
        for c in text.chars() {
            app.key(Key::Char(c));
        }
    }

    #[test]
    fn toggles_and_undoes() {
        let (mut app, memory) = app(None);
        go_to(&mut app, "status_bar.enabled");
        app.key(Key::Char(' '));
        assert_eq!(memory.text(), "[status_bar]\nenabled = true\n");
        assert!(app.config.status_bar.enabled);
        app.key(Key::Char('u'));
        assert_eq!(memory.text(), "");
        assert!(!app.config.status_bar.enabled);
    }

    #[test]
    fn steps_numbers_within_range() {
        let (mut app, memory) = app(Some("[panes]\ndim_inactive = 0.95 # dim\n"));
        go_to(&mut app, "panes.dim_inactive");
        app.key(Key::Right);
        assert_eq!(memory.text(), "[panes]\ndim_inactive = 1.0 # dim\n");
        app.key(Key::Right);
        assert_eq!(memory.text(), "[panes]\ndim_inactive = 1.0 # dim\n");
        keys(&mut app, &[Key::Left, Key::Left]);
        assert_eq!(memory.text(), "[panes]\ndim_inactive = 0.9 # dim\n");

        go_to(&mut app, "font.size");
        app.key(Key::Right);
        assert_eq!(app.config.font.size, 13.5);
    }

    #[test]
    fn cycles_choices() {
        let (mut app, memory) = app(None);
        go_to(&mut app, "tabs.title");
        app.key(Key::Right);
        assert!(memory.text().contains("title = \"path\""));
        keys(&mut app, &[Key::Left, Key::Left]);
        assert!(memory.text().contains("title = \"application\""));
    }

    #[test]
    fn choice_picker_offers_only_valid_values_and_previews() {
        let (mut app, memory) = app(None);
        go_to(&mut app, "window.macos_titlebar");
        app.key(Key::Enter);
        let Mode::Picker { picker, .. } = &app.mode else {
            panic!("no picker");
        };
        let labels: Vec<_> = picker.choices.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(
            labels,
            [
                "(default: derived from decorations)",
                "native",
                "transparent",
                "none"
            ]
        );
        app.key(Key::Down);
        assert!(
            memory.text().contains("macos_titlebar = \"native\""),
            "live"
        );
        app.key(Key::Esc);
        assert_eq!(memory.text(), "", "Esc restores");

        app.key(Key::Enter);
        keys(&mut app, &[Key::Down, Key::Down, Key::Enter]);
        assert!(memory.text().contains("\"transparent\""));
        assert!(matches!(app.mode, Mode::Normal));
        app.key(Key::Char('u'));
        assert_eq!(memory.text(), "", "one undo step for the whole picker");

        // Esc doesn't overwrite a change made elsewhere during the preview.
        app.key(Key::Enter);
        app.key(Key::Down);
        memory.set("scrollback = 7\n");
        app.key(Key::Esc);
        assert_eq!(memory.text(), "scrollback = 7\n");
        assert_eq!(app.config.scrollback, 7);
    }

    #[test]
    fn theme_picker_and_follow_os() {
        let (mut app, memory) = app(None);
        app.section = Section::ALL
            .iter()
            .position(|s| *s == Section::Theme)
            .unwrap();
        app.row = 1;
        app.key(Key::Enter);
        let Mode::Picker { picker, .. } = &app.mode else {
            panic!("no picker");
        };
        assert!(picker.choices.iter().any(|c| c.label == "Dracula"));
        app.key(Key::Char('d'));
        app.key(Key::Char('r'));
        app.key(Key::Enter);
        assert_eq!(memory.text(), "theme = \"Dracula\"\n");

        app.row = 0;
        app.key(Key::Char(' '));
        assert_eq!(
            memory.text(),
            "theme = { light = \"Dracula\", dark = \"Dracula\" }\n"
        );
        assert_eq!(app.rows(Section::Theme).len(), 3);
        app.row = 1;
        app.key(Key::Enter);
        type_text(&mut app, "solarized l");
        app.key(Key::Enter);
        assert_eq!(
            memory.text(),
            "theme = { light = \"Solarized Light\", dark = \"Dracula\" }\n"
        );
    }

    #[test]
    fn font_picker_accepts_any_name() {
        let (mut app, memory) = app(None);
        go_to(&mut app, "font.family");
        app.key(Key::Enter);
        type_text(&mut app, "Hack Nerd Font Mono");
        app.key(Key::Enter);
        assert_eq!(memory.text(), "[font]\nfamily = \"Hack Nerd Font Mono\"\n");
        app.key(Key::Enter);
        app.key(Key::Up);
        app.key(Key::Enter);
        assert_eq!(memory.text(), "", "(system monospace) unsets");
    }

    #[test]
    fn shell_picker_offers_installed_shells() {
        let (mut app, memory) = app(None);
        go_to(&mut app, "shell.program");
        app.key(Key::Enter);
        let Mode::Picker { picker, .. } = &app.mode else {
            panic!("no picker");
        };
        let labels: Vec<_> = picker.visible.iter().map(|&e| picker.label(e)).collect();
        assert_eq!(labels.len(), 3);
        assert!(labels[0].starts_with("(default: "));
        assert_eq!(labels[1..], ["pwsh", "cmd"]);
        app.key(Key::Down);
        app.key(Key::Enter);
        assert_eq!(memory.text(), "shell = { program = \"pwsh\" }\n");

        app.key(Key::Enter);
        type_text(&mut app, "nu");
        // The typed text is the last entry.
        app.key(Key::End);
        app.key(Key::Enter);
        assert_eq!(memory.text(), "shell = { program = \"nu\" }\n");
        app.key(Key::Enter);
        let Mode::Picker { picker, .. } = &app.mode else {
            panic!("no picker");
        };
        assert_eq!(picker.label(picker.visible[picker.selected]), "nu");
        assert_eq!(
            picker.choices[1].help, "Not found on this system.",
            "a value typed earlier stays selectable"
        );
    }

    #[test]
    fn wsl_picker_offers_installed_distributions() {
        let (mut app, memory) = app(None);
        go_to(&mut app, "shell.wsl");
        app.key(Key::Enter);
        type_text(&mut app, "deb");
        app.key(Key::Enter);
        assert_eq!(memory.text(), "shell = { wsl = \"Debian\" }\n");
    }

    #[test]
    fn host_shells_and_distributions_replace_each_other() {
        let (mut app, memory) = app(Some(
            "shell = { wsl = \"Ubuntu\", wsl_user = \"root\", program = \"fish\" }\n",
        ));
        go_to(&mut app, "shell.program");
        app.key(Key::Enter);
        type_text(&mut app, "pwsh");
        app.key(Key::Enter);
        assert_eq!(memory.text(), "shell = { program = \"pwsh\" }\n");

        go_to(&mut app, "shell.wsl");
        app.key(Key::Enter);
        type_text(&mut app, "ubu");
        app.key(Key::Enter);
        assert_eq!(memory.text(), "shell = { wsl = \"Ubuntu\" }\n");

        // A program that isn't a host shell runs inside the distribution.
        go_to(&mut app, "shell.program");
        app.key(Key::Enter);
        type_text(&mut app, "zsh");
        app.key(Key::End);
        app.key(Key::Enter);
        assert_eq!(
            memory.text(),
            "shell = { wsl = \"Ubuntu\", program = \"zsh\" }\n"
        );
    }

    #[test]
    fn text_input_validates_while_typing() {
        let (mut app, memory) = app(None);
        go_to(&mut app, "status_bar.datetime_format");
        app.key(Key::Enter);
        app.key(Key::Ctrl('u'));
        type_text(&mut app, "%H:%");
        let Mode::Input { problem, .. } = &app.mode else {
            panic!("no input");
        };
        assert!(problem.as_deref().unwrap().contains("strftime"));
        app.key(Key::Enter);
        assert_eq!(memory.text(), "", "invalid input isn't written");
        type_text(&mut app, "M");
        app.key(Key::Enter);
        assert_eq!(memory.text(), "[status_bar]\ndatetime_format = \"%H:%M\"\n");
    }

    #[test]
    fn number_input_checks_the_range() {
        let (mut app, memory) = app(None);
        go_to(&mut app, "window.padding.x");
        app.key(Key::Enter);
        app.key(Key::Ctrl('u'));
        type_text(&mut app, "1200");
        let Mode::Input { problem, .. } = &app.mode else {
            panic!("no input");
        };
        assert!(problem.as_deref().unwrap().contains("between 0 and 200"));
        keys(&mut app, &[Key::Backspace, Key::Backspace, Key::Enter]);
        let config = nuntio_config::parse(&memory.text()).unwrap().config;
        assert_eq!(config.window.padding.x, 12);
        assert_eq!(config.window.padding.y, 6);
    }

    #[test]
    fn shell_arguments() {
        let (mut app, memory) = app(Some("shell = { program = \"fish\" }\n"));
        go_to(&mut app, "shell.args");
        app.key(Key::Enter);
        type_text(&mut app, "-l -c 'exec tmux'");
        app.key(Key::Enter);
        assert_eq!(
            memory.text(),
            "shell = { program = \"fish\", args = [\"-l\", \"-c\", \"exec tmux\"] }\n"
        );
        assert_eq!(
            app.row_value(app.current_row().unwrap()).1,
            "-l -c \"exec tmux\""
        );
    }

    #[test]
    fn refuses_changes_that_break_a_valid_config() {
        let (mut app, memory) = app(Some("shell = { program = \"fish\", args = [\"-l\"] }\n"));
        go_to(&mut app, "shell.program");
        app.key(Key::Char('d'));
        assert!(memory.text().contains("fish"), "args need a program");
        assert!(matches!(app.message, Some((Tone::Error, _))));
    }

    #[test]
    fn broken_files_can_still_be_fixed() {
        let (mut app, memory) = app(Some("[font]\nsize = 100.0\n"));
        assert!(app.error.as_deref().unwrap().contains("font.size"));
        go_to(&mut app, "tabs.hide_when_single");
        app.key(Key::Char(' '));
        assert!(memory.text().contains("hide_when_single = false"));
        go_to(&mut app, "font.size");
        app.key(Key::Char('d'));
        assert!(app.error.is_none());
    }

    #[test]
    fn ordered_set_editor() {
        let (mut app, memory) = app(None);
        go_to(&mut app, "status_bar.items");
        app.key(Key::Enter);
        // cpu memory network battery (spring) datetime: drop cpu, move
        // datetime first.
        app.key(Key::Char(' '));
        let items = || {
            nuntio_config::parse(&memory.text())
                .unwrap()
                .config
                .status_bar
                .items
        };
        use nuntio_config::StatusItem::*;
        // The automatic spring is written, so the bar stays as shown.
        assert_eq!(items(), [Memory, Network, Battery, Spring, Datetime]);
        let Mode::Items { selected, .. } = app.mode else {
            panic!("no items");
        };
        // The cursor followed cpu to the end; datetime is right before it.
        assert_eq!(selected, 5);
        app.key(Key::Up);
        for _ in 0..4 {
            app.key(Key::ShiftUp);
        }
        assert_eq!(items(), [Datetime, Memory, Network, Battery, Spring]);
        let Mode::Items { selected, .. } = app.mode else {
            panic!("no items");
        };
        assert_eq!(selected, 0, "the cursor follows the moved item");
    }

    #[test]
    fn springs_are_added_moved_and_removed() {
        let (mut app, memory) = app(Some("[status_bar]\nitems = [\"cpu\", \"datetime\"]\n"));
        go_to(&mut app, "status_bar.items");
        app.key(Key::Enter);
        let setting = SETTINGS
            .iter()
            .find(|s| s.path == "status_bar.items")
            .unwrap();
        let entries = |app: &App| app.item_entries(setting);
        let spring = |implicit| Entry::Spring { implicit };
        let item = |value, on| Entry::Item { value, on };
        assert_eq!(
            entries(&app)[..3],
            [item("cpu", true), spring(true), item("datetime", true)]
        );
        // The automatic spring can't be removed, and nothing is written.
        app.key(Key::Down);
        app.key(Key::Char('d'));
        assert_eq!(entries(&app)[1], spring(true));
        assert!(memory.text().contains("[\"cpu\", \"datetime\"]"));

        // A second spring after cpu: both are written.
        app.key(Key::Up);
        app.key(Key::Char('s'));
        let Mode::Items { selected, .. } = app.mode else {
            panic!("no items");
        };
        assert_eq!(selected, 1, "the cursor is on the new spring");
        assert!(
            memory
                .text()
                .contains("items = [\"cpu\", \"<->\", \"<->\", \"datetime\"]"),
            "{}",
            memory.text()
        );
        // Center cpu: move a spring in front of it.
        app.key(Key::ShiftUp);
        let items = || memory.text();
        assert!(items().contains("[\"<->\", \"cpu\", \"<->\", \"datetime\"]"));
        // Remove the spring after cpu; the cursor stays in the list.
        app.key(Key::Down);
        app.key(Key::Down);
        app.key(Key::Delete);
        assert!(
            items().contains("[\"<->\", \"cpu\", \"datetime\"]"),
            "{}",
            items()
        );
        let Mode::Items { selected, .. } = app.mode else {
            panic!("no items");
        };
        assert_eq!(selected, 2, "on datetime");
        // Space hides items but doesn't remove springs by accident.
        app.key(Key::Up);
        app.key(Key::Char(' '));
        assert!(items().contains("[\"<->\", \"datetime\"]"), "{}", items());
        // `s` below the chosen items adds the spring at their end.
        for _ in 0..10 {
            app.key(Key::Down);
        }
        app.key(Key::Char('s'));
        assert!(
            items().contains("[\"<->\", \"datetime\", \"<->\"]"),
            "{}",
            items()
        );
    }

    #[test]
    fn preview_sketches_the_bar() {
        let item = |value| Entry::Item { value, on: true };
        let spring = Entry::Spring { implicit: false };
        let hidden = Entry::Item {
            value: "battery",
            on: false,
        };
        assert_eq!(
            bar_preview(
                &[
                    item("cpu"),
                    item("memory"),
                    spring,
                    item("datetime"),
                    hidden
                ],
                30
            ),
            "cpu memory ·········· datetime"
        );
        assert_eq!(
            bar_preview(&[spring, item("cpu"), spring], 11),
            "··· cpu ···"
        );
        assert_eq!(
            bar_preview(&[item("cpu"), item("memory")], 12),
            "cpu memory"
        );
        // Too narrow: memory goes, datetime stays.
        assert_eq!(
            bar_preview(&[item("cpu"), item("memory"), spring, item("datetime")], 14),
            "cpu · datetime"
        );
    }

    #[test]
    fn keybindings_are_added_edited_and_removed() {
        let (mut app, memory) = app(None);
        app.section = Section::ALL
            .iter()
            .position(|s| *s == Section::Keybindings)
            .unwrap();
        app.row = 0;
        assert_eq!(app.current_row(), Some(Row::AddKeybinding));
        app.key(Key::Enter);
        type_text(&mut app, "Ctrl+Nope");
        assert!(matches!(
            &app.mode,
            Mode::Input {
                problem: Some(_),
                ..
            }
        ));
        app.key(Key::Enter);
        assert!(matches!(app.mode, Mode::Input { .. }), "Enter is blocked");
        for _ in 0..4 {
            app.key(Key::Backspace);
        }
        type_text(&mut app, "Enter");
        app.key(Key::Enter);
        let Mode::Picker { picker, .. } = &app.mode else {
            panic!("no action picker");
        };
        assert_eq!(picker.choices.len(), ACTIONS.len());
        assert!(
            memory.text().is_empty(),
            "nothing written before the action"
        );
        type_text(&mut app, "split_v");
        app.key(Key::Enter);
        assert_eq!(
            memory.text(),
            "[[keybindings]]\nkey = \"Ctrl+Enter\"\naction = \"split_vertical\"\n"
        );
        assert_eq!(app.current_row(), Some(Row::Keybinding(0)));
        assert_eq!(app.row_label(Row::Keybinding(0)), "Ctrl+Enter");

        // A second binding for the same key is flagged.
        app.key(Key::Char('a'));
        type_text(&mut app, "ctrl + enter");
        let Mode::Input { note, .. } = &app.mode else {
            panic!("no input");
        };
        let (tone, note) = note.as_ref().unwrap();
        assert_eq!(*tone, Tone::Warn);
        assert!(note.contains("already bound"));
        app.key(Key::Enter);
        app.key(Key::Enter);
        assert!(app.binding_problem(1).unwrap().contains("entry 1"));

        app.key(Key::Char('x'));
        app.row = 0;
        app.key(Key::Char('x'));
        assert_eq!(memory.text(), "");
        assert_eq!(app.current_row(), Some(Row::AddKeybinding));
    }

    #[test]
    fn outside_changes_are_reloaded_first() {
        let (mut app, memory) = app(Some("scrollback = 5\n"));
        memory.set("scrollback = 7\n");
        go_to(&mut app, "scrollback");
        app.key(Key::Right);
        assert_eq!(
            memory.text(),
            "scrollback = 7\n",
            "not applied to stale state"
        );
        assert_eq!(app.config.scrollback, 7);
        app.key(Key::Right);
        assert_eq!(memory.text(), "scrollback = 1007\n");
    }

    #[test]
    fn restore_original_is_undoable() {
        let (mut app, memory) = app(Some("# mine\n"));
        go_to(&mut app, "mouse.copy_on_select");
        app.key(Key::Char(' '));
        go_to(&mut app, "tabs.hide_when_single");
        app.key(Key::Char(' '));
        app.key(Key::Char('R'));
        assert_eq!(memory.text(), "# mine\n");
        assert!(!app.is_modified());
        app.key(Key::Char('u'));
        assert!(memory.text().contains("hide_when_single"));
    }

    #[test]
    fn search_jumps_to_settings() {
        let (mut app, _) = app(None);
        app.key(Key::Char('/'));
        type_text(&mut app, "strftime");
        app.key(Key::Enter);
        assert_eq!(
            app.current_row(),
            Some(Row::Setting(
                SETTINGS
                    .iter()
                    .find(|s| s.path == "status_bar.datetime_format")
                    .unwrap()
            ))
        );
    }

    #[test]
    fn navigation_moves_between_sections() {
        let (mut app, _) = app(None);
        assert_eq!(app.current_section(), Section::General);
        // Down past the last row of a section enters the next one.
        for _ in 0..app.rows(Section::General).len() {
            app.key(Key::Down);
        }
        assert_eq!(app.current_section(), Section::Theme);
        app.key(Key::Up);
        assert_eq!(app.current_section(), Section::General);
        app.key(Key::Tab);
        app.key(Key::End);
        assert_eq!(app.current_section(), Section::MacOs);
    }

    #[test]
    fn values_and_defaults_are_shown() {
        let (mut app, _) = app(Some("[panes]\ndim_inactive = 0.3\n"));
        go_to(&mut app, "panes.dim_inactive");
        let row = app.current_row().unwrap();
        assert_eq!(app.row_value(row), (Tone::Normal, "0.3".into()));
        assert!(app.row_is_set(row));
        let details = app.details();
        assert!(
            details.iter().any(|(_, l)| l == "Default: 0.15"),
            "{details:?}"
        );
        go_to(&mut app, "font.family");
        let row = app.current_row().unwrap();
        assert_eq!(app.row_value(row).0, Tone::Dim);
        assert!(!app.row_is_set(row));
    }
}
