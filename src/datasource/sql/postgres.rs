use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, Utc};
use serde_json::Value as JsonValue;
use sqlx::pool::PoolConnection;
use sqlx::postgres::{PgPool, PgPoolOptions, PgRow, PgTypeKind, Postgres};
use sqlx::{Column as _, Row, TypeInfo};
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::datasource::cell::Cell;
use crate::datasource::error::{DatasourceError, DatasourceResult};
use crate::datasource::schema::{
    CatalogInfo, ColumnInfo, DefaultSchema, IndexInfo, SchemaInfo, TableInfo, TableKind,
};
use crate::datasource::sql::{TxEffect, decode_to, tx_effect};
use crate::datasource::{Column, Datasource, QueryResult, Row as CellRow};
use crate::log::Logger;

const DEFAULT_POOL_SIZE: u32 = 3;
const IDLE_TIMEOUT_SECS: u64 = 60;
const TARGET: &str = "postgres";

/// Pinned session connection held only while the user is inside an
/// open transaction. Released back to the pool the moment the
/// transaction closes (and reacquired on the next statement). Outside
/// a transaction the datasource owns no connection — the pool can
/// shrink to zero and idle conns get reaped after `IDLE_TIMEOUT_SECS`.
/// The backend PID is mirrored to `PostgresDatasource::session_pid` so
/// `cancel()` can read it without locking the session.
struct Session {
    conn: PoolConnection<Postgres>,
}

pub struct PostgresDatasource {
    /// The pool itself behind a sync mutex so `:reset` can swap it
    /// (close-and-rebuild) without making every `execute` / introspect
    /// caller take an async lock. `PgPool` is internally `Arc<...>` so
    /// cloning out is O(1); we never hold the guard across `.await`.
    pool: StdMutex<PgPool>,
    /// Connection URL kept around so `:reset` can rebuild the pool from
    /// scratch.
    url: String,
    log: Logger,
    // Backend PID of the pinned session connection, or 0 when no session
    // is currently held. Recorded once when the session is acquired and
    // kept across executes so `cancel()` can target the exact backend
    // running the user's queries — and so it can target an idle session
    // in a transaction (callers can hit `pg_cancel_backend` to break out
    // of a stuck wait without first finding a running statement).
    session_pid: AtomicI32,
    /// Pinned connection held only while a transaction is open. Outside
    /// a tx the slot is `None` and every `execute()` lands on a fresh
    /// pool checkout that's released as soon as the statement finishes.
    /// Introspection and `cancel()` always talk to the pool, never to
    /// this connection — they need to make progress while the session
    /// is busy.
    session: Mutex<Option<Session>>,
}

fn build_pool(url: &str) -> Result<PgPool, sqlx::Error> {
    PgPoolOptions::new()
        .max_connections(DEFAULT_POOL_SIZE)
        .min_connections(0)
        .idle_timeout(Some(Duration::from_secs(IDLE_TIMEOUT_SECS)))
        .connect_lazy(url)
}

impl PostgresDatasource {
    pub async fn connect(url: &str, log: Logger) -> DatasourceResult<Self> {
        log.info(TARGET, format!("connecting to {}", super::redact_url(url)));
        let pool = build_pool(url).map_err(|e| {
            log.error(TARGET, format!("pool build failed: {e}"));
            DatasourceError::Connect(e.to_string())
        })?;
        // Verify connectivity up-front by checking out one conn and
        // releasing it back to the pool. With `min_connections(0)` +
        // `idle_timeout`, the conn drops shortly after; bad URLs still
        // fail here instead of waiting until the first user query.
        let verify_conn = pool.acquire().await.map_err(|e| {
            log.error(TARGET, format!("connect verify failed: {e}"));
            DatasourceError::Connect(e.to_string())
        })?;
        drop(verify_conn);
        log.info(TARGET, "connected");
        Ok(Self {
            pool: StdMutex::new(pool),
            url: url.to_string(),
            log,
            session_pid: AtomicI32::new(0),
            session: Mutex::new(None),
        })
    }

    /// Cheap clone of the current pool handle (`PgPool` is `Arc`-backed).
    /// Never holds the std mutex across an `.await`.
    fn pool(&self) -> PgPool {
        self.pool.lock().expect("pool mutex poisoned").clone()
    }
}

#[async_trait]
impl Datasource for PostgresDatasource {
    async fn default_schema(&self) -> DatasourceResult<DefaultSchema> {
        // `current_schemas(false)` returns the search_path with implicit
        // entries (pg_catalog) excluded; the first element is "where new
        // unqualified objects land" — what users mean by "default schema".
        // `public` is the fallback if the search_path is empty (rare but
        // possible after `SET search_path = ''`).
        let pool = self.pool();
        let row = sqlx::query(
            "SELECT current_database() AS catalog, \
                    COALESCE((current_schemas(false))[1], 'public') AS schema",
        )
        .fetch_one(&pool)
        .await
        .map_err(introspect_err)?;
        let catalog: String = row.try_get("catalog").map_err(introspect_err)?;
        let schema: String = row.try_get("schema").map_err(introspect_err)?;
        Ok(DefaultSchema { catalog, schema })
    }

    async fn introspect_catalogs(&self) -> DatasourceResult<Vec<CatalogInfo>> {
        // A Postgres connection is bound to a single database; expose it as the
        // sole catalog so the tree mirrors the rest of the drivers.
        let pool = self.pool();
        let row = sqlx::query("SELECT current_database() AS name")
            .fetch_one(&pool)
            .await
            .map_err(introspect_err)?;
        let name: String = row.try_get("name").map_err(introspect_err)?;
        Ok(vec![CatalogInfo { name }])
    }

    async fn introspect_schemas(&self, _catalog: &str) -> DatasourceResult<Vec<SchemaInfo>> {
        let pool = self.pool();
        let rows = sqlx::query(
            "SELECT nspname AS name FROM pg_namespace \
             WHERE nspname NOT IN ('pg_catalog', 'information_schema', 'pg_toast') \
               AND nspname NOT LIKE 'pg_temp_%' \
               AND nspname NOT LIKE 'pg_toast_temp_%' \
             ORDER BY nspname",
        )
        .fetch_all(&pool)
        .await
        .map_err(introspect_err)?;
        Ok(rows
            .into_iter()
            .filter_map(|r| r.try_get::<String, _>("name").ok())
            .map(|name| SchemaInfo { name })
            .collect())
    }

    async fn introspect_tables(
        &self,
        catalog: &str,
        schema: &str,
    ) -> DatasourceResult<Vec<TableInfo>> {
        let pool = self.pool();
        let rows = sqlx::query(
            "SELECT table_name AS name, table_type AS kind \
             FROM information_schema.tables \
             WHERE table_catalog = $1 AND table_schema = $2 \
             ORDER BY table_name",
        )
        .bind(catalog)
        .bind(schema)
        .fetch_all(&pool)
        .await
        .map_err(introspect_err)?;
        Ok(rows
            .into_iter()
            .filter_map(|r| {
                let name: String = r.try_get("name").ok()?;
                let kind_str: String = r.try_get("kind").ok()?;
                let kind = match kind_str.as_str() {
                    "VIEW" => TableKind::View,
                    _ => TableKind::Table,
                };
                Some(TableInfo { name, kind })
            })
            .collect())
    }

    async fn introspect_columns(
        &self,
        catalog: &str,
        schema: &str,
        table: &str,
    ) -> DatasourceResult<Vec<ColumnInfo>> {
        let pool = self.pool();
        let rows = sqlx::query(
            "SELECT column_name AS name, data_type AS type_name, is_nullable \
             FROM information_schema.columns \
             WHERE table_catalog = $1 AND table_schema = $2 AND table_name = $3 \
             ORDER BY ordinal_position",
        )
        .bind(catalog)
        .bind(schema)
        .bind(table)
        .fetch_all(&pool)
        .await
        .map_err(introspect_err)?;
        Ok(rows
            .into_iter()
            .filter_map(|r| {
                let name: String = r.try_get("name").ok()?;
                let type_name: String = r.try_get("type_name").ok().unwrap_or_default();
                let is_nullable: String = r.try_get("is_nullable").ok().unwrap_or_default();
                let nullable = match is_nullable.as_str() {
                    "YES" => Some(true),
                    "NO" => Some(false),
                    _ => None,
                };
                Some(ColumnInfo {
                    name,
                    type_name,
                    nullable,
                })
            })
            .collect())
    }

    async fn introspect_indices(
        &self,
        _catalog: &str,
        schema: &str,
        table: &str,
    ) -> DatasourceResult<Vec<IndexInfo>> {
        // pg_indexes doesn't expose `indisunique`, so we walk pg_class/pg_index
        // directly to get the uniqueness flag in a single round-trip.
        let pool = self.pool();
        let rows = sqlx::query(
            "SELECT i.relname AS name, ix.indisunique AS is_unique \
             FROM pg_class i \
             JOIN pg_index ix ON i.oid = ix.indexrelid \
             JOIN pg_class t ON ix.indrelid = t.oid \
             JOIN pg_namespace n ON t.relnamespace = n.oid \
             WHERE n.nspname = $1 AND t.relname = $2 \
             ORDER BY i.relname",
        )
        .bind(schema)
        .bind(table)
        .fetch_all(&pool)
        .await
        .map_err(introspect_err)?;
        Ok(rows
            .into_iter()
            .filter_map(|r| {
                let name: String = r.try_get("name").ok()?;
                let unique: bool = r.try_get("is_unique").ok().unwrap_or(false);
                Some(IndexInfo { name, unique })
            })
            .collect())
    }

    async fn execute(&self, statement: &str) -> DatasourceResult<QueryResult> {
        self.log.info(
            TARGET,
            format!("execute: {}", super::one_line_sql(statement)),
        );
        let started = Instant::now();

        let mut guard = self.session.lock().await;
        let was_in_tx = guard.is_some();
        if !was_in_tx {
            let pool = self.pool();
            let mut conn = pool.acquire().await.map_err(|e| {
                self.log.error(TARGET, format!("acquire failed: {e}"));
                execute_err(e)
            })?;
            let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
                .fetch_one(&mut *conn)
                .await
                .map_err(|e| {
                    self.log
                        .error(TARGET, format!("backend pid fetch failed: {e}"));
                    execute_err(e)
                })?;
            self.session_pid.store(pid, Ordering::SeqCst);
            *guard = Some(Session { conn });
        }
        let session = guard.as_mut().expect("session populated above");

        let outcome: Result<QueryResult, sqlx::Error> =
            if super::is_row_returning(statement, &sqlparser::dialect::PostgreSqlDialect {}) {
                match sqlx::query(statement).fetch_all(&mut *session.conn).await {
                    Ok(rows) => {
                        let elapsed = started.elapsed();
                        let columns = build_columns(&rows);
                        let rows: Vec<CellRow> = rows
                            .iter()
                            .map(|r| row_to_cells(r, columns.len()))
                            .collect();
                        Ok(QueryResult {
                            columns,
                            rows,
                            affected: None,
                            elapsed,
                            statements_run: 1,
                        })
                    }
                    Err(e) => Err(e),
                }
            } else {
                match sqlx::query(statement).execute(&mut *session.conn).await {
                    Ok(outcome) => {
                        let elapsed = started.elapsed();
                        Ok(QueryResult {
                            columns: Vec::new(),
                            rows: Vec::new(),
                            affected: Some(outcome.rows_affected()),
                            elapsed,
                            statements_run: 1,
                        })
                    }
                    Err(e) => Err(e),
                }
            };

        // Compute the resulting in-tx flag and decide whether to keep
        // the pinned conn. Keeping it across an open tx is mandatory;
        // releasing it when no tx is open is what lets the pool shrink
        // back to zero and reap idle conns after `IDLE_TIMEOUT_SECS`.
        let effect = tx_effect(statement, &sqlparser::dialect::PostgreSqlDialect {});
        let new_in_tx = match (was_in_tx, effect) {
            (true, TxEffect::End) => false,
            (true, _) => true,
            (false, TxEffect::Begin) => true,
            (false, _) => false,
        };

        match outcome {
            Ok(r) => {
                self.log.info(
                    TARGET,
                    match r.affected {
                        Some(n) => format!("execute ok: {n} affected in {:?}", r.elapsed),
                        None => format!("execute ok: {} rows in {:?}", r.rows.len(), r.elapsed),
                    },
                );
                if !new_in_tx {
                    *guard = None;
                    self.session_pid.store(0, Ordering::SeqCst);
                }
                Ok(r)
            }
            Err(e) => {
                self.log.error(TARGET, format!("execute failed: {e}"));
                if super::is_connection_lost(&e) {
                    *guard = None;
                    self.session_pid.store(0, Ordering::SeqCst);
                    self.log
                        .warn(TARGET, "session conn dropped after connection loss");
                } else if !was_in_tx {
                    // No tx to preserve — failure means the session we
                    // just acquired serves no purpose. Release it so
                    // the pool can reap the conn.
                    *guard = None;
                    self.session_pid.store(0, Ordering::SeqCst);
                }
                Err(execute_err(e))
            }
        }
    }

    async fn cancel(&self) -> DatasourceResult<()> {
        let pid = self.session_pid.load(Ordering::SeqCst);
        if pid == 0 {
            self.log.info(TARGET, "cancel: no active session");
            return Ok(());
        }
        self.log
            .info(TARGET, format!("cancel: pg_cancel_backend({pid})"));
        // A separate pool connection is used so the cancel doesn't wait on
        // the busy backend. `pg_cancel_backend` returns false if the target
        // PID is no longer running anything — best-effort by design.
        let pool = self.pool();
        let signaled: bool = sqlx::query_scalar("SELECT pg_cancel_backend($1)")
            .bind(pid)
            .fetch_one(&pool)
            .await
            .map_err(|e| {
                self.log.warn(TARGET, format!("cancel failed: {e}"));
                execute_err(e)
            })?;
        if !signaled {
            self.log
                .warn(TARGET, format!("pg_cancel_backend({pid}) returned false"));
        }
        Ok(())
    }

    async fn reset_session(&self) -> DatasourceResult<()> {
        // Drop the pinned session conn first (with an explicit ROLLBACK
        // so the backend isn't left holding an aborted tx).
        let mut guard = self.session.lock().await;
        if let Some(mut session) = guard.take() {
            if let Err(e) = sqlx::query("ROLLBACK").execute(&mut *session.conn).await {
                self.log.warn(TARGET, format!("reset rollback: {e}"));
            }
            drop(session);
            self.log.info(TARGET, "session reset");
        }
        self.session_pid.store(0, Ordering::SeqCst);
        drop(guard);

        // Force-close all pool connections by swapping in a fresh lazy
        // pool. Closing the old pool waits for in-flight queries to
        // return their conns, then disconnects them — equivalent to a
        // hard reset of every backend this datasource was talking to.
        let new_pool = match build_pool(&self.url) {
            Ok(p) => p,
            Err(e) => {
                self.log.error(TARGET, format!("pool rebuild failed: {e}"));
                return Err(DatasourceError::Execute(e.to_string()));
            }
        };
        let old_pool = {
            let mut pool_guard = self.pool.lock().expect("pool mutex poisoned");
            std::mem::replace(&mut *pool_guard, new_pool)
        };
        old_pool.close().await;
        self.log.info(TARGET, "pool drained");
        Ok(())
    }
}

fn build_columns(rows: &[PgRow]) -> Vec<Column> {
    let Some(first) = rows.first() else {
        return Vec::new();
    };
    first
        .columns()
        .iter()
        .map(|col| Column {
            name: col.name().to_string(),
        })
        .collect()
}

fn row_to_cells(row: &PgRow, n: usize) -> CellRow {
    (0..n).map(|i| decode_cell(row, i)).collect()
}

fn decode_cell(row: &PgRow, idx: usize) -> Cell {
    let column = &row.columns()[idx];
    let type_info = column.type_info();
    let type_name = type_info.name().to_string();
    // User-defined ENUM types arrive as `PgType::Custom` with the enum's own
    // name (e.g. `"FlowExecutionStatus"`), so they fall through `decode_typed`.
    // Postgres ships the variant name as plain UTF-8 on the wire, but
    // `try_get::<String>` rejects it because `String`'s `compatible()` only
    // accepts the built-in text OIDs. `try_get_unchecked` skips that gate.
    if matches!(type_info.kind(), PgTypeKind::Enum(_))
        && let Ok(opt) = row.try_get_unchecked::<Option<String>, _>(idx)
    {
        return opt.map(Cell::Text).unwrap_or(Cell::Null);
    }
    if let Some(cell) = decode_typed(row, idx, &type_name) {
        return cell;
    }
    if let Some(cell) = decode_fallback(row, idx) {
        return cell;
    }
    Cell::Other {
        type_name,
        repr: String::new(),
    }
}

fn decode_fallback(row: &PgRow, idx: usize) -> Option<Cell> {
    if let Some(opt) = decode_or_null::<sqlx::types::Json<JsonValue>>(row, idx) {
        return Some(
            opt.map(|w| Cell::Text(w.0.to_string()))
                .unwrap_or(Cell::Null),
        );
    }
    if let Some(opt) = decode_or_null::<String>(row, idx) {
        return Some(opt.map(Cell::Text).unwrap_or(Cell::Null));
    }
    if let Some(opt) = decode_or_null::<Vec<u8>>(row, idx) {
        return Some(opt.map(Cell::Bytes).unwrap_or(Cell::Null));
    }
    None
}

fn decode_typed(row: &PgRow, idx: usize, type_name: &str) -> Option<Cell> {
    if let Some(inner) = type_name.strip_suffix("[]") {
        return decode_array(row, idx, type_name, inner);
    }
    match type_name {
        "BOOL" => decode_to!(row, idx, bool => Cell::Bool),
        "INT2" | "SMALLINT" => decode_to!(row, idx, i16 => |v| Cell::Int(v as i64)),
        "INT4" | "INT" | "INTEGER" => decode_to!(row, idx, i32 => |v| Cell::Int(v as i64)),
        "INT8" | "BIGINT" => decode_to!(row, idx, i64 => Cell::Int),
        "FLOAT4" | "REAL" => decode_to!(row, idx, f32 => |v| Cell::Float(v as f64)),
        "FLOAT8" | "DOUBLE PRECISION" => decode_to!(row, idx, f64 => Cell::Float),
        "NUMERIC" => {
            decode_to!(row, idx, sqlx::types::BigDecimal => |v| Cell::Decimal(v.to_string()))
        }
        "TEXT" | "VARCHAR" | "CHAR" | "BPCHAR" | "NAME" | "CITEXT" => {
            decode_to!(row, idx, String => Cell::Text)
        }
        "BYTEA" => decode_to!(row, idx, Vec<u8> => Cell::Bytes),
        "TIMESTAMPTZ" => decode_to!(row, idx, DateTime<Utc> => Cell::Timestamp),
        "TIMESTAMP" => decode_to!(row, idx, NaiveDateTime => |v| Cell::Text(v.to_string())),
        "DATE" => decode_to!(row, idx, NaiveDate => Cell::Date),
        "TIME" => decode_to!(row, idx, NaiveTime => Cell::Time),
        "UUID" => decode_to!(row, idx, Uuid => Cell::Uuid),
        "JSON" | "JSONB" => {
            decode_to!(row, idx, sqlx::types::Json<JsonValue> => |w| Cell::Text(w.0.to_string()))
        }
        _ => None,
    }
}

fn decode_or_null<'r, T>(row: &'r PgRow, idx: usize) -> Option<Option<T>>
where
    T: sqlx::Decode<'r, sqlx::Postgres> + sqlx::Type<sqlx::Postgres>,
{
    row.try_get::<Option<T>, _>(idx).ok()
}

/// Postgres exposes arrays with a `[]` suffix on the type name (e.g.
/// `JSONB[]`). Strip it, decode `Vec<Option<T>>` for the inner type, and
/// render as a JSON-shaped literal so the TUI shows something useful.
fn decode_array(row: &PgRow, idx: usize, type_name: &str, inner: &str) -> Option<Cell> {
    match inner {
        "BOOL" => format_array::<bool, _>(row, idx, type_name, |v| v.to_string()),
        "INT2" | "SMALLINT" => format_array::<i16, _>(row, idx, type_name, |v| v.to_string()),
        "INT4" | "INT" | "INTEGER" => {
            format_array::<i32, _>(row, idx, type_name, |v| v.to_string())
        }
        "INT8" | "BIGINT" => format_array::<i64, _>(row, idx, type_name, |v| v.to_string()),
        "FLOAT4" | "REAL" => format_array::<f32, _>(row, idx, type_name, |v| v.to_string()),
        "FLOAT8" | "DOUBLE PRECISION" => {
            format_array::<f64, _>(row, idx, type_name, |v| v.to_string())
        }
        "TEXT" | "VARCHAR" | "CHAR" | "BPCHAR" | "NAME" | "CITEXT" => {
            format_array::<String, _>(row, idx, type_name, json_string)
        }
        "UUID" => format_array::<Uuid, _>(row, idx, type_name, |v| json_string(v.to_string())),
        "JSON" | "JSONB" => {
            format_array::<sqlx::types::Json<JsonValue>, _>(row, idx, type_name, |w| {
                w.0.to_string()
            })
        }
        _ => None,
    }
}

fn format_array<'r, T, F>(row: &'r PgRow, idx: usize, type_name: &str, fmt: F) -> Option<Cell>
where
    T: sqlx::Decode<'r, sqlx::Postgres> + sqlx::Type<sqlx::Postgres>,
    F: Fn(T) -> String,
    Vec<Option<T>>: sqlx::Decode<'r, sqlx::Postgres> + sqlx::Type<sqlx::Postgres>,
{
    let opt: Option<Vec<Option<T>>> = row.try_get::<Option<Vec<Option<T>>>, _>(idx).ok()?;
    Some(match opt {
        None => Cell::Null,
        Some(items) => {
            let parts: Vec<String> = items
                .into_iter()
                .map(|o| o.map(&fmt).unwrap_or_else(|| "null".to_string()))
                .collect();
            Cell::Other {
                type_name: type_name.to_string(),
                repr: format!("[{}]", parts.join(", ")),
            }
        }
    })
}

/// JSON-encodes a string (handles quoting and escaping). Used so array
/// elements render as valid JSON literals.
fn json_string(s: String) -> String {
    serde_json::Value::String(s).to_string()
}

fn introspect_err(err: sqlx::Error) -> DatasourceError {
    DatasourceError::Introspect(err.to_string())
}

fn execute_err(err: sqlx::Error) -> DatasourceError {
    DatasourceError::Execute(err.to_string())
}

#[cfg(test)]
mod tests {
    //! Integration tests against a live Postgres. Gated by the
    //! `ROWDY_POSTGRES_URL` environment variable — when unset the test
    //! prints a skip notice and returns Ok, so `cargo test` stays green
    //! on machines without a database. See `compose.yaml` for a one-shot
    //! local setup.
    use super::*;

    fn url() -> Option<String> {
        std::env::var("ROWDY_POSTGRES_URL")
            .ok()
            .filter(|s| !s.is_empty())
    }

    fn unique_table() -> String {
        let id = uuid::Uuid::new_v4().simple().to_string();
        format!("rowdy_test_{}", &id[..16])
    }

    #[tokio::test]
    async fn connect_query_and_introspect() {
        let Some(url) = url() else {
            eprintln!("ROWDY_POSTGRES_URL not set; skipping postgres integration test");
            return;
        };
        let ds = PostgresDatasource::connect(&url, Logger::discard())
            .await
            .expect("connect");
        let table = unique_table();

        ds.execute(&format!("DROP TABLE IF EXISTS {table}"))
            .await
            .expect("pre-clean");
        ds.execute(&format!(
            "CREATE TABLE {table} (id INTEGER PRIMARY KEY, name TEXT NOT NULL, score NUMERIC(10,2))"
        ))
        .await
        .expect("create");
        ds.execute(&format!(
            "INSERT INTO {table}(id, name, score) VALUES (1, 'alice', 9.5), (2, 'bob', NULL)"
        ))
        .await
        .expect("insert");

        let result = ds
            .execute(&format!("SELECT id, name, score FROM {table} ORDER BY id"))
            .await
            .expect("select");
        assert_eq!(result.columns.len(), 3);
        assert_eq!(result.rows.len(), 2);
        assert!(matches!(result.rows[0][0], Cell::Int(1)));
        assert!(matches!(&result.rows[0][1], Cell::Text(s) if s == "alice"));
        match &result.rows[0][2] {
            Cell::Decimal(s) => {
                let v: f64 = s.parse().expect("decimal parses as f64");
                assert!((v - 9.5).abs() < 1e-9, "score = {s}");
            }
            other => panic!("expected Decimal, got {other:?}"),
        }
        assert!(result.rows[1][2].is_null());

        let catalogs = ds.introspect_catalogs().await.expect("catalogs");
        assert_eq!(catalogs.len(), 1, "postgres exposes one catalog");
        let catalog = &catalogs[0].name;
        let schemas = ds.introspect_schemas(catalog).await.expect("schemas");
        assert!(schemas.iter().any(|s| s.name == "public"));
        let tables = ds
            .introspect_tables(catalog, "public")
            .await
            .expect("tables");
        assert!(
            tables.iter().any(|t| t.name == table),
            "table {table:?} not found in: {tables:?}"
        );
        let cols = ds
            .introspect_columns(catalog, "public", &table)
            .await
            .expect("columns");
        let names: Vec<_> = cols.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["id", "name", "score"]);

        ds.execute(&format!("DROP TABLE {table}"))
            .await
            .expect("drop");
    }

    #[tokio::test]
    async fn enum_values_decoded_as_text() {
        let Some(url) = url() else {
            eprintln!("ROWDY_POSTGRES_URL not set; skipping postgres integration test");
            return;
        };
        let ds = PostgresDatasource::connect(&url, Logger::discard())
            .await
            .expect("connect");
        let table = unique_table();
        let enum_type = format!("{table}_status");

        ds.execute(&format!("DROP TABLE IF EXISTS {table}"))
            .await
            .expect("pre-clean");
        ds.execute(&format!("DROP TYPE IF EXISTS {enum_type}"))
            .await
            .expect("pre-clean enum");
        ds.execute(&format!(
            "CREATE TYPE {enum_type} AS ENUM ('created', 'running', 'completed', 'failed')"
        ))
        .await
        .expect("create enum type");
        ds.execute(&format!(
            "CREATE TABLE {table} (id INTEGER PRIMARY KEY, status {enum_type} NOT NULL)"
        ))
        .await
        .expect("create table with enum");
        ds.execute(&format!(
            "INSERT INTO {table}(id, status) VALUES (1, 'running'), (2, 'completed')"
        ))
        .await
        .expect("insert enum values");

        let result = ds
            .execute(&format!("SELECT id, status FROM {table} ORDER BY id"))
            .await
            .expect("select enum");
        assert_eq!(result.columns.len(), 2);
        assert_eq!(result.rows.len(), 2);

        assert!(matches!(result.rows[0][0], Cell::Int(1)));
        assert!(matches!(&result.rows[0][1], Cell::Text(s) if s == "running"));
        assert!(matches!(result.rows[1][0], Cell::Int(2)));
        assert!(matches!(&result.rows[1][1], Cell::Text(s) if s == "completed"));

        ds.execute(&format!("DROP TABLE {table}"))
            .await
            .expect("drop table");
        ds.execute(&format!("DROP TYPE {enum_type}"))
            .await
            .expect("drop enum type");
    }
}
