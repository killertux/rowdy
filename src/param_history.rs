//! Per-connection persistence of "last values supplied for each
//! parameterised query". Mirrors the layout of `session.rs`:
//! `<data_dir>/params/<sanitized-connection>.json`.
//!
//! Storage is a flat list keyed by the normalised statement text
//! (whitespace collapsed, trimmed). LRU-capped so the file stays
//! small on connections where users hack on many one-off queries.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::app::App;
use crate::datasource::sql::placeholders::ParamKey;

const PARAMS_DIR: &str = "params";
const MAX_ENTRIES: usize = 200;

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ParamHistory {
    /// Most-recently-used entries last. `record` pushes the touched
    /// entry to the tail; eviction pops from the head.
    pub entries: Vec<Entry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    /// Whitespace-normalised statement; the lookup key.
    pub key: String,
    /// Original statement verbatim — kept for debugging the params
    /// file by hand. Not consulted on lookup.
    pub sql: String,
    /// Map from `ParamKey::label()` (`"$1"`, `":name"`) to the last
    /// value the user typed. Labels (not raw enum variants) keep the
    /// JSON readable.
    pub values: HashMap<String, String>,
}

fn path_for(data_dir: &Path, connection_name: &str) -> PathBuf {
    data_dir
        .join(PARAMS_DIR)
        .join(format!("{}.json", sanitize(connection_name)))
}

/// Same rules as `session::sanitize` (kept local to avoid cross-module
/// coupling). Different connection names that collapse to the same
/// sanitised form share a file — acceptable for v1.
fn sanitize(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' || ch == '.' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() || out == "." || out == ".." {
        out.insert(0, '_');
    }
    out
}

/// Collapse runs of whitespace, trim ends. Identical statements with
/// trivial reformatting still hit the same history entry — important
/// because users press Enter on a re-formatted version and expect
/// their last values back.
fn normalize(sql: &str) -> String {
    let mut out = String::with_capacity(sql.len());
    let mut prev_ws = true; // suppresses leading whitespace
    for ch in sql.chars() {
        if ch.is_whitespace() {
            if !prev_ws {
                out.push(' ');
                prev_ws = true;
            }
        } else {
            out.push(ch);
            prev_ws = false;
        }
    }
    if out.ends_with(' ') {
        out.pop();
    }
    out
}

fn load(path: &Path) -> ParamHistory {
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return ParamHistory::default(),
        Err(_) => return ParamHistory::default(),
    };
    serde_json::from_slice(&bytes).unwrap_or_default()
}

fn save(path: &Path, history: &ParamHistory) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec_pretty(history)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    fs::write(path, bytes)
}

/// Load (lazily, cached on `App`) and return the values for the most
/// recent execution of `sql` on `connection`. Returns `None` if the
/// connection has no history or `sql` isn't in it.
pub fn lookup(
    app: &mut App,
    connection: &str,
    sql: &str,
) -> Option<HashMap<String, String>> {
    let key = normalize(sql);
    let history = ensure_loaded(app, connection);
    history
        .entries
        .iter()
        .rev()
        .find(|e| e.key == key)
        .map(|e| e.values.clone())
}

/// Record `values` as the last execution for `sql` on `connection`.
/// Touches the in-memory cache and writes through to disk. Best-effort
/// — write failures are logged but don't surface to the user (the
/// query still ran).
pub fn record(
    app: &mut App,
    connection: &str,
    sql: &str,
    values: &[(ParamKey, String)],
) {
    let key = normalize(sql);
    let path = path_for(&app.data_dir, connection);
    let history = ensure_loaded(app, connection);

    history.entries.retain(|e| e.key != key);
    let mut map = HashMap::with_capacity(values.len());
    for (k, v) in values {
        map.insert(k.label(), v.clone());
    }
    history.entries.push(Entry {
        key,
        sql: sql.to_string(),
        values: map,
    });
    while history.entries.len() > MAX_ENTRIES {
        history.entries.remove(0);
    }

    // Snapshot to drop the &mut borrow on app before logging.
    let snapshot = ParamHistory {
        entries: history.entries.clone(),
    };
    if let Err(err) = save(&path, &snapshot) {
        app.log
            .warn("params", format!("param history save failed: {err}"));
    }
}

fn ensure_loaded<'a>(app: &'a mut App, connection: &str) -> &'a mut ParamHistory {
    if !app.param_history.contains_key(connection) {
        let path = path_for(&app.data_dir, connection);
        app.param_history.insert(connection.to_string(), load(&path));
    }
    app.param_history.get_mut(connection).expect("just inserted")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_collapses_whitespace() {
        assert_eq!(normalize("  SELECT\n  1 \t , 2 "), "SELECT 1 , 2");
    }

    #[test]
    fn path_for_uses_sanitized_name() {
        let p = path_for(Path::new("/tmp/.rowdy"), "my db");
        assert_eq!(p, Path::new("/tmp/.rowdy/params/my_db.json"));
    }

    #[test]
    fn save_and_load_round_trips() {
        let dir = std::env::temp_dir().join(format!(
            "rowdy-params-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        let path = path_for(&dir, "c");
        let mut h = ParamHistory::default();
        let mut vals = HashMap::new();
        vals.insert("$1".into(), "42".into());
        h.entries.push(Entry {
            key: "select $1".into(),
            sql: "SELECT $1".into(),
            values: vals,
        });
        save(&path, &h).unwrap();
        let loaded = load(&path);
        assert_eq!(loaded.entries.len(), 1);
        assert_eq!(loaded.entries[0].values.get("$1"), Some(&"42".to_string()));
        let _ = fs::remove_dir_all(&dir);
    }
}
