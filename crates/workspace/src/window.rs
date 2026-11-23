//! Window state: an ordered list of tabs and the active tab.

use astraterm_core::{TabId, WindowId};

use crate::Tab;

/// One application window.
#[derive(Clone, Debug, PartialEq)]
pub struct WindowState {
    pub id: WindowId,
    pub tabs: Vec<Tab>,
    pub active_tab: TabId,
}

impl WindowState {
    /// Creates a window around its first tab.
    pub fn new(id: WindowId, first_tab: Tab) -> Self {
        let active = first_tab.id;
        Self {
            id,
            tabs: vec![first_tab],
            active_tab: active,
        }
    }

    pub fn active_tab(&self) -> Option<&Tab> {
        self.tabs.iter().find(|tab| tab.id == self.active_tab)
    }

    pub fn active_tab_mut(&mut self) -> Option<&mut Tab> {
        let active = self.active_tab;
        self.tabs.iter_mut().find(|tab| tab.id == active)
    }

    pub fn tab(&self, id: TabId) -> Option<&Tab> {
        self.tabs.iter().find(|tab| tab.id == id)
    }

    pub fn tab_mut(&mut self, id: TabId) -> Option<&mut Tab> {
        self.tabs.iter_mut().find(|tab| tab.id == id)
    }

    /// Appends a tab and focuses it.
    pub fn add_tab(&mut self, tab: Tab) {
        self.active_tab = tab.id;
        self.tabs.push(tab);
    }

    /// Inserts a tab at `index`, clamped into range, and focuses it.
    pub fn insert_tab(&mut self, index: usize, tab: Tab) {
        let index = index.min(self.tabs.len());
        self.active_tab = tab.id;
        self.tabs.insert(index, tab);
    }

    /// Closes a tab. Closing the active tab focuses its neighbour.
    ///
    /// Returns `false` when the tab was not found or when it is the last tab; the
    /// caller decides whether to close the window instead.
    pub fn close_tab(&mut self, id: TabId) -> bool {
        if self.tabs.len() <= 1 {
            return false;
        }
        let Some(index) = self.tabs.iter().position(|tab| tab.id == id) else {
            return false;
        };
        self.tabs.remove(index);
        if self.active_tab == id {
            let next = index.min(self.tabs.len() - 1);
            self.active_tab = self.tabs[next].id;
        }
        true
    }

    /// Moves a tab to a new index, keeping it active.
    pub fn move_tab(&mut self, from: usize, to: usize) -> bool {
        if from >= self.tabs.len() || to >= self.tabs.len() || from == to {
            return false;
        }
        let tab = self.tabs.remove(from);
        self.tabs.insert(to, tab);
        true
    }

    /// Selects a tab by index.
    pub fn select_index(&mut self, index: usize) -> bool {
        if let Some(tab) = self.tabs.get(index) {
            self.active_tab = tab.id;
            return true;
        }
        false
    }

    pub fn active_index(&self) -> Option<usize> {
        self.tabs.iter().position(|tab| tab.id == self.active_tab)
    }

    /// Selects the next tab, wrapping around.
    pub fn next_tab(&mut self) -> bool {
        let Some(index) = self.active_index() else {
            return false;
        };
        let next = (index + 1) % self.tabs.len();
        self.select_index(next)
    }

    /// Selects the previous tab, wrapping around.
    pub fn previous_tab(&mut self) -> bool {
        let Some(index) = self.active_index() else {
            return false;
        };
        let next = (index + self.tabs.len() - 1) % self.tabs.len();
        self.select_index(next)
    }

    /// Number of panes across all tabs.
    pub fn pane_count(&self) -> usize {
        self.tabs.iter().map(Tab::pane_count).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use astraterm_core::PaneId;

    fn window_with(tab_count: usize) -> WindowState {
        let first = Tab::new("one", PaneId::next());
        let mut window = WindowState::new(WindowId::next(), first);
        for index in 1..tab_count {
            window.add_tab(Tab::new(format!("tab {index}"), PaneId::next()));
        }
        window
    }

    #[test]
    fn new_window_focuses_its_first_tab() {
        let window = window_with(1);
        assert_eq!(window.tabs.len(), 1);
        assert_eq!(window.active_tab(), window.tabs.first());
    }

    #[test]
    fn added_tabs_become_active() {
        let mut window = window_with(1);
        let tab = Tab::new("two", PaneId::next());
        let id = tab.id;
        window.add_tab(tab);
        assert_eq!(window.active_tab, id);
        assert_eq!(window.tabs.len(), 2);
    }

    #[test]
    fn closing_the_active_tab_focuses_its_neighbour() {
        let mut window = window_with(3);
        let ids: Vec<TabId> = window.tabs.iter().map(|tab| tab.id).collect();
        window.select_index(1);
        assert!(window.close_tab(ids[1]));
        assert_eq!(window.tabs.len(), 2);
        assert_eq!(window.active_tab, ids[2]);
    }

    #[test]
    fn closing_a_background_tab_keeps_focus() {
        let mut window = window_with(3);
        let ids: Vec<TabId> = window.tabs.iter().map(|tab| tab.id).collect();
        window.select_index(0);
        assert!(window.close_tab(ids[2]));
        assert_eq!(window.active_tab, ids[0]);
    }

    #[test]
    fn the_last_tab_cannot_be_closed() {
        let mut window = window_with(1);
        let id = window.active_tab;
        assert!(!window.close_tab(id));
        assert_eq!(window.tabs.len(), 1);
    }

    #[test]
    fn closing_an_unknown_tab_is_refused() {
        let mut window = window_with(2);
        assert!(!window.close_tab(TabId::from_raw(9999)));
    }

    #[test]
    fn tabs_can_be_reordered() {
        let mut window = window_with(3);
        let original: Vec<String> = window.tabs.iter().map(|tab| tab.title.clone()).collect();
        assert!(window.move_tab(0, 2));
        let reordered: Vec<String> = window.tabs.iter().map(|tab| tab.title.clone()).collect();
        assert_eq!(
            reordered,
            vec![
                original[1].clone(),
                original[2].clone(),
                original[0].clone()
            ]
        );
        assert!(!window.move_tab(0, 5));
        assert!(!window.move_tab(1, 1));
    }

    #[test]
    fn next_and_previous_wrap_around() {
        let mut window = window_with(3);
        let ids: Vec<TabId> = window.tabs.iter().map(|tab| tab.id).collect();
        window.select_index(2);
        assert!(window.next_tab());
        assert_eq!(window.active_tab, ids[0]);
        assert!(window.previous_tab());
        assert_eq!(window.active_tab, ids[2]);
    }

    #[test]
    fn selecting_an_out_of_range_index_is_refused() {
        let mut window = window_with(2);
        let active = window.active_tab;
        assert!(!window.select_index(7));
        assert_eq!(window.active_tab, active);
    }

    #[test]
    fn pane_count_sums_all_tabs() {
        let mut window = window_with(2);
        window
            .active_tab_mut()
            .unwrap()
            .split_focused(astraterm_core::SplitAxis::Vertical, PaneId::next());
        assert_eq!(window.pane_count(), 3);
    }

    #[test]
    fn inserting_clamps_the_index() {
        let mut window = window_with(2);
        let tab = Tab::new("inserted", PaneId::next());
        window.insert_tab(99, tab);
        assert_eq!(window.tabs.last().unwrap().title, "inserted");
    }
}
