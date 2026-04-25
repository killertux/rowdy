use std::sync::atomic::{AtomicI32, Ordering};
use std::time::Instant;

use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, Utc};
use serde_json::Value as JsonValue;
use sqlx::postgres::{PgPool, PgPoolOptions, PgRow};
use sqlx::{Column as _, Row, TypeInfo};
use uuid::Uuid;

use crate::datasource::cell::Cell;
use crate::datasource::error::{DatasourceError, DatasourceResult};
use crate::datasource::schema::{
    CatalogInfo, ColumnInfo, IndexInfo, SchemaInfo, TableInfo, TableKind,
};
use crate::datasource::{Column, Datasource, QueryResult, Row as CellRow};
use crate::log::Logger;

const DEFAULT_POOL_SIZE: u32 = 3;
const TARGET: &str = "postgres";

pub struct PostgresDatasource {
    pool: PgPool,
    log: Logger,
    // Backend PID of the currently running `execute()`, or 0 when nothing is
    // in flight. PostgreSQL backend PIDs are positive int4, so 0 is a safe
    // "none" sentinel. Read by `cancel()` to issue `pg_cancel_backend`.
    in_flight_pid: AtomicI32,
}

impl PostgresDatasource {
    pub async fn connect(url: &str, log: Logger) -> DatasourceResult<Self> {
        log.info(TARGET, format!("connecting to {}", super::redact_url(url)));
        let pool = PgPoolOptions::new()
            .max_connections(DEFAULT_POOL_SIZE)
            .connect(url)
            .await
            .map_err(|e| {
                log.error(TARGET, format!("connect failed: {e}"));
                DatasourceError::Connect(e.to_string())
            })?;
        log.info(TARGET, "connected");
        Ok(Self {
            pool,
            log,
            in_flight_pid: AtomicI32::new(0),
        })
    }
}

#[async_trait]
impl Datasource for PostgresDatasource {
    async fn introspect_catalogs(&self) -> DatasourceResult<Vec<CatalogInfo>> {
        // A Postgres connection is bound to a single database; expose it as the
        // sole catalog so the tree mirrors the rest of the drivers.
        let row = sqlx::query("SELECT current_database() AS name")
            .fetch_one(&self.pool)
            .await
            .map_err(introspect_err)?;
        let name: String = row.try_get("name").map_err(introspect_err)?;
        Ok(vec![CatalogInfo { name }])
    }

    async fn introspect_schemas(&self, _catalog: &str) -> DatasourceResult<Vec<SchemaInfo>> {
        let rows = sqlx::query(
            "SELECT nspname AS name FROM pg_namespace \
             WHERE nspname NOT IN ('pg_catalog', 'information_schema', 'pg_toast') \
               AND nspname NOT LIKE 'pg_temp_%' \
               AND nspname NOT LIKE 'pg_toast_temp_%' \
             ORDER BY nspname",
        )
        .fetch_all(&self.pool)
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
        let rows = sqlx::query(
            "SELECT table_name AS name, table_type AS kind \
             FROM information_schema.tables \
             WHERE table_catalog = $1 AND table_schema = $2 \
             ORDER BY table_name",
        )
        .bind(catalog)
        .bind(schema)
        .fetch_all(&self.pool)
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
        let rows = sqlx::query(
            "SELECT column_name AS name, data_type AS type_name, is_nullable \
             FROM information_schema.columns \
             WHERE table_catalog = $1 AND table_schema = $2 AND table_name = $3 \
             ORDER BY ordinal_position",
        )
        .bind(catalog)
        .bind(schema)
        .bind(table)
        .fetch_all(&self.pool)
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
        .fetch_all(&self.pool)
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

        // Pin a single connection to this query so `cancel()` can target the
        // exact backend that's running it. The connection returns to the pool
        // when `conn` drops (including on a `JoinHandle::abort`).
        let mut conn = self.pool.acquire().await.map_err(|e| {
            self.log.error(TARGET, format!("acquire failed: {e}"));
            execute_err(e)
        })?;
        let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *conn)
            .await
            .map_err(|e| {
                self.log.error(TARGET, format!("backend pid fetch failed: {e}"));
                execute_err(e)
            })?;
        self.in_flight_pid.store(pid, Ordering::SeqCst);

        if super::is_row_returning(statement) {
            let result = sqlx::query(statement).fetch_all(&mut *conn).await;
            // Clear the pid before checking the result so a subsequent cancel
            // doesn't try to kill a backend that's already free. On abort,
            // this line never runs and `cancel()` reads the still-set pid.
            self.in_flight_pid.store(0, Ordering::SeqCst);
            let rows = result.map_err(|e| {
                self.log.error(TARGET, format!("execute failed: {e}"));
                execute_err(e)
            })?;
            let elapsed = started.elapsed();
            let columns = build_columns(&rows);
            let rows: Vec<CellRow> = rows
                .iter()
                .map(|r| row_to_cells(r, columns.len()))
                .collect();
            self.log.info(
                TARGET,
                format!("execute ok: {} rows in {:?}", rows.len(), elapsed),
            );
            Ok(QueryResult {
                columns,
                rows,
                affected: None,
                elapsed,
            })
        } else {
            let result = sqlx::query(statement).execute(&mut *conn).await;
            self.in_flight_pid.store(0, Ordering::SeqCst);
            let outcome = result.map_err(|e| {
                self.log.error(TARGET, format!("execute failed: {e}"));
                execute_err(e)
            })?;
            let elapsed = started.elapsed();
            let affected = outcome.rows_affected();
            self.log.info(
                TARGET,
                format!("execute ok: {affected} affected in {elapsed:?}"),
            );
            Ok(QueryResult {
                columns: Vec::new(),
                rows: Vec::new(),
                affected: Some(affected),
                elapsed,
            })
        }
    }

    async fn cancel(&self) -> DatasourceResult<()> {
        let pid = self.in_flight_pid.swap(0, Ordering::SeqCst);
        if pid == 0 {
            self.log.info(TARGET, "cancel: no in-flight query");
            return Ok(());
        }
        self.log
            .info(TARGET, format!("cancel: pg_cancel_backend({pid})"));
        // A separate pool connection is used so the cancel doesn't wait on
        // the busy backend. `pg_cancel_backend` returns false if the target
        // PID is no longer running anything — best-effort by design.
        let signaled: bool = sqlx::query_scalar("SELECT pg_cancel_backend($1)")
            .bind(pid)
            .fetch_one(&self.pool)
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
    let type_name = column.type_info().name().to_string();
    if let Some(cell) = decode_typed(row, idx, &type_name) {
        return cell;
    }
    // Defensive fallback for types not enumerated above (or branches whose
    // decoder failed). sqlx's `compatible()` check makes each attempt a
    // no-op against incompatible columns, so this is safe.
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
        return Some(opt.map(|w| Cell::Text(w.0.to_string())).unwrap_or(Cell::Null));
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
        "BOOL" => decode_or_null::<bool>(row, idx)
            .map(|opt| opt.map(Cell::Bool).unwrap_or(Cell::Null)),
        "INT2" | "SMALLINT" => decode_or_null::<i16>(row, idx)
            .map(|opt| opt.map(|v| Cell::Int(v as i64)).unwrap_or(Cell::Null)),
        "INT4" | "INT" | "INTEGER" => decode_or_null::<i32>(row, idx)
            .map(|opt| opt.map(|v| Cell::Int(v as i64)).unwrap_or(Cell::Null)),
        "INT8" | "BIGINT" => {
            decode_or_null::<i64>(row, idx).map(|opt| opt.map(Cell::Int).unwrap_or(Cell::Null))
        }
        "FLOAT4" | "REAL" => decode_or_null::<f32>(row, idx)
            .map(|opt| opt.map(|v| Cell::Float(v as f64)).unwrap_or(Cell::Null)),
        "FLOAT8" | "DOUBLE PRECISION" => decode_or_null::<f64>(row, idx)
            .map(|opt| opt.map(Cell::Float).unwrap_or(Cell::Null)),
        // NUMERIC needs sqlx's `bigdecimal` or `decimal` feature to decode;
        // without it we let it fall through to Cell::Other. Add the feature if
        // we ever need exact-precision support here.
        "TEXT" | "VARCHAR" | "CHAR" | "BPCHAR" | "NAME" | "CITEXT" => {
            decode_or_null::<String>(row, idx).map(|opt| opt.map(Cell::Text).unwrap_or(Cell::Null))
        }
        "BYTEA" => decode_or_null::<Vec<u8>>(row, idx)
            .map(|opt| opt.map(Cell::Bytes).unwrap_or(Cell::Null)),
        "TIMESTAMPTZ" => decode_or_null::<DateTime<Utc>>(row, idx)
            .map(|opt| opt.map(Cell::Timestamp).unwrap_or(Cell::Null)),
        "TIMESTAMP" => decode_or_null::<NaiveDateTime>(row, idx)
            .map(|opt| opt.map(|v| Cell::Text(v.to_string())).unwrap_or(Cell::Null)),
        "DATE" => decode_or_null::<NaiveDate>(row, idx)
            .map(|opt| opt.map(Cell::Date).unwrap_or(Cell::Null)),
        "TIME" => decode_or_null::<NaiveTime>(row, idx)
            .map(|opt| opt.map(Cell::Time).unwrap_or(Cell::Null)),
        "UUID" => decode_or_null::<Uuid>(row, idx)
            .map(|opt| opt.map(Cell::Uuid).unwrap_or(Cell::Null)),
        "JSON" | "JSONB" => decode_or_null::<sqlx::types::Json<JsonValue>>(row, idx)
            .map(|opt| opt.map(|w| Cell::Text(w.0.to_string())).unwrap_or(Cell::Null)),
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
        "JSON" | "JSONB" => format_array::<sqlx::types::Json<JsonValue>, _>(
            row,
            idx,
            type_name,
            |w| w.0.to_string(),
        ),
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
