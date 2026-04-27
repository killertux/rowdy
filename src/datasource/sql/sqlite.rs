use std::time::Instant;

use async_trait::async_trait;
use sqlx::sqlite::{SqlitePool, SqlitePoolOptions, SqliteRow};
use sqlx::{Column as _, Row, TypeInfo};

use crate::datasource::cell::Cell;
use crate::datasource::error::{DatasourceError, DatasourceResult};
use crate::datasource::schema::{
    CatalogInfo, ColumnInfo, DefaultSchema, IndexInfo, SchemaInfo, TableInfo, TableKind,
};
use crate::datasource::sql::decode_to;
use crate::datasource::{Column, Datasource, QueryResult, Row as CellRow};
use crate::log::Logger;

const DEFAULT_POOL_SIZE: u32 = 3;
const TARGET: &str = "sqlite";

pub struct SqliteDatasource {
    pool: SqlitePool,
    log: Logger,
}

impl SqliteDatasource {
    pub async fn connect(url: &str, log: Logger) -> DatasourceResult<Self> {
        log.info(TARGET, format!("connecting to {url}"));
        let pool = SqlitePoolOptions::new()
            .max_connections(DEFAULT_POOL_SIZE)
            .connect(url)
            .await
            .map_err(|e| {
                log.error(TARGET, format!("connect failed: {e}"));
                DatasourceError::Connect(e.to_string())
            })?;
        log.info(TARGET, "connected");
        Ok(Self { pool, log })
    }
}

#[async_trait]
impl Datasource for SqliteDatasource {
    async fn default_schema(&self) -> DatasourceResult<DefaultSchema> {
        // SQLite has a single (synthetic) catalog and the attached database
        // is always called `main` unless the user attaches more — which we
        // don't support in the UI. Hard-coding both is correct and avoids a
        // round-trip on connect.
        Ok(DefaultSchema {
            catalog: "main".into(),
            schema: "main".into(),
        })
    }

    async fn introspect_catalogs(&self) -> DatasourceResult<Vec<CatalogInfo>> {
        // SQLite has no notion of catalogs; expose a single synthetic root.
        Ok(vec![CatalogInfo {
            name: "main".into(),
        }])
    }

    async fn introspect_schemas(&self, _catalog: &str) -> DatasourceResult<Vec<SchemaInfo>> {
        let rows = sqlx::query("SELECT name FROM pragma_database_list ORDER BY seq")
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
        _catalog: &str,
        schema: &str,
    ) -> DatasourceResult<Vec<TableInfo>> {
        let qualified = format!("\"{}\".sqlite_master", quote_identifier_inner(schema));
        let sql = format!(
            "SELECT name, type FROM {qualified} \
             WHERE type IN ('table', 'view') AND name NOT LIKE 'sqlite_%' \
             ORDER BY name"
        );
        let rows = sqlx::query(&sql)
            .fetch_all(&self.pool)
            .await
            .map_err(introspect_err)?;
        Ok(rows
            .into_iter()
            .filter_map(|r| {
                let name: String = r.try_get("name").ok()?;
                let kind_str: String = r.try_get("type").ok()?;
                let kind = match kind_str.as_str() {
                    "view" => TableKind::View,
                    _ => TableKind::Table,
                };
                Some(TableInfo { name, kind })
            })
            .collect())
    }

    async fn introspect_columns(
        &self,
        _catalog: &str,
        schema: &str,
        table: &str,
    ) -> DatasourceResult<Vec<ColumnInfo>> {
        let rows = sqlx::query("SELECT name, type, \"notnull\" FROM pragma_table_info(?, ?)")
            .bind(table)
            .bind(schema)
            .fetch_all(&self.pool)
            .await
            .map_err(introspect_err)?;
        Ok(rows
            .into_iter()
            .filter_map(|r| {
                let name: String = r.try_get("name").ok()?;
                let ty: String = r.try_get("type").ok().unwrap_or_default();
                let notnull: i64 = r.try_get("notnull").ok().unwrap_or(0);
                Some(ColumnInfo {
                    name,
                    type_name: ty,
                    nullable: Some(notnull == 0),
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
        let sql = "SELECT name, \"unique\" FROM pragma_index_list(?, ?)";
        let rows = sqlx::query(sql)
            .bind(table)
            .bind(schema)
            .fetch_all(&self.pool)
            .await
            .map_err(introspect_err)?;
        Ok(rows
            .into_iter()
            .filter_map(|r| {
                let name: String = r.try_get("name").ok()?;
                let unique: i64 = r.try_get("unique").ok().unwrap_or(0);
                Some(IndexInfo {
                    name,
                    unique: unique == 1,
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
        if super::is_row_returning(statement) {
            let rows = sqlx::query(statement)
                .fetch_all(&self.pool)
                .await
                .map_err(|e| {
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
            let outcome = sqlx::query(statement)
                .execute(&self.pool)
                .await
                .map_err(|e| {
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
        // SQLite has no server-side cancel; the worker aborts the in-flight
        // task instead, which drops the future and releases the connection.
        self.log.info(TARGET, "cancel (no-op for sqlite)");
        Ok(())
    }
}

fn build_columns(rows: &[SqliteRow]) -> Vec<Column> {
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

fn row_to_cells(row: &SqliteRow, n: usize) -> CellRow {
    (0..n).map(|i| decode_cell(row, i)).collect()
}

fn decode_cell(row: &SqliteRow, idx: usize) -> Cell {
    let column = &row.columns()[idx];
    let type_name = column.type_info().name().to_string();
    if let Some(cell) = decode_typed(row, idx, &type_name) {
        return cell;
    }
    decode_dynamic(row, idx).unwrap_or(Cell::Other {
        type_name,
        repr: String::new(),
    })
}

fn decode_typed(row: &SqliteRow, idx: usize, type_name: &str) -> Option<Cell> {
    let upper = type_name.to_uppercase();
    match upper.as_str() {
        "INTEGER" | "INT" | "BIGINT" | "TINYINT" | "SMALLINT" | "MEDIUMINT" => {
            decode_to!(row, idx, i64 => Cell::Int)
        }
        "BOOLEAN" | "BOOL" => decode_to!(row, idx, i64 => |n| Cell::Bool(n != 0)),
        "REAL" | "DOUBLE" | "FLOAT" | "NUMERIC" | "DECIMAL" => {
            decode_to!(row, idx, f64 => Cell::Float)
        }
        "TEXT" | "VARCHAR" | "CHAR" | "DATETIME" | "TIMESTAMP" | "DATE" | "TIME" => {
            decode_to!(row, idx, String => Cell::Text)
        }
        "BLOB" => decode_to!(row, idx, Vec<u8> => Cell::Bytes),
        _ => None,
    }
}

fn decode_dynamic(row: &SqliteRow, idx: usize) -> Option<Cell> {
    if let Ok(opt) = row.try_get::<Option<i64>, _>(idx) {
        return Some(opt.map(Cell::Int).unwrap_or(Cell::Null));
    }
    if let Ok(opt) = row.try_get::<Option<f64>, _>(idx) {
        return Some(opt.map(Cell::Float).unwrap_or(Cell::Null));
    }
    if let Ok(opt) = row.try_get::<Option<String>, _>(idx) {
        return Some(opt.map(Cell::Text).unwrap_or(Cell::Null));
    }
    if let Ok(opt) = row.try_get::<Option<Vec<u8>>, _>(idx) {
        return Some(opt.map(Cell::Bytes).unwrap_or(Cell::Null));
    }
    None
}

fn decode_or_null<'r, T>(row: &'r SqliteRow, idx: usize) -> Option<Option<T>>
where
    T: sqlx::Decode<'r, sqlx::Sqlite> + sqlx::Type<sqlx::Sqlite>,
{
    row.try_get::<Option<T>, _>(idx).ok()
}

/// Escape `"` as `""` for inclusion inside a quoted SQL identifier.
/// The caller is responsible for the surrounding quotes.
fn quote_identifier_inner(ident: &str) -> String {
    ident.replace('"', "\"\"")
}

fn introspect_err(err: sqlx::Error) -> DatasourceError {
    DatasourceError::Introspect(err.to_string())
}

fn execute_err(err: sqlx::Error) -> DatasourceError {
    DatasourceError::Execute(err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn fresh() -> SqliteDatasource {
        // Single connection + shared cache so the DB stays alive across pool checkouts.
        let url = "sqlite::memory:?cache=shared";
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect(url)
            .await
            .expect("connect");
        let ds = SqliteDatasource {
            pool,
            log: Logger::discard(),
        };
        ds.execute("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL, score REAL)")
            .await
            .expect("create table");
        ds.execute("CREATE INDEX users_name_idx ON users(name)")
            .await
            .expect("create index");
        ds.execute("INSERT INTO users(id, name, score) VALUES (1, 'alice', 9.5), (2, 'bob', NULL)")
            .await
            .expect("seed");
        ds
    }

    #[tokio::test]
    async fn introspects_full_chain() {
        let ds = fresh().await;
        let catalogs = ds.introspect_catalogs().await.unwrap();
        assert_eq!(catalogs.len(), 1);

        let schemas = ds.introspect_schemas("main").await.unwrap();
        assert!(schemas.iter().any(|s| s.name == "main"));

        let tables = ds.introspect_tables("main", "main").await.unwrap();
        assert!(tables.iter().any(|t| t.name == "users"));

        let cols = ds
            .introspect_columns("main", "main", "users")
            .await
            .unwrap();
        let names: Vec<_> = cols.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["id", "name", "score"]);
        let name_col = cols.iter().find(|c| c.name == "name").unwrap();
        assert_eq!(name_col.nullable, Some(false));

        let indices = ds
            .introspect_indices("main", "main", "users")
            .await
            .unwrap();
        assert!(
            indices
                .iter()
                .any(|i| i.name == "users_name_idx" && !i.unique)
        );
    }

    #[tokio::test]
    async fn default_schema_is_main() {
        let ds = fresh().await;
        let d = ds.default_schema().await.unwrap();
        assert_eq!(d.catalog, "main");
        assert_eq!(d.schema, "main");
    }

    #[tokio::test]
    async fn execute_returns_typed_cells() {
        let ds = fresh().await;
        let result = ds
            .execute("SELECT id, name, score FROM users ORDER BY id")
            .await
            .unwrap();
        assert_eq!(result.columns.len(), 3);
        assert_eq!(result.rows.len(), 2);
        assert!(matches!(result.rows[0][0], Cell::Int(1)));
        assert!(matches!(&result.rows[0][1], Cell::Text(s) if s == "alice"));
        assert!(matches!(result.rows[0][2], Cell::Float(_)));
        assert!(result.rows[1][2].is_null());
    }
}
