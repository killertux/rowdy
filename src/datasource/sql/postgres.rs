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
use crate::datasource::{Column, Datasource, Dialect, QueryResult, Row as CellRow};

const DEFAULT_POOL_SIZE: u32 = 3;

pub struct PostgresDatasource {
    pool: PgPool,
}

impl PostgresDatasource {
    pub async fn connect(url: &str) -> DatasourceResult<Self> {
        let pool = PgPoolOptions::new()
            .max_connections(DEFAULT_POOL_SIZE)
            .connect(url)
            .await
            .map_err(|e| DatasourceError::Connect(e.to_string()))?;
        Ok(Self { pool })
    }
}

#[async_trait]
impl Datasource for PostgresDatasource {
    fn dialect(&self) -> Dialect {
        Dialect::Postgres
    }

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
        let started = Instant::now();
        let rows = sqlx::query(statement)
            .fetch_all(&self.pool)
            .await
            .map_err(execute_err)?;
        let elapsed = started.elapsed();
        let columns = build_columns(&rows);
        let rows = rows
            .iter()
            .map(|r| row_to_cells(r, columns.len()))
            .collect();
        Ok(QueryResult {
            columns,
            rows,
            affected: None,
            elapsed,
        })
    }

    async fn cancel(&self) -> DatasourceResult<()> {
        // TODO: real cancel via pg_cancel_backend(pid) — needs the worker to
        // track the backend PID of the in-flight query. For now the worker
        // aborts the JoinHandle, which drops the future on the client side.
        Ok(())
    }

    async fn close(self: Box<Self>) -> DatasourceResult<()> {
        self.pool.close().await;
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
            type_name: col.type_info().name().to_string(),
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
    Cell::Other {
        type_name,
        repr: String::new(),
    }
}

fn decode_typed(row: &PgRow, idx: usize, type_name: &str) -> Option<Cell> {
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
        "JSON" | "JSONB" => decode_or_null::<JsonValue>(row, idx)
            .map(|opt| opt.map(|v| Cell::Text(v.to_string())).unwrap_or(Cell::Null)),
        _ => None,
    }
}

fn decode_or_null<'r, T>(row: &'r PgRow, idx: usize) -> Option<Option<T>>
where
    T: sqlx::Decode<'r, sqlx::Postgres> + sqlx::Type<sqlx::Postgres>,
{
    row.try_get::<Option<T>, _>(idx).ok()
}

fn introspect_err(err: sqlx::Error) -> DatasourceError {
    DatasourceError::Introspect(err.to_string())
}

fn execute_err(err: sqlx::Error) -> DatasourceError {
    DatasourceError::Execute(err.to_string())
}
