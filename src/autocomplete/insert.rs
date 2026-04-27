//! Apply an accepted completion to the editor buffer.
//!
//! The `OpenParens` trail variant lands `name()` with the cursor
//! between the parens for arg-taking SQL functions; identifier
//! quoting is delegated to [`crate::sql_quote::smart`].

use edtui::{EditorState, Index2, Lines};

use crate::autocomplete::InsertTrail;
use crate::datasource::DriverKind;
use crate::sql_quote;

/// Wrap `ident` in dialect-appropriate quotes if it can't safely sit
/// unquoted. Thin re-export of [`sql_quote::smart`] kept so call sites
/// in this module read naturally.
pub fn quote_if_needed(ident: &str, dialect: DriverKind) -> String {
    sql_quote::smart(ident, dialect)
}

/// Replace the range `[partial_start, cursor)` with `insert` plus the
/// `trail`'s appendix. The trail also chooses where the cursor lands:
///
/// - `None` → after the inserted text.
/// - `Space` → after the trailing space.
/// - `OpenParens` → between the inserted `()`, ready for arguments.
///
/// `partial_start` is the char offset of the partial token's first
/// char in the flattened buffer; the cursor is always at the end of
/// the partial. Returns the new cursor `Index2` so the caller can
/// update editor state.
pub fn apply_completion(
    state: &mut EditorState,
    partial_start: usize,
    insert: &str,
    trail: InsertTrail,
) -> Index2 {
    let chars: Vec<char> = state.lines.flatten(&Some('\n'));
    let cursor_offset = cursor_to_offset(state);
    let partial_start = partial_start.min(cursor_offset).min(chars.len());

    let (appendix, cursor_back) = trail_pieces(trail);
    let mut next = String::with_capacity(
        partial_start + insert.len() + appendix.len() + (chars.len() - cursor_offset),
    );
    next.extend(chars[..partial_start].iter());
    next.push_str(insert);
    next.push_str(appendix);
    next.extend(chars[cursor_offset..].iter());

    state.lines = Lines::from(next.as_str());
    let new_cursor_offset =
        partial_start + insert.chars().count() + appendix.chars().count() - cursor_back;
    let new_chars: Vec<char> = state.lines.flatten(&Some('\n'));
    let new_cursor = offset_to_index(&new_chars, new_cursor_offset);
    state.cursor = new_cursor;
    state.selection = None;
    new_cursor
}

/// Translate an `InsertTrail` to its raw pieces: the text appended
/// after `insert`, and how many chars to back the cursor up after the
/// whole thing is laid down.
fn trail_pieces(trail: InsertTrail) -> (&'static str, usize) {
    match trail {
        InsertTrail::None => ("", 0),
        InsertTrail::Space => (" ", 0),
        // Insert `()` then back the cursor up by 1 so it lands
        // between the parens.
        InsertTrail::OpenParens => ("()", 1),
    }
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
        state.cursor = Index2::new(0, 16);
        apply_completion(&mut state, 14, "users", InsertTrail::None);
        assert_eq!(flatten(&state), "SELECT * FROM users");
    }

    #[test]
    fn appends_space_trail_after_table() {
        let mut state = EditorState::new(Lines::from("SELECT * FROM us"));
        state.cursor = Index2::new(0, 16);
        apply_completion(&mut state, 14, "users", InsertTrail::Space);
        assert_eq!(flatten(&state), "SELECT * FROM users ");
    }

    #[test]
    fn open_parens_trail_places_cursor_between_parens() {
        let mut state = EditorState::new(Lines::from("SELECT cou"));
        state.cursor = Index2::new(0, 10);
        let cursor = apply_completion(&mut state, 7, "COUNT", InsertTrail::OpenParens);
        assert_eq!(flatten(&state), "SELECT COUNT()");
        // Cursor lands between `(` and `)` — column 13 in "SELECT COUNT(|)".
        assert_eq!(cursor, Index2::new(0, 13));
    }
}
