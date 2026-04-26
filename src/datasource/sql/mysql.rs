use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use async_trait::async_trait;
use chrono::{NaiveDate, NaiveDateTime, NaiveTime};
use serde_json::Value as JsonValue;
use sqlx::mysql::{MySqlPool, MySqlPoolOptions, MySqlRow};
use sqlx::{Column as _, Row, TypeInfo};

use crate::datasource::cell::Cell;
use crate::datasource::error::{DatasourceError, DatasourceResult};
use crate::datasource::schema::{
    CatalogInfo, ColumnInfo, IndexInfo, SchemaInfo, TableInfo, TableKind,
};
use crate::datasource::{Column, Datasource, QueryResult, Row as CellRow};
use crate::log::Logger;

const DEFAULT_POOL_SIZE: u32 = 3;
const MARIADB_SCHEME: &str = "mariadb:";
const MYSQL_SCHEME: &str = "mysql:";
const TARGET: &str = "mysql";

pub struct MysqlDatasource {
    pool: MySqlPool,
    log: Logger,
    // CONNECTION_ID() of the currently running `execute()`, or 0 when nothing
    // is in flight. MySQL connection ids start at 1, so 0 is a safe "none"
    // sentinel. Read by `cancel()` to issue `KILL QUERY`.
    in_flight_conn_id: AtomicU64,
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
        let pool = MySqlPoolOptions::new()
            .max_connections(DEFAULT_POOL_SIZE)
            .connect(&normalized)
            .await
            .map_err(|e| {
                log.error(TARGET, format!("connect failed: {e}"));
                DatasourceError::Connect(e.to_string())
            })?;
        log.info(TARGET, "connected");
        Ok(Self {
            pool,
            log,
            in_flight_conn_id: AtomicU64::new(0),
        })
    }
}

#[async_trait]
impl Datasource for MysqlDatasource {
    async fn introspect_catalogs(&self) -> DatasourceResult<Vec<CatalogInfo>> {
        // MySQL exposes a single static catalog (`def`); we read it from
        // information_schema rather than hard-coding it.
        let rows =
            sqlx::query("SELECT DISTINCT catalog_name AS name FROM information_schema.schemata")
                .fetch_all(&self.pool)
                .await
                .map_err(introspect_err)?;
        Ok(rows
            .into_iter()
            .filter_map(|r| try_string(&r, "name"))
            .map(|name| CatalogInfo { name })
            .collect())
    }

    async fn introspect_schemas(&self, catalog: &str) -> DatasourceResult<Vec<SchemaInfo>> {
        let rows = sqlx::query(
            "SELECT schema_name AS name FROM information_schema.schemata \
             WHERE catalog_name = ? \
               AND schema_name NOT IN ('information_schema', 'mysql', 'performance_schema', 'sys') \
             ORDER BY schema_name",
        )
        .bind(catalog)
        .fetch_all(&self.pool)
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
        let rows = sqlx::query(
            "SELECT table_name AS name, table_type AS kind \
             FROM information_schema.tables \
             WHERE table_catalog = ? AND table_schema = ? \
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
        let rows = sqlx::query(
            "SELECT column_name AS name, column_type AS type_name, is_nullable \
             FROM information_schema.columns \
             WHERE table_catalog = ? AND table_schema = ? AND table_name = ? \
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
        let rows = sqlx::query(
            "SELECT index_name AS name, MIN(non_unique) AS non_unique \
             FROM information_schema.statistics \
             WHERE table_schema = ? AND table_name = ? \
             GROUP BY index_name \
             ORDER BY index_name",
        )
        .bind(schema)
        .bind(table)
        .fetch_all(&self.pool)
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

        // Pin a single connection to this query so `cancel()` can issue
        // `KILL QUERY <conn_id>` against the exact session running it.
        let mut conn = self.pool.acquire().await.map_err(|e| {
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
        self.in_flight_conn_id.store(conn_id, Ordering::SeqCst);

        if super::is_row_returning(statement) {
            let result = sqlx::query(statement).fetch_all(&mut *conn).await;
            // Clear the conn id before checking the result so a subsequent
            // cancel doesn't try to kill a session that's already idle. On
            // abort, this never runs and `cancel()` reads the still-set id.
            self.in_flight_conn_id.store(0, Ordering::SeqCst);
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
            self.in_flight_conn_id.store(0, Ordering::SeqCst);
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
        let conn_id = self.in_flight_conn_id.swap(0, Ordering::SeqCst);
        if conn_id == 0 {
            self.log.info(TARGET, "cancel: no in-flight query");
            return Ok(());
        }
        // `KILL QUERY` is an admin statement and doesn't accept placeholders;
        // formatting the u64 directly is safe (no injection surface). A
        // separate pool connection is used so the kill doesn't wait on the
        // busy session.
        let sql = format!("KILL QUERY {conn_id}");
        self.log.info(TARGET, format!("cancel: {sql}"));
        sqlx::query(&sql).execute(&self.pool).await.map_err(|e| {
            self.log.warn(TARGET, format!("cancel failed: {e}"));
            execute_err(e)
        })?;
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
        "BOOLEAN" => {
            decode_or_null::<bool>(row, idx).map(|opt| opt.map(Cell::Bool).unwrap_or(Cell::Null))
        }
        "TINYINT" => decode_or_null::<i8>(row, idx)
            .map(|opt| opt.map(|v| Cell::Int(v as i64)).unwrap_or(Cell::Null)),
        "SMALLINT" => decode_or_null::<i16>(row, idx)
            .map(|opt| opt.map(|v| Cell::Int(v as i64)).unwrap_or(Cell::Null)),
        "MEDIUMINT" | "INT" => decode_or_null::<i32>(row, idx)
            .map(|opt| opt.map(|v| Cell::Int(v as i64)).unwrap_or(Cell::Null)),
        "BIGINT" => {
            decode_or_null::<i64>(row, idx).map(|opt| opt.map(Cell::Int).unwrap_or(Cell::Null))
        }
        "TINYINT UNSIGNED" => decode_or_null::<u8>(row, idx)
            .map(|opt| opt.map(|v| Cell::Int(v as i64)).unwrap_or(Cell::Null)),
        "SMALLINT UNSIGNED" => decode_or_null::<u16>(row, idx)
            .map(|opt| opt.map(|v| Cell::Int(v as i64)).unwrap_or(Cell::Null)),
        "MEDIUMINT UNSIGNED" | "INT UNSIGNED" => decode_or_null::<u32>(row, idx)
            .map(|opt| opt.map(|v| Cell::Int(v as i64)).unwrap_or(Cell::Null)),
        "BIGINT UNSIGNED" => {
            decode_or_null::<u64>(row, idx).map(|opt| opt.map(Cell::UInt).unwrap_or(Cell::Null))
        }
        "FLOAT" => decode_or_null::<f32>(row, idx)
            .map(|opt| opt.map(|v| Cell::Float(v as f64)).unwrap_or(Cell::Null)),
        "DOUBLE" => {
            decode_or_null::<f64>(row, idx).map(|opt| opt.map(Cell::Float).unwrap_or(Cell::Null))
        }
        "DECIMAL" | "NUMERIC" => decode_or_null::<sqlx::types::BigDecimal>(row, idx).map(|opt| {
            opt.map(|v| Cell::Decimal(v.to_string()))
                .unwrap_or(Cell::Null)
        }),
        "VARCHAR" | "CHAR" | "TEXT" | "TINYTEXT" | "MEDIUMTEXT" | "LONGTEXT" | "ENUM" | "SET" => {
            decode_or_null::<String>(row, idx).map(|opt| opt.map(Cell::Text).unwrap_or(Cell::Null))
        }
        "BINARY" | "VARBINARY" | "BLOB" | "TINYBLOB" | "MEDIUMBLOB" | "LONGBLOB" => {
            decode_or_null::<Vec<u8>>(row, idx)
                .map(|opt| opt.map(Cell::Bytes).unwrap_or(Cell::Null))
        }
        "DATE" => decode_or_null::<NaiveDate>(row, idx)
            .map(|opt| opt.map(Cell::Date).unwrap_or(Cell::Null)),
        "TIME" => decode_or_null::<NaiveTime>(row, idx)
            .map(|opt| opt.map(Cell::Time).unwrap_or(Cell::Null)),
        // MySQL's TIMESTAMP/DATETIME are timezone-naive on the wire; preserve
        // them as text rather than fabricating a UTC offset they don't have.
        "DATETIME" | "TIMESTAMP" => decode_or_null::<NaiveDateTime>(row, idx)
            .map(|opt| opt.map(|v| Cell::Text(v.to_string())).unwrap_or(Cell::Null)),
        "YEAR" => decode_or_null::<u16>(row, idx)
            .map(|opt| opt.map(|v| Cell::Int(v as i64)).unwrap_or(Cell::Null)),
        "JSON" => decode_or_null::<sqlx::types::Json<JsonValue>>(row, idx).map(|opt| {
            opt.map(|w| Cell::Text(w.0.to_string()))
                .unwrap_or(Cell::Null)
        }),
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
