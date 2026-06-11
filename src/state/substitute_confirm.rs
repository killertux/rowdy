//! State for the interactive `:s///c` confirm flow.
//!
//! Matches are scanned live rather than precomputed: each accepted
//! replacement shifts the columns of every later match on the same line, so
//! the state only ever holds the *current* match plus a resume point
//! ([`scan_row`](ConfirmSubstituteState::scan_row) /
//! [`scan_col`](ConfirmSubstituteState::scan_col)) for finding the next one.
//!
//! The command line is single-line, so the replacement can never contain a
//! raw newline — the buffer's line count is invariant, which keeps the scan
//! a simple per-line walk.

use crate::substitute::MatchSpan;
use regex::Regex;

#[derive(Debug)]
pub struct ConfirmSubstituteState {
    pub regex: Regex,
    /// Replacement in `regex`-crate syntax (already translated).
    pub replacement: String,
    pub global: bool,
    /// Inclusive last row of the substitute range.
    pub end_row: usize,
    /// The match currently being prompted, in live-buffer char coords.
    pub current: MatchSpan,
    /// Where [`find_next`](Self::find_next) resumes scanning.
    pub scan_row: usize,
    pub scan_col: usize,
    pub substitutions: usize,
    pub lines_changed: usize,
    /// Row of the last replacement made (for final cursor placement).
    pub last_changed_line: Option<usize>,
    pub last_changed_col: usize,
}

impl ConfirmSubstituteState {
    pub fn new(
        regex: Regex,
        replacement: String,
        global: bool,
        end_row: usize,
        first: MatchSpan,
    ) -> Self {
        Self {
            regex,
            replacement,
            global,
            end_row,
            current: first,
            scan_row: first.line,
            scan_col: first.start_col,
            substitutions: 0,
            lines_changed: 0,
            last_changed_line: None,
            last_changed_col: 0,
        }
    }

    fn current_is_zero_width(&self) -> bool {
        self.current.start_col == self.current.end_col
    }

    /// Replace [`current`](Self::current) in `buffer`, returning the new
    /// buffer and advancing the scan resume point. Capture references in the
    /// replacement are expanded against the actual match.
    pub fn replace_current(&mut self, buffer: &str) -> String {
        let mut lines: Vec<String> = buffer.split('\n').map(str::to_string).collect();
        let row = self.current.line;
        let line = &lines[row];
        let start_byte = char_col_to_byte(line, self.current.start_col);
        let end_byte = char_col_to_byte(line, self.current.end_col);
        let expanded = match self.regex.captures_at(line, start_byte) {
            Some(caps) => {
                let mut dst = String::new();
                caps.expand(&self.replacement, &mut dst);
                dst
            }
            None => String::new(),
        };
        let expanded_cols = expanded.chars().count();
        let new_line = format!("{}{}{}", &line[..start_byte], expanded, &line[end_byte..]);
        lines[row] = new_line;

        self.substitutions += 1;
        if self.last_changed_line != Some(row) {
            self.lines_changed += 1;
        }
        self.last_changed_line = Some(row);
        self.last_changed_col = self.current.start_col;

        if self.global {
            self.scan_row = row;
            // Continue just past the inserted text. Bump by one for a
            // zero-width match with an empty expansion so we never re-match
            // the same spot forever.
            self.scan_col = if self.current_is_zero_width() {
                self.current.start_col + expanded_cols.max(1)
            } else {
                self.current.start_col + expanded_cols
            };
        } else {
            self.scan_row = row + 1;
            self.scan_col = 0;
        }
        lines.join("\n")
    }

    /// Advance past [`current`](Self::current) without replacing it.
    pub fn skip_current(&mut self) {
        if self.global {
            self.scan_row = self.current.line;
            self.scan_col = if self.current_is_zero_width() {
                self.current.start_col + 1
            } else {
                self.current.end_col
            };
        } else {
            self.scan_row = self.current.line + 1;
            self.scan_col = 0;
        }
    }

    /// Find the next match at or after the scan resume point, or `None` when
    /// the range is exhausted.
    pub fn find_next(&self, buffer: &str) -> Option<MatchSpan> {
        next_match(
            buffer,
            &self.regex,
            self.scan_row,
            self.scan_col,
            self.end_row,
            self.global,
        )
    }
}

fn char_col_to_byte(line: &str, col: usize) -> usize {
    line.char_indices()
        .nth(col)
        .map(|(b, _)| b)
        .unwrap_or(line.len())
}

fn byte_to_col(line: &str, byte: usize) -> usize {
    line[..byte].chars().count()
}

/// Find the next match within `[start_row, end_row]` at or after
/// `(start_row, start_col)`. With `global` off, only the first match of each
/// line is considered (and `start_col` is ignored beyond the first row).
pub fn next_match(
    buffer: &str,
    re: &Regex,
    start_row: usize,
    start_col: usize,
    end_row: usize,
    global: bool,
) -> Option<MatchSpan> {
    let lines: Vec<&str> = buffer.split('\n').collect();
    let mut row = start_row;
    let mut first = true;
    while row <= end_row && row < lines.len() {
        let line = lines[row];
        let found = if global {
            let from = if first {
                char_col_to_byte(line, start_col)
            } else {
                0
            };
            re.find_at(line, from)
        } else {
            re.find(line)
        };
        if let Some(m) = found {
            return Some(MatchSpan {
                line: row,
                start_col: byte_to_col(line, m.start()),
                end_col: byte_to_col(line, m.end()),
            });
        }
        row += 1;
        first = false;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::substitute::build_regex;

    fn state(buffer: &str, global: bool) -> (ConfirmSubstituteState, &str) {
        let re = build_regex("x", false).unwrap();
        let end_row = buffer.split('\n').count().saturating_sub(1);
        let first = next_match(buffer, &re, 0, 0, end_row, global).unwrap();
        (
            ConfirmSubstituteState::new(re, "y".to_string(), global, end_row, first),
            buffer,
        )
    }

    #[test]
    fn global_walks_all_matches_on_a_line() {
        let buffer = "x x x";
        let (mut st, mut buf) = {
            let (s, b) = state("x x x", true);
            (s, b.to_string())
        };
        assert_eq!(st.current.start_col, 0);
        buf = st.replace_current(&buf);
        assert_eq!(buf, "y x x");
        let m = st.find_next(&buf).unwrap();
        assert_eq!(m.start_col, 2);
        st.current = m;
        buf = st.replace_current(&buf);
        assert_eq!(buf, "y y x");
        let m = st.find_next(&buf).unwrap();
        assert_eq!(m.start_col, 4);
        st.current = m;
        buf = st.replace_current(&buf);
        assert_eq!(buf, "y y y");
        assert!(st.find_next(&buf).is_none());
        assert_eq!(st.substitutions, 3);
        let _ = buffer;
    }

    #[test]
    fn non_global_one_per_line() {
        let buffer = "x x\nx x";
        let (mut st, _) = state(buffer, false);
        let mut buf = buffer.to_string();
        buf = st.replace_current(&buf);
        assert_eq!(buf, "y x\nx x");
        let m = st.find_next(&buf).unwrap();
        assert_eq!(m.line, 1);
        assert_eq!(m.start_col, 0);
        st.current = m;
        buf = st.replace_current(&buf);
        assert_eq!(buf, "y x\ny x");
        assert!(st.find_next(&buf).is_none());
        assert_eq!(st.lines_changed, 2);
    }

    #[test]
    fn skip_then_find_next_on_same_line_global() {
        let buffer = "x x";
        let (mut st, _) = state(buffer, true);
        st.skip_current();
        let m = st.find_next(buffer).unwrap();
        assert_eq!(m.start_col, 2);
    }

    #[test]
    fn column_shift_after_longer_replacement() {
        // Replacement longer than the match: the next match's resume column
        // must account for the inserted text.
        let re = build_regex("x", false).unwrap();
        let first = next_match("xax", &re, 0, 0, 0, true).unwrap();
        let mut st = ConfirmSubstituteState::new(re, "ZZ".to_string(), true, 0, first);
        let buf = st.replace_current("xax");
        assert_eq!(buf, "ZZax");
        let m = st.find_next(&buf).unwrap();
        // original second x was at col 2 -> shifted to col 3 by the +1 growth.
        assert_eq!(m.start_col, 3);
    }

    #[test]
    fn zero_width_does_not_loop() {
        let re = build_regex("^", false).unwrap();
        let first = next_match("ab", &re, 0, 0, 0, true).unwrap();
        let mut st = ConfirmSubstituteState::new(re, "-".to_string(), true, 0, first);
        let buf = st.replace_current("ab");
        assert_eq!(buf, "-ab");
        // No more `^` matches after column 0 advance.
        assert!(st.find_next(&buf).is_none());
    }
}
