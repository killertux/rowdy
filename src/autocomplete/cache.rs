//! In-memory schema cache shared between the worker (writer) and the app
//! event loop (reader). Wrapped in `Arc<RwLock<...>>` at the call sites.
//!
//! Phase 1 is deliberately bare: catalogs, schemas of the default
//! catalog, and tables of the default schema are eager-loaded on connect.
//! Columns of any table are loaded the first time the autocomplete engine
//! references them — by then the user's typing speed is the rate-limit,
//! not the round-trip.

use std::collections::HashMap;

use crate::datasource::schema::TableKind;

/// What the cache knows about one table or view we've seen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedTable {
    pub name: String,
    pub kind: TableKind,
}

/// What the cache knows about one column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedColumn {
    pub name: String,
    pub type_name: String,
}

/// Snapshot of the active connection's schema, lazily filled in.
///
/// Keys are `(catalog, schema)` and `(catalog, schema, table)`. Catalog
/// and schema are stored as the strings the database itself uses — we
/// don't lower-case them, so cross-driver case-folding is the engine's
/// job, not the cache's.
#[derive(Debug, Default)]
pub struct SchemaCache {
    /// Connection name this cache belongs to. `None` until the first
    /// successful prime; cleared on `:conn use` to avoid stale entries
    /// leaking across connections.
    pub connection: Option<String>,
    pub default_catalog: Option<String>,
    pub default_schema: Option<String>,
    pub catalogs: Vec<String>,
    pub schemas: HashMap<String, Vec<String>>,
    pub tables: HashMap<(String, String), Vec<CachedTable>>,
    pub columns: HashMap<(String, String, String), Vec<CachedColumn>>,
}

impl SchemaCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Reset to the empty state. Called on `:conn use` and on `:reload`
    /// before re-priming. Keeping the same `Arc` (rather than swapping
    /// the whole struct) means handles already cloned across the worker
    /// don't go stale.
    pub fn clear(&mut self) {
        self.connection = None;
        self.default_catalog = None;
        self.default_schema = None;
        self.catalogs.clear();
        self.schemas.clear();
        self.tables.clear();
        self.columns.clear();
    }

    /// Tables for the default schema, if both have been resolved. Used by
    /// the engine's table-context branch — most user queries reference
    /// tables in the default schema, so we shortcut that path rather
    /// than walking the whole `tables` map.
    pub fn default_tables(&self) -> Option<&[CachedTable]> {
        let catalog = self.default_catalog.as_ref()?;
        let schema = self.default_schema.as_ref()?;
        self.tables
            .get(&(catalog.clone(), schema.clone()))
            .map(Vec::as_slice)
    }
}
