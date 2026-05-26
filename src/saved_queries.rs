//! Per-connection named query storage.
//!
//! Each saved query is one `.sql` file at
//! `<data_dir>/queries/<sanitized_conn>/<sanitized_name>.sql`. The layout
//! mirrors `src/session.rs` so the file tree stays predictable and a
//! human can grep / edit queries externally if they want.

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const QUERIES_DIR: &str = "queries";

/// Directory holding one connection's saved queries. Caller is
/// responsible for creating it before writing.
pub fn dir_for(data_dir: &Path, connection_name: &str) -> PathBuf {
    data_dir.join(QUERIES_DIR).join(sanitize(connection_name))
}

/// Path to a single saved query file. Creates no directories.
pub fn path_for(data_dir: &Path, connection_name: &str, name: &str) -> PathBuf {
    dir_for(data_dir, connection_name).join(format!("{}.sql", sanitize(name)))
}

/// Validate a user-supplied query name before persistence. Catches the
/// obviously-bad shapes (empty, `..`, path separators) so the on-disk
/// sanitizer never has to silently collapse them into something that
/// could collide with an unrelated entry.
pub fn validate_name(name: &str) -> Result<(), String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("query name is empty".to_string());
    }
    if trimmed.contains('/') || trimmed.contains('\\') {
        return Err("query name may not contain path separators".to_string());
    }
    if trimmed == "." || trimmed == ".." {
        return Err("query name may not be '.' or '..'".to_string());
    }
    Ok(())
}

/// Write `sql` to the file for `(connection, name)`, creating parents.
pub fn save(data_dir: &Path, connection_name: &str, name: &str, sql: &str) -> io::Result<()> {
    let path = path_for(data_dir, connection_name, name);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, sql)
}

/// Read the saved query body. `NotFound` propagates so callers can
/// surface a clear "no saved query named X" message.
pub fn load(data_dir: &Path, connection_name: &str, name: &str) -> io::Result<String> {
    let path = path_for(data_dir, connection_name, name);
    fs::read_to_string(path)
}

/// True iff the file exists on disk.
pub fn exists(data_dir: &Path, connection_name: &str, name: &str) -> bool {
    path_for(data_dir, connection_name, name).is_file()
}

/// Alphabetically sorted list of saved query names for `connection`.
/// Missing directory → empty vec (a fresh connection just has nothing
/// saved yet).
pub fn list(data_dir: &Path, connection_name: &str) -> io::Result<Vec<String>> {
    let dir = dir_for(data_dir, connection_name);
    let entries = match fs::read_dir(&dir) {
        Ok(it) => it,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err),
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter_map(|e| name_from_filename(&e.file_name()))
        .collect();
    names.sort_unstable();
    names.dedup();
    Ok(names)
}

/// Best-effort delete — idempotent on a missing file. Currently
/// unused; kept for future `:delete-saved` symmetry with `session::delete`.
#[allow(dead_code)]
pub fn delete(data_dir: &Path, connection_name: &str, name: &str) -> io::Result<()> {
    let path = path_for(data_dir, connection_name, name);
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

fn name_from_filename(name: &OsStr) -> Option<String> {
    let s = name.to_str()?;
    let stem = s.strip_suffix(".sql")?;
    if stem.is_empty() {
        return None;
    }
    Some(stem.to_string())
}

/// Same rule as `session::sanitize` — keep alnum / `_-.`, replace the
/// rest with `_`, prefix `_` if the result would be `.` / `..` / empty.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir() -> PathBuf {
        let base = std::env::temp_dir().join(format!(
            "rowdy-saved-queries-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&base).unwrap();
        base
    }

    #[test]
    fn round_trip_save_load() {
        let dir = tempdir();
        save(&dir, "conn", "daily", "SELECT 1;\n").unwrap();
        assert_eq!(load(&dir, "conn", "daily").unwrap(), "SELECT 1;\n");
    }

    #[test]
    fn exists_reports_disk_state() {
        let dir = tempdir();
        assert!(!exists(&dir, "conn", "x"));
        save(&dir, "conn", "x", "SELECT 1;").unwrap();
        assert!(exists(&dir, "conn", "x"));
    }

    #[test]
    fn list_returns_sorted_names() {
        let dir = tempdir();
        save(&dir, "c", "beta", "1").unwrap();
        save(&dir, "c", "alpha", "1").unwrap();
        save(&dir, "c", "gamma", "1").unwrap();
        // Stray non-sql file is ignored.
        let stray = dir_for(&dir, "c").join("notes.txt");
        fs::write(&stray, "hi").unwrap();
        assert_eq!(list(&dir, "c").unwrap(), vec!["alpha", "beta", "gamma"]);
    }

    #[test]
    fn list_returns_empty_when_dir_missing() {
        let dir = tempdir();
        assert!(list(&dir, "fresh").unwrap().is_empty());
    }

    #[test]
    fn per_connection_isolation() {
        let dir = tempdir();
        save(&dir, "prod", "same", "SELECT 1;").unwrap();
        save(&dir, "stage", "same", "SELECT 2;").unwrap();
        assert_eq!(load(&dir, "prod", "same").unwrap(), "SELECT 1;");
        assert_eq!(load(&dir, "stage", "same").unwrap(), "SELECT 2;");
    }

    #[test]
    fn delete_is_idempotent() {
        let dir = tempdir();
        delete(&dir, "c", "missing").unwrap();
        save(&dir, "c", "x", "1").unwrap();
        delete(&dir, "c", "x").unwrap();
        delete(&dir, "c", "x").unwrap();
        assert!(!exists(&dir, "c", "x"));
    }

    #[test]
    fn sanitize_replaces_unsafe_chars() {
        // Names with spaces / slashes are caught up-front by validate_name,
        // but the on-disk sanitizer must still produce a safe path so any
        // bypass (e.g. connection names supplied externally) can't escape.
        assert_eq!(sanitize("a b/c"), "a_b_c");
        assert_eq!(sanitize(".."), "_..");
        assert_eq!(sanitize(""), "_");
    }

    #[test]
    fn validate_name_rejects_bad_shapes() {
        assert!(validate_name("").is_err());
        assert!(validate_name("   ").is_err());
        assert!(validate_name("a/b").is_err());
        assert!(validate_name("a\\b").is_err());
        assert!(validate_name(".").is_err());
        assert!(validate_name("..").is_err());
        assert!(validate_name("ok-name_1.2").is_ok());
    }
}
