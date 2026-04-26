use edtui::{EditorEventHandler, EditorMode, EditorState, Highlight, Index2, Lines};
use ratatui::style::{Color, Style};

pub struct EditorPanel {
    pub state: EditorState,
    pub events: EditorEventHandler,
}

impl EditorPanel {
    pub fn new() -> Self {
        let initial = "-- Welcome to rowdy. Press : to enter a command, :q to quit.\nSELECT 1;\n";
        Self {
            state: EditorState::new(Lines::from(initial)),
            events: EditorEventHandler::default(),
        }
    }

    pub fn editor_mode(&self) -> EditorMode {
        self.state.mode
    }

    /// Replace the buffer with `text` (used when loading a saved session).
    /// Resets cursor and discards any selection/highlight; the editor mode
    /// is left at edtui's default (Normal).
    pub fn replace_text(&mut self, text: &str) {
        self.state = EditorState::new(Lines::from(text));
    }

    /// Current buffer flattened to a String (joined with `\n`).
    pub fn text(&self) -> String {
        self.state.lines.flatten(&Some('\n')).into_iter().collect()
    }
}

/// Slice of the buffer from the start of the current statement up to
/// the cursor, plus the cursor's char offset in the flattened buffer.
/// Used by the autocomplete engine — it needs to look at "what came
/// before" the cursor to classify context.
///
/// The statement boundary is the same naive `;` split as
/// `statement_under_cursor`, with the same caveat (mis-splits across
/// strings/comments). Phase 2 will swap this for a tokenizer-aware
/// boundary.
pub fn statement_prefix_to_cursor(state: &EditorState) -> (String, usize) {
    let chars: Vec<char> = state.lines.flatten(&Some('\n'));
    let cursor_char_offset = cursor_to_offset(state).min(chars.len());
    let stmt_start = chars[..cursor_char_offset]
        .iter()
        .rposition(|c| *c == ';')
        .map(|i| i + 1)
        .unwrap_or(0);
    let prefix: String = chars[stmt_start..cursor_char_offset].iter().collect();
    (prefix, cursor_char_offset)
}

impl Default for EditorPanel {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone)]
pub struct StatementRange {
    pub text: String,
    pub start: Index2,
    pub end: Index2,
}

/// Returns the SQL statement containing the cursor.
///
/// TODO: this is a naive `;` splitter — it will mis-split semicolons inside
/// strings, identifiers, and comments. Replace with a real lexer once the
/// rest of the execution path is solid.
pub fn statement_under_cursor(state: &EditorState) -> Option<StatementRange> {
    let chars: Vec<char> = state.lines.flatten(&Some('\n'));
    let cursor_offset = cursor_to_offset(state);
    let (seg_start, seg_end) = segment_bounds(&chars, cursor_offset);

    let segment_chars = &chars[seg_start..seg_end];
    let leading_ws = leading_whitespace(segment_chars);
    let trailing_ws = trailing_whitespace(segment_chars);
    if leading_ws + trailing_ws >= segment_chars.len() {
        return None;
    }
    let inner_start = seg_start + leading_ws;
    let inner_end = seg_end - trailing_ws;
    let text: String = chars[inner_start..inner_end].iter().collect();
    Some(StatementRange {
        text,
        start: offset_to_index(&chars, inner_start),
        end: offset_to_index(&chars, inner_end.saturating_sub(1)),
    })
}

pub fn highlight_range(state: &mut EditorState, range: &StatementRange, style: Style) {
    state.clear_highlights();
    state.add_highlight(Highlight::new(range.start, range.end, style));
}

pub fn clear_confirm_highlight(state: &mut EditorState) {
    state.clear_highlights();
}

pub fn confirm_highlight_style(bg: Color, fg: Color) -> Style {
    Style::default().bg(bg).fg(fg)
}

pub fn selection_text(state: &EditorState) -> Option<String> {
    let selection = state.selection.as_ref()?;
    let copy = selection.copy_from(&state.lines);
    let text: String = copy.flatten(&Some('\n')).into_iter().collect();
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Replace the buffer with `text` and place the cursor at (0, 0).
/// Caveat: edtui's undo stack is captured by its own actions only; we mutate
/// `lines` directly, so a single `u` won't restore the pre-format buffer.
pub fn replace_buffer_text(state: &mut EditorState, text: &str) {
    state.lines = Lines::from(text);
    state.selection = None;
    state.cursor = Index2::new(0, 0);
}

/// Replace the current selection with `replacement` and drop back to Normal
/// mode. Cursor lands at the start of the replacement so a follow-up `=`
/// (or any motion) starts somewhere predictable. No-ops if there's no
/// active selection.
pub fn replace_selection_text(state: &mut EditorState, replacement: &str) -> bool {
    let Some(sel) = state.selection.as_ref() else {
        return false;
    };
    let chars: Vec<char> = state.lines.flatten(&Some('\n'));
    let start_idx = sel.start();
    let end_idx = sel.end();
    let start_off = index_to_offset(&chars, start_idx);
    // edtui selections are inclusive on both ends; +1 makes the suffix slice
    // exclusive.
    let end_off = index_to_offset(&chars, end_idx)
        .saturating_add(1)
        .min(chars.len());

    let mut next = String::with_capacity(start_off + replacement.len() + (chars.len() - end_off));
    next.extend(chars[..start_off].iter());
    next.push_str(replacement);
    next.extend(chars[end_off..].iter());

    state.lines = Lines::from(next.as_str());
    state.selection = None;
    state.mode = EditorMode::Normal;
    state.cursor = clamp_index(&state.lines, start_idx);
    true
}

fn cursor_to_offset(state: &EditorState) -> usize {
    let mut offset = 0;
    for row in 0..state.cursor.row {
        let len = state.lines.len_col(row).unwrap_or(0);
        offset += len + 1; // +1 for newline separator
    }
    offset + state.cursor.col
}

fn segment_bounds(chars: &[char], cursor: usize) -> (usize, usize) {
    let cursor = cursor.min(chars.len());
    let start = chars[..cursor]
        .iter()
        .rposition(|c| *c == ';')
        .map(|i| i + 1)
        .unwrap_or(0);
    let end = chars[cursor..]
        .iter()
        .position(|c| *c == ';')
        .map(|i| cursor + i)
        .unwrap_or(chars.len());
    (start, end)
}

fn leading_whitespace(chars: &[char]) -> usize {
    chars.iter().take_while(|c| c.is_whitespace()).count()
}

fn trailing_whitespace(chars: &[char]) -> usize {
    chars.iter().rev().take_while(|c| c.is_whitespace()).count()
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

fn index_to_offset(chars: &[char], idx: Index2) -> usize {
    let mut row = 0;
    let mut col = 0;
    for (i, c) in chars.iter().enumerate() {
        if row == idx.row && col == idx.col {
            return i;
        }
        if *c == '\n' {
            row += 1;
            col = 0;
        } else {
            col += 1;
        }
    }
    chars.len()
}

/// Clamp `idx` to a valid position in `lines`. Used after a buffer rewrite
/// to keep the cursor inside the new content.
fn clamp_index(lines: &Lines, idx: Index2) -> Index2 {
    let row = idx.row.min(lines.len().saturating_sub(1));
    let col_max = lines.len_col(row).unwrap_or(0);
    Index2::new(row, idx.col.min(col_max))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flatten(state: &EditorState) -> String {
        state.lines.flatten(&Some('\n')).into_iter().collect()
    }

    #[test]
    fn replace_buffer_text_resets_cursor_and_drops_selection() {
        let mut state = EditorState::new(Lines::from("old\nbuffer"));
        state.cursor = Index2::new(1, 3);

        replace_buffer_text(&mut state, "fresh\ntext");

        assert_eq!(flatten(&state), "fresh\ntext");
        assert_eq!(state.cursor, Index2::new(0, 0));
        assert!(state.selection.is_none());
    }

    #[test]
    fn replace_selection_text_no_op_without_selection() {
        let mut state = EditorState::new(Lines::from("untouched"));
        let did = replace_selection_text(&mut state, "ignored");
        assert!(!did);
        assert_eq!(flatten(&state), "untouched");
    }

    #[test]
    fn index_to_offset_rountrips_with_offset_to_index() {
        let chars: Vec<char> = "ab\ncde\nfg".chars().collect();
        for offset in 0..=chars.len() {
            let idx = offset_to_index(&chars, offset);
            assert_eq!(index_to_offset(&chars, idx), offset, "offset {offset}");
        }
    }

    #[test]
    fn clamp_index_keeps_position_inside_buffer() {
        let lines = Lines::from("ab\ncde");
        assert_eq!(clamp_index(&lines, Index2::new(0, 1)), Index2::new(0, 1));
        // Row past the end pulls back to the last row.
        assert_eq!(clamp_index(&lines, Index2::new(99, 0)), Index2::new(1, 0));
        // Column past the end pulls back to the row's length.
        assert_eq!(clamp_index(&lines, Index2::new(1, 99)), Index2::new(1, 3));
    }
}
