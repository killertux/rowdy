//! Per-connection editor session persistence.
//!
//! Each connection gets its own `session_0.sql` file under
//! `<data_dir>/sessions/<sanitized_name>/`. The editor's buffer is flushed
//! to that file ~800ms after the user stops typing, and reloaded when the
//! same connection is opened again. A future iteration may grow this into
//! multiple `session_<n>.sql` files; the suffix in the filename reserves
//! that shape.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const SESSIONS_DIR: &str = "sessions";
const FILE_NAME: &str = "session_0.sql";

/// Path to the session file for the given connection. Creates no directories.
pub fn path_for(data_dir: &Path, connection_name: &str) -> PathBuf {
    data_dir
        .join(SESSIONS_DIR)
        .join(sanitize(connection_name))
        .join(FILE_NAME)
}

/// Reads the session file. Returns an empty string when the file doesn't
/// exist yet — the next save will create it.
pub fn load(path: &Path) -> io::Result<String> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(String::new()),
        Err(err) => Err(err),
    }
}

/// Writes `contents` to the session file, creating the parent directories
/// on first save.
pub fn save(path: &Path, contents: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, contents)
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
        // Slashes are stripped first, so these never reach the dot check.
        assert_eq!(sanitize("../etc"), ".._etc");
    }

    #[test]
    fn path_for_includes_sessions_segment() {
        let p = path_for(Path::new("/tmp/.rowdy"), "my db");
        assert_eq!(p, Path::new("/tmp/.rowdy/sessions/my_db/session_0.sql"));
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
        let p = dir.join("sessions").join("c").join("session_0.sql");
        save(&p, "SELECT 1;\n").unwrap();
        assert_eq!(load(&p).unwrap(), "SELECT 1;\n");
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
