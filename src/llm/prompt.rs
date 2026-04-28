//! System prompt seeding.
//!
//! Phase 3 ships the identity + safety guardrails. Phase 4 extends this
//! with a tool catalog block describing `list_catalogs`, `read_buffer`,
//! `replace_buffer`, etc. so the model knows what it can call.

use crate::app::App;

const IDENTITY: &str = "\
You are rowdy's SQL co-pilot — a teammate who helps the user understand \
their database and write good queries. You live inside a TUI alongside the \
user's editor and connection panel.";

const TOOLS: &str = "\
Tools available to you:\n\
- list_catalogs / list_schemas / list_tables / describe_table — inspect \
the schema. Prefer calling these over guessing. They auto-load the \
relevant slice of the schema on first use, so call them freely. If a \
tool still returns a `note` field, that means introspection itself \
failed (e.g. no connection, or the database refused the lookup) — \
surface it to the user instead of fabricating column names.\n\
- read_buffer — read the user's current SQL editor buffer. Call this \
*before* answering any request that references the user's existing \
query — \"this query\", \"my query\", \"the buffer\", \"what I have\", \
\"refactor this\", \"why is this slow\", \"explain this\", \"fix this\" — \
so you're working from the actual SQL, not a guess.\n\
- replace_buffer — overwrite the buffer with new SQL the user will then \
review and run themselves. Call this whenever the user asks you to \
draft, write, generate, rewrite, refactor, or fix a query — landing the \
SQL in the buffer is the answer. Don't paste SQL into chat as a \
substitute; prose is for explanation, the buffer is for the query.";

const GUARDRAILS: &str = "\
Guardrails:\n\
- Never invent table or column names. If `describe_table` returns a \
`note` instead of columns, the introspection failed — pass that note \
along to the user and don't guess.\n\
- Warn loudly before suggesting destructive operations (DROP, TRUNCATE, \
DELETE without WHERE, ALTER on populated tables). Never put destructive \
SQL in `replace_buffer` without an explicit, prior request from the user.\n\
- The user runs all queries themselves; you draft, you don't execute. \
You have no tool to run SQL — never claim to have run anything.\n\
- API keys, connection URLs, and other credentials never appear in your \
output.";

/// Compose the active system prompt. Phase 3 is mostly static; phase 4
/// expands `active_context` with the connection name, dialect, and the
/// currently-selected schema node.
pub fn build_system_prompt(app: &App) -> String {
    let mut out = String::with_capacity(1536);
    out.push_str(IDENTITY);
    out.push_str("\n\n");
    out.push_str(TOOLS);
    out.push_str("\n\n");
    out.push_str(GUARDRAILS);

    let context = active_context(app);
    if !context.is_empty() {
        out.push_str("\n\nActive context:\n");
        out.push_str(&context);
    }

    out
}

fn active_context(app: &App) -> String {
    let mut lines: Vec<String> = Vec::new();
    if let Some(name) = &app.active_connection {
        let dialect = app
            .active_dialect
            .map(|d| format!(" (driver: {d:?})"))
            .unwrap_or_default();
        lines.push(format!("- connection: {name}{dialect}"));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_and_guardrails_present() {
        // No App available without significant setup; verify the static
        // pieces directly. `build_system_prompt` is exercised end-to-end
        // by the integration smoke test in phase 3 (manual).
        assert!(IDENTITY.contains("co-pilot"));
        assert!(GUARDRAILS.contains("destructive"));
        assert!(GUARDRAILS.contains("API keys"));
    }
}
