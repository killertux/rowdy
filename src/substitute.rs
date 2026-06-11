//! Pure engine for the vim-style `:[range]s/pattern/replacement/flags`
//! command. Everything here operates on plain `&str`/`String` so it can be
//! unit-tested without touching `App` or edtui.
//!
//! [`parse_substitute`] turns a raw `:`-line into a [`SubstituteCmd`] (or
//! `Ok(None)` when the line isn't substitute-shaped, so the normal command
//! parser keeps it). The dispatcher in `action::substitute` resolves the
//! range against the live buffer, compiles the regex, and applies it.
//!
//! Regex flavour is the `regex` crate (NOT vim's regex). Capture references
//! in the replacement use `$1` / `${name}`; a literal `$` must be written
//! `$$`. For vim muscle-memory we also accept `\1`..`\9` (mapped to
//! `${1}`..`${9}`), `&` / `\0` for the whole match, and `\&` for a literal
//! ampersand.

use regex::{Regex, RegexBuilder};

/// A fully-parsed substitute command. Field values are already unescaped
/// (the separator-escaping is resolved during parsing) and the replacement
/// is translated to `regex`-crate syntax.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubstituteCmd {
    pub range: SubstituteRange,
    /// Empty means "reuse the last search/substitute pattern" — resolved at
    /// dispatch time, not here.
    pub pattern: String,
    pub replacement: String,
    pub flags: SubstituteFlags,
}

/// Which lines the substitution applies to. Resolved to a 0-based inclusive
/// row range by [`resolve_range`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubstituteRange {
    /// No range prefix — the line under the cursor (`:s/…`).
    CurrentLine,
    /// `%` — the whole buffer (`:%s/…`).
    WholeBuffer,
    /// `N,M` / `.,$` / etc. — an explicit address pair.
    Lines(Address, Address),
    /// `'<,'>` — the current visual selection's line span.
    VisualSelection,
}

/// One endpoint of a `Lines` range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Address {
    /// A 1-based absolute line number.
    Line(usize),
    /// `.` — the cursor line.
    Current,
    /// `$` — the last line.
    Last,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SubstituteFlags {
    /// `g` — replace every match on a line, not just the first.
    pub global: bool,
    /// `i` — case-insensitive matching.
    pub ignore_case: bool,
    /// `c` — confirm each substitution interactively.
    pub confirm: bool,
}

/// Result of a non-interactive [`apply_substitute`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubstituteOutcome {
    /// The full new buffer text.
    pub text: String,
    pub substitutions: usize,
    pub lines_changed: usize,
    /// 0-based row of the last line that changed (for cursor placement).
    pub last_changed_line: usize,
    /// Char column of the last substitution's start on that line.
    pub last_changed_col: usize,
}

/// A single match, in char coordinates (edtui `Lines` are char-indexed; the
/// `regex` crate works in bytes, so callers convert). `end_col` is exclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MatchSpan {
    pub line: usize,
    pub start_col: usize,
    pub end_col: usize,
}

/// Parse a raw `:`-line (no leading `:`). Returns `Ok(None)` when the line
/// isn't a substitute command, so the caller falls through to the normal
/// parser — this is what keeps `:save`, `:session`, `:source` working.
pub fn parse_substitute(line: &str) -> Result<Option<SubstituteCmd>, String> {
    let line = line.trim_start();
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;

    let range = scan_range(&chars, &mut i);

    // The command keyword: a run of ASCII letters. Only `s` / `substitute`
    // are us; anything else (save, session, source, …) is not a substitute.
    let kw_start = i;
    while i < chars.len() && chars[i].is_ascii_alphabetic() {
        i += 1;
    }
    let keyword: String = chars[kw_start..i].iter().collect();
    let is_sub_keyword = keyword == "s" || keyword == "substitute";
    if !is_sub_keyword {
        return Ok(None);
    }

    // The separator: any ASCII punctuation except `\`, `"`, `|` (vim's rule).
    let Some(&sep) = chars.get(i) else {
        // `s` / `substitute` with no separator. If the user clearly meant a
        // substitute (explicit range, or the long keyword), surface the
        // usage; bare `:s` falls through so the normal parser reports it.
        if range != SubstituteRange::CurrentLine || keyword == "substitute" {
            return Err(USAGE.to_string());
        }
        return Ok(None);
    };
    if !is_separator(sep) {
        // e.g. `:s ` followed by a space — not substitute-shaped.
        if range != SubstituteRange::CurrentLine || keyword == "substitute" {
            return Err(USAGE.to_string());
        }
        return Ok(None);
    }
    i += 1;

    let rest: String = chars[i..].iter().collect();
    let (pattern, replacement_raw, flags_raw) = split_fields(&rest, sep);

    let flags = parse_flags(&flags_raw)?;
    let replacement = translate_replacement(&replacement_raw);

    Ok(Some(SubstituteCmd {
        range,
        pattern,
        replacement,
        flags,
    }))
}

const USAGE: &str = "usage: :[range]s/pattern/replacement/[flags]";

/// Characters allowed as the `s` separator. Vim forbids `\`, `"`, and `|`.
fn is_separator(c: char) -> bool {
    c.is_ascii_punctuation() && c != '\\' && c != '"' && c != '|'
}

/// Consume a leading range prefix, advancing `i` past it. Returns
/// `CurrentLine` (and leaves `i` untouched) when there is none.
fn scan_range(chars: &[char], i: &mut usize) -> SubstituteRange {
    // `%` — whole buffer.
    if chars.get(*i) == Some(&'%') {
        *i += 1;
        return SubstituteRange::WholeBuffer;
    }
    // `'<,'>` — visual selection.
    if chars[*i..].starts_with(&['\'', '<', ',', '\'', '>']) {
        *i += 5;
        return SubstituteRange::VisualSelection;
    }
    // `addr[,addr]` — explicit line addresses.
    let save = *i;
    if let Some(first) = scan_address(chars, i) {
        if chars.get(*i) == Some(&',') {
            let after_comma = *i + 1;
            let mut j = after_comma;
            if let Some(second) = scan_address(chars, &mut j) {
                *i = j;
                return SubstituteRange::Lines(first, second);
            }
            // `,` not followed by a valid address — not a range.
            *i = save;
            return SubstituteRange::CurrentLine;
        }
        return SubstituteRange::Lines(first, first);
    }
    *i = save;
    SubstituteRange::CurrentLine
}

/// Parse a single address (`.`, `$`, or a run of digits), advancing `i`.
fn scan_address(chars: &[char], i: &mut usize) -> Option<Address> {
    match chars.get(*i) {
        Some('.') => {
            *i += 1;
            Some(Address::Current)
        }
        Some('$') => {
            *i += 1;
            Some(Address::Last)
        }
        Some(c) if c.is_ascii_digit() => {
            let start = *i;
            while *i < chars.len() && chars[*i].is_ascii_digit() {
                *i += 1;
            }
            let n: usize = chars[start..*i].iter().collect::<String>().parse().ok()?;
            Some(Address::Line(n))
        }
        _ => None,
    }
}

/// Split `rest` (everything after the opening separator) into
/// `(pattern, replacement, flags)` on unescaped separators. `\<sep>` becomes
/// a literal separator in the field; all other escapes are preserved so the
/// regex / replacement layers see them. At most two separators are honoured;
/// anything after the second is the flags field verbatim.
fn split_fields(rest: &str, sep: char) -> (String, String, String) {
    let mut fields: Vec<String> = vec![String::new()];
    let mut chars = rest.chars().peekable();
    while let Some(c) = chars.next() {
        if fields.len() >= 3 {
            // Past the replacement: the remainder (incl. any seps) is flags.
            fields.last_mut().unwrap().push(c);
            continue;
        }
        if c == '\\' {
            match chars.next() {
                Some(n) if n == sep => fields.last_mut().unwrap().push(sep),
                Some(n) => {
                    let f = fields.last_mut().unwrap();
                    f.push('\\');
                    f.push(n);
                }
                None => fields.last_mut().unwrap().push('\\'),
            }
            continue;
        }
        if c == sep {
            fields.push(String::new());
            continue;
        }
        fields.last_mut().unwrap().push(c);
    }
    let mut it = fields.into_iter();
    let pattern = it.next().unwrap_or_default();
    let replacement = it.next().unwrap_or_default();
    let flags = it.next().unwrap_or_default();
    (pattern, replacement, flags)
}

fn parse_flags(raw: &str) -> Result<SubstituteFlags, String> {
    let mut flags = SubstituteFlags::default();
    for c in raw.chars() {
        match c {
            'g' => flags.global = true,
            'i' => flags.ignore_case = true,
            'c' => flags.confirm = true,
            other => return Err(format!("unknown :s flag: {other}")),
        }
    }
    Ok(flags)
}

/// Translate a vim-flavoured replacement into `regex`-crate syntax.
/// `\1`..`\9` → `${1}`..`${9}`, `\0` / `&` → `${0}` (whole match), `\&` →
/// literal `&`, `\\` → literal `\`. `$1` / `${name}` pass through untouched
/// (so a literal `$` must be written `$$`, per the regex crate).
fn translate_replacement(repl: &str) -> String {
    let mut out = String::new();
    let mut chars = repl.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some(d) if d.is_ascii_digit() => {
                    out.push_str("${");
                    out.push(d);
                    out.push('}');
                }
                Some('&') => out.push('&'),
                Some('\\') => out.push('\\'),
                Some(other) => out.push(other),
                None => out.push('\\'),
            },
            '&' => out.push_str("${0}"),
            other => out.push(other),
        }
    }
    out
}

/// Compile the pattern, honouring the case-insensitive flag. The error string
/// is shown verbatim in the status bar.
pub fn build_regex(pattern: &str, ignore_case: bool) -> Result<Regex, String> {
    RegexBuilder::new(pattern)
        .case_insensitive(ignore_case)
        .build()
        .map_err(|e| e.to_string())
}

/// Resolve a [`SubstituteRange`] to a 0-based inclusive `(start, end)` row
/// pair, clamped to the buffer. `cursor_row` / `last_row` are 0-based;
/// `selection_rows` is the visual selection span when present.
pub fn resolve_range(
    range: &SubstituteRange,
    cursor_row: usize,
    last_row: usize,
    selection_rows: Option<(usize, usize)>,
) -> Result<(usize, usize), String> {
    let clamp = |r: usize| r.min(last_row);
    match range {
        SubstituteRange::CurrentLine => Ok((clamp(cursor_row), clamp(cursor_row))),
        SubstituteRange::WholeBuffer => Ok((0, last_row)),
        SubstituteRange::VisualSelection => {
            let (a, b) = selection_rows.ok_or("no visual selection")?;
            Ok((clamp(a.min(b)), clamp(a.max(b))))
        }
        SubstituteRange::Lines(a, b) => {
            let resolve = |addr: &Address| -> usize {
                match addr {
                    // 1-based addresses → 0-based rows.
                    Address::Line(n) => clamp(n.saturating_sub(1)),
                    Address::Current => clamp(cursor_row),
                    Address::Last => last_row,
                }
            };
            let start = resolve(a);
            let end = resolve(b);
            if start > end {
                return Err("backwards range".to_string());
            }
            Ok((start, end))
        }
    }
}

/// Convert a byte offset within `line` to a char column.
fn byte_to_col(line: &str, byte: usize) -> usize {
    line[..byte].chars().count()
}

/// Find every match within the inclusive `rows` range. Without `global`,
/// only the first match per line is reported.
pub fn find_matches(
    buffer: &str,
    rows: (usize, usize),
    re: &Regex,
    global: bool,
) -> Vec<MatchSpan> {
    let mut spans = Vec::new();
    for (row, line) in buffer.split('\n').enumerate() {
        if row < rows.0 || row > rows.1 {
            continue;
        }
        for m in re.find_iter(line) {
            spans.push(MatchSpan {
                line: row,
                start_col: byte_to_col(line, m.start()),
                end_col: byte_to_col(line, m.end()),
            });
            if !global {
                break;
            }
        }
    }
    spans
}

/// Apply the substitution to every line in the inclusive `rows` range,
/// returning the new buffer plus statistics. Substitution is strictly
/// line-by-line (vim's default — patterns never match across `\n`).
pub fn apply_substitute(
    buffer: &str,
    rows: (usize, usize),
    re: &Regex,
    replacement: &str,
    global: bool,
) -> SubstituteOutcome {
    let mut out_lines: Vec<String> = Vec::new();
    let mut substitutions = 0;
    let mut lines_changed = 0;
    let mut last_changed_line = 0;
    let mut last_changed_col = 0;

    for (row, line) in buffer.split('\n').enumerate() {
        if row < rows.0 || row > rows.1 {
            out_lines.push(line.to_string());
            continue;
        }
        let count = if global {
            re.find_iter(line).count()
        } else {
            usize::from(re.is_match(line))
        };
        if count == 0 {
            out_lines.push(line.to_string());
            continue;
        }
        // Column of the last substitution start (in original coords) for
        // cursor placement; the editor clamps it after the rebuild.
        if let Some(m) = re
            .find_iter(line)
            .take(if global { count } else { 1 })
            .last()
        {
            last_changed_col = byte_to_col(line, m.start());
        }
        let limit = if global { 0 } else { 1 };
        let replaced = re.replacen(line, limit, replacement).into_owned();
        substitutions += count;
        lines_changed += 1;
        last_changed_line = row;
        out_lines.push(replaced);
    }

    SubstituteOutcome {
        text: out_lines.join("\n"),
        substitutions,
        lines_changed,
        last_changed_line,
        last_changed_col,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(line: &str) -> SubstituteCmd {
        parse_substitute(line)
            .expect("parse ok")
            .expect("is substitute")
    }

    #[test]
    fn parses_basic_current_line() {
        let c = cmd("s/foo/bar/");
        assert_eq!(c.range, SubstituteRange::CurrentLine);
        assert_eq!(c.pattern, "foo");
        assert_eq!(c.replacement, "bar");
        assert_eq!(c.flags, SubstituteFlags::default());
    }

    #[test]
    fn parses_whole_buffer_global() {
        let c = cmd("%s/foo/bar/g");
        assert_eq!(c.range, SubstituteRange::WholeBuffer);
        assert!(c.flags.global);
    }

    #[test]
    fn parses_line_range() {
        let c = cmd("3,7s/a/b/");
        assert_eq!(
            c.range,
            SubstituteRange::Lines(Address::Line(3), Address::Line(7))
        );
    }

    #[test]
    fn parses_dot_dollar_range() {
        let c = cmd(".,$s/a/b/");
        assert_eq!(
            c.range,
            SubstituteRange::Lines(Address::Current, Address::Last)
        );
    }

    #[test]
    fn parses_visual_selection_range_with_flags() {
        let c = cmd("'<,'>s/a/b/gc");
        assert_eq!(c.range, SubstituteRange::VisualSelection);
        assert!(c.flags.global);
        assert!(c.flags.confirm);
    }

    #[test]
    fn alternate_separator() {
        let c = cmd("s#a/b#c#");
        assert_eq!(c.pattern, "a/b");
        assert_eq!(c.replacement, "c");
    }

    #[test]
    fn escaped_separator() {
        let c = cmd(r"s/a\/b/c/");
        assert_eq!(c.pattern, "a/b");
    }

    #[test]
    fn empty_pattern_reuses_last() {
        let c = cmd("s//bar/");
        assert_eq!(c.pattern, "");
        assert_eq!(c.replacement, "bar");
    }

    #[test]
    fn empty_replacement_is_deletion() {
        let c = cmd("s/foo//");
        assert_eq!(c.replacement, "");
    }

    #[test]
    fn missing_trailing_separator() {
        let c = cmd("s/foo/bar");
        assert_eq!(c.pattern, "foo");
        assert_eq!(c.replacement, "bar");
    }

    #[test]
    fn long_keyword() {
        let c = cmd("substitute/a/b/");
        assert_eq!(c.pattern, "a");
    }

    #[test]
    fn save_command_falls_through() {
        assert_eq!(parse_substitute("save my query"), Ok(None));
        assert_eq!(parse_substitute("session 2"), Ok(None));
        assert_eq!(parse_substitute("source"), Ok(None));
    }

    #[test]
    fn bad_flag_errors() {
        assert!(parse_substitute("s/a/b/x").is_err());
    }

    #[test]
    fn substitute_keyword_without_sep_errors() {
        assert!(parse_substitute("substitute").is_err());
    }

    #[test]
    fn translate_backref_and_amp() {
        assert_eq!(translate_replacement(r"\1"), "${1}");
        assert_eq!(translate_replacement("&"), "${0}");
        assert_eq!(translate_replacement(r"\&"), "&");
        assert_eq!(translate_replacement(r"\\"), r"\");
        assert_eq!(translate_replacement("$1"), "$1");
    }

    #[test]
    fn resolve_range_clamps_and_orders() {
        assert_eq!(
            resolve_range(&SubstituteRange::WholeBuffer, 0, 9, None),
            Ok((0, 9))
        );
        assert_eq!(
            resolve_range(&SubstituteRange::CurrentLine, 3, 9, None),
            Ok((3, 3))
        );
        // 1-based 100 clamps to last row.
        assert_eq!(
            resolve_range(
                &SubstituteRange::Lines(Address::Line(1), Address::Line(100)),
                0,
                9,
                None
            ),
            Ok((0, 9))
        );
    }

    #[test]
    fn resolve_range_backwards_errors() {
        assert!(
            resolve_range(
                &SubstituteRange::Lines(Address::Line(7), Address::Line(3)),
                0,
                9,
                None
            )
            .is_err()
        );
    }

    #[test]
    fn resolve_range_visual_without_selection_errors() {
        assert!(resolve_range(&SubstituteRange::VisualSelection, 0, 9, None).is_err());
    }

    #[test]
    fn apply_global_vs_first() {
        let re = build_regex("a", false).unwrap();
        let buf = "a a a";
        let g = apply_substitute(buf, (0, 0), &re, "X", true);
        assert_eq!(g.text, "X X X");
        assert_eq!(g.substitutions, 3);
        let first = apply_substitute(buf, (0, 0), &re, "X", false);
        assert_eq!(first.text, "X a a");
        assert_eq!(first.substitutions, 1);
    }

    #[test]
    fn apply_counts_lines_changed() {
        let re = build_regex("x", false).unwrap();
        let buf = "x\ny\nx";
        let out = apply_substitute(buf, (0, 2), &re, "z", true);
        assert_eq!(out.text, "z\ny\nz");
        assert_eq!(out.substitutions, 2);
        assert_eq!(out.lines_changed, 2);
        assert_eq!(out.last_changed_line, 2);
    }

    #[test]
    fn apply_zero_width_anchor() {
        let re = build_regex("^", false).unwrap();
        let out = apply_substitute("hello", (0, 0), &re, "-- ", false);
        assert_eq!(out.text, "-- hello");
        assert_eq!(out.substitutions, 1);
    }

    #[test]
    fn apply_utf8_columns() {
        let re = build_regex("é", false).unwrap();
        let buf = "café résumé";
        let spans = find_matches(buf, (0, 0), &re, true);
        // café -> é at char col 3; résumé -> é at cols 6 and 10.
        assert_eq!(spans[0].start_col, 3);
        assert_eq!(spans[1].start_col, 6);
        assert_eq!(spans[2].start_col, 10);
        let out = apply_substitute(buf, (0, 0), &re, "e", true);
        assert_eq!(out.text, "cafe resume");
    }

    #[test]
    fn apply_case_insensitive() {
        let re = build_regex("foo", true).unwrap();
        let out = apply_substitute("FOO foo Foo", (0, 0), &re, "bar", true);
        assert_eq!(out.text, "bar bar bar");
    }

    #[test]
    fn apply_capture_groups() {
        let re = build_regex(r"(\w+)\.(\w+)", false).unwrap();
        // Brace form is required when a digit ref is followed by a name char.
        let out = apply_substitute("schema.table", (0, 0), &re, "${2}_${1}", false);
        assert_eq!(out.text, "table_schema");
    }

    #[test]
    fn apply_outside_range_untouched() {
        let re = build_regex("x", false).unwrap();
        let buf = "x\nx\nx";
        let out = apply_substitute(buf, (1, 1), &re, "y", true);
        assert_eq!(out.text, "x\ny\nx");
    }
}
