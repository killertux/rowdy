//! Stoolap: an embedded, pure-Rust MVCC database. Unlike the sqlx
//! drivers this runs the engine in-process through stoolap's
//! synchronous API, so every call hops onto tokio's blocking pool.
//!
//! URLs follow the `sqlite:` shape: `stoolap:./data/mydb` or
//! `stoolap:///abs/path` (a *directory*, created if missing; stoolap
//! options go in the query string, e.g. `?sync_mode=full`), or
//! `stoolap::memory:` for a fresh in-memory database. A native stoolap
//! DSN after the prefix (`stoolap:file:///abs/path`) passes through.

use std::sync::{Arc, Mutex as StdMutex};
use std::time::Instant;

use async_trait::async_trait;
use sqlparser::dialect::{Dialect, PostgreSqlDialect};
use stoolap::executor::context::CancellationHandle;
use stoolap::{Database, ExecutionContext, Executor, ResultRow, Value};

use crate::autocomplete::functions::{FnArity, FunctionDef};
use crate::datasource::cell::Cell;
use crate::datasource::error::{DatasourceError, DatasourceResult};
use crate::datasource::schema::{
    CatalogInfo, ColumnInfo, DefaultSchema, IndexInfo, SchemaInfo, TableInfo, TableKind,
};
use crate::datasource::{Column, Datasource, Driver, QueryResult, Row as CellRow};
use crate::log::Logger;

const TARGET: &str = "stoolap";
/// Stoolap has no catalogs or schemas; surface a single synthetic one
/// of each so the schema tree has somewhere to hang tables.
const NAMESPACE: &str = "main";

/// Stoolap dialect descriptor. See [`Driver`].
#[derive(Debug)]
pub struct StoolapDriver;

#[async_trait]
impl Driver for StoolapDriver {
    fn name(&self) -> &'static str {
        TARGET
    }

    fn schemes(&self) -> &'static [&'static str] {
        &["stoolap"]
    }

    fn sql_dialect(&self) -> &'static dyn Dialect {
        // Stoolap's grammar is Postgres-flavoured: `$N` params, `::`
        // casts, `ON CONFLICT`, `RETURNING`, double-quoted identifiers.
        &PostgreSqlDialect {}
    }

    fn functions(&self) -> &'static [FunctionDef] {
        FUNCTIONS
    }

    async fn connect(&self, url: &str, log: Logger) -> DatasourceResult<Box<dyn Datasource>> {
        Ok(Box::new(StoolapDatasource::connect(url, log).await?))
    }
}

const FUNCTIONS: &[FunctionDef] = &[
    FunctionDef {
        name: "DATE_ADD",
        signature: "DATE_ADD(ts, n, unit)",
        arity: FnArity::Args,
    },
    FunctionDef {
        name: "DATE_DIFF",
        signature: "DATE_DIFF(a, b)",
        arity: FnArity::Args,
    },
    FunctionDef {
        name: "DATE_TRUNC",
        signature: "DATE_TRUNC(unit, ts)",
        arity: FnArity::Args,
    },
    FunctionDef {
        name: "GENERATE_SERIES",
        signature: "GENERATE_SERIES(start, stop[, step])",
        arity: FnArity::Args,
    },
    FunctionDef {
        name: "GREATEST",
        signature: "GREATEST(a, b, …)",
        arity: FnArity::Args,
    },
    FunctionDef {
        name: "IFNULL",
        signature: "IFNULL(a, b)",
        arity: FnArity::Args,
    },
    FunctionDef {
        name: "IIF",
        signature: "IIF(cond, a, b)",
        arity: FnArity::Args,
    },
    FunctionDef {
        name: "JSON_EXTRACT",
        signature: "JSON_EXTRACT(j, path)",
        arity: FnArity::Args,
    },
    FunctionDef {
        name: "LEAST",
        signature: "LEAST(a, b, …)",
        arity: FnArity::Args,
    },
    FunctionDef {
        name: "MEDIAN",
        signature: "MEDIAN(expr)",
        arity: FnArity::Args,
    },
    FunctionDef {
        name: "NOW",
        signature: "NOW()",
        arity: FnArity::ZeroArgParens,
    },
    FunctionDef {
        name: "SPLIT_PART",
        signature: "SPLIT_PART(str, delim, n)",
        arity: FnArity::Args,
    },
    FunctionDef {
        name: "STRING_AGG",
        signature: "STRING_AGG(expr, sep)",
        arity: FnArity::Args,
    },
    FunctionDef {
        name: "TYPEOF",
        signature: "TYPEOF(expr)",
        arity: FnArity::Args,
    },
    FunctionDef {
        name: "VEC_DISTANCE_COSINE",
        signature: "VEC_DISTANCE_COSINE(a, b)",
        arity: FnArity::Args,
    },
    FunctionDef {
        name: "VEC_DISTANCE_L2",
        signature: "VEC_DISTANCE_L2(a, b)",
        arity: FnArity::Args,
    },
];

pub struct StoolapDatasource {
    /// Keeps the engine open for as long as the datasource lives, and
    /// serves introspection through its own auto-commit executor so the
    /// schema tree never waits behind a user statement.
    db: Arc<Database>,
    /// Executor running user statements. It carries `BEGIN … COMMIT`
    /// state across `execute()` calls, playing the role of the sqlx
    /// drivers' pinned session connection. Swapped for a fresh one on
    /// cancel and reset; dropping the old one rolls back its open
    /// transaction once any statement still running on it finishes.
    session: StdMutex<Arc<StdMutex<Executor>>>,
    /// Cancellation flag of the statement in flight, if any.
    in_flight: StdMutex<Option<CancellationHandle>>,
    log: Logger,
}

impl StoolapDatasource {
    pub async fn connect(url: &str, log: Logger) -> DatasourceResult<Self> {
        log.info(TARGET, format!("connecting to {url}"));
        let location = Location::parse(url).map_err(DatasourceError::Connect)?;
        // Opening a file database replays its WAL, which can take a while.
        let db = tokio::task::spawn_blocking(move || match location {
            Location::Memory => Database::open_in_memory(),
            Location::Dsn(dsn) => Database::open(&dsn),
        })
        .await
        .map_err(|e| DatasourceError::Connect(e.to_string()))?
        .map_err(|e| {
            log.error(TARGET, format!("connect failed: {e}"));
            DatasourceError::Connect(e.to_string())
        })?;
        log.info(TARGET, "connected");
        let session = StdMutex::new(fresh_executor(&db));
        Ok(Self {
            db: Arc::new(db),
            session,
            in_flight: StdMutex::new(None),
            log,
        })
    }

    /// Run an introspection statement on the auto-commit handle.
    async fn meta_query(&self, sql: String) -> DatasourceResult<(Vec<String>, Vec<ResultRow>)> {
        let db = Arc::clone(&self.db);
        tokio::task::spawn_blocking(move || {
            let rows = db.query(&sql, ())?;
            let columns = rows.columns().to_vec();
            let rows = rows.collect::<stoolap::Result<Vec<_>>>()?;
            Ok((columns, rows))
        })
        .await
        .map_err(|e| DatasourceError::Introspect(e.to_string()))?
        .map_err(|e: stoolap::Error| DatasourceError::Introspect(e.to_string()))
    }

    /// Give the session a fresh executor, abandoning the current one
    /// (and with it any open transaction).
    fn replace_session(&self) {
        *self.session.lock().expect("session mutex poisoned") = fresh_executor(&self.db);
    }
}

fn fresh_executor(db: &Database) -> Arc<StdMutex<Executor>> {
    Arc::new(StdMutex::new(Executor::new(Arc::clone(db.engine()))))
}

#[async_trait]
impl Datasource for StoolapDatasource {
    async fn default_schema(&self) -> DatasourceResult<DefaultSchema> {
        Ok(DefaultSchema {
            catalog: NAMESPACE.into(),
            schema: NAMESPACE.into(),
        })
    }

    async fn introspect_catalogs(&self) -> DatasourceResult<Vec<CatalogInfo>> {
        Ok(vec![CatalogInfo {
            name: NAMESPACE.into(),
        }])
    }

    async fn introspect_schemas(&self, _catalog: &str) -> DatasourceResult<Vec<SchemaInfo>> {
        Ok(vec![SchemaInfo {
            name: NAMESPACE.into(),
        }])
    }

    async fn introspect_tables(
        &self,
        _catalog: &str,
        _schema: &str,
    ) -> DatasourceResult<Vec<TableInfo>> {
        let (_, tables) = self.meta_query("SHOW TABLES".into()).await?;
        let (_, views) = self.meta_query("SHOW VIEWS".into()).await?;
        let mut out: Vec<TableInfo> = tables
            .iter()
            .filter_map(|r| r.get_by_name::<String>("table_name").ok())
            .map(|name| TableInfo {
                name,
                kind: TableKind::Table,
            })
            .chain(
                views
                    .iter()
                    .filter_map(|r| r.get_by_name::<String>("view_name").ok())
                    .map(|name| TableInfo {
                        name,
                        kind: TableKind::View,
                    }),
            )
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    async fn introspect_columns(
        &self,
        _catalog: &str,
        _schema: &str,
        table: &str,
    ) -> DatasourceResult<Vec<ColumnInfo>> {
        let quoted = quote_identifier(table);
        match self.meta_query(format!("DESCRIBE {quoted}")).await {
            Ok((_, rows)) => Ok(rows
                .iter()
                .filter_map(|r| {
                    let name: String = r.get_by_name("Field").ok()?;
                    let type_name: String = r.get_by_name("Type").unwrap_or_default();
                    let null: String = r.get_by_name("Null").unwrap_or_default();
                    Some(ColumnInfo {
                        name,
                        type_name,
                        nullable: match null.as_str() {
                            "YES" => Some(true),
                            "NO" => Some(false),
                            _ => None,
                        },
                    })
                })
                .collect()),
            // DESCRIBE only knows tables. For a view, project zero rows
            // to recover the column names (types aren't reported).
            Err(describe_err) => {
                let probe = format!("SELECT * FROM {quoted} LIMIT 0");
                let (columns, _) = self.meta_query(probe).await.map_err(|_| describe_err)?;
                Ok(columns
                    .into_iter()
                    .map(|name| ColumnInfo {
                        name,
                        type_name: String::new(),
                        nullable: None,
                    })
                    .collect())
            }
        }
    }

    async fn introspect_indices(
        &self,
        _catalog: &str,
        _schema: &str,
        table: &str,
    ) -> DatasourceResult<Vec<IndexInfo>> {
        let sql = format!("SHOW INDEXES FROM {}", quote_identifier(table));
        let (_, rows) = self.meta_query(sql).await?;
        // One row per index here, but stay defensive about engines that
        // emit one row per indexed column.
        let mut out: Vec<IndexInfo> = Vec::new();
        for r in &rows {
            let Ok(name) = r.get_by_name::<String>("index_name") else {
                continue;
            };
            if out.iter().any(|i| i.name == name) {
                continue;
            }
            let unique = r.get_by_name::<bool>("is_unique").unwrap_or(false);
            out.push(IndexInfo { name, unique });
        }
        Ok(out)
    }

    async fn execute(&self, statement: &str) -> DatasourceResult<QueryResult> {
        self.log.info(
            TARGET,
            format!("execute: {}", super::one_line_sql(statement)),
        );
        let started = Instant::now();
        let ctx = ExecutionContext::new();
        *self.in_flight.lock().expect("in_flight mutex poisoned") = Some(ctx.cancellation_handle());
        let session = Arc::clone(&self.session.lock().expect("session mutex poisoned"));
        let sql = statement.to_string();

        let outcome = tokio::task::spawn_blocking(move || {
            let executor = session.lock().expect("executor mutex poisoned");
            let mut result = executor.execute_with_context(&sql, &ctx)?;
            let columns: Vec<Column> = result
                .columns()
                .iter()
                .map(|name| Column { name: name.clone() })
                .collect();
            let mut rows: Vec<CellRow> = Vec::new();
            while result.next() {
                rows.push(result.row().iter().map(value_to_cell).collect());
            }
            if let Some(e) = result.last_error() {
                return Err(e);
            }
            Ok((columns, rows, result.rows_affected()))
        })
        .await;
        self.in_flight
            .lock()
            .expect("in_flight mutex poisoned")
            .take();

        let (columns, rows, affected) = match outcome {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => {
                self.log.error(TARGET, format!("execute failed: {e}"));
                return Err(DatasourceError::Execute(e.to_string()));
            }
            Err(join) => {
                self.log
                    .error(TARGET, format!("execute task failed: {join}"));
                return Err(DatasourceError::Execute(join.to_string()));
            }
        };
        let elapsed = started.elapsed();
        // The engine knows whether a statement produced a result set
        // (SELECT, SHOW, EXPLAIN, `… RETURNING`), so trust its column
        // list instead of classifying the SQL text.
        let result = if columns.is_empty() {
            let affected = u64::try_from(affected).unwrap_or(0);
            self.log.info(
                TARGET,
                format!("execute ok: {affected} affected in {elapsed:?}"),
            );
            QueryResult {
                columns,
                rows: Vec::new(),
                affected: Some(affected),
                elapsed,
                statements_run: 1,
            }
        } else {
            self.log.info(
                TARGET,
                format!("execute ok: {} rows in {elapsed:?}", rows.len()),
            );
            QueryResult {
                columns,
                rows,
                affected: None,
                elapsed,
                statements_run: 1,
            }
        };
        Ok(result)
    }

    async fn cancel(&self) -> DatasourceResult<()> {
        let Some(handle) = self
            .in_flight
            .lock()
            .expect("in_flight mutex poisoned")
            .take()
        else {
            return Ok(());
        };
        self.log.info(TARGET, "cancel");
        handle.cancel();
        // Stoolap only polls the flag at a few checkpoints (not inside
        // join loops), so the statement may keep running. Move the
        // session onto a fresh executor so the next statement doesn't
        // queue behind it. As on Postgres, a cancelled statement takes
        // its open transaction down with it.
        self.replace_session();
        Ok(())
    }

    async fn reset_session(&self) -> DatasourceResult<()> {
        self.replace_session();
        self.log.info(TARGET, "session reset");
        Ok(())
    }
}

/// Where a `stoolap:` URL points.
#[derive(Debug, PartialEq, Eq)]
enum Location {
    Memory,
    Dsn(String),
}

impl Location {
    fn parse(url: &str) -> Result<Self, String> {
        let rest = url.strip_prefix("stoolap:").unwrap_or(url);
        if rest == ":memory:" {
            return Ok(Self::Memory);
        }
        // `stoolap:file:///…` / `stoolap:memory://` — already a stoolap DSN.
        if let Some((scheme, _)) = rest.split_once("://")
            && !scheme.is_empty()
        {
            return Ok(Self::Dsn(rest.to_string()));
        }
        let path = rest.strip_prefix("//").unwrap_or(rest);
        if path.is_empty() {
            return Err(format!(
                "missing database path in {url:?} (try stoolap:./mydb or stoolap::memory:)"
            ));
        }
        Ok(Self::Dsn(format!("file://{path}")))
    }
}

fn value_to_cell(value: &Value) -> Cell {
    match value {
        Value::Null(_) => Cell::Null,
        Value::Integer(n) => Cell::Int(*n),
        Value::Float(f) => Cell::Float(*f),
        Value::Text(s) => Cell::Text(s.to_string()),
        Value::Boolean(b) => Cell::Bool(*b),
        Value::Timestamp(t) => Cell::Timestamp(*t),
        // JSON renders as text, matching the Postgres driver; anything
        // else (VECTOR today) keeps its type name for the grid.
        Value::Extension(_) => match value.as_json() {
            Some(json) => Cell::Text(json.to_string()),
            None => Cell::Other {
                type_name: value.data_type().to_string(),
                repr: value.to_string(),
            },
        },
    }
}

fn quote_identifier(ident: &str) -> String {
    format!("\"{}\"", ident.replace('"', "\"\""))
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn fresh() -> StoolapDatasource {
        let ds = StoolapDatasource::connect("stoolap::memory:", Logger::discard())
            .await
            .expect("connect");
        ds.execute(
            "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL, \
             score FLOAT, active BOOLEAN, meta JSON)",
        )
        .await
        .expect("create table");
        ds.execute("CREATE UNIQUE INDEX users_name_idx ON users(name)")
            .await
            .expect("create index");
        ds.execute("CREATE VIEW active_users AS SELECT id, name FROM users WHERE active")
            .await
            .expect("create view");
        ds.execute(
            "INSERT INTO users VALUES (1, 'alice', 9.5, true, '{\"k\":1}'), \
             (2, 'bob', NULL, false, NULL)",
        )
        .await
        .expect("seed");
        ds
    }

    #[test]
    fn parses_urls() {
        assert_eq!(Location::parse("stoolap::memory:"), Ok(Location::Memory));
        assert_eq!(
            Location::parse("stoolap:./data/db"),
            Ok(Location::Dsn("file://./data/db".into()))
        );
        assert_eq!(
            Location::parse("stoolap:///var/db?sync_mode=full"),
            Ok(Location::Dsn("file:///var/db?sync_mode=full".into()))
        );
        assert_eq!(
            Location::parse("stoolap:file:///var/db"),
            Ok(Location::Dsn("file:///var/db".into()))
        );
        assert!(Location::parse("stoolap:").is_err());
    }

    #[tokio::test]
    async fn introspects_full_chain() {
        let ds = fresh().await;
        assert_eq!(ds.introspect_catalogs().await.unwrap().len(), 1);
        assert_eq!(ds.introspect_schemas("main").await.unwrap().len(), 1);

        let tables = ds.introspect_tables("main", "main").await.unwrap();
        assert!(
            tables
                .iter()
                .any(|t| t.name == "users" && t.kind == TableKind::Table)
        );
        assert!(
            tables
                .iter()
                .any(|t| t.name == "active_users" && t.kind == TableKind::View)
        );

        let cols = ds
            .introspect_columns("main", "main", "users")
            .await
            .unwrap();
        let names: Vec<_> = cols.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["id", "name", "score", "active", "meta"]);
        let name_col = cols.iter().find(|c| c.name == "name").unwrap();
        assert_eq!(name_col.nullable, Some(false));
        assert!(name_col.type_name.eq_ignore_ascii_case("text"));

        let view_cols = ds
            .introspect_columns("main", "main", "active_users")
            .await
            .unwrap();
        let names: Vec<_> = view_cols.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["id", "name"]);

        let indices = ds
            .introspect_indices("main", "main", "users")
            .await
            .unwrap();
        assert!(
            indices
                .iter()
                .any(|i| i.name == "users_name_idx" && i.unique)
        );
    }

    #[tokio::test]
    async fn decodes_cells() {
        let ds = fresh().await;
        let r = ds
            .execute("SELECT id, name, score, active, meta FROM users ORDER BY id")
            .await
            .unwrap();
        assert_eq!(r.affected, None);
        assert_eq!(r.columns.len(), 5);
        assert!(matches!(r.rows[0][0], Cell::Int(1)));
        assert!(matches!(&r.rows[0][1], Cell::Text(s) if s == "alice"));
        assert!(matches!(r.rows[0][2], Cell::Float(f) if f == 9.5));
        assert!(matches!(r.rows[0][3], Cell::Bool(true)));
        assert!(matches!(&r.rows[0][4], Cell::Text(s) if s.contains("\"k\"")));
        assert!(r.rows[1][2].is_null());
        assert!(r.rows[1][4].is_null());
    }

    #[tokio::test]
    async fn empty_select_keeps_columns_and_dml_reports_affected() {
        let ds = fresh().await;
        let r = ds
            .execute("SELECT * FROM users WHERE id = 99")
            .await
            .unwrap();
        assert_eq!(r.columns.len(), 5);
        assert!(r.rows.is_empty());
        assert_eq!(r.affected, None);

        let r = ds.execute("UPDATE users SET score = 1").await.unwrap();
        assert!(r.columns.is_empty());
        assert_eq!(r.affected, Some(2));

        let r = ds
            .execute("INSERT INTO users (id, name) VALUES (3, 'carol') RETURNING id")
            .await
            .unwrap();
        assert_eq!(r.rows.len(), 1);
    }

    #[tokio::test]
    async fn transaction_spans_execute_calls() {
        let ds = fresh().await;
        ds.execute("BEGIN").await.unwrap();
        ds.execute("DELETE FROM users").await.unwrap();
        // Introspection runs on its own auto-commit executor and must
        // not see the uncommitted delete.
        let (_, rows) = ds
            .meta_query("SELECT COUNT(*) AS n FROM users".into())
            .await
            .unwrap();
        assert_eq!(rows[0].get_by_name::<i64>("n").unwrap(), 2);
        ds.execute("ROLLBACK").await.unwrap();
        let r = ds.execute("SELECT COUNT(*) FROM users").await.unwrap();
        assert!(matches!(r.rows[0][0], Cell::Int(2)));
    }

    #[tokio::test]
    async fn reset_session_rolls_back_open_transaction() {
        let ds = fresh().await;
        ds.execute("BEGIN").await.unwrap();
        ds.execute("DELETE FROM users").await.unwrap();
        ds.reset_session().await.unwrap();
        let r = ds.execute("SELECT COUNT(*) FROM users").await.unwrap();
        assert!(matches!(r.rows[0][0], Cell::Int(2)));
    }

    #[tokio::test]
    async fn file_database_persists_across_reconnects() {
        let dir = std::env::temp_dir().join(format!("rowdy-stoolap-{}", uuid::Uuid::new_v4()));
        let url = format!("stoolap:{}", dir.display());
        {
            let ds = StoolapDatasource::connect(&url, Logger::discard())
                .await
                .expect("create");
            ds.execute("CREATE TABLE t (n INTEGER)").await.unwrap();
            ds.execute("INSERT INTO t VALUES (42)").await.unwrap();
        }
        // Dropping the datasource must release the directory lock so the
        // same process can reopen it.
        let ds = StoolapDatasource::connect(&url, Logger::discard())
            .await
            .expect("reopen");
        let r = ds.execute("SELECT n FROM t").await.unwrap();
        assert!(matches!(r.rows[0][0], Cell::Int(42)));
        drop(ds);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn errors_surface_as_execute_errors() {
        let ds = fresh().await;
        let err = ds.execute("SELECT * FROM nope").await.unwrap_err();
        assert!(matches!(err, DatasourceError::Execute(_)), "{err}");
    }
}
