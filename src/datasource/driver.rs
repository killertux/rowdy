use std::fmt::Debug;

use async_trait::async_trait;
use sqlparser::dialect::Dialect;

use crate::autocomplete::functions::FunctionDef;
use crate::datasource::{Cell, Datasource, DatasourceResult, literal};
use crate::log::Logger;

/// Everything rowdy knows about one database engine.
///
/// A `Driver` is a stateless, `'static` descriptor: the UI holds it to
/// parse, quote, autocomplete and export in the right dialect (even
/// before a connection exists, and for result blocks from a connection
/// that's since been swapped out), and the worker calls [`connect`] on
/// it to get a live [`Datasource`].
///
/// Adding a database means implementing this trait plus [`Datasource`]
/// in one module and listing the driver in [`super::DRIVERS`]. Nothing
/// outside `src/datasource/` should need to change.
///
/// [`connect`]: Driver::connect
#[async_trait]
pub trait Driver: Debug + Send + Sync + 'static {
    /// Short lowercase identifier (`"sqlite"`, `"postgres"`, …). Shown
    /// to the LLM as the active driver and used in log targets.
    fn name(&self) -> &'static str;

    /// URL schemes this driver claims — the part before the first `:`.
    fn schemes(&self) -> &'static [&'static str];

    /// sqlparser dialect used to tokenize and parse user SQL
    /// (autocomplete, placeholder scanning, destructive-statement
    /// guard, `:export sql` source-table inference).
    fn sql_dialect(&self) -> &'static dyn Dialect;

    /// Opening and closing identifier quote.
    fn identifier_quotes(&self) -> (&'static str, &'static str) {
        ("\"", "\"")
    }

    /// Dialect-specific SQL functions offered by autocomplete, on top of
    /// the shared ANSI-ish core in [`crate::autocomplete::functions`].
    fn functions(&self) -> &'static [FunctionDef] {
        &[]
    }

    /// Render a cell as a SQL literal for `:export sql`. Override only
    /// the cells this dialect spells differently and delegate the rest
    /// to [`literal::ansi`].
    fn format_literal(&self, cell: &Cell) -> String {
        literal::ansi(cell)
    }

    async fn connect(&self, url: &str, log: Logger) -> DatasourceResult<Box<dyn Datasource>>;
}
