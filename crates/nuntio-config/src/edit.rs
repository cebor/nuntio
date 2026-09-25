//! Changing single settings in a config file while keeping its comments,
//! order and formatting.

use std::io;
use std::path::Path;
use std::str::FromStr;

use toml_edit::{ArrayOfTables, DocumentMut, InlineTable, Item, Table, TableLike, Value};

use crate::Keybinding;

/// A config file as a document that can be edited in place.
#[derive(Debug, Clone, Default)]
pub struct ConfigDoc {
    doc: DocumentMut,
}

impl ConfigDoc {
    /// Parse TOML source. Only syntax is checked here; use
    /// [`parse`](crate::parse) on the result to validate it.
    pub fn parse(source: &str) -> Result<Self, String> {
        DocumentMut::from_str(source)
            .map(|doc| Self { doc })
            .map_err(|e| e.to_string().trim_end().to_owned())
    }

    /// The item at a dotted path like `font.size`, if the file sets it.
    pub fn get(&self, path: &str) -> Option<&Item> {
        let mut keys = path.split('.');
        let mut item = self.doc.get(keys.next()?)?;
        for key in keys {
            item = item.as_table_like()?.get(key)?;
        }
        Some(item)
    }

    pub fn is_set(&self, path: &str) -> bool {
        self.get(path).is_some()
    }

    /// Set the value at `path`, creating tables on the way: `[section]`
    /// headers at the top level, inline tables below (and for `shell`, as
    /// in the docs). A comment after an old value is kept.
    pub fn set(&mut self, path: &str, value: impl Into<Value>) {
        let keys: Vec<&str> = path.split('.').collect();
        let was_empty = self.doc.as_table().is_empty();
        set_in(self.doc.as_table_mut(), &keys, value.into(), true);
        if was_empty {
            self.keep_comments_on_top();
        }
    }

    /// In a file with nothing but comments, the comments are the document's
    /// trailing text and would end up below the first setting added.
    fn keep_comments_on_top(&mut self) {
        let trailing = self.doc.trailing().as_str().unwrap_or_default().to_owned();
        if trailing.trim().is_empty() {
            return;
        }
        let root = self.doc.as_table_mut();
        let Some(key) = root.iter().next().map(|(key, _)| key.to_owned()) else {
            return;
        };
        let prefix = format!("{}\n\n", trailing.trim_end());
        match root.get_mut(&key) {
            Some(Item::Table(table)) => table.decor_mut().set_prefix(prefix),
            Some(Item::ArrayOfTables(tables)) => match tables.get_mut(0) {
                Some(table) => table.decor_mut().set_prefix(prefix),
                None => return,
            },
            Some(Item::Value(_)) => match root.key_mut(&key) {
                Some(mut key) => key.leaf_decor_mut().set_prefix(prefix),
                None => return,
            },
            _ => return,
        }
        self.doc.set_trailing("");
    }

    /// Remove the value at `path`, so that the default applies. Tables that
    /// become empty are removed too, unless a comment is attached to them.
    pub fn unset(&mut self, path: &str) {
        let keys: Vec<&str> = path.split('.').collect();
        remove_in(self.doc.as_table_mut(), &keys);
    }

    /// The `[[keybindings]]` entries, in file order. Missing fields are
    /// empty strings.
    pub fn keybindings(&self) -> Vec<Keybinding> {
        let read = |table: &dyn TableLike| {
            let field = |key| {
                table
                    .get(key)
                    .and_then(Item::as_str)
                    .unwrap_or_default()
                    .to_owned()
            };
            Keybinding {
                key: field("key"),
                action: field("action"),
            }
        };
        match self.doc.get("keybindings") {
            Some(Item::ArrayOfTables(tables)) => tables.iter().map(|t| read(t)).collect(),
            Some(Item::Value(Value::Array(array))) => array
                .iter()
                .filter_map(Value::as_inline_table)
                .map(|t| read(t))
                .collect(),
            _ => Vec::new(),
        }
    }

    /// Replace the keybinding at `index`; out of range does nothing.
    pub fn set_keybinding(&mut self, index: usize, binding: &Keybinding) {
        let table: Option<&mut dyn TableLike> = match self.doc.get_mut("keybindings") {
            Some(Item::ArrayOfTables(tables)) => tables.get_mut(index).map(|t| t as _),
            Some(Item::Value(Value::Array(array))) => array
                .get_mut(index)
                .and_then(Value::as_inline_table_mut)
                .map(|t| t as _),
            _ => None,
        };
        if let Some(table) = table {
            put(table, "key", binding.key.as_str().into());
            put(table, "action", binding.action.as_str().into());
        }
    }

    pub fn push_keybinding(&mut self, binding: &Keybinding) {
        match self.doc.get_mut("keybindings") {
            Some(Item::Value(Value::Array(array))) => {
                let mut table = InlineTable::new();
                table.insert("key", binding.key.as_str().into());
                table.insert("action", binding.action.as_str().into());
                array.push(table);
            }
            Some(Item::ArrayOfTables(tables)) => tables.push(binding_table(binding)),
            _ => {
                let was_empty = self.doc.as_table().is_empty();
                let mut tables = ArrayOfTables::new();
                tables.push(binding_table(binding));
                self.doc.insert("keybindings", Item::ArrayOfTables(tables));
                if was_empty {
                    self.keep_comments_on_top();
                }
            }
        }
    }

    pub fn remove_keybinding(&mut self, index: usize) {
        let empty = match self.doc.get_mut("keybindings") {
            Some(Item::ArrayOfTables(tables)) if index < tables.len() => {
                tables.remove(index);
                tables.is_empty()
            }
            Some(Item::Value(Value::Array(array))) if index < array.len() => {
                array.remove(index);
                array.is_empty()
            }
            _ => false,
        };
        if empty {
            self.doc.remove("keybindings");
        }
    }
}

impl std::fmt::Display for ConfigDoc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.doc.fmt(f)
    }
}

fn binding_table(binding: &Keybinding) -> Table {
    let mut table = Table::new();
    table.insert("key", Item::Value(binding.key.as_str().into()));
    table.insert("action", Item::Value(binding.action.as_str().into()));
    table
}

/// Insert or replace `key`, keeping the old value's surrounding whitespace
/// and comment.
fn put(table: &mut dyn TableLike, key: &str, mut value: Value) {
    if let Some(Item::Value(old)) = table.get_mut(key) {
        *value.decor_mut() = old.decor().clone();
        *old = value;
    } else {
        table.insert(key, Item::Value(value));
    }
}

fn set_in(table: &mut dyn TableLike, keys: &[&str], value: Value, top: bool) {
    let [key, rest @ ..] = keys else { return };
    if rest.is_empty() {
        put(table, key, value);
        return;
    }
    let is_table = table
        .get(key)
        .is_some_and(|item| item.as_table_like().is_some());
    if !is_table {
        let new = if top && *key != "shell" {
            let mut section = Table::new();
            // Written as `[window]` even when only `[window.padding]` is set.
            section.set_implicit(false);
            Item::Table(section)
        } else {
            Item::Value(Value::InlineTable(InlineTable::new()))
        };
        table.insert(key, new);
    }
    let item = table.get_mut(key).expect("just inserted");
    let child = item
        .as_table_like_mut()
        .expect("just made sure it's a table");
    set_in(child, rest, value, false);
    tidy(item);
}

/// Normalize the spacing of an inline table after keys were added or
/// removed, which would otherwise give `{ x = 8 , y = 6 }`. Inline tables
/// can't hold comments, so nothing is lost.
fn tidy(item: &mut Item) {
    if let Some(table) = item.as_inline_table_mut() {
        table.fmt();
    }
}

/// Returns whether something was removed.
fn remove_in(table: &mut dyn TableLike, keys: &[&str]) -> bool {
    let [key, rest @ ..] = keys else {
        return false;
    };
    if rest.is_empty() {
        return table.remove(key).is_some();
    }
    let Some(item) = table.get_mut(key) else {
        return false;
    };
    let Some(child) = item.as_table_like_mut() else {
        return false;
    };
    let removed = remove_in(child, rest);
    let now_empty = child.is_empty();
    if removed && now_empty && !has_comment(item) {
        table.remove(key);
    } else if removed {
        tidy(item);
    }
    removed
}

fn has_comment(item: &Item) -> bool {
    let decor = match item {
        Item::Table(table) => table.decor(),
        Item::Value(value) => value.decor(),
        _ => return false,
    };
    [decor.prefix(), decor.suffix()]
        .into_iter()
        .flatten()
        .any(|raw| raw.as_str().is_some_and(|s| s.contains('#')))
}

/// Write `contents` to `path` in one step, so a watcher never sees a
/// half-written file. A symlinked config is written at its target; missing
/// directories are created.
pub fn write_config(path: &Path, contents: &str) -> io::Result<()> {
    let target = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
    let dir = match target.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir.to_owned(),
        _ => std::env::current_dir()?,
    };
    std::fs::create_dir_all(&dir)?;
    let name = target
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "not a file path"))?;
    let temp = dir.join(format!(
        ".{}.{}.tmp",
        name.to_string_lossy(),
        std::process::id()
    ));
    std::fs::write(&temp, contents)?;
    if let Ok(meta) = std::fs::metadata(&target) {
        let _ = std::fs::set_permissions(&temp, meta.permissions());
    }
    std::fs::rename(&temp, &target).inspect_err(|_| {
        let _ = std::fs::remove_file(&temp);
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(source: &str) -> ConfigDoc {
        ConfigDoc::parse(source).unwrap()
    }

    fn edited(source: &str, edit: impl FnOnce(&mut ConfigDoc)) -> String {
        let mut doc = doc(source);
        edit(&mut doc);
        doc.to_string()
    }

    #[test]
    fn keeps_comments_and_layout() {
        let source =
            "# my config\nscrollback = 5000 # lines\n\n[font]\n# the font\nsize = 12.0 # pt\n";
        let out = edited(source, |d| {
            d.set("font.size", 14.5);
            d.set("scrollback", 100i64);
        });
        assert_eq!(
            out,
            "# my config\nscrollback = 100 # lines\n\n[font]\n# the font\nsize = 14.5 # pt\n"
        );
    }

    #[test]
    fn creates_sections() {
        let out = edited("", |d| d.set("tabs.title", "path"));
        assert_eq!(out, "[tabs]\ntitle = \"path\"\n");
        let loaded = crate::parse(&out).unwrap();
        assert_eq!(loaded.config.tabs.title, crate::TabTitle::Path);
    }

    #[test]
    fn top_level_keys_stay_before_sections() {
        let out = edited("[font]\nsize = 12.0\n", |d| {
            d.set("theme", "Dracula");
            d.set("shell.program", "fish");
        });
        let loaded = crate::parse(&out).unwrap();
        assert!(loaded.warnings.is_empty(), "{out}");
        assert_eq!(
            loaded.config.theme,
            crate::ThemeSelection::Single("Dracula".into())
        );
        assert_eq!(
            loaded.config.shell.unwrap().program.as_deref(),
            Some("fish")
        );
        assert!(
            out.find("theme").unwrap() < out.find("[font]").unwrap(),
            "{out}"
        );
        assert!(out.contains("shell = { program = \"fish\" }"), "{out}");
    }

    #[test]
    fn nested_values_in_inline_and_regular_tables() {
        let out = edited("[window]\npadding = { x = 8, y = 6 }\n", |d| {
            d.set("window.padding.y", 10i64)
        });
        assert_eq!(out, "[window]\npadding = { x = 8, y = 10 }\n");

        let out = edited("[window.padding]\nx = 1\n", |d| {
            d.set("window.padding.y", 2i64)
        });
        assert_eq!(out, "[window.padding]\nx = 1\ny = 2\n");

        let out = edited("", |d| d.set("window.padding.x", 3i64));
        let config = crate::parse(&out).unwrap().config;
        assert_eq!(config.window.padding.x, 3);
        assert_eq!(config.window.padding.y, 6);
    }

    #[test]
    fn replaces_values_of_the_wrong_shape() {
        let out = edited("theme = \"Dracula\"\n", |d| {
            let mut pair = InlineTable::new();
            pair.insert("light", "Solarized Light".into());
            pair.insert("dark", "Tokyo Night".into());
            d.set("theme", pair);
        });
        assert_eq!(
            out,
            "theme = { light = \"Solarized Light\", dark = \"Tokyo Night\" }\n"
        );
    }

    #[test]
    fn unset_removes_empty_tables_without_comments() {
        let out = edited("[font]\nsize = 12.0\n[tabs]\ntitle = \"path\"\n", |d| {
            d.unset("font.size")
        });
        assert_eq!(out, "[tabs]\ntitle = \"path\"\n");

        let out = edited("# fonts\n[font]\nsize = 12.0\n", |d| d.unset("font.size"));
        assert_eq!(out, "# fonts\n[font]\n");

        let out = edited("shell = { program = \"fish\" }\n", |d| {
            d.unset("shell.program")
        });
        assert_eq!(out, "");

        let out = edited("[window]\npadding = { x = 8, y = 6 }\n", |d| {
            d.unset("window.padding.y")
        });
        assert_eq!(out, "[window]\npadding = { x = 8 }\n");

        let out = edited("[font]\nsize = 12.0\n", |d| d.unset("font.family"));
        assert_eq!(out, "[font]\nsize = 12.0\n");
    }

    #[test]
    fn get_and_is_set() {
        let d = doc("[window]\npadding = { x = 8 }\n");
        assert!(d.is_set("window.padding.x"));
        assert!(!d.is_set("window.padding.y"));
        assert!(!d.is_set("font.size"));
        assert_eq!(
            d.get("window.padding.x").and_then(Item::as_integer),
            Some(8)
        );
    }

    fn kb(key: &str, action: &str) -> Keybinding {
        Keybinding {
            key: key.into(),
            action: action.into(),
        }
    }

    #[test]
    fn keybindings_as_array_of_tables() {
        let mut d = doc("[[keybindings]]\nkey = \"F1\" # help\naction = \"new_tab\"\n");
        assert_eq!(d.keybindings(), [kb("F1", "new_tab")]);
        d.set_keybinding(0, &kb("F2", "copy"));
        d.push_keybinding(&kb("Ctrl+Shift+Enter", "split_vertical"));
        assert_eq!(
            d.to_string(),
            "[[keybindings]]\nkey = \"F2\" # help\naction = \"copy\"\n\n\
             [[keybindings]]\nkey = \"Ctrl+Shift+Enter\"\naction = \"split_vertical\"\n"
        );
        let config = crate::parse(&d.to_string()).unwrap().config;
        assert_eq!(config.keybindings.len(), 2);
        d.remove_keybinding(0);
        d.remove_keybinding(0);
        assert_eq!(d.to_string(), "");
    }

    #[test]
    fn keybindings_as_inline_array() {
        let mut d = doc("keybindings = [{ key = \"F1\", action = \"copy\" }]\n");
        d.push_keybinding(&kb("F2", "paste"));
        d.set_keybinding(0, &kb("F3", "copy"));
        assert_eq!(d.keybindings(), [kb("F3", "copy"), kb("F2", "paste")]);
        d.remove_keybinding(5);
        assert_eq!(d.keybindings().len(), 2);
    }

    #[test]
    fn first_keybinding_in_a_new_file() {
        let mut d = doc("theme = \"Dracula\"\n");
        d.push_keybinding(&kb("F1", "copy"));
        let loaded = crate::parse(&d.to_string()).unwrap();
        assert!(loaded.warnings.is_empty());
        assert_eq!(loaded.config.keybindings, [kb("F1", "copy")]);
    }

    #[test]
    fn comments_in_an_otherwise_empty_file_stay_on_top() {
        for (edit, expected) in [
            (
                (|d: &mut ConfigDoc| d.set("scrollback", 1i64)) as fn(&mut ConfigDoc),
                "# mine\n\nscrollback = 1\n",
            ),
            (
                |d: &mut ConfigDoc| d.set("font.size", 12.0),
                "# mine\n\n[font]\nsize = 12.0\n",
            ),
            (
                |d: &mut ConfigDoc| d.push_keybinding(&kb("F1", "copy")),
                "# mine\n\n[[keybindings]]\nkey = \"F1\"\naction = \"copy\"\n",
            ),
        ] {
            assert_eq!(edited("# mine\n\n", edit), expected);
        }
        // Comments after existing settings stay where they are.
        let out = edited("a = 1\n# end\n", |d| d.set("font.size", 12.0));
        assert!(out.ends_with("# end\n"), "{out}");
    }

    #[test]
    fn syntax_errors_are_reported() {
        assert!(ConfigDoc::parse("[font\n").is_err());
    }

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("nuntio-edit-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn writes_new_files_and_directories() {
        let dir = temp_dir("new");
        let path = dir.join("sub/config.toml");
        write_config(&path, "scrollback = 1\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "scrollback = 1\n");
        write_config(&path, "scrollback = 2\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "scrollback = 2\n");
        let leftovers = std::fs::read_dir(dir.join("sub")).unwrap().count();
        assert_eq!(leftovers, 1, "no temp files left behind");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn writes_through_symlinks() {
        let dir = temp_dir("link");
        std::fs::create_dir_all(dir.join("dotfiles")).unwrap();
        let target = dir.join("dotfiles/nuntio.toml");
        std::fs::write(&target, "").unwrap();
        let link = dir.join("config.toml");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        write_config(&link, "scrollback = 3\n").unwrap();
        assert!(link.symlink_metadata().unwrap().file_type().is_symlink());
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "scrollback = 3\n"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
