use edtui::{EditorEventHandler, EditorMode, EditorState, Highlight, Index2, Lines};
use ratatui::style::{Color, Style};
use sqlparser::dialect::GenericDialect;
use sqlparser::tokenizer::{Token, Tokenizer};

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

    /// Replace the buffer with `text` and park the cursor at column 0 of
    /// the given 0-indexed `row`. Used by the LLM `write_buffer` tool so
    /// the user lands on the line that just changed instead of being
    /// snapped back to (0, 0). Drops any active selection; mode is left
    /// untouched (we don't want to kick the user out of Insert if they
    /// were typing).
    pub fn replace_text_at_row(&mut self, text: &str, row: usize) {
        self.state.lines = Lines::from(text);
        self.state.selection = None;
        let row = row.min(self.state.lines.len().saturating_sub(1));
        self.state.cursor = Index2::new(row, 0);
    }

    /// Current buffer flattened to a String (joined with `\n`).
    pub fn text(&self) -> String {
        self.state.lines.flatten(&Some('\n')).into_iter().collect()
    }
}

/// The statement containing the cursor (semicolon-bounded, same naive
/// rule as `statement_under_cursor`), plus where the cursor sits inside
/// it. The autocomplete engine needs *both*:
///
/// - `statement` — full text including content past the cursor, so
///   FROM/JOIN bindings written *after* the cursor still contribute
///   aliases.
/// - `cursor_byte_in_stmt` — byte offset within `statement` (what
///   sqlparser's tokenizer slices by).
/// - `cursor_char_in_buffer` — char offset within the flattened
///   buffer, used by the editor primitive when applying an accept.
pub struct StatementCursor {
    pub statement: String,
    pub cursor_byte_in_stmt: usize,
    pub cursor_char_in_buffer: usize,
}

pub fn current_statement_with_cursor(state: &EditorState) -> StatementCursor {
    let chars: Vec<char> = state.lines.flatten(&Some('\n'));
    let buffer: String = chars.iter().collect();
    let cursor_char_in_buffer = cursor_to_offset(state).min(chars.len());
    let semis = statement_terminator_char_offsets(&buffer, &chars);
    let stmt_start_char = semis
        .iter()
        .copied()
        .filter(|&p| p < cursor_char_in_buffer)
        .max()
        .map(|p| p + 1)
        .unwrap_or(0);
    let stmt_end_char = semis
        .iter()
        .copied()
        .find(|&p| p >= cursor_char_in_buffer)
        .unwrap_or(chars.len());
    let statement: String = chars[stmt_start_char..stmt_end_char].iter().collect();
    let cursor_byte_in_stmt: usize = chars[stmt_start_char..cursor_char_in_buffer]
        .iter()
        .map(|c| c.len_utf8())
        .sum();
    StatementCursor {
        statement,
        cursor_byte_in_stmt,
        cursor_char_in_buffer,
    }
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
pub fn statement_under_cursor(state: &EditorState) -> Option<StatementRange> {
    let chars: Vec<char> = state.lines.flatten(&Some('\n'));
    let buffer: String = chars.iter().collect();
    let cursor_offset = cursor_to_offset(state);
    let (seg_start, seg_end) = segment_bounds(&buffer, &chars, cursor_offset);

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

/// Replace the statement containing the cursor with `replacement`. The
/// surrounding semicolons (and any whitespace outside the statement
/// proper) are left in place. Returns `false` if the cursor isn't sitting
/// inside any statement (empty buffer, or cursor between two `;` with no
/// content). Cursor lands at the start of the replacement.
pub fn replace_statement_under_cursor(state: &mut EditorState, replacement: &str) -> bool {
    let Some(range) = statement_under_cursor(state) else {
        return false;
    };
    let chars: Vec<char> = state.lines.flatten(&Some('\n'));
    let start_off = index_to_offset(&chars, range.start);
    // `range.end` is the last char index (inclusive); +1 makes it exclusive.
    let end_off = index_to_offset(&chars, range.end)
        .saturating_add(1)
        .min(chars.len());

    let mut next = String::with_capacity(start_off + replacement.len() + (chars.len() - end_off));
    next.extend(chars[..start_off].iter());
    next.push_str(replacement);
    next.extend(chars[end_off..].iter());

    state.lines = Lines::from(next.as_str());
    state.selection = None;
    state.mode = EditorMode::Normal;
    state.cursor = clamp_index(&state.lines, range.start);
    true
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

fn segment_bounds(buffer: &str, chars: &[char], cursor: usize) -> (usize, usize) {
    let cursor = cursor.min(chars.len());
    // edtui Normal-mode cursors at end-of-line sit at `col == row.len`,
    // which flattens to a newline (or end-of-buffer) — one past the
    // statement-terminating ';'. Without this bias the forward search
    // skips over the ';' and picks the *next* statement, contradicting
    // the user's "run the query I'm in" expectation.
    let cursor = if cursor > 0
        && matches!(chars.get(cursor), None | Some(&'\n'))
        && chars.get(cursor - 1) == Some(&';')
    {
        cursor - 1
    } else {
        cursor
    };
    let semis = statement_terminator_char_offsets(buffer, chars);
    let start = semis
        .iter()
        .copied()
        .filter(|&p| p < cursor)
        .max()
        .map(|p| p + 1)
        .unwrap_or(0);
    let end = semis
        .iter()
        .copied()
        .find(|&p| p >= cursor)
        .unwrap_or(chars.len());
    (start, end)
}

/// Char-indexed positions of every `;` that actually terminates a
/// statement — i.e. one outside of string literals, line/block
/// comments, and quoted identifiers. Uses sqlparser's tokenizer so
/// dialect-aware quoting (Postgres dollar-strings, MySQL backticks,
/// `"`-quoted Postgres identifiers, …) doesn't pollute the boundary
/// list.
///
/// On tokenizer error — typically a half-typed string or comment —
/// we fall back to the char-level `;` scan. Mid-typing is normal in
/// an editor, and a hard error here would freeze the splitter
/// (`<Space>r` couldn't pick a statement, the autocomplete couldn't
/// scope its bindings) until the buffer balances out.
fn statement_terminator_char_offsets(buffer: &str, chars: &[char]) -> Vec<usize> {
    let dialect = GenericDialect {};
    let Ok(tokens) = Tokenizer::new(&dialect, buffer).tokenize_with_location() else {
        return chars
            .iter()
            .enumerate()
            .filter(|(_, c)| **c == ';')
            .map(|(i, _)| i)
            .collect();
    };

    // Tokenizer locations are (line, column) 1-indexed in *chars*.
    // Walk the buffer once tracking (line, col, char_idx) and emit
    // the char index whenever the position matches a recorded
    // SemiColon-token start.
    let semi_locs: std::collections::BTreeSet<(u64, u64)> = tokens
        .iter()
        .filter(|t| matches!(t.token, Token::SemiColon))
        .map(|t| (t.span.start.line, t.span.start.column))
        .collect();

    if semi_locs.is_empty() {
        return Vec::new();
    }

    let mut out = Vec::with_capacity(semi_locs.len());
    let mut line: u64 = 1;
    let mut col: u64 = 1;
    for (char_idx, ch) in buffer.chars().enumerate() {
        if semi_locs.contains(&(line, col)) {
            out.push(char_idx);
        }
        if ch == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    out
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
    fn replace_statement_under_cursor_swaps_only_the_active_statement() {
        let mut state = EditorState::new(Lines::from("SELECT 1;\nSELECT 2;\nSELECT 3;"));
        // Park the cursor inside the second statement.
        state.cursor = Index2::new(1, 3);

        let did = replace_statement_under_cursor(&mut state, "SELECT\n  two");
        assert!(did);
        assert_eq!(flatten(&state), "SELECT 1;\nSELECT\n  two;\nSELECT 3;");
    }

    #[test]
    fn replace_statement_under_cursor_returns_false_when_cursor_in_empty_segment() {
        let mut state = EditorState::new(Lines::from(";;"));
        state.cursor = Index2::new(0, 1);
        assert!(!replace_statement_under_cursor(&mut state, "anything"));
        assert_eq!(flatten(&state), ";;");
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
    fn cursor_on_semicolon_returns_preceding_statement_singleline() {
        let state = state_with("SELECT 1; SELECT 2;", 0, 8); // on first ';'
        let range = statement_under_cursor(&state).expect("statement");
        assert_eq!(range.text, "SELECT 1");
    }

    #[test]
    fn cursor_on_semicolon_returns_preceding_statement_multiline() {
        let state = state_with("SELECT 1;\nSELECT 2;", 0, 8); // on first ';'
        let range = statement_under_cursor(&state).expect("statement");
        assert_eq!(range.text, "SELECT 1");
    }

    #[test]
    fn cursor_after_semicolon_returns_following_statement() {
        let state = state_with("SELECT 1; SELECT 2;", 0, 9); // on space after ';'
        let range = statement_under_cursor(&state).expect("statement");
        assert_eq!(range.text, "SELECT 2");
    }

    #[test]
    fn cursor_on_first_letter_after_semicolon_returns_following_statement() {
        let state = state_with("SELECT 1;SELECT 2;", 0, 9); // on 'S' of SELECT 2
        let range = statement_under_cursor(&state).expect("statement");
        assert_eq!(range.text, "SELECT 2");
    }

    #[test]
    fn cursor_on_terminal_semicolon_returns_only_statement() {
        let state = state_with("SELECT 1;", 0, 8); // on ';' at end
        let range = statement_under_cursor(&state).expect("statement");
        assert_eq!(range.text, "SELECT 1");
    }

    #[test]
    fn cursor_one_past_semicolon_at_end_of_line_returns_preceding() {
        // edtui Normal mode often parks the cursor at row.len (the
        // newline) rather than row.len-1 (on the last char). Without the
        // bias in `segment_bounds`, this would pick the next statement.
        let state = state_with("SELECT 1;\nSELECT 2;", 0, 9);
        let range = statement_under_cursor(&state).expect("statement");
        assert_eq!(range.text, "SELECT 1");
    }

    #[test]
    fn cursor_one_past_semicolon_at_end_of_buffer_returns_only_statement() {
        // Same edge case, but the trailing ';' is also EOB.
        let state = state_with("SELECT 1;", 0, 9);
        let range = statement_under_cursor(&state).expect("statement");
        assert_eq!(range.text, "SELECT 1");
    }

    fn state_with(buffer: &str, row: usize, col: usize) -> EditorState {
        let mut state = EditorState::new(Lines::from(buffer));
        state.cursor = Index2::new(row, col);
        state
    }

    #[test]
    fn statement_under_cursor_handles_string_with_semicolon() {
        // The naive splitter cut the buffer at the ';' inside the
        // string literal. The lexer keeps the string intact.
        let state = state_with("SELECT ';'; SELECT 1;", 0, 3);
        let range = statement_under_cursor(&state).expect("statement");
        assert_eq!(range.text, "SELECT ';'");
    }

    #[test]
    fn statement_under_cursor_handles_line_comment() {
        // `;` inside a `--` comment doesn't terminate the
        // statement that begins after the newline.
        let state = state_with("-- a; b\nSELECT 1;", 1, 2);
        let range = statement_under_cursor(&state).expect("statement");
        assert_eq!(range.text, "-- a; b\nSELECT 1");
    }

    #[test]
    fn statement_under_cursor_handles_block_comment() {
        let state = state_with("/* x; y */ SELECT 1;", 0, 14);
        let range = statement_under_cursor(&state).expect("statement");
        assert_eq!(range.text, "/* x; y */ SELECT 1");
    }

    #[test]
    fn statement_under_cursor_handles_quoted_identifier() {
        // Backtick quoting (MySQL flavour). The `;` inside the
        // identifier no longer terminates the statement.
        let state = state_with("SELECT `col;name` FROM t;", 0, 8);
        let range = statement_under_cursor(&state).expect("statement");
        assert_eq!(range.text, "SELECT `col;name` FROM t");
    }

    #[test]
    fn statement_under_cursor_handles_double_quoted_identifier() {
        // Postgres / SQLite use `"..."` for identifiers; `;` inside
        // is still part of the name, not a terminator.
        let state = state_with("SELECT \"col;name\" FROM t;", 0, 8);
        let range = statement_under_cursor(&state).expect("statement");
        assert_eq!(range.text, "SELECT \"col;name\" FROM t");
    }

    #[test]
    fn statement_under_cursor_falls_back_on_tokenizer_error() {
        // Unterminated string — tokenizer errors, fallback kicks in.
        // We don't promise correctness on this buffer (the naive
        // scan still mis-splits the string), only that we don't
        // freeze: a statement comes back, the editor stays
        // responsive.
        let state = state_with("SELECT 'unterminated", 0, 5);
        let _ = statement_under_cursor(&state); // must not panic
    }

    #[test]
    fn current_statement_with_cursor_handles_string_with_semicolon() {
        // The autocomplete classifier uses this entry point — same
        // lexer-awareness applies, so `WHERE x = ';' AND |` doesn't
        // get clipped at the literal's `;`.
        let state = state_with("SELECT * FROM t WHERE x = ';' AND y = 1;", 0, 35);
        let cur = current_statement_with_cursor(&state);
        assert_eq!(cur.statement, "SELECT * FROM t WHERE x = ';' AND y = 1");
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
