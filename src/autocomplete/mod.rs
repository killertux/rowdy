//! SQL autocomplete engine.
//!
//! Pure-CPU, called from the UI thread on demand. Reads from a
//! `SchemaCache` populated by the worker on connect (see
//! `WorkerCommand::PrimeCompletionCache`). The hot path is:
//!
//! 1. Slice the buffer from the start of the current statement up to the
//!    cursor → `prefix`.
//! 2. `context::classify(prefix, dialect)` → which kinds of identifiers
//!    fit syntactically (keyword, table, ...) plus the partial token the
//!    user is typing.
//! 3. `engine::compute(...)` walks the cache + the curated keyword list,
//!    filters by case-insensitive prefix, sorts alphabetically, caps at
//!    `MAX_ITEMS`.
//!
//! Phase 1 ships keyword + table completion, manual `Ctrl+Space` only.
//! Column completion, fuzzy ranking, smart insert, auto-trigger come in
//! later phases.

pub mod cache;
pub mod context;
pub mod engine;
pub mod insert;
pub mod keywords;

pub use cache::{CachedTable, SchemaCache};
pub use context::classify;
pub use engine::compute;

/// One item rendered in the popover. `label` is what the user sees;
/// `insert` is what gets written into the buffer (may differ if we ever
/// add quoting/escaping in Phase 3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionItem {
    pub label: String,
    pub kind: CompletionKind,
    pub detail: Option<String>,
    pub insert: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CompletionKind {
    Keyword,
    Table,
    View,
}

impl CompletionKind {
    /// Single-char glyph rendered in the popover's first column. Kept ASCII
    /// so it lines up regardless of the terminal's font.
    pub fn icon(&self) -> char {
        match self {
            Self::Keyword => 'k',
            Self::Table => 'T',
            Self::View => 'V',
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Keyword => "keyword",
            Self::Table => "table",
            Self::View => "view",
        }
    }
}
