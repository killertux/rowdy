#[derive(Debug, Clone)]
pub struct CatalogInfo {
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct SchemaInfo {
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct TableInfo {
    pub name: String,
    pub kind: TableKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableKind {
    Table,
    View,
}

#[derive(Debug, Clone)]
pub struct ColumnInfo {
    pub name: String,
    pub type_name: String,
    pub nullable: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct IndexInfo {
    pub name: String,
    pub unique: bool,
}

/// Where unqualified identifiers resolve, per the active connection.
/// Resolved once on connect so the autocomplete cache (and any other
/// "default location" UX) doesn't have to re-query on every keystroke.
#[derive(Debug, Clone)]
pub struct DefaultSchema {
    pub catalog: String,
    pub schema: String,
}
