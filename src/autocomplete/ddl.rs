//! DDL detection for cache invalidation.
//!
//! When a user runs `CREATE TABLE …` or `DROP TABLE …`, the autocomplete
//! cache will silently get out of sync until the next `:reload`. We
//! check the just-executed SQL for DDL shape and trigger a re-prime
//! automatically.
//!
//! Detection is intentionally coarse: we sniff the leading keyword
//! through the tokenizer (so comments and whitespace are handled
//! correctly) and treat any of `CREATE` / `ALTER` / `DROP` / `TRUNCATE`
//! / `RENAME` as cache-invalidating. Multi-statement scripts are
//! handled too — any DDL statement in the buffer triggers a reload.

use sqlparser::dialect::GenericDialect;
use sqlparser::keywords::Keyword;
use sqlparser::tokenizer::{Token, Tokenizer, Whitespace};

/// True if `sql` contains at least one DDL statement that may have
/// reshaped the schema (added/removed/renamed tables or columns).
pub fn affects_schema_cache(sql: &str) -> bool {
    let dialect = GenericDialect {};
    let Ok(tokens) = Tokenizer::new(&dialect, sql).tokenize() else {
        // Tokenizer errors fail safe — we'd rather over-trigger a
        // reload than miss one.
        return true;
    };
    let mut at_stmt_start = true;
    for token in tokens {
        match token {
            Token::Whitespace(Whitespace::Space | Whitespace::Newline | Whitespace::Tab) => {}
            Token::Whitespace(
                Whitespace::SingleLineComment { .. } | Whitespace::MultiLineComment(_),
            ) => {}
            Token::SemiColon => {
                at_stmt_start = true;
            }
            Token::Word(w) if at_stmt_start => {
                if matches!(
                    w.keyword,
                    Keyword::CREATE
                        | Keyword::ALTER
                        | Keyword::DROP
                        | Keyword::TRUNCATE
                        | Keyword::RENAME
                ) {
                    return true;
                }
                at_stmt_start = false;
            }
            _ => {
                at_stmt_start = false;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_create_table() {
        assert!(affects_schema_cache("CREATE TABLE users (id INT)"));
    }

    #[test]
    fn detects_drop_table() {
        assert!(affects_schema_cache("DROP TABLE users"));
    }

    #[test]
    fn detects_alter_with_leading_whitespace_and_comment() {
        assert!(affects_schema_cache(
            "  -- migrate up\n  ALTER TABLE users ADD COLUMN age INT"
        ));
    }

    #[test]
    fn detects_truncate() {
        assert!(affects_schema_cache("TRUNCATE TABLE events"));
    }

    #[test]
    fn select_does_not_trigger() {
        assert!(!affects_schema_cache("SELECT * FROM users"));
    }

    #[test]
    fn insert_update_delete_do_not_trigger() {
        assert!(!affects_schema_cache(
            "INSERT INTO users(name) VALUES ('alice')"
        ));
        assert!(!affects_schema_cache("UPDATE users SET name = 'a'"));
        assert!(!affects_schema_cache("DELETE FROM users WHERE id = 1"));
    }

    #[test]
    fn ddl_in_second_statement_triggers() {
        assert!(affects_schema_cache(
            "SELECT 1; CREATE INDEX idx_users_name ON users(name)"
        ));
    }

    #[test]
    fn empty_sql_is_not_ddl() {
        assert!(!affects_schema_cache(""));
        assert!(!affects_schema_cache("   "));
        assert!(!affects_schema_cache("-- just a comment"));
    }
}
