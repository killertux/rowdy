// Several items here exist as the agreed cross-driver API but are only fully
// consumed once the real drivers and the lazy-schema wiring land in upcoming
// chunks. The `allow(dead_code)` flags below mark those scaffolds explicitly.
#![allow(dead_code)]

#[allow(dead_code)]
pub mod cell;
#[allow(dead_code)]
pub mod error;
#[allow(dead_code)]
pub mod schema;
pub mod sql;

use std::time::Duration;

use async_trait::async_trait;

pub use cell::Cell;
pub use error::{DatasourceError, DatasourceResult};
pub use schema::{CatalogInfo, ColumnInfo, IndexInfo, SchemaInfo, TableInfo};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    Postgres,
    MySql,
    Sqlite,
}

#[derive(Debug, Clone)]
pub struct Column {
    pub name: String,
    pub type_name: String,
}

pub type Row = Vec<Cell>;

#[derive(Debug)]
pub struct QueryResult {
    pub columns: Vec<Column>,
    pub rows: Vec<Row>,
    pub affected: Option<u64>,
    pub elapsed: Duration,
}

#[async_trait]
pub trait Datasource: Send + Sync {
    fn dialect(&self) -> Dialect;

    async fn introspect_catalogs(&self) -> DatasourceResult<Vec<CatalogInfo>>;
    async fn introspect_schemas(&self, catalog: &str) -> DatasourceResult<Vec<SchemaInfo>>;
    async fn introspect_tables(
        &self,
        catalog: &str,
        schema: &str,
    ) -> DatasourceResult<Vec<TableInfo>>;
    async fn introspect_columns(
        &self,
        catalog: &str,
        schema: &str,
        table: &str,
    ) -> DatasourceResult<Vec<ColumnInfo>>;
    async fn introspect_indices(
        &self,
        catalog: &str,
        schema: &str,
        table: &str,
    ) -> DatasourceResult<Vec<IndexInfo>>;

    async fn execute(&self, statement: &str) -> DatasourceResult<QueryResult>;
    async fn cancel(&self) -> DatasourceResult<()>;
    async fn close(self: Box<Self>) -> DatasourceResult<()>;
}

/// Builds a datasource from a connection string. Scheme dispatches to the driver.
pub async fn connect(connection: &str) -> DatasourceResult<Box<dyn Datasource>> {
    let scheme = connection
        .split_once(':')
        .map(|(s, _)| s)
        .unwrap_or(connection);
    match scheme {
        "sqlite" => sql::sqlite::SqliteDatasource::connect(connection)
            .await
            .map(|ds| Box::new(ds) as Box<dyn Datasource>),
        "postgres" | "postgresql" => sql::postgres::PostgresDatasource::connect(connection)
            .await
            .map(|ds| Box::new(ds) as Box<dyn Datasource>),
        "mysql" | "mariadb" => sql::mysql::MysqlDatasource::connect(connection)
            .await
            .map(|ds| Box::new(ds) as Box<dyn Datasource>),
        other => Err(DatasourceError::Connect(format!(
            "unsupported scheme: {other}"
        ))),
    }
}
