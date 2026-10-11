//! Where the notes live: a [`Store`] with the folder of `.md` files as the
//! real implementation, so the logic above it can be tested without disk.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Identifies a version of a file on disk, to notice outside changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stamp {
    pub modified: SystemTime,
    pub len: u64,
}

/// A note with its text.
#[derive(Debug, Clone)]
pub struct NoteFile {
    pub title: String,
    pub modified: SystemTime,
    pub text: String,
}

pub trait Store {
    /// Every note with its text (the progress counts need it).
    fn list(&self) -> io::Result<Vec<NoteFile>>;
    fn read(&self, title: &str) -> io::Result<(String, Stamp)>;
    /// `None`: the file is missing.
    fn stamp(&self, title: &str) -> io::Result<Option<Stamp>>;
    fn write(&mut self, title: &str, text: &str) -> io::Result<Stamp>;
    /// An empty file; `ErrorKind::AlreadyExists` if the title is taken.
    fn create(&mut self, title: &str) -> io::Result<()>;
    fn rename(&mut self, from: &str, to: &str) -> io::Result<()>;
    /// Move the note out of the list without deleting it.
    fn trash(&mut self, title: &str) -> io::Result<()>;
}

/// Why `input` can't be a note title, or the trimmed title. `taken` are the
/// titles that exist already (compared case-insensitively).
pub fn validate_title(input: &str, taken: &[&str]) -> Result<String, &'static str> {
    let title = input.trim();
    if title.is_empty() {
        return Err("The title is empty");
    }
    if title
        .chars()
        .any(|c| c.is_control() || "/\\:*?\"<>|".contains(c))
    {
        return Err("A title can't contain / \\ : * ? \" < > |");
    }
    if title.starts_with('.') || title.ends_with('.') {
        return Err("A title can't start or end with a dot");
    }
    if title.len() > 200 {
        return Err("The title is too long");
    }
    let lower = title.to_lowercase();
    if taken.iter().any(|t| t.to_lowercase() == lower) {
        return Err("A note with this title exists");
    }
    Ok(title.to_owned())
}

/// Notes as `{title}.md` files in one folder.
#[derive(Debug, Clone)]
pub struct DirStore(pub PathBuf);

impl DirStore {
    fn path(&self, title: &str) -> PathBuf {
        self.0.join(format!("{title}.md"))
    }

    fn trash_dir(&self) -> PathBuf {
        self.0.join(".trash")
    }
}

fn stamp_of(meta: &fs::Metadata) -> io::Result<Stamp> {
    Ok(Stamp {
        modified: meta.modified()?,
        len: meta.len(),
    })
}

impl Store for DirStore {
    fn list(&self) -> io::Result<Vec<NoteFile>> {
        let mut notes = Vec::new();
        for entry in fs::read_dir(&self.0)? {
            let Ok(entry) = entry else { continue };
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if name.starts_with('.') || path.extension().is_none_or(|e| e != "md") {
                continue;
            }
            let Some(title) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            // Metadata first: a write in between then shows as a change.
            let Ok(meta) = fs::metadata(&path) else {
                continue;
            };
            if !meta.is_file() {
                continue;
            }
            let Ok(text) = fs::read_to_string(&path) else {
                continue;
            };
            notes.push(NoteFile {
                title: title.to_owned(),
                modified: meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                text,
            });
        }
        Ok(notes)
    }

    fn read(&self, title: &str) -> io::Result<(String, Stamp)> {
        let path = self.path(title);
        let stamp = stamp_of(&fs::metadata(&path)?)?;
        let text = fs::read_to_string(&path)?;
        Ok((text, stamp))
    }

    fn stamp(&self, title: &str) -> io::Result<Option<Stamp>> {
        match fs::metadata(self.path(title)) {
            Ok(meta) => stamp_of(&meta).map(Some),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err),
        }
    }

    fn write(&mut self, title: &str, text: &str) -> io::Result<Stamp> {
        let path = self.path(title);
        nuntio_config::write_config(&path, text)?;
        stamp_of(&fs::metadata(&path)?)
    }

    fn create(&mut self, title: &str) -> io::Result<()> {
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(self.path(title))
            .map(drop)
    }

    fn rename(&mut self, from: &str, to: &str) -> io::Result<()> {
        let target = self.path(to);
        let taken = if from.to_lowercase() == to.to_lowercase() {
            // A case-only rename: on a case-insensitive filesystem `target`
            // is the file itself, so look for the exact name.
            let name = format!("{to}.md");
            fs::read_dir(&self.0)?
                .flatten()
                .any(|e| e.file_name().to_str() == Some(name.as_str()))
                && from != to
        } else {
            target.exists()
        };
        if taken {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "a note with this title exists",
            ));
        }
        fs::rename(self.path(from), target)
    }

    fn trash(&mut self, title: &str) -> io::Result<()> {
        let dir = self.trash_dir();
        fs::create_dir_all(&dir)?;
        let mut target = dir.join(format!("{title}.md"));
        let mut n = 2;
        while target.exists() {
            target = dir.join(format!("{title} ({n}).md"));
            n += 1;
        }
        fs::rename(self.path(title), target)
    }
}

/// `path` with the home directory shortened to `~`.
pub fn display_path(path: &Path) -> String {
    if let Some(home) = dirs::home_dir()
        && let Ok(rest) = path.strip_prefix(&home)
    {
        return Path::new("~").join(rest).display().to_string();
    }
    path.display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("nuntio-apps-test-{}-{name}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn store(&self) -> DirStore {
            DirStore(self.0.clone())
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn titles_are_validated() {
        assert_eq!(validate_title("  Ideas ", &[]).as_deref(), Ok("Ideas"));
        assert_eq!(validate_title("   ", &[]), Err("The title is empty"));
        for bad in [
            "a/b", "a\\b", "a:b", "a*b", "a?b", "a\"b", "a<b", "a>b", "a|b", "a\tb",
        ] {
            assert_eq!(
                validate_title(bad, &[]),
                Err("A title can't contain / \\ : * ? \" < > |"),
                "{bad:?}"
            );
        }
        assert_eq!(
            validate_title(".hidden", &[]),
            Err("A title can't start or end with a dot")
        );
        assert_eq!(
            validate_title("end.", &[]),
            Err("A title can't start or end with a dot")
        );
        assert_eq!(
            validate_title(&"a".repeat(201), &[]),
            Err("The title is too long")
        );
        assert!(validate_title(&"a".repeat(200), &[]).is_ok());
        assert_eq!(
            validate_title("ideas", &["Ideas", "Other"]),
            Err("A note with this title exists")
        );
    }

    #[test]
    fn list_skips_dot_files_and_other_extensions() {
        let tmp = TempDir::new("list");
        let mut store = tmp.store();
        store.write("One", "- [ ] a\n").unwrap();
        fs::write(tmp.0.join("notes.txt"), "x").unwrap();
        fs::write(tmp.0.join(".hidden.md"), "x").unwrap();
        fs::create_dir(tmp.0.join("Dir.md")).unwrap();
        store.trash("One").unwrap();
        store.write("Two", "b").unwrap();
        let notes = store.list().unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].title, "Two");
        assert_eq!(notes[0].text, "b");
    }

    #[test]
    fn write_read_and_stamp() {
        let tmp = TempDir::new("write");
        let mut store = tmp.store();
        assert_eq!(store.stamp("A").unwrap(), None);
        let stamp = store.write("A", "hello").unwrap();
        assert_eq!(store.stamp("A").unwrap(), Some(stamp));
        assert_eq!(store.read("A").unwrap(), ("hello".to_owned(), stamp));
        let longer = store.write("A", "hello world").unwrap();
        assert_ne!(stamp, longer);
        assert!(store.read("Missing").is_err());
    }

    #[test]
    fn create_refuses_a_taken_title() {
        let tmp = TempDir::new("create");
        let mut store = tmp.store();
        store.create("A").unwrap();
        assert_eq!(store.read("A").unwrap().0, "");
        let err = store.create("A").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
    }

    #[test]
    fn rename_refuses_collisions_but_allows_case_changes() {
        let tmp = TempDir::new("rename");
        let mut store = tmp.store();
        store.write("a", "1").unwrap();
        store.write("b", "2").unwrap();
        let err = store.rename("a", "b").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(store.read("b").unwrap().0, "2");
        store.rename("a", "A").unwrap();
        assert!(store.stamp("A").unwrap().is_some());
        let names: Vec<_> = fs::read_dir(&tmp.0)
            .unwrap()
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .collect();
        assert!(names.contains(&"A.md".to_owned()), "{names:?}");
        store.rename("A", "c").unwrap();
        assert_eq!(store.read("c").unwrap().0, "1");
        assert!(store.read("A").is_err());
    }

    #[test]
    fn trash_numbers_duplicates() {
        let tmp = TempDir::new("trash");
        let mut store = tmp.store();
        for text in ["1", "2", "3"] {
            store.write("A", text).unwrap();
            store.trash("A").unwrap();
        }
        assert_eq!(store.stamp("A").unwrap(), None);
        let trash = tmp.0.join(".trash");
        assert_eq!(fs::read_to_string(trash.join("A.md")).unwrap(), "1");
        assert_eq!(fs::read_to_string(trash.join("A (2).md")).unwrap(), "2");
        assert_eq!(fs::read_to_string(trash.join("A (3).md")).unwrap(), "3");
    }

    #[test]
    fn dir_store_is_a_trait_object() {
        let tmp = TempDir::new("object");
        let mut store: Box<dyn Store> = Box::new(tmp.store());
        store.create("A").unwrap();
        assert_eq!(store.list().unwrap().len(), 1);
    }
}
