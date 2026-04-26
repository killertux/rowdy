//! Candidate assembly + ranking for the popover.
//!
//! Phase 1: case-insensitive prefix match, alphabetical sort, top
//! `MAX_ITEMS`. No fuzzy matching, no usage frequency, no boosts. The
//! interface is shaped to swap in `nucleo-matcher` (Phase 3) by
//! changing only this file.

use crate::autocomplete::cache::SchemaCache;
use crate::autocomplete::context::CompletionContext;
use crate::autocomplete::keywords::KEYWORDS;
use crate::autocomplete::{CompletionItem, CompletionKind};
use crate::datasource::schema::TableKind;

pub const MAX_ITEMS: usize = 20;

/// Build the popover items for one keystroke. `prefix` is the partial
/// the user is typing — empty means "no filter, just the top-N
/// alphabetical items in this context."
pub fn compute(
    context: &CompletionContext,
    cache: &SchemaCache,
    prefix: &str,
) -> Vec<CompletionItem> {
    let mut items: Vec<CompletionItem> = Vec::new();
    match context {
        CompletionContext::Keyword | CompletionContext::Mixed => collect_keywords(&mut items),
        CompletionContext::Table { schema: _ } => {
            // Phase 1: only the default schema's tables are eagerly
            // populated; qualified contexts produce no results yet.
            if let Some(tables) = cache.default_tables() {
                collect_tables(&mut items, tables);
            }
        }
        CompletionContext::Unknown => {}
    }
    rank_and_truncate(items, prefix)
}

fn collect_keywords(out: &mut Vec<CompletionItem>) {
    for kw in KEYWORDS {
        out.push(CompletionItem {
            label: (*kw).to_string(),
            kind: CompletionKind::Keyword,
            detail: None,
            insert: (*kw).to_string(),
        });
    }
}

fn collect_tables(out: &mut Vec<CompletionItem>, tables: &[crate::autocomplete::CachedTable]) {
    for t in tables {
        let kind = match t.kind {
            TableKind::Table => CompletionKind::Table,
            TableKind::View => CompletionKind::View,
        };
        out.push(CompletionItem {
            label: t.name.clone(),
            kind,
            detail: None,
            insert: t.name.clone(),
        });
    }
}

/// Filter by case-insensitive prefix, then sort case-insensitively, then
/// truncate. With an empty prefix, every candidate matches and we still
/// return the top `MAX_ITEMS` alphabetically.
fn rank_and_truncate(items: Vec<CompletionItem>, prefix: &str) -> Vec<CompletionItem> {
    let prefix_lower = prefix.to_lowercase();
    let mut filtered: Vec<CompletionItem> = if prefix_lower.is_empty() {
        items
    } else {
        items
            .into_iter()
            .filter(|i| i.label.to_lowercase().starts_with(&prefix_lower))
            .collect()
    };
    filtered.sort_by(|a, b| {
        a.label
            .to_lowercase()
            .cmp(&b.label.to_lowercase())
            .then_with(|| a.label.cmp(&b.label))
    });
    filtered.truncate(MAX_ITEMS);
    filtered
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::autocomplete::CachedTable;

    fn cache_with_tables(names: &[&str]) -> SchemaCache {
        let mut cache = SchemaCache::new();
        cache.default_catalog = Some("main".into());
        cache.default_schema = Some("main".into());
        cache.tables.insert(
            ("main".into(), "main".into()),
            names
                .iter()
                .map(|n| CachedTable {
                    name: (*n).to_string(),
                    kind: TableKind::Table,
                })
                .collect(),
        );
        cache
    }

    #[test]
    fn keyword_context_filters_by_prefix() {
        let cache = SchemaCache::new();
        let items = compute(&CompletionContext::Keyword, &cache, "SELE");
        assert!(!items.is_empty());
        assert!(
            items
                .iter()
                .all(|i| i.label.to_lowercase().starts_with("sele"))
        );
        assert!(items.iter().any(|i| i.label == "SELECT"));
    }

    #[test]
    fn keyword_context_empty_prefix_returns_top_n() {
        let cache = SchemaCache::new();
        let items = compute(&CompletionContext::Keyword, &cache, "");
        assert_eq!(items.len(), MAX_ITEMS);
        // Alphabetical: ALL is the first when starting from a curated set.
        assert_eq!(items[0].label, "ALL");
    }

    #[test]
    fn table_context_returns_default_schema_tables() {
        let cache = cache_with_tables(&["users", "user_roles", "orders"]);
        let items = compute(&CompletionContext::Table { schema: None }, &cache, "user");
        let labels: Vec<_> = items.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(labels, vec!["user_roles", "users"]);
        assert!(items.iter().all(|i| i.kind == CompletionKind::Table));
    }

    #[test]
    fn table_context_without_cache_returns_nothing() {
        let cache = SchemaCache::new();
        let items = compute(&CompletionContext::Table { schema: None }, &cache, "u");
        assert!(items.is_empty());
    }

    #[test]
    fn case_insensitive_prefix_matches_either_case() {
        let cache = cache_with_tables(&["Users", "USERS_LOG"]);
        let items = compute(&CompletionContext::Table { schema: None }, &cache, "us");
        let labels: Vec<_> = items.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(labels, vec!["Users", "USERS_LOG"]);
    }
}
