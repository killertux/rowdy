//! Modal theme picker state. Opened by bare `:theme` and lives on
//! `Screen::ThemePicker` until the user picks (Enter) or cancels (Esc).
//!
//! Each cursor move re-applies the hovered theme to `app.theme` so the
//! whole window previews live; `Cancel` restores `original_theme_name`.

use std::fmt;

use edtui::{EditorState, Lines};

use crate::ui::theme::{Theme, ThemeKind};

/// Static SQL shown in the preview pane. Picked to exercise comments,
/// keywords, and string literals so the user can compare highlights.
const SAMPLE_SQL: &str = "-- Sample query\nSELECT id, name, created_at\nFROM users\nWHERE active = TRUE\nAND name = 'rowdy'\nORDER BY created_at DESC\nLIMIT 1;";

/// One row in the picker list. Headers (`Dark` / `Light`) are not stored
/// here — they're rendered as section breaks computed from the kind
/// transition in the rendered slice.
#[derive(Debug, Clone)]
pub struct ThemePickerItem {
    pub name: String,
    pub kind: ThemeKind,
    /// `Some(path)` when the theme came from a user file. Always `None`
    /// today — runtime user-theme loading is out of scope. The field is
    /// wired so the renderer can already show `(path)` labels once the
    /// loader lands.
    pub source_path: Option<String>,
}

pub struct ThemePickerState {
    /// Dark items first, then light. Within each group, alphabetical by
    /// name. Headers are rendered, not stored.
    pub items: Vec<ThemePickerItem>,
    /// Index into `items` of the currently hovered row.
    pub cursor: usize,
    /// Theme name pinned in project config when the picker opened. Used
    /// to draw the "current" highlight and to restore on cancel.
    pub original_theme_name: String,
    /// Same as `original_theme_name` — kept separately so a future
    /// "preview but don't persist" flow can diverge them.
    pub current_theme_name: String,
    /// Read-only edtui buffer holding the sample SQL preview. Carried in
    /// state so `EditorView` (which needs `&mut EditorState`) can borrow
    /// it from `app.screen` during render. The translator never feeds
    /// keys to this state, so it stays static for the picker's lifetime.
    pub preview_editor: EditorState,
}

impl fmt::Debug for ThemePickerState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ThemePickerState")
            .field("items", &self.items)
            .field("cursor", &self.cursor)
            .field("original_theme_name", &self.original_theme_name)
            .field("current_theme_name", &self.current_theme_name)
            .field("preview_editor", &"<EditorState>")
            .finish()
    }
}

impl ThemePickerState {
    /// Build the picker from the bundled registry, sorted dark-then-light,
    /// alphabetical within each kind. The cursor starts on
    /// `current_name` if it's in the list, else on row 0.
    pub fn new(current_name: &str) -> Self {
        let mut items: Vec<ThemePickerItem> = crate::ui::theme::all_themes_sorted()
            .into_iter()
            .map(|(name, theme)| ThemePickerItem {
                name,
                kind: theme.kind,
                source_path: None,
            })
            .collect();
        // Dark group first, then light. Stable sort preserves alpha order.
        items.sort_by_key(|i| (matches!(i.kind, ThemeKind::Light), i.name.clone()));
        let cursor = items
            .iter()
            .position(|i| i.name == current_name)
            .unwrap_or(0);
        Self {
            items,
            cursor,
            original_theme_name: current_name.to_string(),
            current_theme_name: current_name.to_string(),
            preview_editor: EditorState::new(Lines::from(SAMPLE_SQL)),
        }
    }

    pub fn move_down(&mut self) {
        if self.items.is_empty() {
            return;
        }
        if self.cursor + 1 < self.items.len() {
            self.cursor += 1;
        }
    }

    pub fn move_up(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
        }
    }

    pub fn top(&mut self) {
        self.cursor = 0;
    }

    pub fn bottom(&mut self) {
        if !self.items.is_empty() {
            self.cursor = self.items.len() - 1;
        }
    }

    pub fn selected(&self) -> Option<&ThemePickerItem> {
        self.items.get(self.cursor)
    }

    pub fn hovered_theme(&self) -> Option<Theme> {
        self.selected().and_then(|i| Theme::by_name(&i.name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_starts_on_current_theme() {
        let state = ThemePickerState::new("light");
        assert_eq!(state.selected().map(|i| i.name.as_str()), Some("light"));
    }

    #[test]
    fn cursor_starts_on_row_zero_when_current_unknown() {
        let state = ThemePickerState::new("does-not-exist");
        assert_eq!(state.cursor, 0);
    }

    #[test]
    fn move_down_clamps_at_end() {
        let mut state = ThemePickerState::new("dark");
        for _ in 0..1000 {
            state.move_down();
        }
        assert_eq!(state.cursor, state.items.len() - 1);
    }

    #[test]
    fn move_up_clamps_at_zero() {
        let mut state = ThemePickerState::new("dark");
        state.cursor = 0;
        state.move_up();
        assert_eq!(state.cursor, 0);
    }

    #[test]
    fn top_and_bottom_jump() {
        let mut state = ThemePickerState::new("dark");
        state.bottom();
        assert_eq!(state.cursor, state.items.len() - 1);
        state.top();
        assert_eq!(state.cursor, 0);
    }

    #[test]
    fn dark_group_comes_before_light_group() {
        let state = ThemePickerState::new("dark");
        let first_light = state
            .items
            .iter()
            .position(|i| matches!(i.kind, ThemeKind::Light))
            .expect("light themes exist");
        // Every item before the first light row must be dark.
        for item in &state.items[..first_light] {
            assert!(matches!(item.kind, ThemeKind::Dark));
        }
        // Every item from there on must be light.
        for item in &state.items[first_light..] {
            assert!(matches!(item.kind, ThemeKind::Light));
        }
    }
}
