//! Dialect-aware SQL identifier quoting.
//!
//! Two callers, two policies:
//!
//! - [`always`] — wraps every identifier in dialect quotes. Used by
//!   `:export sql` where every name must round-trip safely regardless
//!   of what it looks like.
//! - [`smart`] — quotes only when the bare form would be misparsed
//!   (mixed case, non-identifier chars, leading digit, reserved
//!   keyword). Used by the autocomplete inserter so unnecessary quotes
//!   don't clutter user-typed SQL.
//!
//! Both share the dialect quote-pair table and the close-quote
//! doubling rule.

use crate::datasource::DriverKind;

/// Wrap `ident` in dialect quotes unconditionally, doubling any
/// internal close-quote chars.
pub fn always(ident: &str, dialect: DriverKind) -> String {
    let (open, close) = quotes(dialect);
    let escaped = ident.replace(close, &format!("{close}{close}"));
    format!("{open}{escaped}{close}")
}

/// Wrap `ident` in dialect quotes only when the bare form can't sit
/// unquoted. The rule covers four cases:
///
/// 1. Empty (parser rejects).
/// 2. Mixed-case (Postgres folds unquoted to lowercase, so `Users`
///    silently becomes `users` and misses the table).
/// 3. Non-`[A-Za-z0-9_]` chars or leading digit — parser rejects the
///    bare form.
/// 4. Reserved keyword — bare form parses as a keyword, not an ident.
///    Checked against the curated autocomplete keyword list rather
///    than `ALL_KEYWORDS_INDEX` so we don't gratuitously quote rarely
///    used SQL words like `ABORT`.
pub fn smart(ident: &str, dialect: DriverKind) -> String {
    if needs_quoting(ident) {
        always(ident, dialect)
    } else {
        ident.to_string()
    }
}

fn needs_quoting(ident: &str) -> bool {
    if ident.is_empty() {
        return true;
    }
    let first = ident.chars().next().unwrap();
    if !first.is_ascii_alphabetic() && first != '_' {
        return true;
    }
    let mut has_upper = false;
    for c in ident.chars() {
        if !c.is_ascii_alphanumeric() && c != '_' {
            return true;
        }
        if c.is_ascii_uppercase() {
            has_upper = true;
        }
    }
    if has_upper {
        return true;
    }
    is_reserved_keyword(ident)
}

fn is_reserved_keyword(ident: &str) -> bool {
    let upper = ident.to_ascii_uppercase();
    crate::autocomplete::keywords::KEYWORDS.contains(&upper.as_str())
}

fn quotes(dialect: DriverKind) -> (&'static str, &'static str) {
    match dialect {
        DriverKind::Mysql => ("`", "`"),
        DriverKind::Sqlite | DriverKind::Postgres => ("\"", "\""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn always_wraps_simple_ident() {
        assert_eq!(always("users", DriverKind::Postgres), "\"users\"");
        assert_eq!(always("users", DriverKind::Mysql), "`users`");
        assert_eq!(always("users", DriverKind::Sqlite), "\"users\"");
    }

    #[test]
    fn always_doubles_inner_quote() {
        assert_eq!(always("a\"b", DriverKind::Postgres), "\"a\"\"b\"");
        assert_eq!(always("a`b", DriverKind::Mysql), "`a``b`");
    }

    #[test]
    fn smart_lowercase_simple_ident_is_passthrough() {
        assert_eq!(smart("users", DriverKind::Postgres), "users");
        assert_eq!(smart("user_id", DriverKind::Sqlite), "user_id");
        assert_eq!(smart("a1", DriverKind::Mysql), "a1");
    }

    #[test]
    fn smart_mixed_case_postgres_uses_double_quotes() {
        assert_eq!(smart("Users", DriverKind::Postgres), "\"Users\"");
        assert_eq!(smart("MyTable", DriverKind::Sqlite), "\"MyTable\"");
    }

    #[test]
    fn smart_mysql_uses_backticks() {
        assert_eq!(smart("Users", DriverKind::Mysql), "`Users`");
    }

    #[test]
    fn smart_special_chars() {
        assert_eq!(smart("user name", DriverKind::Postgres), "\"user name\"");
        assert_eq!(smart("with-dash", DriverKind::Sqlite), "\"with-dash\"");
        assert_eq!(smart("1st", DriverKind::Postgres), "\"1st\"");
    }

    #[test]
    fn smart_reserved_keyword() {
        assert_eq!(smart("select", DriverKind::Postgres), "\"select\"");
        assert_eq!(smart("FROM", DriverKind::Mysql), "`FROM`");
    }

    #[test]
    fn smart_escapes_inner_quote() {
        assert_eq!(smart("a\"b", DriverKind::Postgres), "\"a\"\"b\"");
        assert_eq!(smart("a`b", DriverKind::Mysql), "`a``b`");
    }
}
