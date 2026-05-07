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
pub mod ddl;
pub mod engine;
pub mod functions;
pub mod insert;
pub mod keywords;

pub use cache::{CachedColumn, CachedTable, SchemaCache};
pub use context::{ClassifyResult, CompletionContext, ResolveContext, TableBinding, classify};
pub use engine::compute;

/// One item rendered in the popover. `label` is what the user sees;
/// `insert` is the raw identifier text — the action layer wraps it in
/// dialect-appropriate quotes when needed (Phase 3). `trail` is what
/// gets appended after the inserted text and where the cursor lands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionItem {
    pub label: String,
    pub kind: CompletionKind,
    pub detail: Option<String>,
    pub insert: String,
    pub trail: InsertTrail,
}

/// Post-insert cursor placement. Lives on each item rather than being
/// derived at accept-time so the engine can encode per-item nuances
/// (zero-arg vs arg functions, e.g.).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InsertTrail {
    /// Cursor lands right after the inserted text. Default for
    /// keywords, columns.
    None,
    /// Append `()` and put the cursor *between* the parens, ready
    /// for arguments. Used for arg-taking functions like `COUNT(`.
    OpenParens,
    /// Append a single space. Used for table/view/CTE completions
    /// so an optional alias can be typed or accepted next.
    Space,
    /// Append a space and a generated short alias (e.g. ` u` for
    /// `users`). Reserved for future alias-on-accept flows.
    #[allow(dead_code)]
    Alias(String),
}

/// Generate a short alias from a table name by taking the first
/// lowercased letter of each underscore-separated segment.
/// `"user_roles"` → `"ur"`, `"users"` → `"u"`, `"AB_CD"` → `"ac"`.
pub fn generate_alias(table_name: &str) -> String {
    let mut alias = String::new();
    for segment in table_name.split('_') {
        if let Some(c) = segment.chars().next() {
            alias.push(c.to_ascii_lowercase());
        }
    }
    alias
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CompletionKind {
    Keyword,
    Table,
    View,
    Column,
    /// SQL function (built-in or dialect-specific). Inserts with `()`
    /// and the cursor placed between them (or at the end for
    /// zero-arg functions).
    Function,
    /// CTE name introduced by a `WITH … AS (…)` clause. Appears in
    /// table contexts as a candidate; column completion against it
    /// returns empty until we parse the CTE body (Phase 5).
    Cte,
    /// Placeholder shown while a lazy column load is in flight. Not
    /// acceptable — the action layer drops accept events for this kind
    /// rather than inserting the placeholder text.
    Loading,
}

impl CompletionKind {
    /// Single-char glyph rendered in the popover's first column. Kept ASCII
    /// so it lines up regardless of the terminal's font.
    pub fn icon(&self) -> char {
        match self {
            Self::Keyword => 'k',
            Self::Table => 'T',
            Self::View => 'V',
            Self::Column => 'c',
            Self::Function => 'f',
            Self::Cte => 'C',
            Self::Loading => '.',
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Keyword => "keyword",
            Self::Table => "table",
            Self::View => "view",
            Self::Column => "column",
            Self::Function => "function",
            Self::Cte => "cte",
            Self::Loading => "loading",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alias_single_word() {
        assert_eq!(generate_alias("users"), "u");
        assert_eq!(generate_alias("products"), "p");
    }

    #[test]
    fn alias_underscore_segments() {
        assert_eq!(generate_alias("user_roles"), "ur");
        assert_eq!(generate_alias("order_items"), "oi");
        assert_eq!(generate_alias("user_role_permissions"), "urp");
    }

    #[test]
    fn alias_lowercases() {
        assert_eq!(generate_alias("AB_CD"), "ac");
        assert_eq!(generate_alias("Users"), "u");
    }

    #[test]
    fn alias_empty_name() {
        assert_eq!(generate_alias(""), "");
    }

    #[test]
    fn alias_single_char() {
        assert_eq!(generate_alias("a"), "a");
    }
}
