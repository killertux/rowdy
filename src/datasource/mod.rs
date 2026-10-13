pub mod cell;
pub mod driver;
pub mod error;
pub mod literal;
pub mod schema;
pub mod sql;

use std::time::Duration;

use async_trait::async_trait;

use crate::log::Logger;

pub use cell::Cell;
pub use driver::Driver;
pub use error::{DatasourceError, DatasourceResult};
pub use schema::{CatalogInfo, ColumnInfo, DefaultSchema, IndexInfo, SchemaInfo, TableInfo};

#[derive(Debug, Clone)]
pub struct Column {
    pub name: String,
}

/// Every driver rowdy can connect with. Adding a database is one entry
/// here plus its module under `sql/` — see [`Driver`].
pub static DRIVERS: &[&dyn Driver] = &[
    &sql::sqlite::SqliteDriver,
    &sql::postgres::PostgresDriver,
    &sql::mysql::MysqlDriver,
];

/// Dialect assumed when no connection is active (autocomplete and the
/// destructive-statement guard still run on a buffer typed before
/// connecting).
pub static FALLBACK_DRIVER: &dyn Driver = &sql::sqlite::SqliteDriver;

/// The driver claiming `url`'s scheme (the part before the first `:`).
pub fn driver_for_url(url: &str) -> Option<&'static dyn Driver> {
    let scheme = url.split_once(':').map(|(s, _)| s).unwrap_or(url);
    DRIVERS
        .iter()
        .copied()
        .find(|d| d.schemes().contains(&scheme))
}

/// Every scheme any driver accepts, formatted for UI hints
/// (`"sqlite:, postgres:, …"`).
pub fn supported_schemes() -> String {
    DRIVERS
        .iter()
        .flat_map(|d| d.schemes())
        .map(|s| format!("{s}:"))
        .collect::<Vec<_>>()
        .join(", ")
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
    match driver_for_url(connection) {
        Some(driver) => driver.connect(connection, logger).await,
        None => {
            let scheme = connection
                .split_once(':')
                .map(|(s, _)| s)
                .unwrap_or(connection);
            logger.error("datasource", format!("unsupported scheme: {scheme}"));
            Err(DatasourceError::Connect(format!(
                "unsupported scheme: {scheme}"
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn driver_for_url_dispatches_on_scheme() {
        let cases = [
            ("sqlite:./sample.db", "sqlite"),
            ("sqlite::memory:", "sqlite"),
            ("postgres://u@h/db", "postgres"),
            ("postgresql://u@h/db", "postgres"),
            ("mysql://u@h/db", "mysql"),
            ("mariadb://u@h/db", "mysql"),
        ];
        for (url, name) in cases {
            assert_eq!(driver_for_url(url).map(|d| d.name()), Some(name), "{url}");
        }
        assert!(driver_for_url("oracle://h/db").is_none());
    }

    #[test]
    fn no_two_drivers_claim_the_same_scheme() {
        let mut seen = std::collections::HashSet::new();
        for scheme in DRIVERS.iter().flat_map(|d| d.schemes()) {
            assert!(seen.insert(*scheme), "scheme {scheme:?} claimed twice");
        }
    }

    #[test]
    fn supported_schemes_lists_every_driver() {
        assert_eq!(
            supported_schemes(),
            "sqlite:, postgres:, postgresql:, mysql:, mariadb:"
        );
    }
}
