//! Tokenize-only SQL context classifier.
//!
//! Given a statement and a cursor byte-offset within it, decides what
//! kinds of identifiers fit syntactically here. The result drives which
//! candidates `engine::compute` returns.
//!
//! We never invoke the AST parser — partial input usually doesn't parse,
//! and we don't need a tree, just the local "what came before me" view.
//! sqlparser's tokenizer handles dialect-specific quoting, comments, and
//! string escapes for us; on a tokenize error we fall back to
//! `CompletionContext::Mixed` so the user still gets keywords.
//!
//! ## Phase 2 additions
//!
//! - **`Column` context** — fires after a SELECT projection /
//!   `WHERE` / `ON` / etc. With a `Qualifier` it's columns of a
//!   specific FROM-bound table; without one, columns of every binding
//!   currently in scope.
//! - **Schema-qualified `Table` context** — `FROM <schema>.<partial>`
//!   pins the popover to tables in `<schema>`.
//! - **FROM/JOIN alias resolution** — a forward pass over the *whole*
//!   statement (not just up-to-cursor) so `SELECT u.|` autocompletes
//!   even when the FROM clause comes after the cursor. Subqueries /
//!   CTEs are out of scope (Phase 4).

use std::collections::HashMap;

use sqlparser::keywords::Keyword;
use sqlparser::tokenizer::{Token, TokenWithSpan, Tokenizer, Whitespace, Word};

use crate::datasource::DriverKind;
use crate::sql_infer::dialect_for;

/// What the popover should populate for the cursor's syntactic
/// position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompletionContext {
    /// Start of a statement, or after `;`. Suggest top-level keywords.
    Keyword,
    /// Position where a table name fits — after FROM, JOIN, INTO,
    /// UPDATE, TABLE. `schema` is `Some(name)` when the user has typed
    /// `<schema>.<partial>`, narrowing the suggestion list to one
    /// schema; `None` for the default-schema case.
    Table { schema: Option<String> },
    /// Position where a column name fits. `qualifier` is `Some(...)`
    /// when the user typed `<alias_or_table>.<partial>`, narrowing the
    /// suggestion list to one bound table. `None` means "all FROM
    /// bindings in scope" (typical SELECT projection / WHERE).
    Column { qualifier: Option<TableBinding> },
    /// SELECT-projection-or-similar, but no FROM bindings have been
    /// collected yet — falls back to keywords + columns from any
    /// resolvable bindings.
    Mixed,
    /// Tokenize error or cursor inside a string/comment. The popover
    /// is suppressed in this state unless the user manually triggers.
    Unknown,
}

/// What a `<qualifier>.<partial>` resolves to: a specific
/// `(catalog, schema, table)` triple that the engine can look up in
/// the column cache. CTE bindings are tagged so the engine can route
/// them differently (column completion against a CTE returns nothing
/// until we parse the CTE body).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableBinding {
    pub catalog: String,
    pub schema: String,
    pub table: String,
    /// `true` for `WITH <name> AS (…)` bindings. Phase 4 surfaces
    /// these as CTE table candidates; column completion against them
    /// returns empty pending Phase 5 (parse the CTE body).
    pub is_cte: bool,
}

impl TableBinding {
    pub fn is_cte(&self) -> bool {
        self.is_cte
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassifyResult {
    pub context: CompletionContext,
    /// Identifier prefix the user is currently typing (may be empty —
    /// e.g. cursor right after `FROM `). Always sliced from the input,
    /// so multi-byte chars don't get split.
    pub partial: String,
    /// Byte length of the partial in the original statement string;
    /// caller subtracts this from the cursor offset to get the
    /// replacement-range start.
    pub partial_byte_len: usize,
    /// All `(catalog, schema, table)` triples this statement references
    /// via FROM/JOIN. Used by the action layer to fire lazy column
    /// loads regardless of where the cursor sits.
    pub bindings: Vec<TableBinding>,
}

/// Caller-supplied hints for resolving unqualified table names.
#[derive(Debug, Clone, Copy)]
pub struct ResolveContext<'a> {
    pub default_catalog: Option<&'a str>,
    pub default_schema: Option<&'a str>,
}

impl ResolveContext<'_> {
    /// `None`/`None` resolver — used in tests where the cache hasn't
    /// been seeded with default schema info. Production callers always
    /// supply real defaults.
    #[cfg(test)]
    pub const fn empty() -> ResolveContext<'static> {
        ResolveContext {
            default_catalog: None,
            default_schema: None,
        }
    }
}

/// Classify the cursor position. `cursor` is a *byte* offset within
/// `statement`. Binding extraction tokenizes the whole `statement` so
/// FROM/JOIN clauses past the cursor still contribute aliases —
/// crucial when the user writes the skeleton first and fills in
/// SELECT later.
pub fn classify(
    statement: &str,
    cursor: usize,
    dialect: DriverKind,
    resolve: ResolveContext,
) -> ClassifyResult {
    let cursor = cursor.min(statement.len());
    let prefix = &statement[..cursor];
    let partial = extract_partial(prefix);
    let partial_byte_len = partial.len();
    let head_end = cursor - partial_byte_len;
    let head = &statement[..head_end];

    let dialect_obj = dialect_for(dialect);
    let head_tokens = match Tokenizer::new(&*dialect_obj, head).tokenize_with_location() {
        Ok(t) => t,
        Err(_) => {
            return ClassifyResult {
                context: CompletionContext::Unknown,
                partial: partial.to_string(),
                partial_byte_len,
                bindings: Vec::new(),
            };
        }
    };

    let full_tokens = Tokenizer::new(&*dialect_obj, statement)
        .tokenize_with_location()
        .unwrap_or_default();
    let bindings_map = collect_bindings(&full_tokens, &resolve);
    let bindings: Vec<TableBinding> = bindings_map.values().cloned().collect();

    let context = classify_from_tokens(&head_tokens, &bindings_map);
    ClassifyResult {
        context,
        partial: partial.to_string(),
        partial_byte_len,
        bindings,
    }
}

fn classify_from_tokens(
    tokens: &[TokenWithSpan],
    bindings: &HashMap<String, TableBinding>,
) -> CompletionContext {
    let mut iter = tokens.iter().rev().filter(|t| !is_trivia(&t.token));

    let Some(t1) = iter.next() else {
        return CompletionContext::Keyword;
    };
    let t2 = iter.next();
    let t3 = iter.next();

    // `<qualifier> . <partial>` shape — the partial has already been
    // stripped from `tokens`, so the trailing token is `.`.
    if matches!(t1.token, Token::Period)
        && let Some(t2) = t2
        && let Token::Word(qualifier) = &t2.token
    {
        return classify_qualified(qualifier, t3, bindings);
    }

    match &t1.token {
        Token::SemiColon => CompletionContext::Keyword,
        Token::Word(word) => match word.keyword {
            Keyword::FROM | Keyword::JOIN | Keyword::INTO | Keyword::UPDATE | Keyword::TABLE => {
                CompletionContext::Table { schema: None }
            }
            // Operators / clauses that mark a column-expression slot.
            Keyword::SELECT
            | Keyword::WHERE
            | Keyword::ON
            | Keyword::AND
            | Keyword::OR
            | Keyword::HAVING
            | Keyword::ORDER
            | Keyword::GROUP
            | Keyword::BY
            | Keyword::SET
            | Keyword::USING
            | Keyword::IN => column_or_mixed(bindings),
            _ => CompletionContext::Mixed,
        },
        Token::Comma | Token::LParen => column_or_mixed(bindings),
        Token::Eq
        | Token::Neq
        | Token::Lt
        | Token::Gt
        | Token::LtEq
        | Token::GtEq
        | Token::Plus
        | Token::Minus
        | Token::Mul
        | Token::Div => column_or_mixed(bindings),
        _ => CompletionContext::Mixed,
    }
}

fn classify_qualified(
    qualifier: &Word,
    third: Option<&TokenWithSpan>,
    bindings: &HashMap<String, TableBinding>,
) -> CompletionContext {
    let qname = qualifier.value.as_str();
    // `FROM <schema>.` / `JOIN <schema>.` shape — the qualifier is a
    // schema name regardless of whether the binding pass also picked
    // it up as a table reference (it might, transiently, while the
    // user is still typing).
    if let Some(third) = third
        && let Token::Word(prev) = &third.token
        && matches!(
            prev.keyword,
            Keyword::FROM | Keyword::JOIN | Keyword::INTO | Keyword::UPDATE | Keyword::TABLE
        )
    {
        return CompletionContext::Table {
            schema: Some(qname.to_string()),
        };
    }
    if let Some(binding) = lookup_binding(bindings, qname) {
        return CompletionContext::Column {
            qualifier: Some(binding.clone()),
        };
    }
    CompletionContext::Mixed
}

fn column_or_mixed(bindings: &HashMap<String, TableBinding>) -> CompletionContext {
    if bindings.is_empty() {
        CompletionContext::Mixed
    } else {
        CompletionContext::Column { qualifier: None }
    }
}

fn lookup_binding<'a>(
    bindings: &'a HashMap<String, TableBinding>,
    name: &str,
) -> Option<&'a TableBinding> {
    if let Some(b) = bindings.get(name) {
        return Some(b);
    }
    let lower = name.to_lowercase();
    bindings
        .iter()
        .find(|(k, _)| k.to_lowercase() == lower)
        .map(|(_, v)| v)
}

/// Forward pass over the statement that collects every
/// `FROM <table> [AS] <alias>?`, `JOIN <table> [AS] <alias>?`, and
/// `WITH <name> AS (…)` (CTE) introduction. Keys are the alias if one
/// was supplied (FROM/JOIN), the CTE name (WITH), or the bare table
/// name — what the user types as a qualifier.
fn collect_bindings(
    tokens: &[TokenWithSpan],
    resolve: &ResolveContext,
) -> HashMap<String, TableBinding> {
    let mut out = HashMap::new();
    let toks: Vec<&Token> = tokens.iter().map(|t| &t.token).collect();
    // Sweep for CTE definitions first so a later FROM <cte> doesn't
    // overwrite the CTE binding with a freshly-resolved table one.
    collect_cte_bindings(&toks, &mut out);
    let mut i = 0;
    while i < toks.len() {
        let is_intro = matches!(
            toks[i],
            Token::Word(w) if matches!(w.keyword, Keyword::FROM | Keyword::JOIN | Keyword::UPDATE)
        );
        if !is_intro {
            i += 1;
            continue;
        }
        i += 1;
        skip_trivia(&toks, &mut i);
        let Some(parts) = take_dotted_ident(&toks, &mut i) else {
            continue;
        };
        // Skip if the table reference matches an already-collected CTE
        // — the CTE binding wins.
        if parts.len() == 1 && out.get(&parts[0]).is_some_and(|b| b.is_cte) {
            // Still consume the optional alias to keep the cursor
            // accurate, but don't overwrite the CTE entry.
            skip_trivia(&toks, &mut i);
            if let Some(Token::Word(w)) = toks.get(i)
                && w.keyword == Keyword::AS
            {
                i += 1;
                skip_trivia(&toks, &mut i);
            }
            if let Some(Token::Word(w)) = toks.get(i)
                && !is_table_terminator(w)
            {
                i += 1;
            }
            continue;
        }
        let binding = resolve_binding(&parts, resolve);
        skip_trivia(&toks, &mut i);
        if let Some(Token::Word(w)) = toks.get(i)
            && w.keyword == Keyword::AS
        {
            i += 1;
            skip_trivia(&toks, &mut i);
        }
        let alias = if let Some(Token::Word(w)) = toks.get(i) {
            if is_table_terminator(w) {
                None
            } else {
                let v = w.value.clone();
                i += 1;
                Some(v)
            }
        } else {
            None
        };
        let key = alias.clone().unwrap_or_else(|| binding.table.clone());
        out.insert(key, binding);
    }
    out
}

/// Walk the token stream looking for `WITH <name> [AS] (...)` or
/// `, <name> [AS] (...)` immediately following a WITH clause. Each
/// CTE name becomes a binding tagged with `is_cte: true`. We stop
/// at the first non-CTE keyword (SELECT/INSERT/UPDATE/DELETE) so a
/// WITH that's part of, say, an UPDATE doesn't bleed into following
/// statements.
fn collect_cte_bindings(toks: &[&Token], out: &mut HashMap<String, TableBinding>) {
    let mut i = 0;
    while i < toks.len() {
        let with_here = matches!(
            toks[i],
            Token::Word(w) if w.keyword == Keyword::WITH
        );
        if !with_here {
            i += 1;
            continue;
        }
        i += 1;
        // Some dialects allow `WITH RECURSIVE`. Skip it.
        skip_trivia(toks, &mut i);
        if let Some(Token::Word(w)) = toks.get(i)
            && w.keyword == Keyword::RECURSIVE
        {
            i += 1;
        }
        loop {
            skip_trivia(toks, &mut i);
            let Some(Token::Word(name_word)) = toks.get(i) else {
                break;
            };
            // `WITH SELECT …` doesn't start a CTE — bail.
            if matches!(
                name_word.keyword,
                Keyword::SELECT | Keyword::INSERT | Keyword::UPDATE | Keyword::DELETE
            ) {
                break;
            }
            let name = name_word.value.clone();
            i += 1;
            skip_trivia(toks, &mut i);
            // Optional column list `(c1, c2, …)` — skip.
            if matches!(toks.get(i), Some(Token::LParen)) {
                if !skip_balanced_parens(toks, &mut i) {
                    break;
                }
                skip_trivia(toks, &mut i);
            }
            // Optional `AS`.
            if let Some(Token::Word(w)) = toks.get(i)
                && w.keyword == Keyword::AS
            {
                i += 1;
                skip_trivia(toks, &mut i);
            }
            // Body — must be a parenthesised SELECT. Skip past it.
            if matches!(toks.get(i), Some(Token::LParen)) && !skip_balanced_parens(toks, &mut i) {
                break;
            }
            out.insert(
                name.clone(),
                TableBinding {
                    catalog: String::new(),
                    schema: String::new(),
                    table: name,
                    is_cte: true,
                },
            );
            skip_trivia(toks, &mut i);
            // More CTEs follow as `, <name> AS (...)`. Anything else
            // ends the WITH chain.
            if matches!(toks.get(i), Some(Token::Comma)) {
                i += 1;
                continue;
            }
            break;
        }
    }
}

fn skip_balanced_parens(toks: &[&Token], i: &mut usize) -> bool {
    if !matches!(toks.get(*i), Some(Token::LParen)) {
        return false;
    }
    *i += 1;
    let mut depth: u32 = 1;
    while *i < toks.len() && depth > 0 {
        match toks[*i] {
            Token::LParen => depth += 1,
            Token::RParen => depth -= 1,
            _ => {}
        }
        *i += 1;
    }
    depth == 0
}

fn skip_trivia(toks: &[&Token], i: &mut usize) {
    while *i < toks.len() && is_trivia(toks[*i]) {
        *i += 1;
    }
}

fn is_table_terminator(word: &Word) -> bool {
    matches!(
        word.keyword,
        Keyword::WHERE
            | Keyword::JOIN
            | Keyword::INNER
            | Keyword::OUTER
            | Keyword::LEFT
            | Keyword::RIGHT
            | Keyword::FULL
            | Keyword::CROSS
            | Keyword::ON
            | Keyword::USING
            | Keyword::GROUP
            | Keyword::ORDER
            | Keyword::HAVING
            | Keyword::LIMIT
            | Keyword::OFFSET
            | Keyword::UNION
            | Keyword::INTERSECT
            | Keyword::EXCEPT
            | Keyword::SET
            | Keyword::FETCH
            | Keyword::FOR
            | Keyword::RETURNING
    )
}

fn take_dotted_ident(toks: &[&Token], i: &mut usize) -> Option<Vec<String>> {
    let mut parts = Vec::new();
    let Some(Token::Word(w)) = toks.get(*i) else {
        return None;
    };
    parts.push(w.value.clone());
    *i += 1;
    while matches!(toks.get(*i), Some(Token::Period)) {
        *i += 1;
        match toks.get(*i) {
            Some(Token::Word(w)) => {
                parts.push(w.value.clone());
                *i += 1;
            }
            _ => break,
        }
    }
    Some(parts)
}

fn resolve_binding(parts: &[String], resolve: &ResolveContext) -> TableBinding {
    let default_catalog = resolve.default_catalog.unwrap_or("").to_string();
    let default_schema = resolve.default_schema.unwrap_or("").to_string();
    match parts.len() {
        1 => TableBinding {
            catalog: default_catalog,
            schema: default_schema,
            table: parts[0].clone(),
            is_cte: false,
        },
        2 => TableBinding {
            catalog: default_catalog,
            schema: parts[0].clone(),
            table: parts[1].clone(),
            is_cte: false,
        },
        _ => TableBinding {
            catalog: parts[0].clone(),
            schema: parts[1].clone(),
            table: parts[parts.len() - 1].clone(),
            is_cte: false,
        },
    }
}

fn is_trivia(token: &Token) -> bool {
    matches!(token, Token::Whitespace(_) | Token::EOF)
        || matches!(
            token,
            Token::Whitespace(
                Whitespace::SingleLineComment { .. } | Whitespace::MultiLineComment(_)
            )
        )
}

/// Walks back from the end of `prefix` through identifier-shaped chars
/// and returns that suffix as the partial. Returns `""` when the cursor
/// is on whitespace or punctuation.
fn extract_partial(prefix: &str) -> &str {
    let start = prefix
        .char_indices()
        .rev()
        .take_while(|(_, c)| is_ident_char(*c))
        .last()
        .map(|(i, _)| i)
        .unwrap_or(prefix.len());
    &prefix[start..]
}

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classify_at_end(stmt: &str) -> ClassifyResult {
        classify(
            stmt,
            stmt.len(),
            DriverKind::Sqlite,
            ResolveContext::empty(),
        )
    }

    fn classify_at(stmt: &str, cursor: usize) -> ClassifyResult {
        classify(
            stmt,
            cursor,
            DriverKind::Sqlite,
            ResolveContext {
                default_catalog: Some("main"),
                default_schema: Some("main"),
            },
        )
    }

    #[test]
    fn empty_input_is_keyword() {
        let r = classify_at_end("");
        assert_eq!(r.context, CompletionContext::Keyword);
        assert_eq!(r.partial, "");
    }

    #[test]
    fn typing_first_keyword() {
        let r = classify_at_end("SELE");
        assert_eq!(r.context, CompletionContext::Keyword);
        assert_eq!(r.partial, "SELE");
    }

    #[test]
    fn after_select_with_no_from_is_mixed() {
        let r = classify_at_end("SELECT ");
        assert_eq!(r.context, CompletionContext::Mixed);
    }

    #[test]
    fn after_select_with_from_resolves_to_unqualified_column() {
        // Cursor at end of "SELECT " inside a fuller statement.
        let r = classify_at("SELECT  FROM users", 7);
        assert_eq!(
            r.context,
            CompletionContext::Column { qualifier: None },
            "{r:?}"
        );
        assert_eq!(r.bindings.len(), 1);
        assert_eq!(r.bindings[0].table, "users");
    }

    #[test]
    fn after_from_is_table() {
        let r = classify_at_end("SELECT * FROM ");
        assert_eq!(r.context, CompletionContext::Table { schema: None });
        assert_eq!(r.partial, "");
    }

    #[test]
    fn typing_table_after_from() {
        let r = classify_at_end("SELECT * FROM us");
        assert_eq!(r.context, CompletionContext::Table { schema: None });
        assert_eq!(r.partial, "us");
    }

    #[test]
    fn after_join_is_table() {
        let r = classify_at_end("SELECT * FROM users u JOIN ");
        assert_eq!(r.context, CompletionContext::Table { schema: None });
    }

    #[test]
    fn after_into_is_table() {
        let r = classify_at_end("INSERT INTO ");
        assert_eq!(r.context, CompletionContext::Table { schema: None });
    }

    #[test]
    fn after_semicolon_resets_to_keyword() {
        let r = classify_at_end("SELECT 1; ");
        assert_eq!(r.context, CompletionContext::Keyword);
    }

    #[test]
    fn comment_does_not_become_anchor() {
        let r = classify_at_end("SELECT * FROM -- note\n");
        assert_eq!(r.context, CompletionContext::Table { schema: None });
    }

    #[test]
    fn unicode_partial_does_not_split_codepoints() {
        let r = classify_at_end("SELECT * FROM таб");
        assert_eq!(r.partial, "таб");
    }

    #[test]
    fn alias_dot_with_from_after_cursor_resolves_to_column() {
        // Cursor right after "u." — FROM clause comes after.
        let stmt = "SELECT u. FROM users u";
        let cursor = stmt.find('.').unwrap() + 1;
        let r = classify_at(stmt, cursor);
        match r.context {
            CompletionContext::Column { qualifier: Some(b) } => {
                assert_eq!(b.table, "users");
                assert_eq!(b.schema, "main");
            }
            other => panic!("expected qualified column, got {other:?}"),
        }
    }

    #[test]
    fn alias_dot_with_partial() {
        let stmt = "SELECT u.id FROM users u";
        let cursor = stmt.find("id").unwrap() + 2;
        let r = classify_at(stmt, cursor);
        match r.context {
            CompletionContext::Column { qualifier: Some(b) } => {
                assert_eq!(b.table, "users");
            }
            other => panic!("expected qualified column, got {other:?}"),
        }
        assert_eq!(r.partial, "id");
    }

    #[test]
    fn bare_table_name_resolves_too() {
        let r = classify_at("SELECT users.id FROM users", 15);
        match r.context {
            CompletionContext::Column { qualifier: Some(b) } => assert_eq!(b.table, "users"),
            other => panic!("expected qualified column, got {other:?}"),
        }
    }

    #[test]
    fn schema_qualified_table_after_from() {
        let r = classify_at_end("SELECT * FROM public.");
        match r.context {
            CompletionContext::Table { schema: Some(s) } => assert_eq!(s, "public"),
            other => panic!("expected schema-qualified table, got {other:?}"),
        }
    }

    #[test]
    fn join_with_alias_collects_binding() {
        let stmt = "SELECT * FROM users u JOIN posts p ON p.user_id = u.id WHERE ";
        let r = classify_at_end(stmt);
        let names: Vec<&str> = r.bindings.iter().map(|b| b.table.as_str()).collect();
        assert!(names.contains(&"users"), "{names:?}");
        assert!(names.contains(&"posts"), "{names:?}");
    }

    #[test]
    fn unqualified_column_in_where() {
        let r = classify_at_end("SELECT * FROM users u WHERE ");
        assert!(matches!(
            r.context,
            CompletionContext::Column { qualifier: None }
        ));
    }

    #[test]
    fn schema_qualified_table_then_column() {
        let stmt = "SELECT * FROM public.users u WHERE u.";
        let r = classify_at_end(stmt);
        match r.context {
            CompletionContext::Column { qualifier: Some(b) } => {
                assert_eq!(b.schema, "public");
                assert_eq!(b.table, "users");
            }
            other => panic!("expected qualified column, got {other:?}"),
        }
    }

    #[test]
    fn cte_definition_collected_as_binding() {
        let stmt = "WITH recent AS (SELECT * FROM orders) SELECT * FROM recent";
        let r = classify_at_end(stmt);
        let cte = r
            .bindings
            .iter()
            .find(|b| b.is_cte)
            .expect("CTE binding present");
        assert_eq!(cte.table, "recent");
    }

    #[test]
    fn cte_with_recursive_keyword() {
        let stmt = "WITH RECURSIVE walk AS (SELECT 1) SELECT * FROM walk";
        let r = classify_at_end(stmt);
        assert!(
            r.bindings.iter().any(|b| b.is_cte && b.table == "walk"),
            "{:?}",
            r.bindings
        );
    }

    #[test]
    fn multiple_ctes_in_with_chain() {
        let stmt = "WITH a AS (SELECT 1), b AS (SELECT 2) SELECT * FROM b";
        let r = classify_at_end(stmt);
        let cte_names: Vec<&str> = r
            .bindings
            .iter()
            .filter(|b| b.is_cte)
            .map(|b| b.table.as_str())
            .collect();
        assert!(cte_names.contains(&"a"), "{cte_names:?}");
        assert!(cte_names.contains(&"b"), "{cte_names:?}");
    }

    #[test]
    fn cte_qualifier_resolves_to_cte_binding() {
        let stmt = "WITH recent AS (SELECT * FROM orders) SELECT recent. FROM recent";
        let cursor = stmt.find("recent.").unwrap() + "recent.".len();
        let r = classify_at(stmt, cursor);
        match r.context {
            CompletionContext::Column { qualifier: Some(b) } => {
                assert!(b.is_cte);
                assert_eq!(b.table, "recent");
            }
            other => panic!("expected CTE qualifier, got {other:?}"),
        }
    }

    #[test]
    fn from_with_cte_name_keeps_cte_binding() {
        // The CTE definition wins over a fresh resolved-table binding
        // for `recent` in the FROM clause.
        let stmt = "WITH recent AS (SELECT 1) SELECT * FROM recent";
        let r = classify_at_end(stmt);
        let recent = r
            .bindings
            .iter()
            .find(|b| b.table == "recent")
            .expect("recent in bindings");
        assert!(recent.is_cte);
    }

    #[test]
    fn update_statement_collects_target_table() {
        let r = classify_at_end("UPDATE users SET ");
        assert_eq!(r.bindings.len(), 1);
        assert_eq!(r.bindings[0].table, "users");
        assert_eq!(r.context, CompletionContext::Column { qualifier: None });
    }

    #[test]
    fn update_with_alias_collects_binding() {
        let stmt = "UPDATE users u SET u.name = ";
        let r = classify_at_end(stmt);
        assert_eq!(r.bindings.len(), 1);
        let u = r.bindings.iter().find(|b| b.table == "users").unwrap();
        assert_eq!(u.table, "users");
    }
}
