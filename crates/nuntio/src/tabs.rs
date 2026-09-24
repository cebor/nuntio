//! The tab list of a window: order, active tab, activity and bell flags.
//! Generic over the tab content so it can be tested without terminals.

#[derive(Debug)]
pub struct Tab<P> {
    pub pane: P,
    /// Title set by the application (OSC 0/2).
    pub title: Option<String>,
    /// Output arrived while the tab was in the background.
    pub activity: bool,
    /// The bell rang while the tab was in the background.
    pub bell: bool,
}

impl<P> Tab<P> {
    fn new(pane: P) -> Self {
        Self {
            pane,
            title: None,
            activity: false,
            bell: false,
        }
    }
}

#[derive(Debug)]
pub struct Tabs<P> {
    tabs: Vec<Tab<P>>,
    active: usize,
}

impl<P> Tabs<P> {
    pub fn new(first: P) -> Self {
        Self {
            tabs: vec![Tab::new(first)],
            active: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.tabs.len()
    }

    pub fn active_index(&self) -> usize {
        self.active
    }

    pub fn active(&self) -> &Tab<P> {
        &self.tabs[self.active]
    }

    pub fn iter(&self) -> impl Iterator<Item = &Tab<P>> {
        self.tabs.iter()
    }

    pub fn get_mut(&mut self, index: usize) -> Option<&mut Tab<P>> {
        self.tabs.get_mut(index)
    }

    pub fn position(&self, mut predicate: impl FnMut(&P) -> bool) -> Option<usize> {
        self.tabs.iter().position(|t| predicate(&t.pane))
    }

    /// Open a tab right after the active one and activate it.
    pub fn open(&mut self, pane: P) {
        self.active += 1;
        self.tabs.insert(self.active, Tab::new(pane));
    }

    /// Close a tab. Returns its pane, or `None` if this was the last tab
    /// (which stays, so the caller can quit).
    pub fn close(&mut self, index: usize) -> Option<P> {
        if self.tabs.len() == 1 || index >= self.tabs.len() {
            return None;
        }
        let tab = self.tabs.remove(index);
        if index < self.active || self.active == self.tabs.len() {
            self.active -= 1;
        }
        self.clear_flags(self.active);
        Some(tab.pane)
    }

    pub fn select(&mut self, index: usize) {
        if index < self.tabs.len() {
            self.active = index;
            self.clear_flags(index);
        }
    }

    /// Move a tab to another position; the active tab stays active.
    pub fn move_tab(&mut self, from: usize, to: usize) {
        if from >= self.tabs.len() || to >= self.tabs.len() || from == to {
            return;
        }
        let active_is_moving = self.active == from;
        let tab = self.tabs.remove(from);
        self.tabs.insert(to, tab);
        if active_is_moving {
            self.active = to;
        } else if from < self.active && to >= self.active {
            self.active -= 1;
        } else if from > self.active && to <= self.active {
            self.active += 1;
        }
    }

    fn clear_flags(&mut self, index: usize) {
        let tab = &mut self.tabs[index];
        tab.activity = false;
        tab.bell = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tabs(n: u32) -> Tabs<u32> {
        let mut t = Tabs::new(0);
        for i in 1..n {
            t.open(i);
        }
        t
    }

    fn order(t: &Tabs<u32>) -> Vec<u32> {
        t.iter().map(|t| t.pane).collect()
    }

    #[test]
    fn open_inserts_after_active() {
        let mut t = tabs(3); // [0, 1, 2], active 2
        t.select(0);
        t.open(9);
        assert_eq!(order(&t), [0, 9, 1, 2]);
        assert_eq!(t.active().pane, 9);
    }

    #[test]
    fn close_keeps_a_sensible_active_tab() {
        let mut t = tabs(4); // active 3
        assert_eq!(t.close(3), Some(3));
        assert_eq!(
            t.active().pane,
            2,
            "closing the last activates the new last"
        );

        t.select(1);
        assert_eq!(t.close(0), Some(0));
        assert_eq!(t.active().pane, 1, "closing before active keeps it active");

        assert_eq!(t.close(1), Some(2));
        assert_eq!(t.close(0), None, "the last tab stays");
        assert_eq!(t.len(), 1);
    }

    #[test]
    fn move_tracks_active_tab() {
        let mut t = tabs(4);
        t.select(1);
        t.move_tab(1, 3);
        assert_eq!(order(&t), [0, 2, 3, 1]);
        assert_eq!(t.active().pane, 1);

        t.move_tab(0, 3);
        assert_eq!(order(&t), [2, 3, 1, 0]);
        assert_eq!(t.active().pane, 1);

        t.move_tab(3, 0);
        assert_eq!(order(&t), [0, 2, 3, 1]);
        assert_eq!(t.active().pane, 1);
    }

    #[test]
    fn selecting_clears_indicators() {
        let mut t = tabs(2);
        t.get_mut(0).unwrap().activity = true;
        t.get_mut(0).unwrap().bell = true;
        t.select(0);
        assert!(!t.active().activity && !t.active().bell);
    }
}
