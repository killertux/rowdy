//! Apply an accepted completion to the editor buffer.
//!
//! Phase 1 just replaces the partial token with the chosen item's
//! `insert` field. No quoting, no trail handling, no auto-alias —
//! Phase 3 covers those.

use edtui::{EditorState, Index2, Lines};

/// Replace the range `[start_offset, cursor)` with `insert`. `start_offset`
/// is the byte offset of the partial token's first char in the flattened
/// buffer; `cursor` is the byte offset where the cursor sits today (which
/// is always at the end of the partial). Returns the new cursor `Index2`
/// for the caller to set on `state.cursor`.
pub fn apply_completion(state: &mut EditorState, partial_start: usize, insert: &str) -> Index2 {
    let chars: Vec<char> = state.lines.flatten(&Some('\n'));
    let cursor_offset = cursor_to_offset(state);
    let partial_start = partial_start.min(cursor_offset).min(chars.len());

    let mut next =
        String::with_capacity(partial_start + insert.len() + (chars.len() - cursor_offset));
    next.extend(chars[..partial_start].iter());
    next.push_str(insert);
    next.extend(chars[cursor_offset..].iter());

    state.lines = Lines::from(next.as_str());
    let new_cursor_offset = partial_start + insert.chars().count();
    let new_chars: Vec<char> = state.lines.flatten(&Some('\n'));
    let new_cursor = offset_to_index(&new_chars, new_cursor_offset);
    state.cursor = new_cursor;
    state.selection = None;
    new_cursor
}

fn cursor_to_offset(state: &EditorState) -> usize {
    let mut offset = 0;
    for row in 0..state.cursor.row {
        let len = state.lines.len_col(row).unwrap_or(0);
        offset += len + 1; // +1 for the joining newline
    }
    offset + state.cursor.col
}

fn offset_to_index(chars: &[char], offset: usize) -> Index2 {
    let mut row = 0;
    let mut col = 0;
    for (i, c) in chars.iter().enumerate() {
        if i == offset {
            return Index2::new(row, col);
        }
        if *c == '\n' {
            row += 1;
            col = 0;
        } else {
            col += 1;
        }
    }
    Index2::new(row, col)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flatten(state: &EditorState) -> String {
        state.lines.flatten(&Some('\n')).into_iter().collect()
    }

    #[test]
    fn replaces_partial_with_completion() {
        let mut state = EditorState::new(Lines::from("SELECT * FROM us"));
        // Cursor at end of "us" — row 0, col 16.
        state.cursor = Index2::new(0, 16);
        // Partial "us" starts at offset 14.
        apply_completion(&mut state, 14, "users");
        assert_eq!(flatten(&state), "SELECT * FROM users");
        assert_eq!(state.cursor, Index2::new(0, 19));
    }

    #[test]
    fn inserts_at_cursor_when_partial_empty() {
        let mut state = EditorState::new(Lines::from("SELECT * FROM "));
        state.cursor = Index2::new(0, 14);
        // Empty partial — start == cursor.
        apply_completion(&mut state, 14, "users");
        assert_eq!(flatten(&state), "SELECT * FROM users");
        assert_eq!(state.cursor, Index2::new(0, 19));
    }

    #[test]
    fn keeps_following_text_intact() {
        let mut state = EditorState::new(Lines::from("FROM us WHERE id = 1"));
        state.cursor = Index2::new(0, 7);
        apply_completion(&mut state, 5, "users");
        assert_eq!(flatten(&state), "FROM users WHERE id = 1");
    }
}
