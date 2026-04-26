//! Tokenize-only SQL context classifier.
//!
//! Given the buffer text up to the cursor, decides what kinds of
//! identifiers fit syntactically here. The result drives which
//! candidates `engine::compute` returns.
//!
//! We never invoke the AST parser — partial input usually doesn't parse,
//! and we don't need a tree, just the local "what came before me" view.
//! sqlparser's tokenizer handles dialect-specific quoting, comments, and
//! string escapes for us; on a tokenize error we fall back to
//! `CompletionContext::Mixed` so the user still gets keywords.

use sqlparser::keywords::Keyword;
use sqlparser::tokenizer::{Token, TokenWithSpan, Tokenizer, Whitespace};

use crate::datasource::DriverKind;
use crate::sql_infer::dialect_for;

/// Phase 1 distinguishes Keyword vs Table; everything else is Mixed
/// (which falls back to keyword completion). Phase 2 will add `Column`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompletionContext {
    /// Start of a statement, or after `;`. Suggest top-level keywords.
    Keyword,
    /// Position where a table name fits — after FROM, JOIN, INTO,
    /// UPDATE, TABLE. Optionally schema-qualified (Phase 2; in Phase 1
    /// we always set this to `None`).
    Table { schema: Option<String> },
    /// SELECT-projection or any unclassified position. We surface
    /// keywords here; columns join in Phase 2.
    Mixed,
    /// Tokenize error or cursor inside a string/comment. The popover
    /// is suppressed in this state unless the user manually triggers.
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassifyResult {
    pub context: CompletionContext,
    /// Identifier prefix the user is currently typing (may be empty —
    /// e.g. cursor right after `FROM `). Always sliced from the input,
    /// so multi-byte chars don't get split.
    pub partial: String,
}

/// Classify the cursor position inside `prefix` (text from the start of
/// the current statement up to the cursor).
pub fn classify(prefix: &str, dialect: DriverKind) -> ClassifyResult {
    let partial = extract_partial(prefix);
    let partial_start = prefix.len() - partial.len();
    let head = &prefix[..partial_start];

    let dialect = dialect_for(dialect);
    let tokens = match Tokenizer::new(&*dialect, head).tokenize_with_location() {
        Ok(t) => t,
        Err(_) => {
            return ClassifyResult {
                context: CompletionContext::Unknown,
                partial: partial.to_string(),
            };
        }
    };

    let context = classify_from_tokens(&tokens);
    ClassifyResult {
        context,
        partial: partial.to_string(),
    }
}

fn classify_from_tokens(tokens: &[TokenWithSpan]) -> CompletionContext {
    // Walk back over whitespace/comments/EOF to find the anchor — the
    // first "real" token preceding the cursor.
    let anchor = tokens.iter().rev().find(|t| !is_trivia(&t.token));

    let Some(anchor) = anchor else {
        // Nothing before us → start of statement → keyword context.
        return CompletionContext::Keyword;
    };

    match &anchor.token {
        Token::SemiColon => CompletionContext::Keyword,
        Token::Word(word) => match word.keyword {
            Keyword::FROM | Keyword::JOIN | Keyword::INTO | Keyword::UPDATE | Keyword::TABLE => {
                CompletionContext::Table { schema: None }
            }
            _ => CompletionContext::Mixed,
        },
        _ => CompletionContext::Mixed,
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

    fn classify_sqlite(prefix: &str) -> ClassifyResult {
        classify(prefix, DriverKind::Sqlite)
    }

    #[test]
    fn empty_input_is_keyword() {
        let r = classify_sqlite("");
        assert_eq!(r.context, CompletionContext::Keyword);
        assert_eq!(r.partial, "");
    }

    #[test]
    fn typing_first_keyword() {
        let r = classify_sqlite("SELE");
        assert_eq!(r.context, CompletionContext::Keyword);
        assert_eq!(r.partial, "SELE");
    }

    #[test]
    fn after_select_is_mixed() {
        let r = classify_sqlite("SELECT ");
        assert_eq!(r.context, CompletionContext::Mixed);
        assert_eq!(r.partial, "");
    }

    #[test]
    fn after_from_is_table() {
        let r = classify_sqlite("SELECT * FROM ");
        assert_eq!(r.context, CompletionContext::Table { schema: None });
        assert_eq!(r.partial, "");
    }

    #[test]
    fn typing_table_after_from() {
        let r = classify_sqlite("SELECT * FROM us");
        assert_eq!(r.context, CompletionContext::Table { schema: None });
        assert_eq!(r.partial, "us");
    }

    #[test]
    fn after_join_is_table() {
        let r = classify_sqlite("SELECT * FROM users u JOIN ");
        assert_eq!(r.context, CompletionContext::Table { schema: None });
        assert_eq!(r.partial, "");
    }

    #[test]
    fn after_into_is_table() {
        let r = classify_sqlite("INSERT INTO ");
        assert_eq!(r.context, CompletionContext::Table { schema: None });
    }

    #[test]
    fn after_update_is_table() {
        let r = classify_sqlite("UPDATE ");
        assert_eq!(r.context, CompletionContext::Table { schema: None });
    }

    #[test]
    fn after_semicolon_resets_to_keyword() {
        let r = classify_sqlite("SELECT 1; ");
        assert_eq!(r.context, CompletionContext::Keyword);
        assert_eq!(r.partial, "");
    }

    #[test]
    fn comment_does_not_become_anchor() {
        // Trailing comment shouldn't change context — anchor walks back
        // past it.
        let r = classify_sqlite("SELECT * FROM -- note\n");
        assert_eq!(r.context, CompletionContext::Table { schema: None });
    }

    #[test]
    fn partial_with_underscore() {
        let r = classify_sqlite("SELECT * FROM user_a");
        assert_eq!(r.partial, "user_a");
        assert_eq!(r.context, CompletionContext::Table { schema: None });
    }

    #[test]
    fn unicode_partial_does_not_split_codepoints() {
        let r = classify_sqlite("SELECT * FROM таб");
        assert_eq!(r.partial, "таб");
    }
}
