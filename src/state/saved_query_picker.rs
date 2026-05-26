//! Modal list of saved queries — opens via bare `:run-saved`.
//!
//! Same shape as `state::conn_list` but lives as an overlay (not a
//! screen) so the editor stays visible underneath; Esc returns the user
//! to whatever they were typing without disturbing it. Enter dispatches
//! the chosen query through the usual `dispatch_query` path so the
//! params prompt still fires for placeholder-bearing SQL.

#[derive(Debug)]
pub struct SavedQueryPickerState {
    pub entries: Vec<String>,
    pub selected: usize,
}

impl SavedQueryPickerState {
    pub fn new(entries: Vec<String>) -> Self {
        Self {
            entries,
            selected: 0,
        }
    }

    pub fn selected_name(&self) -> Option<&str> {
        self.entries.get(self.selected).map(String::as_str)
    }

    pub fn move_selection(&mut self, delta: i32) {
        if self.entries.is_empty() {
            return;
        }
        let max = self.entries.len() as i32 - 1;
        let next = (self.selected as i32 + delta).clamp(0, max);
        self.selected = next as usize;
    }

    pub fn jump_top(&mut self) {
        self.selected = 0;
    }

    pub fn jump_bottom(&mut self) {
        if !self.entries.is_empty() {
            self.selected = self.entries.len() - 1;
        }
    }
}
