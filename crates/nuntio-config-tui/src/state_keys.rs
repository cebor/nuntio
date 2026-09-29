//! Keys: navigation, the editors (pickers, text inputs, the ordered-set
//! checklist) and search.

use nuntio_config::schema::{Kind, SPRING, Section, Setting};
use nuntio_config::toml_edit;
use nuntio_config::{ACTIONS, ConfigDoc, KeyCombo, Keybinding, ThemeSelection};

use super::{
    App, Entry, Focus, InputMode, InputTarget, ItemsMode, Key, Mode, PickTarget, PickerMode, Row,
    SearchMode, ThemeSlot, Tone, UPDATE_ITEM, format_float, format_value, round_to_step,
    theme_value, value_at,
};
use crate::args;
use crate::detect::Found;
use crate::widgets::{Choice, Pick, Picker, TextInput};

impl App {
    // ----- Keys ------------------------------------------------------------

    pub fn key(&mut self, key: Key) {
        if key == Key::Ctrl('c') {
            self.quit = true;
            return;
        }
        match std::mem::replace(&mut self.mode, Mode::Normal) {
            Mode::Normal => self.normal_key(key),
            Mode::Picker(mode) => self.picker_key(key, mode),
            Mode::Input(mode) => self.input_key(key, mode),
            Mode::Items(mode) => self.items_key(key, mode),
            Mode::Search(mode) => self.search_key(key, mode),
        }
    }

    fn normal_key(&mut self, key: Key) {
        self.message = None;
        match key {
            Key::Char('q') => self.quit = true,
            Key::Char('u') => self.undo(),
            Key::Char('R') => self.restore_original(),
            Key::Char('e') => self.open_editor = true,
            Key::Char('/') => {
                self.mode = Mode::Search(SearchMode {
                    input: TextInput::default(),
                    results: self.search(""),
                    selected: 0,
                })
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
                self.mode = Mode::Items(ItemsMode {
                    setting,
                    selected: 0,
                })
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
        self.mode = Mode::Picker(PickerMode {
            picker,
            target,
            before: self.source.clone(),
        });
    }

    /// Whether `program` is one of the shells found on this system.
    fn is_detected_shell(&self, program: &str) -> bool {
        self.shells
            .iter()
            .flatten()
            .any(|f| f.value.eq_ignore_ascii_case(program))
    }

    /// The edit that choosing `pick` makes.
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

    fn picker_key(&mut self, key: Key, mut mode: PickerMode) {
        let live = !matches!(mode.target, PickTarget::Action { .. });
        let reloads = self.reloads;
        match key {
            Key::Esc => {
                // Unless the file was changed elsewhere meanwhile: then
                // that change wins and is reloaded instead.
                if live && self.source != mode.before && self.in_sync() {
                    self.restore_text(mode.before);
                }
                return;
            }
            Key::Enter => {
                if let Some(pick) = mode.picker.current()
                    && self.apply(self.pick_edit(&mode.target, pick), false)
                {
                    if self.source != mode.before {
                        self.undo.push(mode.before);
                    }
                    if let PickTarget::Action { index: None, .. } = mode.target {
                        // Select the new entry.
                        self.row = self.doc.keybindings().len() - 1;
                    }
                    return;
                }
            }
            key => {
                if mode.picker.key(key)
                    && live
                    && let Some(pick) = mode.picker.current()
                {
                    // Live preview; Enter keeps it, Esc goes back.
                    self.apply(self.pick_edit(&mode.target, pick), false);
                }
            }
        }
        // A reload meanwhile is the new state to return to.
        if self.reloads != reloads {
            mode.before = self.source.clone();
        }
        self.mode = Mode::Picker(mode);
    }

    fn open_input(&mut self, target: InputTarget, title: &str, text: &str) {
        let (problem, note) = self.check_input(&target, text);
        self.mode = Mode::Input(InputMode {
            input: TextInput::new(text),
            target,
            title: title.into(),
            problem,
            note,
        });
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

    fn input_key(&mut self, key: Key, mut mode: InputMode) {
        match key {
            Key::Esc => return,
            Key::Enter if mode.problem.is_none() => match &mode.target {
                InputTarget::Setting(setting) => {
                    if let Ok(value) = Self::parse_input(setting, &mode.input.text)
                        && self.commit(Self::input_edit(setting, value))
                    {
                        return;
                    }
                }
                InputTarget::BindingKey { index } => {
                    self.open_action_picker(*index, mode.input.text.trim().to_owned());
                    return;
                }
            },
            key => {
                if mode.input.key(key) {
                    (mode.problem, mode.note) = self.check_input(&mode.target, &mode.input.text);
                }
            }
        }
        self.mode = Mode::Input(mode);
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

    /// An item of an ordered set that is switched off elsewhere: the
    /// `update` status bar item while its switch under Updates is off.
    pub fn item_disabled(&self, value: &str) -> bool {
        value == UPDATE_ITEM && !self.config.updates.status_bar
    }

    fn items_key(&mut self, key: Key, mode: ItemsMode) {
        let ItemsMode {
            setting,
            mut selected,
        } = mode;
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
        self.mode = Mode::Items(ItemsMode { setting, selected });
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

    fn search_key(&mut self, key: Key, mut mode: SearchMode) {
        match key {
            Key::Esc => return,
            Key::Enter => {
                if let Some(&(section, row)) = mode.results.get(mode.selected) {
                    self.section = section;
                    self.row = row;
                    self.focus = Focus::Rows;
                }
                return;
            }
            Key::Up => mode.selected = mode.selected.saturating_sub(1),
            Key::Down => {
                mode.selected = (mode.selected + 1).min(mode.results.len().saturating_sub(1));
            }
            key => {
                if mode.input.key(key) {
                    mode.results = self.search(&mode.input.text);
                    mode.selected = 0;
                }
            }
        }
        self.mode = Mode::Search(mode);
    }
}
