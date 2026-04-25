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
