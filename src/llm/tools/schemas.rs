//! Tool schema definitions handed to the LLM provider. Kept separate
//! from the dispatch logic in `tools.rs` because these descriptions are
//! prompt-engineering surface, not control flow — most edits in here
//! are wording tweaks, and isolating them keeps the noisy churn out of
//! the dispatcher's git log.

use std::collections::HashMap;

use llm::chat::{FunctionTool, ParameterProperty, ParametersSchema, Tool};
use serde_json::Value;

use super::{
    DESCRIBE_TABLE, GREP_FILES, LIST_CATALOGS, LIST_DIRECTORY, LIST_SCHEMAS, LIST_TABLES,
    READ_BUFFER, READ_FILE, WRITE_BUFFER,
};

/// All tools registered with the LLM, ready to pass to
/// `LLMProvider::chat_stream_with_tools`. Built from the public
/// `Tool`/`FunctionTool` types so we don't depend on `FunctionBuilder`'s
/// private `build()`. The caller filters this list by
/// [`crate::user_config::ReadToolsMode`] in `super::for_mode`.
pub(super) fn all() -> Vec<Tool> {
    vec![
        function_tool(
            LIST_CATALOGS,
            "List the catalogs (databases) available on the active connection. \
             No arguments. Returns { catalogs: [string] }.",
            &[],
            &[],
        ),
        function_tool(
            LIST_SCHEMAS,
            "List the schemas (namespaces) inside a catalog. \
             Returns { schemas: [string] } (empty if the catalog is unknown \
             or its schemas haven't been loaded yet).",
            &[(
                "catalog",
                "string",
                "Catalog name. Use list_catalogs to discover.",
            )],
            &["catalog"],
        ),
        function_tool(
            LIST_TABLES,
            "List the tables and views inside a (catalog, schema). \
             Returns { tables: [{name, kind}] } where kind is 'table' or 'view'.",
            &[
                ("catalog", "string", "Catalog name."),
                ("schema", "string", "Schema name."),
            ],
            &["catalog", "schema"],
        ),
        function_tool(
            DESCRIBE_TABLE,
            "Get column names + types for a (catalog, schema, table). \
             Returns { columns: [{name, type}] }. Auto-loads the table's \
             columns on first use. If introspection fails the response \
             includes a `note` describing why — pass that to the user.",
            &[
                ("catalog", "string", "Catalog name."),
                ("schema", "string", "Schema name."),
                ("table", "string", "Table or view name."),
            ],
            &["catalog", "schema", "table"],
        ),
        function_tool(
            READ_BUFFER,
            "Read the user's SQL editor buffer (their working SQL file — a \
             scratchpad with multiple queries, comments, and \
             work-in-progress they iterate on and run). Paginated: returns \
             { text, start_line, end_line, total_lines, remaining_lines }. \
             `text` carries the lines from `start_line` through `end_line` \
             joined with '\\n'. If `remaining_lines > 0`, call again with \
             `start_line = end_line + 1` to keep paging until you've seen \
             all of it. ALWAYS read the full buffer before any \
             write_buffer call: you need to know what queries the user \
             has there so you don't overwrite their work.",
            &[
                (
                    "start_line",
                    "integer",
                    "1-indexed line to start reading at. Defaults to 1.",
                ),
                (
                    "limit",
                    "integer",
                    "Maximum number of lines to return. Defaults to 200, capped at 1000.",
                ),
            ],
            &[],
        ),
        function_tool(
            WRITE_BUFFER,
            "Splice a snippet into the user's SQL editor buffer (find / \
             replace). `search` must match exactly once in the eligible \
             region — zero or multiple matches return an error and you \
             must extend `search` with more surrounding context. Returns \
             { ok: true, line } where `line` is the 1-indexed start line \
             of the replacement. \
             \
             The buffer is the user's working SQL file — it usually \
             contains queries they wrote and are iterating on. Treat \
             everything you didn't author this session as theirs; do NOT \
             delete or overwrite it. \
             \
             Correct uses: \
             (1) editing SQL you wrote earlier this session; \
             (2) rewriting a snippet the user explicitly asked you to \
             rewrite — point `search` at exactly that snippet, not at \
             unrelated surrounding content; \
             (3) ADDING a new query alongside existing user SQL — pick a \
             small anchor near the end of the buffer (e.g. the final `;` \
             of the last query, or the trailing newline) as `search`, and \
             set `replacement` to that same anchor followed by a blank \
             line and your new SQL. \
             \
             Anti-patterns (do NOT do these): setting `search` to the \
             entire buffer to overwrite everything; replacing the user's \
             existing queries to make room for yours; calling write_buffer \
             without first reading the buffer end-to-end. \
             \
             The user reviews and runs the SQL themselves — you do NOT \
             execute. Never paste SQL in chat as a substitute; if a write \
             fails, retry with a more specific snippet.",
            &[
                (
                    "search",
                    "string",
                    "Exact substring already present in the buffer. Include \
                     enough surrounding context to make it match exactly once. \
                     To append new SQL alongside existing user queries, use a \
                     small anchor at the end of the buffer (e.g. the last `;` \
                     plus newline) — do NOT set this to the entire buffer.",
                ),
                (
                    "replacement",
                    "string",
                    "Text to substitute in place of `search`. To append, set \
                     this to the anchor + blank line + your new SQL so the \
                     anchor is preserved and your SQL lands after it.",
                ),
                (
                    "start_line",
                    "integer",
                    "Optional 1-indexed line; only consider matches whose \
                     start byte is at or after the start of this line.",
                ),
            ],
            &["search", "replacement"],
        ),
        function_tool(
            READ_FILE,
            "Read a file from the user's project (the directory rowdy was \
             launched from). Paginated like read_buffer: returns \
             { text, start_line, end_line, total_lines, remaining_lines }. \
             Path is relative to the project root. `.env` files (and any \
             .env.* variant) are off-limits — the call will return a \
             refusal and you should NOT retry. Use this to ground SQL \
             suggestions in the user's real schema definitions: \
             migrations, ORM models, schema files, string-builder SQL. \
             Prefer grep_files first if you don't yet know which file \
             holds what you need.",
            &[
                ("path", "string", "Path relative to the project root."),
                (
                    "start_line",
                    "integer",
                    "1-indexed line to start at. Defaults to 1.",
                ),
                (
                    "limit",
                    "integer",
                    "Max lines to return. Defaults to 200, capped at 1000.",
                ),
            ],
            &["path"],
        ),
        function_tool(
            LIST_DIRECTORY,
            "List the contents of a directory inside the user's project. \
             Returns { entries: [{name, kind}] } where kind is 'file', \
             'dir', or 'symlink'. Path is relative to the project root; \
             omit it (or pass an empty string) to list the project root \
             itself. `.env*` files are filtered out — neither their \
             names nor contents are exposed.",
            &[(
                "path",
                "string",
                "Optional directory path relative to the project root. \
                 Empty / omitted lists the root.",
            )],
            &[],
        ),
        function_tool(
            GREP_FILES,
            "Search the user's project for a regex pattern (Rust regex \
             syntax — same flavor ripgrep uses). Walks the project \
             respecting .gitignore, .ignore, and .git/info/exclude — so \
             target/, node_modules/, build artefacts, and other \
             gitignored noise are skipped automatically. Returns \
             { matches: [{path, line, text}], truncated: bool }. \
             Use this to find migration files, table definitions, query \
             strings in app code, fixture/seed scripts, etc., before you \
             draft SQL or claim a column exists.",
            &[
                (
                    "pattern",
                    "string",
                    "Regex pattern. Use (?i) at the start for \
                     case-insensitive matching, or set case_insensitive=true.",
                ),
                (
                    "path",
                    "string",
                    "Optional subdirectory to confine the search to, \
                     relative to the project root.",
                ),
                (
                    "case_insensitive",
                    "boolean",
                    "If true, the pattern matches case-insensitively. \
                     Defaults to false.",
                ),
                (
                    "max_matches",
                    "integer",
                    "Cap on total matches returned. Defaults to 100, \
                     capped at 500.",
                ),
            ],
            &["pattern"],
        ),
    ]
}

/// Build one `Tool` value — name, description, parameters schema, required
/// list. `params` is `(name, json-type, description)` triples.
fn function_tool(
    name: &str,
    description: &str,
    params: &[(&str, &str, &str)],
    required: &[&str],
) -> Tool {
    let mut properties: HashMap<String, ParameterProperty> = HashMap::new();
    for (pname, ptype, pdesc) in params {
        properties.insert(
            (*pname).to_string(),
            ParameterProperty {
                property_type: (*ptype).to_string(),
                description: (*pdesc).to_string(),
                items: None,
                enum_list: None,
            },
        );
    }
    let schema = ParametersSchema {
        schema_type: "object".to_string(),
        properties,
        required: required.iter().map(|s| (*s).to_string()).collect(),
    };
    Tool {
        tool_type: "function".to_string(),
        function: FunctionTool {
            name: name.to_string(),
            description: description.to_string(),
            parameters: serde_json::to_value(schema).unwrap_or(Value::Null),
        },
        cache_control: None,
    }
}
