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
        let rows = sqlx::query(
            "SELECT DISTINCT catalog_name AS name FROM information_schema.schemata",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(introspect_err)?;
        Ok(rows
            .into_iter()
            .filter_map(|r| r.try_get::<String, _>("name").ok())
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
                let name: String = r.try_get("name").ok()?;
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
                self.log.error(TARGET, format!("connection id fetch failed: {e}"));
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
        sqlx::query(&sql)
            .execute(&self.pool)
            .await
            .map_err(|e| {
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

fn decode_typed(row: &MySqlRow, idx: usize, type_name: &str) -> Option<Cell> {
    match type_name {
        "BOOLEAN" => decode_or_null::<bool>(row, idx)
            .map(|opt| opt.map(Cell::Bool).unwrap_or(Cell::Null)),
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
        "DOUBLE" => decode_or_null::<f64>(row, idx)
            .map(|opt| opt.map(Cell::Float).unwrap_or(Cell::Null)),
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
        "JSON" => decode_or_null::<sqlx::types::Json<JsonValue>>(row, idx)
            .map(|opt| opt.map(|w| Cell::Text(w.0.to_string())).unwrap_or(Cell::Null)),
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
