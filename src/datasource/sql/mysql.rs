use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::{NaiveDate, NaiveDateTime, NaiveTime};
use serde_json::Value as JsonValue;
use sqlx::mysql::{MySql, MySqlPool, MySqlPoolOptions, MySqlRow};
use sqlx::pool::PoolConnection;
use sqlx::{Column as _, Row, TypeInfo};
use tokio::sync::Mutex;

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
const MARIADB_SCHEME: &str = "mariadb:";
const MYSQL_SCHEME: &str = "mysql:";
const TARGET: &str = "mysql";

/// Pinned session connection — see `postgres::Session` for the shape;
/// here the cancel handle is the `CONNECTION_ID()` we already mirror to
/// `session_conn_id`.
struct Session {
    conn: PoolConnection<MySql>,
}

pub struct MysqlDatasource {
    pool: StdMutex<MySqlPool>,
    url: String,
    log: Logger,
    // CONNECTION_ID() of the pinned session connection, or 0 when no
    // session is currently held. Recorded once on acquire and kept
    // across executes so `cancel()` can `KILL QUERY <id>` even when
    // the spawn_query task is mid-await.
    session_conn_id: AtomicU64,
    /// Pinned connection held only while a transaction is open.
    /// Introspection and cancel always talk to the pool.
    session: Mutex<Option<Session>>,
}

fn build_pool(url: &str) -> Result<MySqlPool, sqlx::Error> {
    MySqlPoolOptions::new()
        .max_connections(DEFAULT_POOL_SIZE)
        .min_connections(0)
        .idle_timeout(Some(Duration::from_secs(IDLE_TIMEOUT_SECS)))
        .connect_lazy(url)
}

impl MysqlDatasource {
    pub async fn connect(url: &str, log: Logger) -> DatasourceResult<Self> {
        // sqlx only recognises `mysql://`; `mariadb://` is the same wire
        // protocol so we rewrite it before handing it off.
        let normalized = if let Some(rest) = url.strip_prefix(MARIADB_SCHEME) {
            format!("{MYSQL_SCHEME}{rest}")
        } else {
            url.to_string()
        };
        log.info(
            TARGET,
            format!("connecting to {}", super::redact_url(&normalized)),
        );
        let pool = build_pool(&normalized).map_err(|e| {
            log.error(TARGET, format!("pool build failed: {e}"));
            DatasourceError::Connect(e.to_string())
        })?;
        // Upfront ping so bad URLs surface here, not on the first query.
        let verify_conn = pool.acquire().await.map_err(|e| {
            log.error(TARGET, format!("connect verify failed: {e}"));
            DatasourceError::Connect(e.to_string())
        })?;
        drop(verify_conn);
        log.info(TARGET, "connected");
        Ok(Self {
            pool: StdMutex::new(pool),
            url: normalized,
            log,
            session_conn_id: AtomicU64::new(0),
            session: Mutex::new(None),
        })
    }

    fn pool(&self) -> MySqlPool {
        self.pool.lock().expect("pool mutex poisoned").clone()
    }
}

#[async_trait]
impl Datasource for MysqlDatasource {
    async fn default_schema(&self) -> DatasourceResult<DefaultSchema> {
        // MySQL exposes a single static catalog (`def`); the schema (= "current
        // database") comes from the connection URL via `DATABASE()`. If the
        // user connected without selecting a database, we return an empty
        // string so the caller can decide what to do (skip prime, etc.).
        let pool = self.pool();
        let row = sqlx::query(
            "SELECT \
                COALESCE(\
                    (SELECT catalog_name FROM information_schema.schemata LIMIT 1),\
                    'def'\
                ) AS catalog, \
                COALESCE(DATABASE(), '') AS schema",
        )
        .fetch_one(&pool)
        .await
        .map_err(introspect_err)?;
        let catalog = try_string(&row, "catalog").unwrap_or_else(|| "def".to_string());
        let schema = try_string(&row, "schema").unwrap_or_default();
        Ok(DefaultSchema { catalog, schema })
    }

    async fn introspect_catalogs(&self) -> DatasourceResult<Vec<CatalogInfo>> {
        // MySQL exposes a single static catalog (`def`); we read it from
        // information_schema rather than hard-coding it.
        let pool = self.pool();
        let rows =
            sqlx::query("SELECT DISTINCT catalog_name AS name FROM information_schema.schemata")
                .fetch_all(&pool)
                .await
                .map_err(introspect_err)?;
        Ok(rows
            .into_iter()
            .filter_map(|r| try_string(&r, "name"))
            .map(|name| CatalogInfo { name })
            .collect())
    }

    async fn introspect_schemas(&self, catalog: &str) -> DatasourceResult<Vec<SchemaInfo>> {
        let pool = self.pool();
        let rows = sqlx::query(
            "SELECT schema_name AS name FROM information_schema.schemata \
             WHERE catalog_name = ? \
               AND schema_name NOT IN ('information_schema', 'mysql', 'performance_schema', 'sys') \
             ORDER BY schema_name",
        )
        .bind(catalog)
        .fetch_all(&pool)
        .await
        .map_err(introspect_err)?;
        Ok(rows
            .into_iter()
            .filter_map(|r| try_string(&r, "name"))
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
             WHERE table_catalog = ? AND table_schema = ? \
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
                let name = try_string(&r, "name")?;
                let kind_str = try_string(&r, "kind").unwrap_or_default();
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
        // `column_type` carries the full declared type (e.g. `int(11) unsigned`),
        // which is more useful for display than the normalised `data_type`.
        let pool = self.pool();
        let rows = sqlx::query(
            "SELECT column_name AS name, column_type AS type_name, is_nullable \
             FROM information_schema.columns \
             WHERE table_catalog = ? AND table_schema = ? AND table_name = ? \
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
                let name = try_string(&r, "name")?;
                let type_name = try_string(&r, "type_name").unwrap_or_default();
                let is_nullable = try_string(&r, "is_nullable").unwrap_or_default();
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
        // information_schema.statistics has one row per index column; collapse
        // by index_name and take the lowest non_unique value (0 wins, meaning
        // unique).
        let pool = self.pool();
        let rows = sqlx::query(
            "SELECT index_name AS name, MIN(non_unique) AS non_unique \
             FROM information_schema.statistics \
             WHERE table_schema = ? AND table_name = ? \
             GROUP BY index_name \
             ORDER BY index_name",
        )
        .bind(schema)
        .bind(table)
        .fetch_all(&pool)
        .await
        .map_err(introspect_err)?;
        Ok(rows
            .into_iter()
            .filter_map(|r| {
                let name = try_string(&r, "name")?;
                let non_unique: i64 = r.try_get("non_unique").ok().unwrap_or(1);
                Some(IndexInfo {
                    name,
                    unique: non_unique == 0,
                })
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
            let conn_id: u64 = sqlx::query_scalar("SELECT CONNECTION_ID()")
                .fetch_one(&mut *conn)
                .await
                .map_err(|e| {
                    self.log
                        .error(TARGET, format!("connection id fetch failed: {e}"));
                    execute_err(e)
                })?;
            self.session_conn_id.store(conn_id, Ordering::SeqCst);
            *guard = Some(Session { conn });
        }
        let session = guard.as_mut().expect("session populated above");

        let outcome: Result<QueryResult, sqlx::Error> =
            if super::is_row_returning(statement, &sqlparser::dialect::MySqlDialect {}) {
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

        let effect = tx_effect(statement, &sqlparser::dialect::MySqlDialect {});
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
                    self.session_conn_id.store(0, Ordering::SeqCst);
                }
                Ok(r)
            }
            Err(e) => {
                self.log.error(TARGET, format!("execute failed: {e}"));
                if super::is_connection_lost(&e) {
                    *guard = None;
                    self.session_conn_id.store(0, Ordering::SeqCst);
                    self.log
                        .warn(TARGET, "session conn dropped after connection loss");
                } else if !was_in_tx {
                    *guard = None;
                    self.session_conn_id.store(0, Ordering::SeqCst);
                }
                Err(execute_err(e))
            }
        }
    }

    async fn cancel(&self) -> DatasourceResult<()> {
        let conn_id = self.session_conn_id.load(Ordering::SeqCst);
        if conn_id == 0 {
            self.log.info(TARGET, "cancel: no active session");
            return Ok(());
        }
        // `KILL QUERY` is an admin statement and doesn't accept placeholders;
        // formatting the u64 directly is safe (no injection surface). A
        // separate pool connection is used so the kill doesn't wait on the
        // busy session.
        let sql = format!("KILL QUERY {conn_id}");
        self.log.info(TARGET, format!("cancel: {sql}"));
        let pool = self.pool();
        sqlx::query(&sql).execute(&pool).await.map_err(|e| {
            self.log.warn(TARGET, format!("cancel failed: {e}"));
            execute_err(e)
        })?;
        Ok(())
    }

    async fn reset_session(&self) -> DatasourceResult<()> {
        // Drop the pinned session conn first (with an explicit ROLLBACK
        // so the backend isn't left holding an open tx).
        let mut guard = self.session.lock().await;
        if let Some(mut session) = guard.take() {
            if let Err(e) = sqlx::query("ROLLBACK").execute(&mut *session.conn).await {
                self.log.warn(TARGET, format!("reset rollback: {e}"));
            }
            drop(session);
            self.log.info(TARGET, "session reset");
        }
        self.session_conn_id.store(0, Ordering::SeqCst);
        drop(guard);

        // Swap in a fresh lazy pool and close the old one — that forces
        // every existing backend connection this datasource was holding
        // (idle or otherwise) to disconnect.
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

fn build_columns(rows: &[MySqlRow]) -> Vec<Column> {
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

fn row_to_cells(row: &MySqlRow, n: usize) -> CellRow {
    (0..n).map(|i| decode_cell(row, i)).collect()
}

fn decode_cell(row: &MySqlRow, idx: usize) -> Cell {
    let column = &row.columns()[idx];
    let type_name = column.type_info().name().to_string();
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

fn decode_fallback(row: &MySqlRow, idx: usize) -> Option<Cell> {
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

fn decode_typed(row: &MySqlRow, idx: usize, type_name: &str) -> Option<Cell> {
    match type_name {
        "BOOLEAN" => decode_to!(row, idx, bool => Cell::Bool),
        "TINYINT" => decode_to!(row, idx, i8 => |v| Cell::Int(v as i64)),
        "SMALLINT" => decode_to!(row, idx, i16 => |v| Cell::Int(v as i64)),
        "MEDIUMINT" | "INT" => decode_to!(row, idx, i32 => |v| Cell::Int(v as i64)),
        "BIGINT" => decode_to!(row, idx, i64 => Cell::Int),
        "TINYINT UNSIGNED" => decode_to!(row, idx, u8 => |v| Cell::Int(v as i64)),
        "SMALLINT UNSIGNED" => decode_to!(row, idx, u16 => |v| Cell::Int(v as i64)),
        "MEDIUMINT UNSIGNED" | "INT UNSIGNED" => {
            decode_to!(row, idx, u32 => |v| Cell::Int(v as i64))
        }
        "BIGINT UNSIGNED" => decode_to!(row, idx, u64 => Cell::UInt),
        "FLOAT" => decode_to!(row, idx, f32 => |v| Cell::Float(v as f64)),
        "DOUBLE" => decode_to!(row, idx, f64 => Cell::Float),
        "DECIMAL" | "NUMERIC" => {
            decode_to!(row, idx, sqlx::types::BigDecimal => |v| Cell::Decimal(v.to_string()))
        }
        "VARCHAR" | "CHAR" | "TEXT" | "TINYTEXT" | "MEDIUMTEXT" | "LONGTEXT" | "ENUM" | "SET" => {
            decode_to!(row, idx, String => Cell::Text)
        }
        "BINARY" | "VARBINARY" | "BLOB" | "TINYBLOB" | "MEDIUMBLOB" | "LONGBLOB" => {
            decode_to!(row, idx, Vec<u8> => Cell::Bytes)
        }
        "DATE" => decode_to!(row, idx, NaiveDate => Cell::Date),
        "TIME" => decode_to!(row, idx, NaiveTime => Cell::Time),
        // MySQL's TIMESTAMP/DATETIME are timezone-naive on the wire; preserve
        // them as text rather than fabricating a UTC offset they don't have.
        "DATETIME" | "TIMESTAMP" => {
            decode_to!(row, idx, NaiveDateTime => |v| Cell::Text(v.to_string()))
        }
        "YEAR" => decode_to!(row, idx, u16 => |v| Cell::Int(v as i64)),
        "JSON" => {
            decode_to!(row, idx, sqlx::types::Json<JsonValue> => |w| Cell::Text(w.0.to_string()))
        }
        _ => None,
    }
}

fn decode_or_null<'r, T>(row: &'r MySqlRow, idx: usize) -> Option<Option<T>>
where
    T: sqlx::Decode<'r, sqlx::MySql> + sqlx::Type<sqlx::MySql>,
{
    row.try_get::<Option<T>, _>(idx).ok()
}

fn introspect_err(err: sqlx::Error) -> DatasourceError {
    DatasourceError::Introspect(err.to_string())
}

fn execute_err(err: sqlx::Error) -> DatasourceError {
    DatasourceError::Execute(err.to_string())
}

/// Read a column as `String`, falling back to `Vec<u8>` → UTF-8 if the
/// driver reports the column as `VARBINARY`. MySQL 8 returns most
/// `information_schema` text columns as `VARBINARY` even though the
/// values are UTF-8 names — `try_get::<String>` rejects that on a strict
/// type-match, so we coerce here and let the rare non-UTF-8 row drop.
fn try_string(row: &MySqlRow, column: &str) -> Option<String> {
    if let Ok(s) = row.try_get::<String, _>(column) {
        return Some(s);
    }
    let bytes: Vec<u8> = row.try_get(column).ok()?;
    String::from_utf8(bytes).ok()
}

#[cfg(test)]
mod tests {
    //! Integration tests against a live MySQL. Gated by the
    //! `ROWDY_MYSQL_URL` environment variable — when unset the test
    //! prints a skip notice and returns Ok, so `cargo test` stays green
    //! on machines without a database. See `compose.yaml` for a one-shot
    //! local setup.
    use super::*;

    fn url() -> Option<String> {
        std::env::var("ROWDY_MYSQL_URL")
            .ok()
            .filter(|s| !s.is_empty())
    }

    fn unique_table() -> String {
        let id = uuid::Uuid::new_v4().simple().to_string();
        format!("rowdy_test_{}", &id[..16])
    }

    /// Pull the database name out of a mysql URL — the connect string the
    /// user supplied is the schema we'll find our table in.
    fn schema_from(url: &str) -> String {
        url.rsplit('/')
            .next()
            .and_then(|tail| tail.split('?').next())
            .unwrap_or("")
            .to_string()
    }

    #[tokio::test]
    async fn connect_query_and_introspect() {
        let Some(url) = url() else {
            eprintln!("ROWDY_MYSQL_URL not set; skipping mysql integration test");
            return;
        };
        let schema = schema_from(&url);
        assert!(!schema.is_empty(), "ROWDY_MYSQL_URL must end in /<dbname>");
        let ds = MysqlDatasource::connect(&url, Logger::discard())
            .await
            .expect("connect");
        let table = unique_table();

        ds.execute(&format!("DROP TABLE IF EXISTS {table}"))
            .await
            .expect("pre-clean");
        ds.execute(&format!(
            "CREATE TABLE {table} (id INT PRIMARY KEY, name VARCHAR(64) NOT NULL, score DECIMAL(10,2))"
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
        let catalog = &catalogs[0].name;
        let schemas = ds.introspect_schemas(catalog).await.expect("schemas");
        assert!(
            schemas.iter().any(|s| s.name == schema),
            "schema {schema:?} not in {schemas:?}"
        );
        let tables = ds
            .introspect_tables(catalog, &schema)
            .await
            .expect("tables");
        assert!(
            tables.iter().any(|t| t.name == table),
            "table {table:?} not found in: {tables:?}"
        );
        let cols = ds
            .introspect_columns(catalog, &schema, &table)
            .await
            .expect("columns");
        let names: Vec<_> = cols.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["id", "name", "score"]);

        ds.execute(&format!("DROP TABLE {table}"))
            .await
            .expect("drop");
    }
}
