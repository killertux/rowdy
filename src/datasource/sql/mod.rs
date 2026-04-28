pub mod mysql;
pub mod postgres;
pub mod sqlite;

/// Lift the per-arm boilerplate in each driver's `decode_typed` switch
/// into a single line. Expands to:
///
/// ```ignore
/// decode_or_null::<$T>(row, idx).map(|opt| opt.map($build).unwrap_or(Cell::Null))
/// ```
///
/// `$build` is anything callable through `Option::map` — usually a
/// `Cell::*` constructor or a closure. The macro defers `decode_or_null`
/// resolution to the call site so each driver's own row/`Decode` bounds
/// pick up automatically.
macro_rules! decode_to {
    ($row:expr, $idx:expr, $T:ty => $build:expr) => {
        decode_or_null::<$T>($row, $idx).map(|opt| opt.map($build).unwrap_or(Cell::Null))
    };
}
pub(crate) use decode_to;

/// Collapse whitespace runs into single spaces and trim — used to flatten SQL
/// statements onto a single log line.
pub(crate) fn one_line_sql(sql: &str) -> String {
    let mut out = String::with_capacity(sql.len());
    let mut last_space = false;
    for ch in sql.chars() {
        if ch.is_whitespace() {
            if !last_space {
                out.push(' ');
                last_space = true;
            }
        } else {
            out.push(ch);
            last_space = false;
        }
    }
    out.trim().to_string()
}

/// True for statements that produce rows (SELECT-shaped) and false for
/// statements we should run via `execute()` so we can report
/// `rows_affected`. Drives the per-driver query/execute branch.
///
/// Strategy: parse with sqlparser using the driver's dialect. If parsing
/// succeeds, classify the first statement off the AST — that handles
/// leading SQL comments (`-- …` / `/* … */`), CTEs (`WITH … SELECT`),
/// `EXPLAIN`, dialect-specific show/describe forms, etc. without
/// hand-rolling token logic. If parsing fails (dialect quirks, partial
/// SQL), fall back to a hardened first-keyword sniffer that strips
/// leading comments and whitespace before matching.
///
/// Trade-off retained: `INSERT … RETURNING …` is currently classified
/// as DML and its returned rows are dropped (the AST exposes a
/// `returning` arm we could special-case in a follow-up; not needed for
/// correctness on the cases we ship today).
pub(crate) fn is_row_returning(sql: &str, dialect: &dyn sqlparser::dialect::Dialect) -> bool {
    use sqlparser::ast::Statement;
    use sqlparser::parser::Parser;

    if let Ok(stmts) = Parser::parse_sql(dialect, sql) {
        if let Some(first) = stmts.first() {
            return matches!(
                first,
                Statement::Query(_)
                    | Statement::Explain { .. }
                    | Statement::ExplainTable { .. }
                    | Statement::ShowTables { .. }
                    | Statement::ShowColumns { .. }
                    | Statement::ShowDatabases { .. }
                    | Statement::ShowSchemas { .. }
                    | Statement::ShowVariables { .. }
                    | Statement::ShowVariable { .. }
                    | Statement::ShowStatus { .. }
                    | Statement::ShowFunctions { .. }
                    | Statement::ShowCharset { .. }
                    | Statement::ShowCollation { .. }
                    | Statement::ShowCreate { .. }
                    | Statement::ShowObjects { .. }
                    | Statement::Pragma { .. }
            );
        }
        // Empty parse — nothing to run; treat as non-row-returning so we
        // don't pretend to have a result set.
        return false;
    }

    // Fallback path: parser refused (could be a dialect-specific
    // construct sqlparser doesn't model yet, or partial input the user
    // typed). Sniff the first keyword after stripping leading comments
    // and whitespace.
    let head: String = strip_leading_comments_and_ws(sql)
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    matches!(
        head.to_ascii_uppercase().as_str(),
        "SELECT"
            | "WITH"
            | "EXPLAIN"
            | "SHOW"
            | "DESCRIBE"
            | "DESC"
            | "PRAGMA"
            | "VALUES"
            | "TABLE"
    )
}

/// Skip past any combination of leading whitespace, `-- …\n` line
/// comments, and `/* … */` block comments. Used by the
/// `is_row_returning` fallback when sqlparser refuses the input — we
/// still want a leading comment in front of a `SELECT` to route
/// correctly.
fn strip_leading_comments_and_ws(sql: &str) -> &str {
    let mut s = sql;
    loop {
        let trimmed = s.trim_start();
        if let Some(rest) = trimmed.strip_prefix("--") {
            // Line comment runs to the next newline (or EOF).
            s = match rest.find('\n') {
                Some(idx) => &rest[idx + 1..],
                None => "",
            };
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("/*") {
            // Block comment runs to the next `*/`. If unterminated, we
            // give up and return what's left so the caller's keyword
            // match falls through to the not-row-returning branch.
            s = match rest.find("*/") {
                Some(idx) => &rest[idx + 2..],
                None => "",
            };
            continue;
        }
        return trimmed;
    }
}

/// Hides the password between `://user:` and `@host` so it doesn't end up in
/// the log file. Other URL forms are returned untouched.
pub(crate) fn redact_url(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_string();
    };
    let Some((authority, tail)) = rest.split_once('@') else {
        return url.to_string();
    };
    let user = authority
        .split_once(':')
        .map(|(u, _)| u)
        .unwrap_or(authority);
    format!("{scheme}://{user}:***@{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlparser::dialect::{GenericDialect, MySqlDialect, PostgreSqlDialect, SQLiteDialect};

    #[test]
    fn select_is_row_returning() {
        let d = PostgreSqlDialect {};
        assert!(is_row_returning("SELECT 1", &d));
        assert!(is_row_returning("  select * from users", &d));
    }

    #[test]
    fn with_cte_is_row_returning() {
        let d = PostgreSqlDialect {};
        assert!(is_row_returning("WITH x AS (SELECT 1) SELECT * FROM x", &d));
    }

    #[test]
    fn cte_after_line_comment_is_row_returning() {
        // The original bug: a leading `-- comment` line caused the
        // first-token sniffer to see an empty head and fall through to
        // the DML branch, so the CTE ran via `execute()` and only the
        // affected-rows count was shown.
        let d = PostgreSqlDialect {};
        let sql = "-- Check form submissions for CPF 726.770.903-68\n\
                   WITH x AS (SELECT 1) SELECT * FROM x";
        assert!(is_row_returning(sql, &d));
    }

    #[test]
    fn select_after_line_comment_is_row_returning() {
        let d = PostgreSqlDialect {};
        let sql = "-- pull recent users\nSELECT * FROM users LIMIT 10";
        assert!(is_row_returning(sql, &d));
    }

    #[test]
    fn select_after_block_comment_is_row_returning() {
        let d = PostgreSqlDialect {};
        let sql = "/* preamble\n   spanning lines */ SELECT 1";
        assert!(is_row_returning(sql, &d));
    }

    #[test]
    fn update_is_not_row_returning() {
        let d = PostgreSqlDialect {};
        assert!(!is_row_returning(
            "UPDATE users SET name='x' WHERE id=1",
            &d
        ));
    }

    #[test]
    fn update_after_line_comment_is_not_row_returning() {
        let d = PostgreSqlDialect {};
        let sql = "-- fix a name\nUPDATE users SET name='x' WHERE id=1";
        assert!(!is_row_returning(sql, &d));
    }

    #[test]
    fn delete_is_not_row_returning() {
        let d = PostgreSqlDialect {};
        assert!(!is_row_returning("DELETE FROM users WHERE id=1", &d));
    }

    #[test]
    fn insert_is_not_row_returning() {
        let d = PostgreSqlDialect {};
        assert!(!is_row_returning(
            "INSERT INTO users (name) VALUES ('a')",
            &d
        ));
    }

    #[test]
    fn explain_is_row_returning() {
        let d = PostgreSqlDialect {};
        assert!(is_row_returning("EXPLAIN SELECT * FROM users", &d));
    }

    #[test]
    fn pragma_is_row_returning_in_sqlite() {
        let d = SQLiteDialect {};
        assert!(is_row_returning("PRAGMA table_info(users)", &d));
    }

    #[test]
    fn show_tables_is_row_returning_in_mysql() {
        let d = MySqlDialect {};
        assert!(is_row_returning("SHOW TABLES", &d));
    }

    #[test]
    fn unparsable_falls_back_to_keyword_sniff() {
        // Garbage that sqlparser refuses but starts with SELECT after
        // a comment — fallback should still route to row-returning.
        let d = GenericDialect {};
        let sql = "-- weird\n  SELECT @@@@@ from"; // invalid but SELECT-shaped
        assert!(is_row_returning(sql, &d));
    }

    #[test]
    fn empty_input_is_not_row_returning() {
        let d = PostgreSqlDialect {};
        assert!(!is_row_returning("", &d));
        assert!(!is_row_returning("   \n  ", &d));
        assert!(!is_row_returning("-- only a comment\n", &d));
    }

    #[test]
    fn strip_leading_handles_mixed_comments() {
        assert_eq!(
            strip_leading_comments_and_ws("-- a\n/* b */ -- c\nSELECT 1"),
            "SELECT 1"
        );
        assert_eq!(strip_leading_comments_and_ws("   /* x */SELECT"), "SELECT");
    }
}
