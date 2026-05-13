pub mod cell;
pub mod error;
pub mod schema;
pub mod sql;

use std::time::Duration;

use async_trait::async_trait;

use crate::log::Logger;

pub use cell::Cell;
pub use error::{DatasourceError, DatasourceResult};
pub use schema::{CatalogInfo, ColumnInfo, DefaultSchema, IndexInfo, SchemaInfo, TableInfo};

#[derive(Debug, Clone)]
pub struct Column {
    pub name: String,
}

/// Which SQL backend a connection (or stored result) belongs to. Drives
/// dialect-specific behaviour at the edges: parsing for source-table
/// inference, literal escaping when emitting INSERTs, etc.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DriverKind {
    Sqlite,
    Postgres,
    Mysql,
}

impl DriverKind {
    pub fn from_url(url: &str) -> Option<Self> {
        let scheme = url.split_once(':').map(|(s, _)| s).unwrap_or(url);
        match scheme {
            "sqlite" => Some(Self::Sqlite),
            "postgres" | "postgresql" => Some(Self::Postgres),
            "mysql" | "mariadb" => Some(Self::Mysql),
            _ => None,
        }
    }
}

pub type Row = Vec<Cell>;

#[derive(Debug)]
pub struct QueryResult {
    pub columns: Vec<Column>,
    pub rows: Vec<Row>,
    pub affected: Option<u64>,
    pub elapsed: Duration,
    /// How many statements contributed to this result. The worker
    /// splits the user-submitted SQL on top-level `;` and runs the
    /// pieces in sequence on the pinned session connection; the
    /// `QueryResult` it surfaces carries the *last* statement's
    /// columns/rows plus this count so the UI can say "ran N
    /// statements". Drivers themselves always set this to 1.
    pub statements_run: usize,
}

#[async_trait]
pub trait Datasource: Send + Sync {
    /// Where unqualified identifiers resolve. Called once on connect to
    /// seed the autocomplete cache; drivers should fall back to a sane
    /// default rather than error out so a transient permissions glitch
    /// doesn't break the rest of the prime.
    async fn default_schema(&self) -> DatasourceResult<DefaultSchema>;
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

    /// Drop the pinned session connection (if any) and forget the
    /// associated backend identity. The next `execute()` re-acquires a
    /// fresh connection from the pool, which severs whatever
    /// transaction state was open on the prior one. Default impl is a
    /// no-op for drivers that don't pin a session.
    ///
    /// Wired to the user-facing `:reset` command (and to `:clear`,
    /// which resets the session in addition to wiping the editor
    /// buffer and result history).
    async fn reset_session(&self) -> DatasourceResult<()> {
        Ok(())
    }
}

/// Builds a datasource from a connection string. Scheme dispatches to the driver.
pub async fn connect(connection: &str, logger: Logger) -> DatasourceResult<Box<dyn Datasource>> {
    let scheme = connection
        .split_once(':')
        .map(|(s, _)| s)
        .unwrap_or(connection);
    match scheme {
        "sqlite" => sql::sqlite::SqliteDatasource::connect(connection, logger)
            .await
            .map(|ds| Box::new(ds) as Box<dyn Datasource>),
        "postgres" | "postgresql" => sql::postgres::PostgresDatasource::connect(connection, logger)
            .await
            .map(|ds| Box::new(ds) as Box<dyn Datasource>),
        "mysql" | "mariadb" => sql::mysql::MysqlDatasource::connect(connection, logger)
            .await
            .map(|ds| Box::new(ds) as Box<dyn Datasource>),
        other => {
            logger.error("datasource", format!("unsupported scheme: {other}"));
            Err(DatasourceError::Connect(format!(
                "unsupported scheme: {other}"
            )))
        }
    }
}
