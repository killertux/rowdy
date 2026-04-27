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
/// statements we should run via `execute()` so we can report `rows_affected`.
/// Sniffs the first non-whitespace token, case-insensitive.
///
/// Trade-off: an `INSERT … RETURNING …` is sniffed as DML and its returned
/// rows are dropped. Acceptable today; we can teach this about RETURNING when
/// the need comes up.
pub(crate) fn is_row_returning(sql: &str) -> bool {
    let head: String = sql
        .trim_start()
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
