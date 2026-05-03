//! Per-connection editor session persistence.
//!
//! Each connection gets one or more `session_<N>.sql` files under
//! `<data_dir>/sessions/<sanitized_name>/`. The editor's buffer is
//! flushed to the **active** session ~800ms after the user stops
//! typing, and reloaded when the same connection is opened again.
//!
//! Multiple sessions per connection let the user keep, say, a
//! long-running migration draft in `session_0.sql` separate from
//! ad-hoc queries in `session_1.sql`. Indices may have holes after
//! deletion (`0` and `2` is fine without a `1`); the active set
//! is computed by scanning the directory at connect-time, not by
//! maintaining a counter.

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const SESSIONS_DIR: &str = "sessions";

/// Path to the session file for a given connection + index.
/// Creates no directories.
pub fn path_for(data_dir: &Path, connection_name: &str, index: usize) -> PathBuf {
    data_dir
        .join(SESSIONS_DIR)
        .join(sanitize(connection_name))
        .join(format!("session_{index}.sql"))
}

/// Reads the session file. Returns an empty string when the file
/// doesn't exist yet — the next save will create it.
pub fn load(path: &Path) -> io::Result<String> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(String::new()),
        Err(err) => Err(err),
    }
}

/// Writes `contents` to the session file, creating the parent
/// directories on first save.
pub fn save(path: &Path, contents: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, contents)
}

/// Sorted list of session indices that exist on disk for
/// `connection_name`. Returns `[0]` when the directory is missing or
/// has no `session_<N>.sql` files — every connection always has
/// session 0 from the user's perspective even before its file
/// exists. Unparseable filenames are skipped silently.
pub fn list_indices(data_dir: &Path, connection_name: &str) -> Vec<usize> {
    let dir = data_dir.join(SESSIONS_DIR).join(sanitize(connection_name));
    let mut indices: Vec<usize> = match fs::read_dir(&dir) {
        Ok(entries) => entries
            .flatten()
            .filter_map(|e| index_from_filename(&e.file_name()))
            .collect(),
        Err(_) => Vec::new(),
    };
    if indices.is_empty() {
        indices.push(0);
    }
    indices.sort_unstable();
    indices.dedup();
    indices
}

/// Lowest non-negative integer not present in `existing`. Used by
/// `:session new` to pick a fresh index without colliding with a
/// hole in the sequence (`[0, 2]` → `1`).
pub fn next_free_index(existing: &[usize]) -> usize {
    let mut sorted: Vec<usize> = existing.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    for (i, n) in sorted.iter().enumerate() {
        if *n != i {
            return i;
        }
    }
    sorted.len()
}

/// Best-effort delete. Returns `Ok` when the file is gone after the
/// call (including the not-yet-saved case where it never existed).
pub fn delete(data_dir: &Path, connection_name: &str, index: usize) -> io::Result<()> {
    let path = path_for(data_dir, connection_name, index);
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

/// Replace anything outside `[A-Za-z0-9_.-]` with `_` so the result is safe
/// to drop into a path. Different names with the same sanitized form share
/// the same session file — acceptable for v1. Names that would collapse to
/// `.` or `..` (which would resolve to other directories) get an underscore
/// prefix.
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

/// Parse the index from a `session_<N>.sql` filename. Anything else
/// (missing prefix, wrong extension, non-numeric N) returns `None`
/// so the directory walk can skip stray files cleanly.
fn index_from_filename(name: &OsStr) -> Option<usize> {
    let s = name.to_str()?;
    let stem = s.strip_suffix(".sql")?;
    let n_str = stem.strip_prefix("session_")?;
    n_str.parse::<usize>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_keeps_safe_characters() {
        assert_eq!(sanitize("local-dev_v2.0"), "local-dev_v2.0");
    }

    #[test]
    fn sanitize_replaces_path_unsafe_characters() {
        assert_eq!(sanitize("foo bar/baz"), "foo_bar_baz");
        assert_eq!(sanitize("a:b@c"), "a_b_c");
    }

    #[test]
    fn sanitize_neutralises_dot_components() {
        assert_eq!(sanitize(""), "_");
        assert_eq!(sanitize("."), "_.");
        assert_eq!(sanitize(".."), "_..");
        assert_eq!(sanitize("../etc"), ".._etc");
    }

    #[test]
    fn path_for_includes_indexed_filename() {
        let p = path_for(Path::new("/tmp/.rowdy"), "my db", 0);
        assert_eq!(p, Path::new("/tmp/.rowdy/sessions/my_db/session_0.sql"));
        let p = path_for(Path::new("/tmp/.rowdy"), "my db", 7);
        assert_eq!(p, Path::new("/tmp/.rowdy/sessions/my_db/session_7.sql"));
    }

    #[test]
    fn load_returns_empty_when_missing() {
        let dir = tempdir();
        let p = dir.join("missing.sql");
        assert_eq!(load(&p).unwrap(), "");
    }

    #[test]
    fn save_creates_parent_dirs_then_load_round_trips() {
        let dir = tempdir();
        let p = path_for(&dir, "c", 0);
        save(&p, "SELECT 1;\n").unwrap();
        assert_eq!(load(&p).unwrap(), "SELECT 1;\n");
    }

    #[test]
    fn list_indices_returns_zero_when_dir_missing() {
        let dir = tempdir();
        assert_eq!(list_indices(&dir, "fresh"), vec![0]);
    }

    #[test]
    fn list_indices_sorts_existing_files_and_skips_strays() {
        let dir = tempdir();
        save(&path_for(&dir, "c", 2), "x").unwrap();
        save(&path_for(&dir, "c", 0), "y").unwrap();
        // Stray non-session file in the same directory — must be ignored.
        let stray = dir.join(SESSIONS_DIR).join("c").join("notes.txt");
        save(&stray, "hi").unwrap();
        // Stray with the right prefix but garbage index — also ignored.
        let stray2 = dir.join(SESSIONS_DIR).join("c").join("session_oops.sql");
        save(&stray2, "hi").unwrap();
        assert_eq!(list_indices(&dir, "c"), vec![0, 2]);
    }

    #[test]
    fn next_free_index_picks_lowest_gap() {
        assert_eq!(next_free_index(&[]), 0);
        assert_eq!(next_free_index(&[0]), 1);
        assert_eq!(next_free_index(&[0, 1, 2]), 3);
        assert_eq!(next_free_index(&[0, 2]), 1);
        assert_eq!(next_free_index(&[1, 2]), 0);
        assert_eq!(next_free_index(&[0, 0, 1]), 2); // duplicates de-dup
    }

    #[test]
    fn delete_is_idempotent_on_missing_file() {
        let dir = tempdir();
        // First call: nothing there → still Ok.
        delete(&dir, "c", 5).unwrap();
        // Second call after creating + deleting once.
        save(&path_for(&dir, "c", 5), "x").unwrap();
        delete(&dir, "c", 5).unwrap();
        delete(&dir, "c", 5).unwrap();
    }

    fn tempdir() -> PathBuf {
        let base = std::env::temp_dir().join(format!(
            "rowdy-session-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&base).unwrap();
        base
    }
}
