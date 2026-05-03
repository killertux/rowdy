//! Tools the LLM can call.
//!
//! Three domains:
//! 1. **Schema lookup** — `list_catalogs`, `list_schemas`, `list_tables`,
//!    `describe_table`. These read from the in-memory `SchemaCache`
//!    populated on connect / `:reload`. If a node hasn't been loaded
//!    (most often: columns of a table the user hasn't expanded yet),
//!    the tool returns an empty result with a `note` telling the LLM
//!    to ask the user to expand it. Phase 4 deliberately avoids
//!    triggering on-demand introspection from the worker — that would
//!    mean blocking the chat turn on a database round-trip and adds
//!    a whole new failure mode for marginal benefit.
//!
//! 2. **Editor buffer** — `read_buffer` (paginated) and `write_buffer`
//!    (precise find/replace). The LLM drafts SQL into the user's
//!    editor; the user reviews and runs it themselves. There is no
//!    `execute_query` tool by design — the user retains the run/cancel
//!    decision.
//!
//!    `write_buffer` is intentionally NOT a full overwrite: it requires
//!    a `search` snippet that must match exactly once (optionally
//!    constrained to lines at or after `start_line`). Zero or multiple
//!    matches surface as an error so the LLM extends the snippet rather
//!    than blindly clobbering the buffer. The exact-match contract is
//!    also our main lever against the model treating the buffer as a
//!    scratch surface it can wipe — both the prompt and the tool
//!    description push it toward "anchor + replacement" splicing when
//!    adding fresh SQL alongside the user's existing queries.
//!
//! 3. **Filesystem read** — `read_file`, `list_directory`, `grep_files`.
//!    Let the model read the user's project (migrations, ORM models,
//!    string-builder SQL in app code) so its suggestions are grounded in
//!    the real codebase rather than guessed. All paths route through
//!    [`crate::llm::fs_root::resolve`], which jails them inside the
//!    project root and refuses any `.env*` file. The user's
//!    [`crate::user_config::ReadToolsMode`] decides whether each call
//!    runs immediately (`Auto`), pauses on a y/n approval overlay
//!    (`Ask`, the default), or is refused entirely (`Off`).
//!
//! Tool execution is sync: the action layer pulls the request off
//! the worker channel, calls [`dispatch`], and replies via oneshot.
//! No tool reaches into the network or spawns its own task.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use grep_regex::RegexMatcherBuilder;
use grep_searcher::sinks::UTF8;
use grep_searcher::{BinaryDetection, SearcherBuilder};
use ignore::WalkBuilder;
use llm::chat::{FunctionTool, ParameterProperty, ParametersSchema, Tool};
use serde::Serialize;
use serde_json::Value;

use crate::app::App;
use crate::autocomplete::SchemaCache;
use crate::llm::fs_root;
use crate::worker::IntrospectTarget;

pub const LIST_CATALOGS: &str = "list_catalogs";
pub const LIST_SCHEMAS: &str = "list_schemas";
pub const LIST_TABLES: &str = "list_tables";
pub const DESCRIBE_TABLE: &str = "describe_table";
pub const READ_BUFFER: &str = "read_buffer";
pub const WRITE_BUFFER: &str = "write_buffer";
pub const READ_FILE: &str = "read_file";
pub const LIST_DIRECTORY: &str = "list_directory";
pub const GREP_FILES: &str = "grep_files";

/// Default page size for `read_file`. Mirrors the buffer pagination
/// defaults so the model only has to learn one shape.
const READ_FILE_DEFAULT_LIMIT: usize = 200;
const READ_FILE_MAX_LIMIT: usize = 1000;

/// Don't inhale anything bigger than this. A migration with a million
/// rows of seed data shouldn't blow up the model's context — the LLM
/// can use `grep_files` to find what it actually wants instead.
const READ_FILE_MAX_BYTES: u64 = 512 * 1024;

/// Cap on entries returned by `list_directory`. Anything past this is
/// almost certainly noise (`target/`, `node_modules/`, etc. — though
/// `WalkBuilder` skips those by default for `grep_files`, plain
/// `read_dir` does not).
const LIST_DIRECTORY_MAX_ENTRIES: usize = 500;

/// Defaults / ceilings for `grep_files`.
const GREP_FILES_DEFAULT_MATCHES: usize = 100;
const GREP_FILES_MAX_MATCHES: usize = 500;

/// True for the fs read tools — the chat dispatcher uses this to
/// decide whether the call should pause for user approval when
/// `ReadToolsMode::Ask` is active, or refuse when `Off`.
pub fn is_fs_read_tool(name: &str) -> bool {
    matches!(name, READ_FILE | LIST_DIRECTORY | GREP_FILES)
}

/// Default page size for `read_buffer` when the LLM doesn't pass `limit`.
/// Sized so a typical migration / multi-statement script fits in one or
/// two reads without flooding the model's context.
const READ_BUFFER_DEFAULT_LIMIT: usize = 200;
/// Hard ceiling on `read_buffer.limit` so a runaway request can't dump a
/// pathological buffer in one go.
const READ_BUFFER_MAX_LIMIT: usize = 1000;

/// All tools registered with the LLM, ready to pass to
/// `LLMProvider::chat_stream_with_tools`. Built from the public
/// `Tool`/`FunctionTool` types so we don't depend on `FunctionBuilder`'s
/// private `build()`. Use [`for_mode`] to filter the list according
/// to the user's `ReadToolsMode` preference.
pub fn all() -> Vec<Tool> {
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

/// Tool list filtered by the user's read-tools preference. When
/// `ReadToolsMode::Off`, the fs read tools are stripped from the list
/// so the LLM doesn't even see them in its function catalog. The
/// other modes return the full list — the runtime gate in
/// `action::chat::on_tool_request` decides whether each call pauses
/// for approval (Ask) or runs immediately (Auto).
pub fn for_mode(mode: crate::user_config::ReadToolsMode) -> Vec<Tool> {
    let tools = all();
    if mode == crate::user_config::ReadToolsMode::Off {
        tools
            .into_iter()
            .filter(|t| !is_fs_read_tool(&t.function.name))
            .collect()
    } else {
        tools
    }
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

/// True when `name` reads from the in-memory schema cache. The chat
/// dispatcher uses this to decide whether a cache miss should trigger an
/// auto-introspection (schema tools) or fall through to the regular
/// "tool returned an error" path (buffer tools never miss the cache).
pub fn is_schema_tool(name: &str) -> bool {
    matches!(
        name,
        LIST_CATALOGS | LIST_SCHEMAS | LIST_TABLES | DESCRIBE_TABLE
    )
}

/// Decode the `IntrospectTarget` a schema tool would need. Returns
/// `None` if `name` is not a schema tool or the args don't carry the
/// required fields — the dispatcher then falls back to the synchronous
/// path which surfaces the missing-arg error to the LLM.
pub fn target_for(name: &str, args_json: &str) -> Option<IntrospectTarget> {
    let args: Value = serde_json::from_str(args_json).unwrap_or(Value::Null);
    match name {
        LIST_CATALOGS => Some(IntrospectTarget::Catalogs),
        LIST_SCHEMAS => {
            let catalog = args.get("catalog").and_then(|v| v.as_str())?.to_string();
            Some(IntrospectTarget::Schemas { catalog })
        }
        LIST_TABLES => {
            let catalog = args.get("catalog").and_then(|v| v.as_str())?.to_string();
            let schema = args.get("schema").and_then(|v| v.as_str())?.to_string();
            Some(IntrospectTarget::Tables { catalog, schema })
        }
        DESCRIBE_TABLE => {
            let catalog = args.get("catalog").and_then(|v| v.as_str())?.to_string();
            let schema = args.get("schema").and_then(|v| v.as_str())?.to_string();
            let table = args.get("table").and_then(|v| v.as_str())?.to_string();
            Some(IntrospectTarget::Columns {
                catalog,
                schema,
                table,
            })
        }
        _ => None,
    }
}

/// Whether the slice of the cache `target` references is already
/// populated. `Catalogs` is treated as "cached" iff at least one is
/// present — meaning the empty-database edge case will trigger one
/// re-introspection (acceptable; the result lands in the same empty
/// state and the retry guard prevents a loop).
pub fn is_cached(cache: &SchemaCache, target: &IntrospectTarget) -> bool {
    match target {
        IntrospectTarget::Catalogs => !cache.catalogs.is_empty(),
        IntrospectTarget::Schemas { catalog } => cache.schemas.contains_key(catalog),
        IntrospectTarget::Tables { catalog, schema } => cache
            .tables
            .contains_key(&(catalog.clone(), schema.clone())),
        IntrospectTarget::Columns {
            catalog,
            schema,
            table,
        } => cache
            .columns
            .contains_key(&(catalog.clone(), schema.clone(), table.clone())),
        // Indices aren't surfaced as a tool — never expected here.
        IntrospectTarget::Indices { .. } => true,
    }
}

/// Execute a schema or buffer tool call against `app` state. Returns
/// the result as JSON suitable for stuffing into a `tool_result` message
/// back to the LLM. All errors are surfaced as `{"error": "..."}`
/// rather than `Result::Err` so the LLM can read and recover instead of
/// the worker aborting the whole turn.
///
/// **Filesystem tools** (`read_file`, `list_directory`, `grep_files`)
/// are NOT routed through here — they take real I/O time (grep can
/// walk a whole repo) and would block the UI loop. The action layer
/// runs them through [`dispatch_fs`] inside `tokio::task::spawn_blocking`
/// instead. Calling `dispatch` with one of those names returns an
/// internal-error payload rather than silently working on the main
/// thread.
pub fn dispatch(app: &mut App, name: &str, args_json: &str) -> Value {
    let args: Value = serde_json::from_str(args_json).unwrap_or(Value::Null);
    match name {
        LIST_CATALOGS => list_catalogs(app),
        LIST_SCHEMAS => list_schemas(app, &args),
        LIST_TABLES => list_tables(app, &args),
        DESCRIBE_TABLE => describe_table(app, &args),
        READ_BUFFER => read_buffer(app, &args),
        WRITE_BUFFER => write_buffer(app, &args),
        READ_FILE | LIST_DIRECTORY | GREP_FILES => err(format!(
            "internal: {name} must be dispatched via dispatch_fs (spawn_blocking)"
        )),
        _ => err(format!("unknown tool: {name}")),
    }
}

/// Off-thread tool dispatch for the filesystem read tools. Takes only
/// `(project_root, args_json)` so the call can travel into a
/// `tokio::task::spawn_blocking` closure without holding any reference
/// to `App`. Returns the same JSON shape as [`dispatch`] — caller
/// serializes into the `tool_result` payload + UI display string.
/// Only invoked when [`crate::user_config::ReadToolsMode`] is `Auto`
/// or post-approval `Ask`; `Off` mode is intercepted earlier.
pub fn dispatch_fs(project_root: &Path, name: &str, args_json: &str) -> Value {
    let args: Value = serde_json::from_str(args_json).unwrap_or(Value::Null);
    match name {
        READ_FILE => read_file_at(project_root, &args),
        LIST_DIRECTORY => list_directory_at(project_root, &args),
        GREP_FILES => grep_files_at(project_root, &args),
        _ => err(format!("not an fs read tool: {name}")),
    }
}

#[derive(Debug, Serialize)]
struct CatalogList {
    catalogs: Vec<String>,
}

#[derive(Debug, Serialize)]
struct SchemaList {
    schemas: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
}

#[derive(Debug, Serialize)]
struct TableEntry {
    name: String,
    kind: &'static str,
}

#[derive(Debug, Serialize)]
struct TableList {
    tables: Vec<TableEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
}

#[derive(Debug, Serialize)]
struct ColumnEntry {
    name: String,
    #[serde(rename = "type")]
    ty: String,
}

#[derive(Debug, Serialize)]
struct ColumnList {
    columns: Vec<ColumnEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
}

fn list_catalogs(app: &App) -> Value {
    let cache = app.schema_cache.read().unwrap();
    serde_json::to_value(CatalogList {
        catalogs: cache.catalogs.clone(),
    })
    .unwrap_or(Value::Null)
}

fn list_schemas(app: &App, args: &Value) -> Value {
    let Some(catalog) = args.get("catalog").and_then(|v| v.as_str()) else {
        return err("missing required `catalog` argument");
    };
    let cache = app.schema_cache.read().unwrap();
    let (schemas, note) = match cache.schemas.get(catalog) {
        Some(v) => (v.clone(), None),
        None => (
            Vec::new(),
            Some(format!(
                "schemas of catalog {catalog:?} could not be loaded — verify the catalog name with list_catalogs"
            )),
        ),
    };
    serde_json::to_value(SchemaList { schemas, note }).unwrap_or(Value::Null)
}

fn list_tables(app: &App, args: &Value) -> Value {
    let Some(catalog) = args.get("catalog").and_then(|v| v.as_str()) else {
        return err("missing required `catalog` argument");
    };
    let Some(schema) = args.get("schema").and_then(|v| v.as_str()) else {
        return err("missing required `schema` argument");
    };
    let cache = app.schema_cache.read().unwrap();
    let key = (catalog.to_string(), schema.to_string());
    let (tables, note) = match cache.tables.get(&key) {
        Some(v) => (
            v.iter()
                .map(|t| TableEntry {
                    name: t.name.clone(),
                    kind: table_kind_str(t.kind),
                })
                .collect(),
            None,
        ),
        None => (
            Vec::new(),
            Some(format!(
                "tables of {catalog:?}.{schema:?} could not be loaded — verify the names with list_schemas"
            )),
        ),
    };
    serde_json::to_value(TableList { tables, note }).unwrap_or(Value::Null)
}

fn describe_table(app: &App, args: &Value) -> Value {
    let Some(catalog) = args.get("catalog").and_then(|v| v.as_str()) else {
        return err("missing required `catalog` argument");
    };
    let Some(schema) = args.get("schema").and_then(|v| v.as_str()) else {
        return err("missing required `schema` argument");
    };
    let Some(table) = args.get("table").and_then(|v| v.as_str()) else {
        return err("missing required `table` argument");
    };
    let cache = app.schema_cache.read().unwrap();
    let key = (catalog.to_string(), schema.to_string(), table.to_string());
    let (columns, note) = match cache.columns.get(&key) {
        Some(v) => (
            v.iter()
                .map(|c| ColumnEntry {
                    name: c.name.clone(),
                    ty: c.type_name.clone(),
                })
                .collect(),
            None,
        ),
        None => (
            Vec::new(),
            Some(format!(
                "columns of {catalog:?}.{schema:?}.{table:?} could not be loaded — \
                 verify the names with list_tables"
            )),
        ),
    };
    serde_json::to_value(ColumnList { columns, note }).unwrap_or(Value::Null)
}

fn read_buffer(app: &App, args: &Value) -> Value {
    paginate_buffer(&app.editor.text(), args)
}

fn write_buffer(app: &mut App, args: &Value) -> Value {
    let buffer = app.editor.text();
    match splice_buffer(&buffer, args) {
        Err(msg) => err(msg),
        Ok(spliced) => {
            app.editor
                .replace_text_at_row(&spliced.new_text, spliced.match_row);
            app.editor_dirty = true;
            serde_json::json!({
                "ok": true,
                "line": spliced.match_row + 1,
            })
        }
    }
}

fn read_file_at(project_root: &Path, args: &Value) -> Value {
    let Some(path_arg) = args.get("path").and_then(|v| v.as_str()) else {
        return err("missing required `path` argument");
    };
    let resolved = match fs_root::resolve(project_root, path_arg, true) {
        Ok(p) => p,
        Err(msg) => return err(msg),
    };
    if !resolved.is_file() {
        return err(format!("not a file: {path_arg:?}"));
    }
    let metadata = match fs::metadata(&resolved) {
        Ok(m) => m,
        Err(e) => return err(format!("stat failed: {e}")),
    };
    if metadata.len() > READ_FILE_MAX_BYTES {
        return err(format!(
            "{path_arg:?} is {} bytes, larger than the {} byte cap — use grep_files to find what you need",
            metadata.len(),
            READ_FILE_MAX_BYTES
        ));
    }
    let bytes = match fs::read(&resolved) {
        Ok(b) => b,
        Err(e) => return err(format!("read failed: {e}")),
    };
    let text = match String::from_utf8(bytes) {
        Ok(s) => s,
        Err(_) => {
            return err(format!(
                "{path_arg:?} is not valid UTF-8 — use grep_files for a binary or non-UTF-8 file"
            ));
        }
    };
    paginate_file(&text, args)
}

fn list_directory_at(project_root: &Path, args: &Value) -> Value {
    let path_arg = args
        .get("path")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let resolved = match fs_root::resolve(project_root, path_arg, true) {
        Ok(p) => p,
        Err(msg) => return err(msg),
    };
    if !resolved.is_dir() {
        return err(format!("not a directory: {path_arg:?}"));
    }

    let read = match fs::read_dir(&resolved) {
        Ok(r) => r,
        Err(e) => return err(format!("read_dir failed: {e}")),
    };

    let mut entries: Vec<DirEntry> = Vec::new();
    let mut truncated = false;
    for raw in read {
        let Ok(raw) = raw else { continue };
        let name = raw.file_name().to_string_lossy().to_string();
        // Defense in depth: even though `resolve` would refuse a
        // subsequent read_file on a .env*, hide them from the listing
        // so the model doesn't even learn they exist.
        if is_env_filename(&name) {
            continue;
        }
        let kind = match raw.file_type() {
            Ok(ft) if ft.is_dir() => "dir",
            Ok(ft) if ft.is_symlink() => "symlink",
            Ok(_) => "file",
            Err(_) => continue,
        };
        if entries.len() >= LIST_DIRECTORY_MAX_ENTRIES {
            truncated = true;
            break;
        }
        entries.push(DirEntry { name, kind });
    }
    // Dirs first, then files, alphabetical within each group.
    entries.sort_by(|a, b| match (a.kind, b.kind) {
        ("dir", "dir") | ("file", "file") | ("symlink", "symlink") => a.name.cmp(&b.name),
        ("dir", _) => std::cmp::Ordering::Less,
        (_, "dir") => std::cmp::Ordering::Greater,
        _ => a.name.cmp(&b.name),
    });

    let mut json = serde_json::json!({
        "entries": entries,
    });
    if truncated
        && let Some(obj) = json.as_object_mut()
    {
        obj.insert(
            "note".into(),
            Value::String(format!(
                "listing truncated at {LIST_DIRECTORY_MAX_ENTRIES} entries — narrow with a subdirectory path"
            )),
        );
    }
    json
}

fn grep_files_at(project_root: &Path, args: &Value) -> Value {
    let Some(pattern) = args.get("pattern").and_then(|v| v.as_str()) else {
        return err("missing required `pattern` argument");
    };
    if pattern.is_empty() {
        return err("`pattern` must not be empty");
    }
    let path_arg = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
    let case_insensitive = args
        .get("case_insensitive")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let cap = args
        .get("max_matches")
        .and_then(|v| v.as_u64())
        .map(|n| (n as usize).clamp(1, GREP_FILES_MAX_MATCHES))
        .unwrap_or(GREP_FILES_DEFAULT_MATCHES);

    let root = match fs_root::resolve(project_root, path_arg, true) {
        Ok(p) => p,
        Err(msg) => return err(msg),
    };

    let matcher = match RegexMatcherBuilder::new()
        .case_insensitive(case_insensitive)
        .build(pattern)
    {
        Ok(m) => m,
        Err(e) => return err(format!("invalid regex: {e}")),
    };

    let mut searcher = SearcherBuilder::new()
        .binary_detection(BinaryDetection::quit(b'\0'))
        .line_number(true)
        .build();

    let mut matches: Vec<Value> = Vec::new();
    let mut truncated = false;

    let walker = WalkBuilder::new(&root)
        // The walker already skips hidden dot-files by default, which
        // covers `.env*` from this side. Belt-and-braces: the per-file
        // check below skips them again in case `WalkBuilder` ever
        // changes its defaults.
        .build();
    for entry in walker {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        if path
            .file_name()
            .and_then(|s| s.to_str())
            .map(is_env_filename)
            .unwrap_or(false)
        {
            continue;
        }

        let rel = display_relative(project_root, path);
        // Each call to `search_path` is bounded by the per-file file
        // size in grep-searcher's buffer; we further bound the total
        // collected matches via `cap`. Returning `Ok(false)` from the
        // sink callback tells grep-searcher to stop searching the
        // current file.
        let _ = searcher.search_path(
            &matcher,
            path,
            UTF8(|line_num, line| {
                matches.push(serde_json::json!({
                    "path": rel.clone(),
                    "line": line_num,
                    "text": line.trim_end_matches('\n').trim_end_matches('\r'),
                }));
                Ok(matches.len() < cap)
            }),
        );
        if matches.len() >= cap {
            // The sink stops the search before pushing past `cap`, so
            // landing exactly on the cap is the natural "we filled up"
            // signal — flag truncation and stop walking further files.
            truncated = true;
            break;
        }
    }

    serde_json::json!({
        "matches": matches,
        "truncated": truncated,
    })
}

fn display_relative(root: &Path, path: &Path) -> String {
    fs_root::display_relative(root, path)
}

fn is_env_filename(name: &str) -> bool {
    name == ".env" || name.starts_with(".env.")
}

#[derive(Debug, Serialize)]
struct DirEntry {
    name: String,
    kind: &'static str,
}

/// Pagination for arbitrary text — same envelope as `paginate_buffer`.
/// Lifted into a separate name so the file tool can be tested without
/// constructing an `App`, and so the buffer-pagination contract isn't
/// accidentally widened by file edits.
fn paginate_file(text: &str, args: &Value) -> Value {
    let lines: Vec<&str> = if text.is_empty() {
        Vec::new()
    } else {
        text.split('\n').collect()
    };
    let total = lines.len();
    let start_line = args
        .get("start_line")
        .and_then(|v| v.as_u64())
        .map(|n| (n.max(1)) as usize)
        .unwrap_or(1);
    let limit = args
        .get("limit")
        .and_then(|v| v.as_u64())
        .map(|n| (n as usize).clamp(1, READ_FILE_MAX_LIMIT))
        .unwrap_or(READ_FILE_DEFAULT_LIMIT);

    if total == 0 || start_line > total {
        let end_line = start_line.saturating_sub(1);
        return serde_json::json!({
            "text": "",
            "start_line": start_line,
            "end_line": end_line,
            "total_lines": total,
            "remaining_lines": 0,
            "note": format!(
                "start_line {start_line} is past end of file ({total} lines total)"
            ),
        });
    }

    let start_idx = start_line - 1;
    let end_idx = (start_idx + limit).min(total);
    let slice = &lines[start_idx..end_idx];
    let body = slice.join("\n");
    let end_line = end_idx;
    let remaining = total.saturating_sub(end_line);

    serde_json::json!({
        "text": body,
        "start_line": start_line,
        "end_line": end_line,
        "total_lines": total,
        "remaining_lines": remaining,
    })
}

/// Pure pagination over `buffer`. Lifted out of [`read_buffer`] so tests
/// can hit it without constructing an `App`.
///
/// Lines are 1-indexed in the API. The implementation splits on `\n` and
/// treats an empty buffer as zero lines (so `start_line=1` past EOF
/// returns the empty / past-EOF shape rather than panicking on slicing).
pub(crate) fn paginate_buffer(buffer: &str, args: &Value) -> Value {
    let lines: Vec<&str> = if buffer.is_empty() {
        Vec::new()
    } else {
        buffer.split('\n').collect()
    };
    let total = lines.len();

    let start_line = args
        .get("start_line")
        .and_then(|v| v.as_u64())
        .map(|n| (n.max(1)) as usize)
        .unwrap_or(1);
    let limit = args
        .get("limit")
        .and_then(|v| v.as_u64())
        .map(|n| (n as usize).clamp(1, READ_BUFFER_MAX_LIMIT))
        .unwrap_or(READ_BUFFER_DEFAULT_LIMIT);

    if total == 0 || start_line > total {
        // Past EOF (or empty buffer): return an empty page with a `note`
        // so the LLM stops paging instead of looping forever.
        let end_line = start_line.saturating_sub(1);
        return serde_json::json!({
            "text": "",
            "start_line": start_line,
            "end_line": end_line,
            "total_lines": total,
            "remaining_lines": 0,
            "note": format!(
                "start_line {start_line} is past end of buffer ({total} lines total)"
            ),
        });
    }

    let start_idx = start_line - 1;
    let end_idx = (start_idx + limit).min(total); // exclusive
    let slice = &lines[start_idx..end_idx];
    let text = slice.join("\n");
    let end_line = end_idx; // 1-indexed inclusive end
    let remaining = total.saturating_sub(end_line);

    serde_json::json!({
        "text": text,
        "start_line": start_line,
        "end_line": end_line,
        "total_lines": total,
        "remaining_lines": remaining,
    })
}

/// Successful result of [`splice_buffer`]: the new buffer plus the
/// 0-indexed row where the replacement now starts (so the editor can
/// park the cursor there).
#[derive(Debug)]
pub(crate) struct Splice {
    pub new_text: String,
    pub match_row: usize,
}

/// Pure find-replace over `buffer`. Lifted out of [`write_buffer`] so
/// tests can exercise it directly. Errors come back as `Err(String)`
/// which the caller wraps with [`err`] before sending to the LLM.
///
/// Matching is plain UTF-8 substring (not regex) because we want the
/// LLM to copy a verbatim chunk from `read_buffer` output. `start_line`
/// (1-indexed) restricts the search to the byte range from that line's
/// start onward — handy for "replace the second SELECT" without growing
/// the snippet to disambiguate.
pub(crate) fn splice_buffer(buffer: &str, args: &Value) -> Result<Splice, String> {
    let Some(search) = args.get("search").and_then(|v| v.as_str()) else {
        return Err("missing required `search` argument".to_string());
    };
    if search.is_empty() {
        return Err("`search` must not be empty".to_string());
    }
    let Some(replacement) = args.get("replacement").and_then(|v| v.as_str()) else {
        return Err("missing required `replacement` argument".to_string());
    };
    let start_line = args
        .get("start_line")
        .and_then(|v| v.as_u64())
        .map(|n| (n.max(1)) as usize)
        .unwrap_or(1);

    // Resolve the byte offset of `start_line`. Line 1 is offset 0; line N
    // begins one byte past the (N-1)th newline.
    let region_start_byte = if start_line == 1 {
        0
    } else {
        let mut nl_count = 0usize;
        let mut found: Option<usize> = None;
        for (i, b) in buffer.bytes().enumerate() {
            if b == b'\n' {
                nl_count += 1;
                if nl_count == start_line - 1 {
                    found = Some(i + 1);
                    break;
                }
            }
        }
        match found {
            Some(off) => off,
            None => {
                let total = if buffer.is_empty() {
                    0
                } else {
                    buffer.split('\n').count()
                };
                return Err(format!(
                    "start_line {start_line} is past end of buffer ({total} lines)"
                ));
            }
        }
    };

    let region = &buffer[region_start_byte..];
    let mut matches = region.match_indices(search);
    let first = matches.next();
    let second = matches.next();
    match (first, second) {
        (None, _) => {
            let scope = if start_line > 1 {
                format!(" (searching from line {start_line})")
            } else {
                String::new()
            };
            Err(format!(
                "search string not found in buffer{scope} — call read_buffer to confirm the actual text"
            ))
        }
        (Some(_), Some(_)) => Err(
            "search string matches more than once — extend it with surrounding context so it's unique"
                .to_string(),
        ),
        (Some((rel_pos, _)), None) => {
            let match_start_byte = region_start_byte + rel_pos;
            let match_end_byte = match_start_byte + search.len();
            let mut new_text =
                String::with_capacity(buffer.len() - search.len() + replacement.len());
            new_text.push_str(&buffer[..match_start_byte]);
            new_text.push_str(replacement);
            new_text.push_str(&buffer[match_end_byte..]);

            // 0-indexed row of the match (used by EditorPanel::replace_text_at_row).
            let match_row = buffer[..match_start_byte]
                .bytes()
                .filter(|&b| b == b'\n')
                .count();

            Ok(Splice {
                new_text,
                match_row,
            })
        }
    }
}

fn table_kind_str(kind: crate::datasource::schema::TableKind) -> &'static str {
    use crate::datasource::schema::TableKind;
    match kind {
        TableKind::Table => "table",
        TableKind::View => "view",
    }
}

fn err(msg: impl Into<String>) -> Value {
    serde_json::json!({ "error": msg.into() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_tools_have_unique_names_matching_constants() {
        let tools = all();
        let names: Vec<&str> = tools.iter().map(|t| t.function.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                LIST_CATALOGS,
                LIST_SCHEMAS,
                LIST_TABLES,
                DESCRIBE_TABLE,
                READ_BUFFER,
                WRITE_BUFFER,
                READ_FILE,
                LIST_DIRECTORY,
                GREP_FILES,
            ]
        );
        // No duplicates.
        let mut sorted = names.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), names.len());
    }

    #[test]
    fn is_fs_read_tool_only_matches_new_tools() {
        assert!(is_fs_read_tool(READ_FILE));
        assert!(is_fs_read_tool(LIST_DIRECTORY));
        assert!(is_fs_read_tool(GREP_FILES));
        assert!(!is_fs_read_tool(READ_BUFFER));
        assert!(!is_fs_read_tool(WRITE_BUFFER));
        assert!(!is_fs_read_tool(LIST_CATALOGS));
        assert!(!is_fs_read_tool(DESCRIBE_TABLE));
    }

    #[test]
    fn for_mode_strips_fs_tools_when_off() {
        use crate::user_config::ReadToolsMode;
        let off = for_mode(ReadToolsMode::Off);
        let names: Vec<&str> = off.iter().map(|t| t.function.name.as_str()).collect();
        assert!(!names.contains(&READ_FILE));
        assert!(!names.contains(&LIST_DIRECTORY));
        assert!(!names.contains(&GREP_FILES));
        // Schema and buffer tools must still be present.
        assert!(names.contains(&LIST_CATALOGS));
        assert!(names.contains(&READ_BUFFER));
    }

    #[test]
    fn for_mode_keeps_fs_tools_for_ask_and_auto() {
        use crate::user_config::ReadToolsMode;
        for mode in [ReadToolsMode::Ask, ReadToolsMode::Auto] {
            let list = for_mode(mode);
            let names: Vec<&str> = list.iter().map(|t| t.function.name.as_str()).collect();
            assert!(names.contains(&READ_FILE), "{mode:?} missing READ_FILE");
            assert!(names.contains(&LIST_DIRECTORY));
            assert!(names.contains(&GREP_FILES));
        }
    }

    #[test]
    fn tools_have_object_parameters_schema() {
        // Both OpenAI and Anthropic require parameters be a JSON-schema
        // `object` even when the function takes no arguments. Catch any
        // accidental `Value::Null` slipping through.
        for tool in all() {
            let params = &tool.function.parameters;
            let ty = params
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            assert_eq!(
                ty, "object",
                "{} parameters not an object",
                tool.function.name
            );
            assert!(params.get("properties").is_some());
            assert!(params.get("required").is_some());
        }
    }

    #[test]
    fn err_helper_shape() {
        let v = err("boom");
        assert_eq!(v.get("error").and_then(|s| s.as_str()), Some("boom"));
    }

    #[test]
    fn target_for_decodes_each_schema_tool() {
        assert_eq!(
            target_for(LIST_CATALOGS, "{}"),
            Some(IntrospectTarget::Catalogs)
        );
        assert_eq!(
            target_for(LIST_SCHEMAS, r#"{"catalog":"db"}"#),
            Some(IntrospectTarget::Schemas {
                catalog: "db".into()
            })
        );
        assert_eq!(
            target_for(LIST_TABLES, r#"{"catalog":"db","schema":"public"}"#),
            Some(IntrospectTarget::Tables {
                catalog: "db".into(),
                schema: "public".into(),
            })
        );
        assert_eq!(
            target_for(
                DESCRIBE_TABLE,
                r#"{"catalog":"db","schema":"public","table":"users"}"#
            ),
            Some(IntrospectTarget::Columns {
                catalog: "db".into(),
                schema: "public".into(),
                table: "users".into(),
            })
        );
    }

    #[test]
    fn target_for_returns_none_when_args_missing() {
        assert!(target_for(LIST_SCHEMAS, "{}").is_none());
        assert!(target_for(LIST_TABLES, r#"{"catalog":"db"}"#).is_none());
        assert!(target_for(READ_BUFFER, "{}").is_none());
    }

    // ----- paginate_buffer -----------------------------------------------

    #[test]
    fn paginate_buffer_defaults_return_full_short_buffer() {
        let text = "one\ntwo\nthree";
        let v = paginate_buffer(text, &Value::Null);
        assert_eq!(
            v.get("text").and_then(|s| s.as_str()),
            Some("one\ntwo\nthree")
        );
        assert_eq!(v.get("start_line").and_then(|n| n.as_u64()), Some(1));
        assert_eq!(v.get("end_line").and_then(|n| n.as_u64()), Some(3));
        assert_eq!(v.get("total_lines").and_then(|n| n.as_u64()), Some(3));
        assert_eq!(v.get("remaining_lines").and_then(|n| n.as_u64()), Some(0));
        assert!(v.get("note").is_none());
    }

    #[test]
    fn paginate_buffer_respects_start_line_and_limit() {
        let text = (1..=20)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        let args = serde_json::json!({ "start_line": 5, "limit": 3 });
        let v = paginate_buffer(&text, &args);
        assert_eq!(
            v.get("text").and_then(|s| s.as_str()),
            Some("line 5\nline 6\nline 7")
        );
        assert_eq!(v.get("start_line").and_then(|n| n.as_u64()), Some(5));
        assert_eq!(v.get("end_line").and_then(|n| n.as_u64()), Some(7));
        assert_eq!(v.get("total_lines").and_then(|n| n.as_u64()), Some(20));
        assert_eq!(v.get("remaining_lines").and_then(|n| n.as_u64()), Some(13));
    }

    #[test]
    fn paginate_buffer_limit_is_clamped() {
        // Limit=0 clamps up to 1; limit > MAX clamps down. Both branches
        // must produce a valid page rather than panicking.
        let text = "a\nb\nc\nd";
        let lo = paginate_buffer(text, &serde_json::json!({ "limit": 0 }));
        assert_eq!(lo.get("end_line").and_then(|n| n.as_u64()), Some(1));
        let hi = paginate_buffer(
            text,
            &serde_json::json!({ "limit": (READ_BUFFER_MAX_LIMIT * 2) as u64 }),
        );
        assert_eq!(hi.get("end_line").and_then(|n| n.as_u64()), Some(4));
    }

    #[test]
    fn paginate_buffer_past_eof_returns_note() {
        let text = "only one";
        let v = paginate_buffer(text, &serde_json::json!({ "start_line": 5 }));
        assert_eq!(v.get("text").and_then(|s| s.as_str()), Some(""));
        assert_eq!(v.get("remaining_lines").and_then(|n| n.as_u64()), Some(0));
        assert!(v.get("note").is_some());
    }

    #[test]
    fn paginate_buffer_handles_empty_buffer() {
        let v = paginate_buffer("", &Value::Null);
        assert_eq!(v.get("text").and_then(|s| s.as_str()), Some(""));
        assert_eq!(v.get("total_lines").and_then(|n| n.as_u64()), Some(0));
        assert!(v.get("note").is_some());
    }

    // ----- splice_buffer -------------------------------------------------

    #[test]
    fn splice_buffer_replaces_unique_match() {
        let buf = "SELECT a, b\nFROM users\nWHERE id = 1;";
        let args = serde_json::json!({ "search": "FROM users", "replacement": "FROM accounts" });
        let out = splice_buffer(buf, &args).expect("splice succeeds");
        assert_eq!(out.new_text, "SELECT a, b\nFROM accounts\nWHERE id = 1;");
        // Match is on row 1 (0-indexed) — the second line.
        assert_eq!(out.match_row, 1);
    }

    #[test]
    fn splice_buffer_errors_on_no_match() {
        let buf = "SELECT 1;";
        let args = serde_json::json!({ "search": "DROP TABLE", "replacement": "SELECT 2" });
        let err = splice_buffer(buf, &args).unwrap_err();
        assert!(err.contains("not found"), "got: {err}");
    }

    #[test]
    fn splice_buffer_errors_on_ambiguous_match() {
        // Two SELECTs — `SELECT` alone matches both, the LLM must extend.
        let buf = "SELECT a FROM t1;\nSELECT b FROM t2;";
        let args = serde_json::json!({ "search": "SELECT", "replacement": "WITH" });
        let err = splice_buffer(buf, &args).unwrap_err();
        assert!(err.contains("more than once"), "got: {err}");
    }

    #[test]
    fn splice_buffer_start_line_constrains_search() {
        // `SELECT 1` appears on lines 1 and 3. With start_line=2 only the
        // second occurrence is in scope, so the splice succeeds and lands
        // on row index 2.
        let buf = "SELECT 1;\n-- comment\nSELECT 1;";
        let args = serde_json::json!({
            "search": "SELECT 1",
            "replacement": "SELECT 99",
            "start_line": 2,
        });
        let out = splice_buffer(buf, &args).expect("splice succeeds");
        assert_eq!(out.new_text, "SELECT 1;\n-- comment\nSELECT 99;");
        assert_eq!(out.match_row, 2);
    }

    #[test]
    fn splice_buffer_start_line_past_eof_errors() {
        let buf = "one line";
        let args = serde_json::json!({
            "search": "one",
            "replacement": "two",
            "start_line": 5,
        });
        assert!(
            splice_buffer(buf, &args)
                .unwrap_err()
                .contains("past end of buffer")
        );
    }

    #[test]
    fn splice_buffer_rejects_empty_search() {
        let args = serde_json::json!({ "search": "", "replacement": "x" });
        assert!(
            splice_buffer("abc", &args)
                .unwrap_err()
                .contains("must not be empty")
        );
    }

    #[test]
    fn splice_buffer_missing_args() {
        // Missing search.
        let args = serde_json::json!({ "replacement": "x" });
        assert!(
            splice_buffer("abc", &args)
                .unwrap_err()
                .contains("`search`")
        );
        // Missing replacement.
        let args = serde_json::json!({ "search": "abc" });
        assert!(
            splice_buffer("abc", &args)
                .unwrap_err()
                .contains("`replacement`")
        );
    }

    #[test]
    fn is_cached_tracks_population() {
        use crate::autocomplete::cache::CachedTable;
        use crate::datasource::schema::TableKind;
        let mut cache = SchemaCache::new();
        let tgt = IntrospectTarget::Tables {
            catalog: "db".into(),
            schema: "public".into(),
        };
        assert!(!is_cached(&cache, &tgt));
        cache.tables.insert(
            ("db".into(), "public".into()),
            vec![CachedTable {
                name: "users".into(),
                kind: TableKind::Table,
            }],
        );
        assert!(is_cached(&cache, &tgt));

        // Empty-but-present is still "cached" — introspection succeeded
        // with no rows, so the tool should report the empty list, not
        // re-introspect.
        let cols = IntrospectTarget::Columns {
            catalog: "db".into(),
            schema: "public".into(),
            table: "empty".into(),
        };
        cache
            .columns
            .insert(("db".into(), "public".into(), "empty".into()), Vec::new());
        assert!(is_cached(&cache, &cols));

        // Catalogs uses non-empty as the "loaded" signal.
        assert!(!is_cached(&cache, &IntrospectTarget::Catalogs));
        cache.catalogs.push("db".into());
        assert!(is_cached(&cache, &IntrospectTarget::Catalogs));
    }

    // ----- read_file / list_directory / grep_files ----------------------
    //
    // These tests target the pure helpers (`paginate_file`) and the
    // `grep_files`/`list_directory` flow with a hand-rolled fixture
    // tempdir + a stub `App.project_root`. They don't need a worker
    // channel or schema cache — those fields aren't touched by the fs
    // tools.

    use std::fs as stdfs;
    use std::path::PathBuf;

    fn fs_tempdir() -> PathBuf {
        let p = std::env::temp_dir().join(format!("rowdy-tools-{}", uuid::Uuid::new_v4()));
        stdfs::create_dir_all(&p).unwrap();
        p.canonicalize().unwrap()
    }

    #[test]
    fn paginate_file_returns_full_short_text() {
        let v = paginate_file("a\nb\nc", &Value::Null);
        assert_eq!(v.get("text").and_then(|s| s.as_str()), Some("a\nb\nc"));
        assert_eq!(v.get("end_line").and_then(|n| n.as_u64()), Some(3));
        assert_eq!(v.get("remaining_lines").and_then(|n| n.as_u64()), Some(0));
    }

    #[test]
    fn paginate_file_respects_pagination() {
        let body = (1..=20)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        let args = serde_json::json!({ "start_line": 5, "limit": 3 });
        let v = paginate_file(&body, &args);
        assert_eq!(
            v.get("text").and_then(|s| s.as_str()),
            Some("line 5\nline 6\nline 7")
        );
        assert_eq!(v.get("remaining_lines").and_then(|n| n.as_u64()), Some(13));
    }

    #[test]
    fn paginate_file_past_eof_returns_note() {
        let v = paginate_file("only one", &serde_json::json!({ "start_line": 5 }));
        assert!(v.get("note").is_some());
    }

    #[test]
    fn list_directory_filters_dotenv_and_sorts_dirs_first() {
        let root = fs_tempdir();
        stdfs::write(root.join(".env"), "X=1").unwrap();
        stdfs::write(root.join(".env.local"), "X=1").unwrap();
        stdfs::write(root.join("file_b.txt"), "b").unwrap();
        stdfs::write(root.join("file_a.txt"), "a").unwrap();
        stdfs::create_dir_all(root.join("dir_z")).unwrap();
        stdfs::create_dir_all(root.join("dir_a")).unwrap();

        let v = list_directory_at(&root, &Value::Null);

        let entries = v.get("entries").and_then(|e| e.as_array()).unwrap();
        let names: Vec<&str> = entries
            .iter()
            .map(|e| e.get("name").and_then(|n| n.as_str()).unwrap())
            .collect();
        assert_eq!(names, vec!["dir_a", "dir_z", "file_a.txt", "file_b.txt"]);
        assert!(!names.iter().any(|n| n.starts_with(".env")));
    }

    #[test]
    fn read_file_paginates_and_refuses_dotenv() {
        let root = fs_tempdir();
        stdfs::write(root.join("README.md"), "alpha\nbeta\ngamma").unwrap();
        stdfs::write(root.join(".env"), "SECRET=1").unwrap();

        let v = read_file_at(&root, &serde_json::json!({"path": "README.md"}));
        assert_eq!(
            v.get("text").and_then(|s| s.as_str()),
            Some("alpha\nbeta\ngamma")
        );

        let denied = read_file_at(&root, &serde_json::json!({"path": ".env"}));
        assert!(
            denied
                .get("error")
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .contains(".env"),
            "got: {denied:?}"
        );
    }

    #[test]
    fn read_file_rejects_oversized_files() {
        let root = fs_tempdir();
        let huge = "x".repeat((READ_FILE_MAX_BYTES + 1) as usize);
        stdfs::write(root.join("huge.txt"), huge).unwrap();
        let v = read_file_at(&root, &serde_json::json!({"path": "huge.txt"}));
        let err_msg = v.get("error").and_then(|s| s.as_str()).unwrap_or("");
        assert!(err_msg.contains("larger than"), "got: {err_msg}");
    }

    #[test]
    fn grep_files_finds_regex_matches_and_skips_dotenv() {
        let root = fs_tempdir();
        stdfs::write(
            root.join("schema.sql"),
            "CREATE TABLE users (id INT);\nCREATE TABLE orders (id INT);\n",
        )
        .unwrap();
        stdfs::write(
            root.join("seed.sql"),
            "INSERT INTO users VALUES (1);\n",
        )
        .unwrap();
        stdfs::write(root.join(".env"), "DATABASE_URL=postgres://u@h/db").unwrap();

        let v = grep_files_at(&root, &serde_json::json!({"pattern": "(?i)create table"}));
        let matches = v.get("matches").and_then(|m| m.as_array()).unwrap();
        assert_eq!(matches.len(), 2, "got: {matches:?}");
        assert!(matches.iter().all(|m| m
            .get("path")
            .and_then(|p| p.as_str())
            .map(|p| p.ends_with("schema.sql"))
            .unwrap_or(false)));
        // The .env match must NOT appear.
        let v2 = grep_files_at(&root, &serde_json::json!({"pattern": "DATABASE_URL"}));
        let m2 = v2.get("matches").and_then(|m| m.as_array()).unwrap();
        assert!(m2.is_empty(), "should not leak .env: {m2:?}");
    }

    #[test]
    fn grep_files_rejects_invalid_regex() {
        let root = fs_tempdir();
        let v = grep_files_at(&root, &serde_json::json!({"pattern": "[unclosed"}));
        assert!(v.get("error").is_some());
    }

    #[test]
    fn grep_files_caps_max_matches() {
        let root = fs_tempdir();
        let many = (1..=300)
            .map(|n| format!("hit_{n}"))
            .collect::<Vec<_>>()
            .join("\n");
        stdfs::write(root.join("hits.txt"), many).unwrap();
        let v = grep_files_at(
            &root,
            &serde_json::json!({"pattern": "hit_", "max_matches": 50}),
        );
        let matches = v.get("matches").and_then(|m| m.as_array()).unwrap();
        assert_eq!(matches.len(), 50);
        assert_eq!(v.get("truncated").and_then(|b| b.as_bool()), Some(true));
    }

    #[test]
    fn dispatch_fs_handles_each_fs_tool() {
        // Round-trip every fs tool through `dispatch_fs` so a future
        // refactor that drops an arm fails loudly here. Empty args is
        // intentional — the tools should return a structured error
        // rather than panic.
        for tool in [READ_FILE, LIST_DIRECTORY, GREP_FILES] {
            let root = fs_tempdir();
            let v = dispatch_fs(&root, tool, "{}");
            assert!(v.is_object(), "{tool} returned non-object: {v:?}");
        }
    }

    #[test]
    fn dispatch_fs_rejects_non_fs_tool_names() {
        // A schema or buffer tool name routed to `dispatch_fs` should
        // fail closed — that path runs in `spawn_blocking` and has no
        // access to the App-state those tools need.
        let root = fs_tempdir();
        let v = dispatch_fs(&root, LIST_CATALOGS, "{}");
        assert!(v.get("error").is_some());
    }
}
