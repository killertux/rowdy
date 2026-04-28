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
//! 2. **Editor buffer** — `read_buffer` and `replace_buffer`. The LLM
//!    drafts SQL into the user's editor; the user reviews and runs it
//!    themselves. There is no `execute_query` tool by design — the
//!    user retains the run/cancel decision.
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
pub const REPLACE_BUFFER: &str = "replace_buffer";

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
            "Read the user's current SQL editor buffer. No arguments. \
             Returns { text: string }.",
            &[],
            &[],
        ),
        function_tool(
            REPLACE_BUFFER,
            "Overwrite the user's SQL editor buffer with new text. \
             The user will review and run it themselves — you do NOT \
             execute SQL. Use this when they ask you to draft or rewrite \
             a query. Returns { ok: true }.",
            &[("text", "string", "Full SQL text to put in the buffer.")],
            &["text"],
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
        READ_BUFFER => read_buffer(app),
        REPLACE_BUFFER => replace_buffer(app, &args),
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

fn read_buffer(app: &App) -> Value {
    serde_json::json!({ "text": app.editor.text() })
}

fn replace_buffer(app: &mut App, args: &Value) -> Value {
    let Some(text) = args.get("text").and_then(|v| v.as_str()) else {
        return err("missing required `text` argument");
    };
    app.editor.replace_text(text);
    app.editor_dirty = true;
    serde_json::json!({ "ok": true })
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
                REPLACE_BUFFER,
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
