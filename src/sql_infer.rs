//! Source-table inference for `:export sql`.
//!
//! Walks a SELECT AST and, when the projection unambiguously points at a
//! single source table, returns its name. Otherwise returns a short
//! human-readable hint so the caller can surface it to the user (who
//! must then pass `:export sql <table>`).
//!
//! ## What we accept
//!
//! - One statement, of `SELECT` shape (no UNION, no `WITH` (CTE), no INSERT/UPDATE/DELETE).
//! - `FROM` has exactly one *bare* table — no JOINs, no derived tables,
//!   no table-valued functions.
//! - Either:
//!   1. The projection is a single `*` or `<table>.*` (pure wildcard); or
//!   2. Every selected projection item is a bare or table-qualified
//!      identifier with no alias, no expression, no function call.
//!
//! Anything else (aliases, expressions, mixed wildcard/explicit, joins,
//! subqueries, CTEs, set ops) refuses inference.

use sqlparser::ast::{
    Expr, ObjectName, ObjectNamePart, SelectItem, SelectItemQualifiedWildcardKind, SetExpr,
    Statement, TableFactor,
};
use sqlparser::dialect::{Dialect, MySqlDialect, PostgreSqlDialect, SQLiteDialect};
use sqlparser::parser::Parser;

use crate::datasource::DriverKind;

pub fn dialect_for(kind: DriverKind) -> Box<dyn Dialect> {
    match kind {
        DriverKind::Sqlite => Box::new(SQLiteDialect {}),
        DriverKind::Postgres => Box::new(PostgreSqlDialect {}),
        DriverKind::Mysql => Box::new(MySqlDialect {}),
    }
}

/// Try to infer the source table.
///
/// `column_indices` is a list of result-column positions the caller cares
/// about. Pass `None` to require *every* projected column to satisfy the
/// rules (used when exporting the whole result block); pass `Some(&[..])`
/// when only some columns matter (Visual selection sub-rectangle).
pub fn infer_source_table(
    sql: &str,
    kind: DriverKind,
    column_indices: Option<&[usize]>,
) -> Result<String, String> {
    let dialect = dialect_for(kind);
    let stmts = Parser::parse_sql(&*dialect, sql)
        .map_err(|e| format!("could not parse query for inference ({e})"))?;
    let stmt = match stmts.as_slice() {
        [s] => s,
        [] => return Err("query is empty".into()),
        _ => return Err("multiple statements; pass an explicit table".into()),
    };
    let query = match stmt {
        Statement::Query(q) => q,
        _ => return Err("not a SELECT; pass an explicit table".into()),
    };
    if query.with.is_some() {
        return Err("CTEs aren't supported by inference; pass an explicit table".into());
    }
    let select = match query.body.as_ref() {
        SetExpr::Select(s) => s,
        _ => return Err("query body isn't a plain SELECT; pass an explicit table".into()),
    };

    if select.from.len() != 1 {
        return Err("query has 0 or multiple FROM tables; pass an explicit table".into());
    }
    let from0 = &select.from[0];
    if !from0.joins.is_empty() {
        return Err("query has joins; pass an explicit table".into());
    }
    let (table_name, table_alias) = match &from0.relation {
        TableFactor::Table { name, alias, .. } => {
            let alias_name = alias.as_ref().map(|a| a.name.value.clone());
            (object_name_to_string(name)?, alias_name)
        }
        _ => return Err("query FROM isn't a bare table; pass an explicit table".into()),
    };

    let projection = &select.projection;
    if projection.is_empty() {
        return Err("query has no projection; pass an explicit table".into());
    }

    // Pure-wildcard projection (the only item is `*` or `<table>.*` /
    // `<alias>.*`): every result column trivially traces back to the
    // single FROM table.
    if projection.len() == 1
        && is_full_wildcard(&projection[0], &table_name, table_alias.as_deref())
    {
        return Ok(table_name);
    }

    // Identifier-list projection: each item maps to exactly one result
    // column. Check only the indices the caller asked about; others can
    // be aliased/computed and still let an inference succeed for a
    // Visual subset.
    let indices: Vec<usize> = match column_indices {
        Some(slice) => slice.to_vec(),
        None => (0..projection.len()).collect(),
    };
    for idx in indices {
        let item = projection.get(idx).ok_or_else(|| {
            format!("can't infer table: result column {idx} doesn't map to a known projection item")
        })?;
        check_identifier_item(item, &table_name, table_alias.as_deref())?;
    }
    Ok(table_name)
}

fn is_full_wildcard(item: &SelectItem, table: &str, alias: Option<&str>) -> bool {
    match item {
        SelectItem::Wildcard(_) => true,
        SelectItem::QualifiedWildcard(kind, _) => match kind {
            SelectItemQualifiedWildcardKind::ObjectName(name) => {
                let parts: Vec<String> = name
                    .0
                    .iter()
                    .filter_map(|p| match p {
                        ObjectNamePart::Identifier(id) => Some(id.value.clone()),
                        _ => None,
                    })
                    .collect();
                // Accept either `<alias>.*` or `<table>.*`.
                let last = parts.last().map(String::as_str);
                matches!(last, Some(t) if t == table)
                    || matches!((last, alias), (Some(a), Some(b)) if a == b)
            }
            SelectItemQualifiedWildcardKind::Expr(_) => false,
        },
        _ => false,
    }
}

fn check_identifier_item(
    item: &SelectItem,
    table: &str,
    alias: Option<&str>,
) -> Result<(), String> {
    match item {
        SelectItem::UnnamedExpr(expr) => check_identifier_expr(expr, table, alias),
        SelectItem::ExprWithAlias { .. } => Err(
            "selected column has an alias; aliases break round-trip — pass an explicit table"
                .into(),
        ),
        SelectItem::Wildcard(_) | SelectItem::QualifiedWildcard(_, _) => Err(
            "projection mixes wildcard with explicit items; can't map result columns — pass an explicit table"
                .into(),
        ),
    }
}

fn check_identifier_expr(expr: &Expr, table: &str, alias: Option<&str>) -> Result<(), String> {
    match expr {
        Expr::Identifier(_) => Ok(()),
        Expr::CompoundIdentifier(parts) => {
            let qualifier = parts
                .first()
                .map(|p| p.value.as_str())
                .ok_or_else(|| "empty qualified identifier".to_string())?;
            if qualifier == table || alias.is_some_and(|a| a == qualifier) {
                Ok(())
            } else {
                Err(format!(
                    "selected column references {qualifier:?}, not the FROM table; pass an explicit table",
                ))
            }
        }
        _ => {
            Err("selected column is computed (function/expression); pass an explicit table".into())
        }
    }
}

fn object_name_to_string(name: &ObjectName) -> Result<String, String> {
    // Use the *last* segment as the table — this strips any
    // database/schema prefix (`db.users` → `users`). The export emits
    // unqualified `INSERT INTO <name>`; the user can supply a qualified
    // name explicitly if they need it.
    name.0
        .iter()
        .filter_map(|p| match p {
            ObjectNamePart::Identifier(id) => Some(id.value.clone()),
            _ => None,
        })
        .next_back()
        .ok_or_else(|| "empty FROM table name".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn infer(sql: &str) -> Result<String, String> {
        infer_source_table(sql, DriverKind::Sqlite, None)
    }

    fn infer_subset(sql: &str, idx: &[usize]) -> Result<String, String> {
        infer_source_table(sql, DriverKind::Sqlite, Some(idx))
    }

    #[test]
    fn star_from_single_table() {
        assert_eq!(infer("SELECT * FROM users").unwrap(), "users");
    }

    #[test]
    fn explicit_columns_from_single_table() {
        assert_eq!(infer("SELECT id, name FROM users").unwrap(), "users");
    }

    #[test]
    fn qualified_columns_from_single_table() {
        assert_eq!(
            infer("SELECT users.id, users.name FROM users").unwrap(),
            "users"
        );
    }

    #[test]
    fn aliased_table_with_qualified_columns() {
        assert_eq!(infer("SELECT u.id, u.name FROM users u").unwrap(), "users");
    }

    #[test]
    fn aliased_table_wildcard() {
        assert_eq!(infer("SELECT u.* FROM users u").unwrap(), "users");
    }

    #[test]
    fn schema_qualified_table_strips_prefix() {
        assert_eq!(infer("SELECT * FROM public.users").unwrap(), "users");
    }

    #[test]
    fn aliased_column_refused() {
        let err = infer("SELECT id AS user_id FROM users").unwrap_err();
        assert!(err.contains("alias"), "{err}");
    }

    #[test]
    fn computed_column_refused() {
        let err = infer("SELECT id, lower(name) FROM users").unwrap_err();
        assert!(
            err.contains("computed") || err.contains("function"),
            "{err}"
        );
    }

    #[test]
    fn join_refused() {
        let err = infer("SELECT u.id, p.title FROM users u JOIN posts p ON p.user_id = u.id")
            .unwrap_err();
        assert!(err.contains("join"), "{err}");
    }

    #[test]
    fn cte_refused() {
        let err = infer("WITH x AS (SELECT 1) SELECT * FROM x").unwrap_err();
        assert!(err.contains("CTE"), "{err}");
    }

    #[test]
    fn union_refused() {
        let err = infer("SELECT * FROM users UNION SELECT * FROM admins").unwrap_err();
        assert!(err.contains("plain SELECT"), "{err}");
    }

    #[test]
    fn aggregate_refused() {
        let err = infer("SELECT count(*) FROM users").unwrap_err();
        assert!(
            err.contains("computed") || err.contains("function"),
            "{err}"
        );
    }

    #[test]
    fn subset_with_aliased_other_columns_passes_for_clean_subset() {
        // Project: id (clean), display_name (aliased). Select only [0].
        let sql = "SELECT id, name AS display_name FROM users";
        assert_eq!(infer_subset(sql, &[0]).unwrap(), "users");
        // Selecting the aliased one fails.
        assert!(infer_subset(sql, &[1]).is_err());
    }

    #[test]
    fn unparseable_sql_refused() {
        let err = infer("not even sql").unwrap_err();
        assert!(err.contains("parse"), "{err}");
    }
}
