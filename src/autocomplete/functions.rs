//! Curated SQL-function list per dialect.
//!
//! Like the keyword list, this is hand-maintained on purpose: a longer
//! list always works (it's just an autocomplete suggestion), but a
//! tighter one keeps the popover useful. The shared core covers
//! ANSI-ish functions; each driver layers its own table on top via
//! [`Driver::functions`].
//!
//! Each entry knows whether it's zero-argument (NOW, CURRENT_DATE…) so
//! the action layer can place the cursor correctly: between the parens
//! for arg-taking functions, after `)` for zero-arg ones.

use crate::datasource::Driver;

/// How a function is invoked, which decides what gets inserted on
/// accept and where the cursor lands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FnArity {
    /// Takes one or more arguments — accept inserts the bare name and
    /// the action layer appends `()` with the cursor between them.
    Args,
    /// Niladic but parenthesised in SQL — accept inserts `name()` and
    /// the cursor lands at the end (e.g., `NOW()`, `CURDATE()`).
    ZeroArgParens,
    /// Bare-name value function — must NOT have parens. SQLite is
    /// strict here (`CURRENT_TIMESTAMP()` is a syntax error). Accept
    /// inserts just the name with no trail.
    Bare,
}

#[derive(Debug, Clone, Copy)]
pub struct FunctionDef {
    pub name: &'static str,
    /// One-line signature shown in the popover detail column.
    pub signature: &'static str,
    pub arity: FnArity,
}

const COMMON: &[FunctionDef] = &[
    FunctionDef {
        name: "AVG",
        signature: "AVG(expr)",
        arity: FnArity::Args,
    },
    FunctionDef {
        name: "CAST",
        signature: "CAST(expr AS type)",
        arity: FnArity::Args,
    },
    FunctionDef {
        name: "COALESCE",
        signature: "COALESCE(a, b, …)",
        arity: FnArity::Args,
    },
    FunctionDef {
        name: "CONCAT",
        signature: "CONCAT(a, b, …)",
        arity: FnArity::Args,
    },
    FunctionDef {
        name: "COUNT",
        signature: "COUNT(expr | *)",
        arity: FnArity::Args,
    },
    // The CURRENT_* triplet is bare-name on every supported dialect.
    // SQLite strictly rejects `CURRENT_TIMESTAMP()`; Postgres and
    // MySQL accept the bare form too, so this is the safe canonical.
    FunctionDef {
        name: "CURRENT_DATE",
        signature: "CURRENT_DATE",
        arity: FnArity::Bare,
    },
    FunctionDef {
        name: "CURRENT_TIME",
        signature: "CURRENT_TIME",
        arity: FnArity::Bare,
    },
    FunctionDef {
        name: "CURRENT_TIMESTAMP",
        signature: "CURRENT_TIMESTAMP",
        arity: FnArity::Bare,
    },
    FunctionDef {
        name: "LENGTH",
        signature: "LENGTH(str)",
        arity: FnArity::Args,
    },
    FunctionDef {
        name: "LOWER",
        signature: "LOWER(str)",
        arity: FnArity::Args,
    },
    FunctionDef {
        name: "MAX",
        signature: "MAX(expr)",
        arity: FnArity::Args,
    },
    FunctionDef {
        name: "MIN",
        signature: "MIN(expr)",
        arity: FnArity::Args,
    },
    FunctionDef {
        name: "NULLIF",
        signature: "NULLIF(a, b)",
        arity: FnArity::Args,
    },
    FunctionDef {
        name: "ROUND",
        signature: "ROUND(num [, places])",
        arity: FnArity::Args,
    },
    FunctionDef {
        name: "SUBSTRING",
        signature: "SUBSTRING(str FROM n FOR len)",
        arity: FnArity::Args,
    },
    FunctionDef {
        name: "SUM",
        signature: "SUM(expr)",
        arity: FnArity::Args,
    },
    FunctionDef {
        name: "TRIM",
        signature: "TRIM(str)",
        arity: FnArity::Args,
    },
    FunctionDef {
        name: "UPPER",
        signature: "UPPER(str)",
        arity: FnArity::Args,
    },
];

/// Iterate over every function relevant to `dialect`. Common functions
/// come first; dialect-specific ones (from [`Driver::functions`]) come
/// second.
pub fn for_dialect(dialect: &dyn Driver) -> impl Iterator<Item = &'static FunctionDef> {
    COMMON.iter().chain(dialect.functions().iter())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::datasource::sql::{mysql::MysqlDriver, postgres::PostgresDriver};

    #[test]
    fn each_dialect_includes_common_functions() {
        for kind in crate::datasource::DRIVERS.iter().copied() {
            let names: Vec<&str> = for_dialect(kind).map(|f| f.name).collect();
            assert!(names.contains(&"COUNT"), "{kind:?}");
            assert!(names.contains(&"COALESCE"), "{kind:?}");
        }
    }

    #[test]
    fn postgres_specific_functions_present() {
        let names: Vec<&str> = for_dialect(&PostgresDriver).map(|f| f.name).collect();
        assert!(names.contains(&"DATE_TRUNC"));
        assert!(names.contains(&"GENERATE_SERIES"));
    }

    #[test]
    fn mysql_specific_functions_present() {
        let names: Vec<&str> = for_dialect(&MysqlDriver).map(|f| f.name).collect();
        assert!(names.contains(&"IFNULL"));
        assert!(names.contains(&"GROUP_CONCAT"));
    }

    #[test]
    fn now_is_parens_zero_arg_in_pg_and_mysql() {
        for kind in [&PostgresDriver as &dyn Driver, &MysqlDriver] {
            let now = for_dialect(kind).find(|f| f.name == "NOW").unwrap();
            assert_eq!(now.arity, FnArity::ZeroArgParens, "{kind:?}");
        }
    }

    #[test]
    fn current_timestamp_is_bare_on_every_dialect() {
        // SQLite explicitly rejects `CURRENT_TIMESTAMP()`. Treating
        // it as bare across dialects keeps the canonical SQL valid
        // everywhere.
        for kind in crate::datasource::DRIVERS.iter().copied() {
            let f = for_dialect(kind)
                .find(|f| f.name == "CURRENT_TIMESTAMP")
                .unwrap();
            assert_eq!(f.arity, FnArity::Bare, "{kind:?}");
        }
    }
}
