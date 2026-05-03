//! Candidate assembly + ranking for the popover.
//!
//! Phase 3: fuzzy ranking via `nucleo-matcher`, with a kind-aware
//! bonus so columns / tables aren't shadowed by a coincidentally-
//! shorter keyword in their natural context. Empty needle takes the
//! cheap alphabetical path (no fuzzy matching needed).

use nucleo_matcher::pattern::{AtomKind, CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

use crate::autocomplete::cache::SchemaCache;
use crate::autocomplete::context::{CompletionContext, TableBinding};
use crate::autocomplete::keywords::KEYWORDS;
use crate::autocomplete::{CachedTable, CompletionItem, CompletionKind, InsertTrail, functions};
use crate::datasource::DriverKind;
use crate::datasource::schema::TableKind;

pub const MAX_ITEMS: usize = 20;

/// Build the popover items for one keystroke. `prefix` is the partial
/// the user is typing — empty means "no filter, just the top-N
/// alphabetical items in this context."
///
/// `bindings` is every FROM/JOIN-bound table seen in the current
/// statement; the column branch unions their columns when the cursor
/// has no explicit qualifier.
pub fn compute(
    context: &CompletionContext,
    cache: &SchemaCache,
    prefix: &str,
    bindings: &[TableBinding],
    dialect: DriverKind,
) -> Vec<CompletionItem> {
    let mut items: Vec<CompletionItem> = Vec::new();
    match context {
        CompletionContext::Keyword => collect_keywords(&mut items),
        CompletionContext::Mixed => {
            collect_keywords(&mut items);
            collect_functions(&mut items, dialect);
            // Even Mixed surfaces columns when we know about FROM
            // bindings — handles the cursor between SELECT and FROM
            // when bindings have been seen.
            collect_columns_from_bindings(&mut items, cache, bindings);
            collect_cte_bindings(&mut items, bindings);
        }
        CompletionContext::Table { schema } => match schema {
            None => {
                if let Some(tables) = cache.default_tables() {
                    collect_tables(&mut items, tables);
                }
                collect_cte_bindings(&mut items, bindings);
            }
            Some(name) => {
                let key = (
                    cache.default_catalog.clone().unwrap_or_default(),
                    name.clone(),
                );
                if let Some(tables) = cache.tables.get(&key) {
                    collect_tables(&mut items, tables);
                }
            }
        },
        CompletionContext::Column { qualifier } => match qualifier {
            Some(b) => collect_columns_from_binding(&mut items, cache, b),
            None => {
                collect_columns_from_bindings(&mut items, cache, bindings);
                collect_functions(&mut items, dialect);
            }
        },
        CompletionContext::Unknown => {}
    }
    rank_and_truncate(items, prefix, context)
}

fn collect_keywords(out: &mut Vec<CompletionItem>) {
    for kw in KEYWORDS {
        out.push(CompletionItem {
            label: (*kw).to_string(),
            kind: CompletionKind::Keyword,
            detail: None,
            insert: (*kw).to_string(),
            trail: InsertTrail::None,
        });
    }
}

fn collect_tables(out: &mut Vec<CompletionItem>, tables: &[CachedTable]) {
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
            trail: InsertTrail::Space,
        });
    }
}

fn collect_columns_from_binding(
    out: &mut Vec<CompletionItem>,
    cache: &SchemaCache,
    binding: &TableBinding,
) {
    // Synthesised columns from a CTE / derived-table body skip the
    // schema cache — the projection extractor already gave us the
    // exact list this scope exposes.
    if let Some(cols) = &binding.synthetic_columns {
        for name in cols {
            out.push(CompletionItem {
                label: name.clone(),
                kind: CompletionKind::Column,
                detail: Some(format!("from {}", binding.table)),
                insert: name.clone(),
                trail: InsertTrail::None,
            });
        }
        return;
    }
    let key = (
        binding.catalog.clone(),
        binding.schema.clone(),
        binding.table.clone(),
    );
    let Some(cols) = cache.columns.get(&key) else {
        return;
    };
    for c in cols {
        out.push(CompletionItem {
            label: c.name.clone(),
            kind: CompletionKind::Column,
            detail: Some(c.type_name.clone()),
            insert: c.name.clone(),
            trail: InsertTrail::None,
        });
    }
}

fn collect_columns_from_bindings(
    out: &mut Vec<CompletionItem>,
    cache: &SchemaCache,
    bindings: &[TableBinding],
) {
    use std::collections::HashSet;
    let mut seen = HashSet::new();
    for b in bindings {
        let key = (b.catalog.clone(), b.schema.clone(), b.table.clone());
        let Some(cols) = cache.columns.get(&key) else {
            continue;
        };
        for c in cols {
            // Tag duplicate names from different tables with their
            // table — `id` shows up as `id` (first time) then `id @
            // posts` if posts also has one.
            let label = if seen.insert(c.name.to_lowercase()) {
                c.name.clone()
            } else {
                format!("{} @ {}", c.name, b.table)
            };
            out.push(CompletionItem {
                label,
                kind: CompletionKind::Column,
                detail: Some(format!("{} · {}", c.type_name, b.table)),
                insert: c.name.clone(),
                trail: InsertTrail::None,
            });
        }
    }
}

fn collect_functions(out: &mut Vec<CompletionItem>, dialect: DriverKind) {
    use crate::autocomplete::functions::FnArity;
    for f in functions::for_dialect(dialect) {
        // Three modes: arg-taking (insert bare name + trail OpenParens
        // so the action layer adds `()` with cursor between them);
        // zero-arg parenthesised (insert `name()`, no trail);
        // bare-name value function (insert just the name, no trail —
        // SQLite rejects parens on `CURRENT_TIMESTAMP` etc.).
        let (insert, trail) = match f.arity {
            FnArity::Args => (f.name.to_string(), InsertTrail::OpenParens),
            FnArity::ZeroArgParens => (format!("{}()", f.name), InsertTrail::None),
            FnArity::Bare => (f.name.to_string(), InsertTrail::None),
        };
        out.push(CompletionItem {
            label: f.name.to_string(),
            kind: CompletionKind::Function,
            detail: Some(f.signature.to_string()),
            insert,
            trail,
        });
    }
}

/// Surface CTE bindings (collected by `context::collect_bindings`)
/// in table contexts so `WITH x AS (...) SELECT * FROM x|` completes
/// `x` as a candidate. We can't reach into the CTE body to extract
/// columns yet, so column completion against a CTE qualifier returns
/// nothing — the binding lookup still resolves but the cache has no
/// entry. Phase 5 can parse the body.
fn collect_cte_bindings(out: &mut Vec<CompletionItem>, bindings: &[TableBinding]) {
    for b in bindings {
        // CTE bindings are flagged by an empty schema *and* an empty
        // catalog, since `collect_bindings` only sets that pair when
        // a CTE was detected (real tables get the resolve defaults).
        if b.is_cte() {
            out.push(CompletionItem {
                label: b.table.clone(),
                kind: CompletionKind::Cte,
                detail: Some("WITH …".into()),
                insert: b.table.clone(),
                trail: InsertTrail::Space,
            });
        }
    }
}

/// Score, sort, truncate.
///
/// Both branches apply the kind bonus so that, in a column slot, the
/// columns rank ahead of functions/keywords even when nothing has
/// been typed yet. Without this, the alphabetical path would push
/// real columns off the top-N when functions sort earlier.
///
/// - **Empty needle** → kind bonus + alphabetical. Skips fuzzy
///   matching entirely.
/// - **Non-empty needle** → fuzzy score from `nucleo-matcher` plus
///   the same kind bonus and a prefix bonus so the user's exact
///   prefix wins over deep subsequence matches. Tiebreakers cascade
///   through label length and alphabetical order so the result is
///   deterministic.
fn rank_and_truncate(
    items: Vec<CompletionItem>,
    needle: &str,
    context: &CompletionContext,
) -> Vec<CompletionItem> {
    if needle.is_empty() {
        let mut items = items;
        items.sort_by(|a, b| {
            kind_bonus(b.kind, context)
                .cmp(&kind_bonus(a.kind, context))
                .then_with(|| a.label.to_lowercase().cmp(&b.label.to_lowercase()))
                .then_with(|| a.label.cmp(&b.label))
        });
        items.truncate(MAX_ITEMS);
        return items;
    }
    let mut config = Config::DEFAULT;
    config.prefer_prefix = true;
    let mut matcher = Matcher::new(config);
    let pattern = Pattern::new(
        needle,
        CaseMatching::Ignore,
        Normalization::Smart,
        AtomKind::Fuzzy,
    );

    let mut scored: Vec<(i64, CompletionItem)> = items
        .into_iter()
        .filter_map(|item| {
            let mut buf = Vec::new();
            let h = Utf32Str::new(&item.label, &mut buf);
            let raw = pattern.score(h, &mut matcher)?;
            let combined = combine_score(raw, &item, context, needle);
            Some((combined, item))
        })
        .collect();

    scored.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| a.1.label.chars().count().cmp(&b.1.label.chars().count()))
            .then_with(|| a.1.label.to_lowercase().cmp(&b.1.label.to_lowercase()))
            .then_with(|| a.1.label.cmp(&b.1.label))
    });
    scored.truncate(MAX_ITEMS);
    scored.into_iter().map(|(_, i)| i).collect()
}

/// Bonus by `(kind, context)` pair. Higher = more relevant to the
/// cursor's syntactic slot. Used by both the empty-needle alphabetical
/// path and the fuzzy-scored path.
fn kind_bonus(kind: CompletionKind, context: &CompletionContext) -> i64 {
    match (kind, context) {
        (CompletionKind::Column, CompletionContext::Column { .. }) => 1000,
        (CompletionKind::Column, CompletionContext::Mixed) => 200,
        (CompletionKind::Table | CompletionKind::View, CompletionContext::Table { .. }) => 1000,
        (CompletionKind::Cte, CompletionContext::Table { .. }) => 1100,
        (CompletionKind::Keyword, CompletionContext::Keyword) => 100,
        (CompletionKind::Function, CompletionContext::Mixed) => 50,
        _ => 0,
    }
}

/// Apply kind- and prefix-aware bonuses on top of nucleo's raw score.
fn combine_score(
    raw: u32,
    item: &CompletionItem,
    context: &CompletionContext,
    needle: &str,
) -> i64 {
    let mut score = raw as i64;
    score += kind_bonus(item.kind, context);
    // Exact case-insensitive prefix wins over a deep subsequence
    // match — matches what the user is *typing*, not what the
    // tokenizer happens to discover later in the label.
    let prefix_bonus = if item
        .label
        .to_lowercase()
        .starts_with(&needle.to_lowercase())
    {
        500
    } else {
        0
    };
    score += prefix_bonus;
    score
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::autocomplete::CachedColumn;

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

    fn cache_with_columns(table: &str, cols: &[(&str, &str)]) -> SchemaCache {
        let mut cache = cache_with_tables(&[table]);
        cache.columns.insert(
            ("main".into(), "main".into(), table.to_string()),
            cols.iter()
                .map(|(n, t)| CachedColumn {
                    name: (*n).to_string(),
                    type_name: (*t).to_string(),
                })
                .collect(),
        );
        cache
    }

    fn binding(table: &str) -> TableBinding {
        TableBinding {
            catalog: "main".into(),
            schema: "main".into(),
            table: table.to_string(),
            is_cte: false,
            synthetic_columns: None,
        }
    }

    fn compute_sqlite(
        context: &CompletionContext,
        cache: &SchemaCache,
        prefix: &str,
        bindings: &[TableBinding],
    ) -> Vec<CompletionItem> {
        compute(context, cache, prefix, bindings, DriverKind::Sqlite)
    }

    #[test]
    fn keyword_context_filters_by_prefix() {
        let cache = SchemaCache::new();
        let items = compute_sqlite(&CompletionContext::Keyword, &cache, "SELE", &[]);
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
        let items = compute_sqlite(&CompletionContext::Keyword, &cache, "", &[]);
        assert_eq!(items.len(), MAX_ITEMS);
        assert_eq!(items[0].label, "ALL");
    }

    #[test]
    fn table_context_returns_default_schema_tables() {
        let cache = cache_with_tables(&["users", "user_roles", "orders"]);
        let items = compute_sqlite(
            &CompletionContext::Table { schema: None },
            &cache,
            "user",
            &[],
        );
        let labels: Vec<_> = items.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(labels, vec!["users", "user_roles"]);
    }

    #[test]
    fn schema_qualified_table_context_uses_named_schema() {
        let mut cache = cache_with_tables(&[]);
        cache.tables.insert(
            ("main".into(), "other".into()),
            vec![CachedTable {
                name: "products".into(),
                kind: TableKind::Table,
            }],
        );
        let items = compute_sqlite(
            &CompletionContext::Table {
                schema: Some("other".into()),
            },
            &cache,
            "",
            &[],
        );
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, "products");
    }

    #[test]
    fn qualified_column_returns_only_that_tables_columns() {
        let cache = cache_with_columns("users", &[("id", "INTEGER"), ("name", "TEXT")]);
        let items = compute_sqlite(
            &CompletionContext::Column {
                qualifier: Some(binding("users")),
            },
            &cache,
            "",
            &[],
        );
        let labels: Vec<_> = items.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(labels, vec!["id", "name"]);
        assert!(items.iter().all(|i| i.kind == CompletionKind::Column));
    }

    #[test]
    fn qualified_column_returns_nothing_when_uncached() {
        let cache = cache_with_tables(&["users"]);
        let items = compute_sqlite(
            &CompletionContext::Column {
                qualifier: Some(binding("users")),
            },
            &cache,
            "",
            &[],
        );
        assert!(items.is_empty());
    }

    #[test]
    fn unqualified_column_unions_bindings_and_dedupes_overlap() {
        let mut cache = cache_with_columns("users", &[("id", "INTEGER"), ("name", "TEXT")]);
        cache.columns.insert(
            ("main".into(), "main".into(), "posts".into()),
            vec![
                CachedColumn {
                    name: "id".into(),
                    type_name: "INTEGER".into(),
                },
                CachedColumn {
                    name: "title".into(),
                    type_name: "TEXT".into(),
                },
            ],
        );
        let bindings = vec![binding("users"), binding("posts")];
        let items = compute_sqlite(
            &CompletionContext::Column { qualifier: None },
            &cache,
            "",
            &bindings,
        );
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"id"), "{labels:?}");
        assert!(labels.contains(&"id @ posts"), "{labels:?}");
        assert!(labels.contains(&"name"), "{labels:?}");
        assert!(labels.contains(&"title"), "{labels:?}");
    }

    #[test]
    fn case_insensitive_prefix_matches_either_case() {
        let cache = cache_with_tables(&["Users", "USERS_LOG"]);
        let items = compute_sqlite(
            &CompletionContext::Table { schema: None },
            &cache,
            "us",
            &[],
        );
        let labels: Vec<_> = items.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(labels, vec!["Users", "USERS_LOG"]);
    }

    #[test]
    fn fuzzy_subsequence_matches_with_gaps() {
        let cache = cache_with_tables(&["user_roles", "uncategorised", "orders"]);
        let items = compute_sqlite(
            &CompletionContext::Table { schema: None },
            &cache,
            "ur",
            &[],
        );
        let labels: Vec<_> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"user_roles"), "{labels:?}");
        assert!(!labels.contains(&"orders"), "{labels:?}");
    }

    #[test]
    fn kind_boost_promotes_columns_in_column_context() {
        let cache = cache_with_columns("users", &[("id", "INTEGER")]);
        let bindings = vec![binding("users")];
        let items = compute_sqlite(
            &CompletionContext::Column { qualifier: None },
            &cache,
            "id",
            &bindings,
        );
        assert_eq!(items[0].label, "id");
        assert_eq!(items[0].kind, CompletionKind::Column);
    }

    #[test]
    fn keyword_context_excludes_non_matching_keywords() {
        let cache = SchemaCache::new();
        let items = compute_sqlite(&CompletionContext::Keyword, &cache, "nope_xyz", &[]);
        assert!(items.is_empty(), "{items:?}");
    }

    #[test]
    fn function_appears_in_mixed_context_with_arg_trail() {
        let cache = SchemaCache::new();
        let items = compute(
            &CompletionContext::Mixed,
            &cache,
            "COUN",
            &[],
            DriverKind::Sqlite,
        );
        let count = items
            .iter()
            .find(|i| i.label == "COUNT")
            .expect("COUNT in items");
        assert_eq!(count.kind, CompletionKind::Function);
        assert_eq!(count.insert, "COUNT");
        assert_eq!(count.trail, InsertTrail::OpenParens);
    }

    #[test]
    fn zero_arg_function_inserts_with_parens_and_no_trail() {
        let cache = SchemaCache::new();
        let items = compute(
            &CompletionContext::Mixed,
            &cache,
            "NOW",
            &[],
            DriverKind::Postgres,
        );
        let now = items
            .iter()
            .find(|i| i.label == "NOW")
            .expect("NOW in items");
        assert_eq!(now.kind, CompletionKind::Function);
        assert_eq!(now.insert, "NOW()");
        assert_eq!(now.trail, InsertTrail::None);
    }

    #[test]
    fn bare_function_inserts_without_parens() {
        // SQLite rejects `CURRENT_TIMESTAMP()` — the popover must
        // surface the bare form on every supported dialect.
        for dialect in [DriverKind::Sqlite, DriverKind::Postgres, DriverKind::Mysql] {
            let cache = SchemaCache::new();
            let items = compute(&CompletionContext::Mixed, &cache, "CURRENT_T", &[], dialect);
            let ts = items
                .iter()
                .find(|i| i.label == "CURRENT_TIMESTAMP")
                .unwrap_or_else(|| panic!("CURRENT_TIMESTAMP missing for {dialect:?}"));
            assert_eq!(ts.kind, CompletionKind::Function);
            assert_eq!(
                ts.insert, "CURRENT_TIMESTAMP",
                "no parens on bare function for {dialect:?}"
            );
            assert_eq!(ts.trail, InsertTrail::None);
        }
    }

    #[test]
    fn synthetic_columns_short_circuit_cache_lookup() {
        // No cache entry for the binding's (catalog, schema, table)
        // — but synthetic_columns is populated. The engine must
        // surface those names instead of falling through to an empty
        // result.
        let cache = SchemaCache::new();
        let bindings = vec![TableBinding {
            catalog: String::new(),
            schema: String::new(),
            table: "u".into(),
            is_cte: true,
            synthetic_columns: Some(vec!["id".into(), "email".into()]),
        }];
        let items = compute_sqlite(
            &CompletionContext::Column {
                qualifier: Some(bindings[0].clone()),
            },
            &cache,
            "",
            &bindings,
        );
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        // Empty-needle path sorts kind-then-alphabetical, so the
        // exact order is alphabetised.
        assert_eq!(labels, vec!["email", "id"]);
        assert!(items.iter().all(|i| i.kind == CompletionKind::Column));
    }

    #[test]
    fn cte_binding_surfaces_in_table_context() {
        let cache = SchemaCache::new();
        let bindings = vec![TableBinding {
            catalog: String::new(),
            schema: String::new(),
            table: "recent".into(),
            is_cte: true,
            synthetic_columns: None,
        }];
        let items = compute(
            &CompletionContext::Table { schema: None },
            &cache,
            "rec",
            &bindings,
            DriverKind::Sqlite,
        );
        let cte = items
            .iter()
            .find(|i| i.label == "recent")
            .expect("CTE name in items");
        assert_eq!(cte.kind, CompletionKind::Cte);
    }
}
