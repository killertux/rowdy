//! Tools the LLM can call.
//!
//! Two domains:
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
//! Tool execution is sync: the action layer pulls the request off
//! the worker channel, calls [`dispatch`], and replies via oneshot.
//! No tool reaches into the network or spawns its own task.

use std::collections::HashMap;

use llm::chat::{FunctionTool, ParameterProperty, ParametersSchema, Tool};
use serde::Serialize;
use serde_json::Value;

use crate::app::App;
use crate::autocomplete::SchemaCache;
use crate::worker::IntrospectTarget;

pub const LIST_CATALOGS: &str = "list_catalogs";
pub const LIST_SCHEMAS: &str = "list_schemas";
pub const LIST_TABLES: &str = "list_tables";
pub const DESCRIBE_TABLE: &str = "describe_table";
pub const READ_BUFFER: &str = "read_buffer";
pub const WRITE_BUFFER: &str = "write_buffer";

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
/// private `build()`.
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

/// Execute a tool call against `app` state. Returns the result as JSON
/// suitable for stuffing into a `tool_result` message back to the LLM.
/// All errors are surfaced as `{"error": "..."}` rather than `Result::Err`
/// so the LLM can read and recover instead of the worker aborting the
/// whole turn.
pub fn dispatch(app: &mut App, name: &str, args_json: &str) -> Value {
    let args: Value = serde_json::from_str(args_json).unwrap_or(Value::Null);
    match name {
        LIST_CATALOGS => list_catalogs(app),
        LIST_SCHEMAS => list_schemas(app, &args),
        LIST_TABLES => list_tables(app, &args),
        DESCRIBE_TABLE => describe_table(app, &args),
        READ_BUFFER => read_buffer(app, &args),
        WRITE_BUFFER => write_buffer(app, &args),
        _ => err(format!("unknown tool: {name}")),
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
            ]
        );
        // No duplicates.
        let mut sorted = names.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), names.len());
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
}
