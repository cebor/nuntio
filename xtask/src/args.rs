//! Command line arguments of the xtask commands.

use anyhow::{Context, Result, ensure};

/// Arguments of one command, taken out as they are used. `finish` rejects
/// whatever the command did not consume, so a typo is an error instead of
/// being ignored.
pub struct Args(Vec<String>);

impl Args {
    pub fn new(args: impl IntoIterator<Item = String>) -> Self {
        Self(args.into_iter().collect())
    }

    pub fn flag(&mut self, name: &str) -> bool {
        let found = self.0.iter().position(|a| a == name);
        found.map(|i| self.0.remove(i)).is_some()
    }

    pub fn option(&mut self, name: &str) -> Result<Option<String>> {
        let Some(i) = self.0.iter().position(|a| a == name) else {
            return Ok(None);
        };
        ensure!(i + 1 < self.0.len(), "{name} needs a value");
        self.0.remove(i);
        Ok(Some(self.0.remove(i)))
    }

    pub fn option_u64(&mut self, name: &str) -> Result<Option<u64>> {
        self.option(name)?
            .map(|v| v.parse().with_context(|| format!("{name}: not a number")))
            .transpose()
    }

    /// The `count` values after the option `name`.
    pub fn option_values(&mut self, name: &str, count: usize) -> Result<Option<Vec<String>>> {
        let Some(i) = self.0.iter().position(|a| a == name) else {
            return Ok(None);
        };
        ensure!(i + count < self.0.len(), "{name} needs {count} values");
        Ok(Some(self.0.drain(i..=i + count).skip(1).collect()))
    }

    /// Removes the first `marker` and everything after it; returns what
    /// followed the marker.
    pub fn split_off_after(&mut self, marker: &str) -> Vec<String> {
        match self.0.iter().position(|a| a == marker) {
            Some(i) => self.0.split_off(i).split_off(1),
            None => Vec::new(),
        }
    }

    pub fn positional(&mut self) -> Option<String> {
        (!self.0.is_empty()).then(|| self.0.remove(0))
    }

    pub fn rest(&mut self) -> Vec<String> {
        std::mem::take(&mut self.0)
    }

    /// Fails if arguments of `command` are left over.
    pub fn finish(&self, command: &str) -> Result<()> {
        ensure!(self.0.is_empty(), "{command}: unexpected {:?}", self.0);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Args {
        Args::new(list.iter().map(|a| (*a).to_owned()))
    }

    #[test]
    fn args_are_taken_out() {
        let mut args = args(&["a", "--pane", "3", "b"]);
        assert_eq!(args.option_u64("--pane").unwrap(), Some(3));
        assert_eq!(args.positional().as_deref(), Some("a"));
        assert_eq!(args.rest(), ["b"]);
    }

    #[test]
    fn leftovers_are_rejected() {
        let mut args = args(&["serve", "--typo"]);
        assert!(args.flag("serve"));
        let error = args.finish("site").unwrap_err().to_string();
        assert_eq!(error, "site: unexpected [\"--typo\"]");
        args.rest();
        assert!(args.finish("site").is_ok());
    }

    #[test]
    fn option_values_take_the_option_and_its_values() {
        let mut args = args(&["click", "--cell", "5", "3", "--pane", "1"]);
        assert_eq!(
            args.option_values("--cell", 2).unwrap().unwrap(),
            ["5", "3"]
        );
        assert_eq!(args.rest(), ["click", "--pane", "1"]);
        assert!(
            self::args(&["--cell", "5"])
                .option_values("--cell", 2)
                .is_err()
        );
    }

    #[test]
    fn everything_after_the_marker_is_split_off() {
        let mut args = args(&["--visible", "--", "ls", "-l"]);
        assert_eq!(args.split_off_after("--"), ["ls", "-l"]);
        assert_eq!(args.rest(), ["--visible"]);
        assert!(args.split_off_after("--").is_empty());
    }
}
