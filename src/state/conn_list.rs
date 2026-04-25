#[derive(Debug)]
pub struct ConnListState {
    pub entries: Vec<String>,
    pub selected: usize,
    /// `Some(name)` while waiting on `y`/`n` confirmation for a delete.
    /// While set, all other keys are inert.
    pub pending_delete: Option<String>,
}

impl ConnListState {
    pub fn new(entries: Vec<String>) -> Self {
        Self {
            entries,
            selected: 0,
            pending_delete: None,
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

    pub fn begin_delete(&mut self) {
        if let Some(name) = self.selected_name() {
            self.pending_delete = Some(name.to_string());
        }
    }

    pub fn cancel_delete(&mut self) {
        self.pending_delete = None;
    }

    /// Returns the name to delete and clears the pending state. `None` if no
    /// delete was pending.
    pub fn take_pending_delete(&mut self) -> Option<String> {
        self.pending_delete.take()
    }

    pub fn is_confirming(&self) -> bool {
        self.pending_delete.is_some()
    }

    /// Repopulates the list (after add/edit/delete) and clamps selection.
    pub fn refresh(&mut self, entries: Vec<String>) {
        let prev = self.selected_name().map(|s| s.to_string());
        self.entries = entries;
        self.pending_delete = None;
        if let Some(name) = prev
            && let Some(idx) = self.entries.iter().position(|n| n == &name)
        {
            self.selected = idx;
            return;
        }
        if self.selected >= self.entries.len() {
            self.selected = self.entries.len().saturating_sub(1);
        }
    }
}
