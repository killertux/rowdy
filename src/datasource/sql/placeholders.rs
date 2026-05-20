//! Scan user SQL for placeholder tokens (`$N` and `:name`) and splice
//! user-supplied values back in by textual substitution.
//!
//! We rely on sqlparser's tokenizer so string literals and comments
//! are skipped for free — a `:name` that lives inside `'…'` or a
//! `-- …` line stays untouched. `$N` rides on `Token::Placeholder`,
//! which sqlparser emits in every dialect (the tokenizer only treats
//! `$$` specially for Postgres dollar-quoted strings).
//!
//! `?` is intentionally **not** treated as a placeholder: it would
//! collide with literal `?` inside `LIKE` patterns and JSON path
//! operators. Users who want positional binding write `$1`.

use std::collections::HashMap;
use std::ops::Range;

use sqlparser::dialect::Dialect;
use sqlparser::tokenizer::{Location, Token, TokenWithSpan, Tokenizer};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ParamKey {
    /// `$1`, `$2`, … — value is the number after the `$`.
    Numeric(u32),
    /// `:name` — value is the bare identifier (no leading colon).
    Named(String),
}

impl ParamKey {
    /// User-facing label including the original sigil. Used as the
    /// popup field label and as the JSON key in the on-disk history.
    pub fn label(&self) -> String {
        match self {
            ParamKey::Numeric(n) => format!("${n}"),
            ParamKey::Named(name) => format!(":{name}"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Placeholder {
    pub key: ParamKey,
    /// Byte range in the original SQL string covering the entire
    /// placeholder including its sigil (`$1`, `:name`).
    pub span: Range<usize>,
}

/// Scan `sql` for placeholders. On tokenizer error returns an empty
/// vec — caller falls back to running the query verbatim (the driver
/// will produce the real syntax error).
pub fn scan(sql: &str, dialect: &dyn Dialect) -> Vec<Placeholder> {
    let Ok(tokens) = Tokenizer::new(dialect, sql).tokenize_with_location() else {
        return Vec::new();
    };
    let pos_map = build_pos_map(sql);

    let mut out = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        let t = &tokens[i];
        match &t.token {
            Token::Placeholder(s) if s.starts_with('$') => {
                if let Ok(n) = s[1..].parse::<u32>()
                    && let Some(span) = token_byte_span(t, &pos_map, sql)
                {
                    out.push(Placeholder {
                        key: ParamKey::Numeric(n),
                        span,
                    });
                }
            }
            // MySQL admits `$` as an identifier-start character, so
            // `$1` tokenizes as a Word, not a Placeholder. Recognise
            // the same `$<digits>` shape there too — users shouldn't
            // have to remember per-dialect quirks.
            Token::Word(w) if w.quote_style.is_none() && w.value.starts_with('$') => {
                if let Ok(n) = w.value[1..].parse::<u32>()
                    && let Some(span) = token_byte_span(t, &pos_map, sql)
                {
                    out.push(Placeholder {
                        key: ParamKey::Numeric(n),
                        span,
                    });
                }
            }
            Token::Colon => {
                // `:` immediately followed by a `Word` is `:name`. We
                // require no intervening whitespace token so timestamps
                // like `'12:30'` stay safe (they live inside a string
                // and are never visible here anyway) and stray `:` in
                // weird positions don't accidentally swallow the next
                // identifier.
                if let Some(next) = tokens.get(i + 1)
                    && let Token::Word(w) = &next.token
                    && let Some(colon_span) = token_byte_span(t, &pos_map, sql)
                    && let Some(word_span) = token_byte_span(next, &pos_map, sql)
                    && colon_span.end == word_span.start
                {
                    out.push(Placeholder {
                        key: ParamKey::Named(w.value.clone()),
                        span: colon_span.start..word_span.end,
                    });
                    i += 2;
                    continue;
                }
            }
            _ => {}
        }
        i += 1;
    }
    out
}

/// First-occurrence-ordered, deduplicated list of unique parameter
/// keys. The popup shows one field per unique key; `$1` appearing
/// three times = one input.
pub fn unique_params(placeholders: &[Placeholder]) -> Vec<ParamKey> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for p in placeholders {
        if seen.insert(p.key.clone()) {
            out.push(p.key.clone());
        }
    }
    out
}

/// Splice `values` into `sql` at every placeholder span. Unknown keys
/// (shouldn't happen in normal flow) leave the original placeholder
/// text in place — better than panicking and at worst the driver
/// rejects the unsubstituted SQL with a clear error.
pub fn substitute(
    sql: &str,
    placeholders: &[Placeholder],
    values: &HashMap<ParamKey, String>,
) -> String {
    let mut sorted: Vec<&Placeholder> = placeholders.iter().collect();
    sorted.sort_by_key(|p| p.span.start);
    let mut out = String::with_capacity(sql.len());
    let mut cursor = 0;
    for p in sorted {
        if p.span.start < cursor {
            continue; // shouldn't happen; defensive against overlap.
        }
        out.push_str(&sql[cursor..p.span.start]);
        match values.get(&p.key) {
            Some(v) => out.push_str(v),
            None => out.push_str(&sql[p.span.clone()]),
        }
        cursor = p.span.end;
    }
    out.push_str(&sql[cursor..]);
    out
}

fn token_byte_span(t: &TokenWithSpan, pos_map: &PosMap, sql: &str) -> Option<Range<usize>> {
    let start = pos_map.byte_at(&t.span.start)?;
    // sqlparser's end Location is the position *after* the last
    // character of the token, so the byte offset of `end` is already
    // exclusive. Empty/synthetic spans (start == end) fall back to a
    // best-effort length derived from the token's display text.
    let end = pos_map.byte_at(&t.span.end).unwrap_or(start);
    if end > start {
        Some(start..end)
    } else {
        let len = t.token.to_string().len();
        if start + len <= sql.len() {
            Some(start..start + len)
        } else {
            None
        }
    }
}

struct PosMap {
    /// (line, column) → byte index in the source string. column is the
    /// 1-based column of the character that begins at this byte index.
    map: HashMap<(u64, u64), usize>,
}

impl PosMap {
    fn byte_at(&self, loc: &Location) -> Option<usize> {
        if loc.line == 0 || loc.column == 0 {
            return None;
        }
        self.map.get(&(loc.line, loc.column)).copied()
    }
}

fn build_pos_map(sql: &str) -> PosMap {
    let mut map = HashMap::with_capacity(sql.len());
    let mut line: u64 = 1;
    let mut col: u64 = 1;
    for (byte_idx, ch) in sql.char_indices() {
        map.insert((line, col), byte_idx);
        if ch == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    // The "one past last char" position so end-of-token lookups at EOF
    // resolve to sql.len().
    map.insert((line, col), sql.len());
    PosMap { map }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlparser::dialect::{MySqlDialect, PostgreSqlDialect, SQLiteDialect};

    fn pg() -> PostgreSqlDialect {
        PostgreSqlDialect {}
    }

    #[test]
    fn detects_numeric_placeholder() {
        let sql = "SELECT * FROM t WHERE id = $1";
        let ps = scan(sql, &pg());
        assert_eq!(ps.len(), 1);
        assert!(matches!(ps[0].key, ParamKey::Numeric(1)));
        assert_eq!(&sql[ps[0].span.clone()], "$1");
    }

    #[test]
    fn detects_named_placeholder() {
        let sql = "SELECT * FROM t WHERE name = :name";
        let ps = scan(sql, &pg());
        assert_eq!(ps.len(), 1);
        match &ps[0].key {
            ParamKey::Named(n) => assert_eq!(n, "name"),
            _ => panic!("expected named"),
        }
        assert_eq!(&sql[ps[0].span.clone()], ":name");
    }

    #[test]
    fn skips_placeholder_inside_string_literal() {
        let sql = "SELECT 'hi :world and $1'";
        let ps = scan(sql, &pg());
        assert!(ps.is_empty(), "got {ps:?}");
    }

    #[test]
    fn skips_placeholder_inside_line_comment() {
        let sql = "-- $1 :name\nSELECT 1";
        let ps = scan(sql, &pg());
        assert!(ps.is_empty(), "got {ps:?}");
    }

    #[test]
    fn skips_double_colon_cast() {
        let sql = "SELECT '1'::int";
        let ps = scan(sql, &pg());
        assert!(ps.is_empty());
    }

    #[test]
    fn named_then_cast() {
        let sql = "SELECT :id::int";
        let ps = scan(sql, &pg());
        assert_eq!(ps.len(), 1);
        assert!(matches!(&ps[0].key, ParamKey::Named(n) if n == "id"));
        assert_eq!(&sql[ps[0].span.clone()], ":id");
    }

    #[test]
    fn dedup_preserves_first_order() {
        let sql = "SELECT $2, :b, $1, :a, $2, :b";
        let ps = scan(sql, &pg());
        let uniq = unique_params(&ps);
        assert_eq!(uniq.len(), 4);
        assert!(matches!(&uniq[0], ParamKey::Numeric(2)));
        assert!(matches!(&uniq[1], ParamKey::Named(s) if s == "b"));
        assert!(matches!(&uniq[2], ParamKey::Numeric(1)));
        assert!(matches!(&uniq[3], ParamKey::Named(s) if s == "a"));
    }

    #[test]
    fn substitute_replaces_all_occurrences() {
        let sql = "SELECT $1, :name, $1";
        let ps = scan(sql, &pg());
        let mut vals = HashMap::new();
        vals.insert(ParamKey::Numeric(1), "42".into());
        vals.insert(ParamKey::Named("name".into()), "'alice'".into());
        let out = substitute(sql, &ps, &vals);
        assert_eq!(out, "SELECT 42, 'alice', 42");
    }

    #[test]
    fn substitute_handles_multibyte_prefix() {
        let sql = "SELECT '😀' AS x, $1";
        let ps = scan(sql, &pg());
        let mut vals = HashMap::new();
        vals.insert(ParamKey::Numeric(1), "99".into());
        let out = substitute(sql, &ps, &vals);
        assert_eq!(out, "SELECT '😀' AS x, 99");
    }

    #[test]
    fn works_in_mysql_dialect() {
        let sql = "SELECT * FROM t WHERE id = $1 AND name = :n";
        let ps = scan(sql, &MySqlDialect {});
        assert_eq!(ps.len(), 2);
    }

    #[test]
    fn works_in_sqlite_dialect() {
        let sql = "SELECT * FROM t WHERE id = $1 AND name = :n";
        let ps = scan(sql, &SQLiteDialect {});
        assert_eq!(ps.len(), 2);
    }

    #[test]
    fn tokenizer_error_returns_empty() {
        let sql = "SELECT 'unterminated";
        let ps = scan(sql, &pg());
        assert!(ps.is_empty());
    }

    #[test]
    fn missing_value_leaves_placeholder_literal() {
        let sql = "SELECT $1, :name";
        let ps = scan(sql, &pg());
        let mut vals = HashMap::new();
        vals.insert(ParamKey::Numeric(1), "7".into());
        let out = substitute(sql, &ps, &vals);
        assert_eq!(out, "SELECT 7, :name");
    }
}
