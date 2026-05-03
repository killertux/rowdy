//! Project-root path safety for the chat agent's filesystem tools.
//!
//! All three fs read tools (`read_file`, `list_directory`, `grep_files`)
//! route every path through [`resolve`] before doing anything with it.
//! That gives one place to enforce both invariants the chat tools rely on:
//!
//! 1. **Jail**: the resolved path must be within the project root the App
//!    snapshotted at startup. We canonicalize first so `..`, symlinks, and
//!    `../../etc/passwd` style escapes get neutralised before the prefix
//!    check.
//! 2. **`.env` refusal**: any path component (basename or any directory in
//!    between) named `.env` or starting with `.env.` is refused. The
//!    refusal bubbles up as `Err(String)` so the tool dispatch surfaces it
//!    as `{"error": "..."}` to the LLM rather than panicking — that lets
//!    the model say "I can't read that" instead of stalling the turn.
//!
//! The error strings are deliberately LLM-readable: short, no jargon, no
//! absolute paths. The model uses them to decide whether to retry with a
//! different argument or hand the failure to the user.
//!
//! Why a separate module: this is the only place where path → bytes
//! decisions happen, so concentrating it makes the security review cheap.
//! The module has no dependencies on `App` — callers pass the root in.

use std::path::{Component, Path, PathBuf};

/// Resolve `input` against `root`, returning a canonical path that is
/// guaranteed to live inside `root` and not refer to any `.env` file.
///
/// `input` is treated as relative to `root` unless it's absolute. The
/// empty string resolves to `root` itself (used by `list_directory()`
/// when called with no arguments).
///
/// `must_exist` controls whether we report a missing file as an error.
/// `read_file` and `grep_files` set this true; `list_directory` also sets
/// it true (since canonicalization needs the directory to exist anyway,
/// and `read_dir` would fail on a missing path regardless).
pub fn resolve(root: &Path, input: &str, must_exist: bool) -> Result<PathBuf, String> {
    if has_env_component(Path::new(input)) {
        return Err(format!("refused: {input:?} is a .env file"));
    }

    let joined = if input.is_empty() {
        root.to_path_buf()
    } else {
        let p = Path::new(input);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            root.join(p)
        }
    };

    let canonical_root = root
        .canonicalize()
        .map_err(|e| format!("project root unavailable: {e}"))?;

    let canonical = match joined.canonicalize() {
        Ok(c) => c,
        Err(e) => {
            if must_exist {
                return Err(format!("path not found: {input:?} ({e})"));
            }
            // Best-effort canonicalisation: fall back to lexical
            // normalisation against the canonical root so the prefix
            // check still has something concrete to compare. This branch
            // is reachable for callers that want the path back even when
            // the file doesn't exist yet — none today, but the option
            // keeps `resolve` reusable.
            normalise(&canonical_root, &joined)
        }
    };

    if !canonical.starts_with(&canonical_root) {
        return Err(format!("refused: {input:?} escapes the project root"));
    }
    if has_env_component(&canonical) {
        return Err(format!("refused: {input:?} resolves to a .env file"));
    }

    Ok(canonical)
}

/// Display form of a path inside `root`: relative if it lives there,
/// absolute (lossy-string) otherwise. Used for log messages and the
/// permission prompt copy — never for security decisions.
pub fn display_relative(root: &Path, path: &Path) -> String {
    match (root.canonicalize(), path.canonicalize()) {
        (Ok(r), Ok(p)) => p
            .strip_prefix(&r)
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|_| p.to_string_lossy().to_string()),
        _ => path.to_string_lossy().to_string(),
    }
}

/// True if any component of `path` is `.env` or starts with `.env.`.
/// Checked on both the input and the canonicalised path so an attacker
/// can't smuggle one in via symlink or `..`.
fn has_env_component(path: &Path) -> bool {
    path.components().any(|c| match c {
        Component::Normal(seg) => is_env_name(&seg.to_string_lossy()),
        _ => false,
    })
}

fn is_env_name(name: &str) -> bool {
    name == ".env" || name.starts_with(".env.")
}

/// Lexical normalisation of `path` rooted at `root`. Resolves `..` /
/// `.` without touching the filesystem. Used as a fallback when
/// `canonicalize` fails because the target doesn't exist.
fn normalise(root: &Path, path: &Path) -> PathBuf {
    let mut out = PathBuf::from(root);
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(seg) => out.push(seg),
            Component::RootDir => out = PathBuf::from("/"),
            Component::Prefix(_) => out = PathBuf::from(c.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tempdir() -> PathBuf {
        let p = std::env::temp_dir().join(format!("rowdy-fs-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&p).unwrap();
        p.canonicalize().unwrap()
    }

    #[test]
    fn rejects_dotenv_at_root() {
        let root = tempdir();
        fs::write(root.join(".env"), "SECRET=1").unwrap();
        let err = resolve(&root, ".env", true).unwrap_err();
        assert!(err.contains(".env"), "got: {err}");
    }

    #[test]
    fn rejects_dotenv_variants() {
        let root = tempdir();
        fs::write(root.join(".env.local"), "X=1").unwrap();
        fs::write(root.join(".env.production"), "X=1").unwrap();
        assert!(resolve(&root, ".env.local", true).is_err());
        assert!(resolve(&root, ".env.production", true).is_err());
    }

    #[test]
    fn rejects_dotenv_inside_subdir() {
        let root = tempdir();
        let sub = root.join("config");
        fs::create_dir_all(&sub).unwrap();
        fs::write(sub.join(".env.test"), "X=1").unwrap();
        let err = resolve(&root, "config/.env.test", true).unwrap_err();
        assert!(err.contains(".env"), "got: {err}");
    }

    #[test]
    fn rejects_path_escaping_root() {
        let root = tempdir();
        let outside = root.parent().unwrap().join(format!(
            "rowdy-outside-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("secret.txt"), "x").unwrap();
        // Climb out via `..`.
        let err = resolve(
            &root,
            outside
                .strip_prefix(root.parent().unwrap())
                .unwrap()
                .join("secret.txt")
                .to_str()
                .map(|s| format!("../{s}"))
                .unwrap()
                .as_str(),
            true,
        )
        .unwrap_err();
        assert!(err.contains("escapes"), "got: {err}");
    }

    #[test]
    fn rejects_absolute_path_outside_root() {
        let root = tempdir();
        let err = resolve(&root, "/etc/hosts", true).unwrap_err();
        // Either "escapes" (most likely on macOS/Linux where /etc exists
        // and canonicalises) or "path not found" if it doesn't — both
        // are correct refusals.
        assert!(
            err.contains("escapes") || err.contains("not found"),
            "got: {err}"
        );
    }

    #[test]
    fn accepts_plain_relative_file() {
        let root = tempdir();
        fs::write(root.join("Cargo.toml"), "[package]").unwrap();
        let resolved = resolve(&root, "Cargo.toml", true).unwrap();
        assert!(resolved.ends_with("Cargo.toml"));
        assert!(resolved.starts_with(&root));
    }

    #[test]
    fn accepts_root_for_empty_input() {
        let root = tempdir();
        let resolved = resolve(&root, "", true).unwrap();
        assert_eq!(resolved, root);
    }

    #[test]
    fn missing_file_when_must_exist() {
        let root = tempdir();
        let err = resolve(&root, "no-such-file.txt", true).unwrap_err();
        assert!(err.contains("not found"), "got: {err}");
    }

    #[test]
    fn display_relative_strips_root_prefix() {
        let root = tempdir();
        let f = root.join("a/b.txt");
        fs::create_dir_all(f.parent().unwrap()).unwrap();
        fs::write(&f, "hi").unwrap();
        assert_eq!(display_relative(&root, &f), "a/b.txt");
    }
}
