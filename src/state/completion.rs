//! UI-side state for the autocomplete popover.
//!
//! Lives on `App` as `Option<CompletionState>`. The presence of the
//! `Some` is what tells the keymap to intercept Esc/Tab/Enter/Up/Down
//! before they reach edtui (see `event::translate_normal_key`).

use crate::autocomplete::CompletionItem;

/// How many rows of the candidate list are visible in the popover at
/// once. Lives here (not in the popover Widget) because `move_selection`
/// needs it to keep the selected item inside the visible window.
pub const MAX_VISIBLE_ROWS: usize = 10;

#[derive(Debug, Clone)]
pub struct CompletionState {
    /// Visible candidates in the order they should render.
    pub items: Vec<CompletionItem>,
    /// Index of the highlighted item; clamped to `items.len() - 1`.
    pub selected: usize,
    /// Index of the first row drawn in the popover. Slides as `selected`
    /// moves out of `[scroll_offset, scroll_offset + MAX_VISIBLE_ROWS)`.
    pub scroll_offset: usize,
    /// Char offset (in the flattened buffer) of the partial token's
    /// first char. Recorded once at open time and used both as the
    /// replacement-range start on accept and as the "did the cursor
    /// drift out?" anchor on each refresh.
    pub anchor_offset: usize,
    /// What the user has typed inside the partial. Rendered in the
    /// popover header so the user knows what they're filtering by.
    pub partial: String,
}

impl CompletionState {
    pub fn new(items: Vec<CompletionItem>, anchor_offset: usize, partial: String) -> Self {
        Self {
            items,
            selected: 0,
            scroll_offset: 0,
            anchor_offset,
            partial,
        }
    }

    /// Move the selection by `delta`, clamped to `[0, len - 1]`. No
    /// wrap-around — pressing Down at the bottom is a no-op so the user
    /// always sees something change visibly when navigation has effect.
    pub fn move_selection(&mut self, delta: i32) {
        if self.items.is_empty() {
            return;
        }
        let last = (self.items.len() - 1) as i32;
        let next = (self.selected as i32 + delta).clamp(0, last) as usize;
        self.selected = next;
        self.clamp_scroll();
    }

    /// Replace the candidate list (called from `refresh_completion` on
    /// each keystroke). Pulls `selected` and `scroll_offset` back into
    /// bounds — the new list might be shorter than the old one.
    pub fn replace_items(&mut self, items: Vec<CompletionItem>) {
        self.items = items;
        if self.items.is_empty() {
            self.selected = 0;
            self.scroll_offset = 0;
            return;
        }
        if self.selected >= self.items.len() {
            self.selected = self.items.len() - 1;
        }
        self.clamp_scroll();
    }

    fn clamp_scroll(&mut self) {
        let visible = MAX_VISIBLE_ROWS.min(self.items.len());
        let max_offset = self.items.len().saturating_sub(visible);
        if self.scroll_offset > max_offset {
            self.scroll_offset = max_offset;
        }
        if self.selected < self.scroll_offset {
            self.scroll_offset = self.selected;
        } else if self.selected >= self.scroll_offset + visible {
            self.scroll_offset = self.selected + 1 - visible;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::autocomplete::{CompletionKind, InsertTrail};

    fn make_state(n: usize) -> CompletionState {
        let items: Vec<CompletionItem> = (0..n)
            .map(|i| CompletionItem {
                label: format!("item_{i}"),
                kind: CompletionKind::Keyword,
                detail: None,
                insert: format!("item_{i}"),
                trail: InsertTrail::None,
            })
            .collect();
        CompletionState::new(items, 0, String::new())
    }

    #[test]
    fn move_clamps_at_boundaries() {
        let mut s = make_state(5);
        s.move_selection(-1);
        assert_eq!(s.selected, 0, "Up at top is a no-op");
        s.selected = 4;
        s.move_selection(1);
        assert_eq!(s.selected, 4, "Down at bottom is a no-op");
    }

    #[test]
    fn scroll_follows_selection_past_visible_window() {
        let mut s = make_state(20);
        // Move down past the visible window — scroll should slide.
        for _ in 0..15 {
            s.move_selection(1);
        }
        assert_eq!(s.selected, 15);
        assert!(
            s.scroll_offset > 0
                && s.selected >= s.scroll_offset
                && s.selected < s.scroll_offset + MAX_VISIBLE_ROWS,
            "selected={} scroll={} visible={}",
            s.selected,
            s.scroll_offset,
            MAX_VISIBLE_ROWS
        );
    }

    #[test]
    fn scroll_pulls_back_when_selecting_above_window() {
        let mut s = make_state(20);
        s.selected = 19;
        s.clamp_scroll();
        assert!(s.scroll_offset > 0);
        // Now jump back to the top.
        s.move_selection(-100);
        assert_eq!(s.selected, 0);
        assert_eq!(s.scroll_offset, 0);
    }

    #[test]
    fn replace_items_clamps_selection_when_list_shrinks() {
        let mut s = make_state(20);
        s.selected = 15;
        s.clamp_scroll();
        s.replace_items(make_state(3).items);
        assert_eq!(s.selected, 2);
        assert_eq!(s.scroll_offset, 0);
    }
}
