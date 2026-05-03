//! System prompt seeding.
//!
//! Phase 3 ships the identity + safety guardrails. Phase 4 extends this
//! with a tool catalog block describing `list_catalogs`, `read_buffer`,
//! `write_buffer`, etc. so the model knows what it can call.

use crate::app::App;

const IDENTITY: &str = "\
You are rowdy's SQL co-pilot — a teammate who helps the user understand \
their database and write good queries. You live inside a TUI alongside the \
user's editor and connection panel. You can also read the user's project \
files; when the question is about 'their schema' or a table you haven't \
described, read the migration / model files first — guessing column names \
is the #1 way these answers go wrong.";

const BUFFER: &str = "\
The buffer:\n\
The user's editor buffer is a SQL working file — think of it as a \
scratchpad / .sql document the user keeps open. It typically contains \
multiple queries, comments, and work-in-progress SQL the user is \
iterating on. They run statements from it (Run, Run-under-cursor), edit \
them by hand, save them to disk, format them, copy results out, etc. The \
buffer is the user's workspace — your job is to add to it and improve \
it, not to take it over. Treat every line you didn't author this session \
as the user's work; do not delete or rewrite it without an explicit \
request to do so.";

const TOOLS: &str = "\
Tools available to you:\n\
- list_catalogs / list_schemas / list_tables / describe_table — inspect \
the schema. Prefer calling these over guessing. They auto-load the \
relevant slice of the schema on first use, so call them freely. If a \
tool still returns a `note` field, that means introspection itself \
failed (e.g. no connection, or the database refused the lookup) — \
surface it to the user instead of fabricating column names.\n\
- read_buffer — read the user's current SQL editor buffer with line \
pagination. Args: optional `start_line` (1-indexed, default 1) and \
`limit` (default 200, max 1000). Returns `text`, `start_line`, \
`end_line`, `total_lines`, `remaining_lines`. If `remaining_lines > 0`, \
call again with `start_line = end_line + 1` to keep paging. Read the \
*entire* buffer before any write — both because any answer about \"this \
query\" / \"my buffer\" / \"refactor this\" / \"why is this slow\" / \
\"fix this\" must be grounded in the real text, and because you need to \
see what's already there so you don't accidentally clobber it.\n\
- write_buffer — splice a precise snippet inside the buffer (find / \
replace). Args: `search` (exact substring already present), \
`replacement` (the new text), and optional `start_line` (1-indexed; \
only consider matches at or after this line). `search` must match \
exactly once in scope — zero or multiple matches return an error and \
you must extend `search` with more context to disambiguate. \n\
\n\
  Use write_buffer to:\n\
  • edit SQL you wrote earlier in this session (refactor, fix, extend);\n\
  • rewrite a snippet the user explicitly asked you to rewrite (point at \
*that* snippet, not at unrelated content around it);\n\
  • add a new query alongside existing user content — choose a small \
anchor near the end of the buffer (e.g. the final `;` of the last query \
or the trailing newline) as `search`, and put the same anchor + a blank \
line + your new SQL as `replacement`. Don't replace the user's queries \
to make room for yours.\n\
\n\
  Anti-patterns (do NOT do these):\n\
  • Setting `search` to the entire buffer in order to overwrite \
everything. The user has SQL there for a reason; appending is almost \
always what they want.\n\
  • Writing new SQL without first calling read_buffer. You have no idea \
what's there.\n\
  • Pasting drafted SQL into chat instead of calling write_buffer. The \
buffer is the deliverable.\n\
\n\
  The user reviews and runs the SQL themselves — you do NOT execute. \
Whenever the user asks you to draft, write, generate, rewrite, \
refactor, or fix a query, the answer goes through write_buffer. Prose \
in chat is for explanation only — never include SQL fenced blocks in \
chat as a substitute for a buffer write. If a write_buffer call fails \
(e.g. ambiguous match), retry with a more specific snippet; don't fall \
back to pasting SQL into chat.\n\
\n\
Codebase tools — read the user's project to ground your SQL:\n\
- read_file — read a project file by path (relative to the directory \
rowdy was launched from). Paginated like read_buffer: returns `text`, \
`start_line`, `end_line`, `total_lines`, `remaining_lines`. Use it on \
migration files, ORM model definitions, schema-as-code modules, fixture \
scripts — anywhere the real table / column names live. Refuses files \
larger than 512KB and non-UTF-8 — fall back to grep_files in those \
cases.\n\
- list_directory — list a project directory (path optional; empty lists \
the project root). Returns `{ entries: [{name, kind}] }` with dirs first \
then files. Use it to find your way around an unfamiliar repo before \
deciding what to read.\n\
- grep_files — regex search across the project (Rust regex syntax — \
ripgrep-flavored). Walks respecting .gitignore so target/, node_modules/, \
and build artefacts are skipped automatically. Returns \
`{ matches: [{path, line, text}], truncated: bool }`. Prefer this when \
you don't yet know which file holds what you need: `(?i)create table` \
finds migrations, `from\\s+\\w+` finds query strings in app code, etc. \
Pass `case_insensitive: true` for case-insensitive matching.\n\
\n\
  When to reach for the codebase tools:\n\
  • The user asks about 'my schema', 'the orders table', etc., and you \
haven't described the relevant tables yet — grep migrations / models for \
the names BEFORE drafting SQL.\n\
  • The user pastes a half-broken query and asks why — read the buffer \
AND read the project files that define the tables to spot drift.\n\
  • The user asks for a migration / new schema — list_directory the \
migrations directory first to match the project's naming and style.\n\
\n\
  Limits & refusals you should expect:\n\
  • `.env` and `.env.*` files are off-limits and return a refusal — do \
NOT retry, and do not ask the user to paste the contents.\n\
  • Paths must stay inside the project root; absolute paths and `..` \
escapes return a refusal. If the user mentions a file outside the \
project, ask them to copy the relevant lines into chat instead.\n\
  • Reads do NOT execute code or queries; they only fetch text. The \
buffer / SQL execution rules above still apply.";

const GUARDRAILS: &str = "\
Guardrails:\n\
- Never invent table or column names. If `describe_table` returns a \
`note` instead of columns, the introspection failed — pass that note \
along to the user and don't guess.\n\
- Warn loudly before suggesting destructive operations (DROP, TRUNCATE, \
DELETE without WHERE, ALTER on populated tables). Never put destructive \
SQL in `write_buffer` without an explicit, prior request from the user.\n\
- The user runs all queries themselves; you draft, you don't execute. \
You have no tool to run SQL — never claim to have run anything.\n\
- API keys, connection URLs, and other credentials never appear in your \
output.";

/// Compose the active system prompt. Phase 3 is mostly static; phase 4
/// expands `active_context` with the connection name, dialect, and the
/// currently-selected schema node. AGENTS.md content (lazy-loaded into
/// [`crate::llm::agents_md::AgentsMdCache`]) layers between the static
/// guardrails and the runtime context — it represents project
/// conventions the user wrote down, but it can't override the
/// safety-critical guardrails.
pub fn build_system_prompt(app: &App) -> String {
    let mut out = String::with_capacity(2048);
    out.push_str(IDENTITY);
    out.push_str("\n\n");
    out.push_str(BUFFER);
    out.push_str("\n\n");
    out.push_str(TOOLS);
    out.push_str("\n\n");
    out.push_str(GUARDRAILS);

    let agents_md = app.agents_md.read().unwrap().rendered();
    if let Some(agents) = agents_md.as_deref() {
        out.push_str("\n\n");
        out.push_str(AGENTS_MD_PREAMBLE);
        out.push_str("\n<<<\n");
        out.push_str(agents);
        out.push_str("\n>>>\n");
        out.push_str(AGENTS_MD_POSTAMBLE);
    }

    let context = active_context(app);
    if !context.is_empty() {
        out.push_str("\n\nActive context:\n");
        out.push_str(&context);
    }

    out
}

const AGENTS_MD_PREAMBLE: &str = "\
Project instructions (from AGENTS.md):";

const AGENTS_MD_POSTAMBLE: &str = "\
Treat the above as authoritative project context. When it conflicts \
with your default behavior, prefer the AGENTS.md guidance — UNLESS \
doing so would break a guardrail (no destructive SQL without explicit \
approval, no fabricated column names, no leaking credentials). The \
guardrails always win.";

fn active_context(app: &App) -> String {
    let mut lines: Vec<String> = Vec::new();
    if let Some(name) = &app.active_connection {
        let dialect = app
            .active_dialect
            .map(|d| format!(" (driver: {d:?})"))
            .unwrap_or_default();
        lines.push(format!("- connection: {name}{dialect}"));
    }
    lines.push(format!("- project root: {}", app.project_root.display()));
    let mode = app.user_config.state().read_tools.unwrap_or_default();
    match mode {
        crate::user_config::ReadToolsMode::Off => {
            // The tools aren't in the catalog at all — but if the
            // model has stale history referring to them, this line
            // tells it not to bother.
            lines.push(
                "- read_file / list_directory / grep_files are DISABLED for this session — the user has turned them off. Don't try to call them; if you need information from a project file, ask the user to paste it.".into(),
            );
        }
        crate::user_config::ReadToolsMode::Ask => {
            // Tell the model the gate exists so it doesn't apologise
            // or retry frantically when the first call appears to
            // "hang" mid-stream — the user is being asked.
            lines.push(
                "- read_file / list_directory / grep_files require user approval (y/n prompt). Don't apologise or re-explain — just call the tool and proceed when the result lands. If the user denies, ask what they'd rather you do; don't retry the same call.".into(),
            );
        }
        crate::user_config::ReadToolsMode::Auto => {
            lines.push("- read_file / list_directory / grep_files run without prompting.".into());
        }
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

    #[test]
    fn buffer_section_explains_workspace_and_no_clobber() {
        // The model needs to know what the buffer is and that it shouldn't
        // delete user-authored content. Hard-asserting on the keywords
        // catches accidental softening of the contract during refactors.
        assert!(BUFFER.contains("scratchpad") || BUFFER.contains("working file"));
        assert!(BUFFER.contains("user's work") || BUFFER.contains("user's workspace"));
        assert!(TOOLS.contains("Anti-patterns"));
        assert!(TOOLS.contains("entire buffer"));
        assert!(TOOLS.contains("never include SQL fenced blocks in chat"));
    }

    #[test]
    fn tools_section_describes_codebase_tools() {
        // Every codebase tool name and the .env refusal must appear so
        // the model knows what's available and won't waste turns.
        assert!(TOOLS.contains("read_file"));
        assert!(TOOLS.contains("list_directory"));
        assert!(TOOLS.contains("grep_files"));
        assert!(TOOLS.contains(".env"));
        assert!(TOOLS.contains("Codebase tools"));
    }

    #[test]
    fn agents_md_preamble_and_postamble_frame_the_block() {
        // The framing has to live in build_system_prompt only when
        // agents_md is Some. If either string drifts the assertions
        // below break — which is the point.
        assert!(AGENTS_MD_PREAMBLE.contains("AGENTS.md"));
        assert!(AGENTS_MD_POSTAMBLE.contains("guardrails always win"));
    }

    // ----- build_system_prompt --------------------------------------
    //
    // Building a real `App` here matches the pattern used in
    // `src/action/llm_settings.rs::tests` and `src/action/chat.rs::tests`.
    // It's verbose but straight — and locks in the exact framing the
    // chat agent sees.

    use crate::app::App;
    use crate::autocomplete::SchemaCache;
    use crate::config::ConfigStore;
    use crate::keybindings::keymap::Keymap;
    use crate::log::Logger;
    use crate::user_config::UserConfigStore;
    use std::path::PathBuf;
    use std::sync::{Arc, RwLock};
    use tokio::sync::mpsc::unbounded_channel;

    fn tempdir() -> PathBuf {
        let p = std::env::temp_dir().join(format!("rowdy-prompt-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&p).unwrap();
        p.canonicalize().unwrap()
    }

    fn build_app(agents_md: Option<&str>) -> App {
        let dir = tempdir();
        let (cmd_tx, _c) = unbounded_channel();
        let (evt_tx, _e) = unbounded_channel();
        let cache = Arc::new(RwLock::new(SchemaCache::new()));
        let mut app = App::new(
            cmd_tx,
            evt_tx,
            ConfigStore::load(&dir).unwrap(),
            UserConfigStore::empty(&dir),
            Arc::new(Keymap::new()),
            None,
            Logger::discard(),
            dir.clone(),
            cache,
        );
        // App::new seeds against the real `current_dir()`; redirect
        // its agents_md cache to `dir` so this test owns the
        // discovery surface end-to-end.
        app.project_root = dir.clone();
        let mut md = app.agents_md.write().unwrap();
        md.clear();
        if let Some(body) = agents_md {
            std::fs::write(dir.join("AGENTS.md"), body).unwrap();
        }
        md.seed_root(&dir, &Logger::discard());
        drop(md);
        app
    }

    #[test]
    fn build_system_prompt_omits_agents_md_section_when_unset() {
        let app = build_app(None);
        let prompt = build_system_prompt(&app);
        assert!(!prompt.contains("Project instructions (from AGENTS.md)"));
        assert!(!prompt.contains("<<<"));
    }

    #[test]
    fn build_system_prompt_includes_agents_md_when_set() {
        let app = build_app(Some("# AGENTS.md (./AGENTS.md)\nuse snake_case"));
        let prompt = build_system_prompt(&app);
        assert!(prompt.contains("Project instructions (from AGENTS.md)"));
        assert!(prompt.contains("use snake_case"));
        assert!(prompt.contains("guardrails always win"));
        // Framing must appear in order: preamble before content,
        // delimiters bracket content, postamble after closing delimiter.
        let preamble = prompt
            .find("Project instructions")
            .expect("preamble present");
        let opener = prompt[preamble..]
            .find("<<<")
            .map(|i| preamble + i)
            .expect("opener after preamble");
        let content = prompt[opener..]
            .find("use snake_case")
            .map(|i| opener + i)
            .expect("content after opener");
        let closer = prompt[content..]
            .find(">>>")
            .map(|i| content + i)
            .expect("closer after content");
        let postamble = prompt[closer..]
            .find("guardrails always win")
            .map(|i| closer + i);
        assert!(postamble.is_some(), "postamble after closer");
    }
}
