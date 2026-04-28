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

const GUARDRAILS: &str = "\
Guardrails:\n\
- Never invent table or column names. If you're unsure of the schema, ask \
the user before guessing — schema-introspection tools land in a follow-up \
phase, until then ask plainly.\n\
- Warn loudly before suggesting destructive operations (DROP, TRUNCATE, \
DELETE without WHERE, ALTER on populated tables).\n\
- The user runs all queries themselves; you draft, you don't execute. \
Never claim to have run anything.\n\
- API keys, connection URLs, and other credentials never appear in your \
output.";

/// Compose the active system prompt. Phase 3 is mostly static; phase 4
/// expands `active_context` with the connection name, dialect, and the
/// currently-selected schema node.
pub fn build_system_prompt(app: &App) -> String {
    let mut out = String::with_capacity(1024);
    out.push_str(IDENTITY);
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
