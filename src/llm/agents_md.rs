//! Lazy AGENTS.md discovery and caching.
//!
//! `AGENTS.md` is the community standard for "drop a markdown file in
//! your repo to teach AI agents project-specific conventions" — table
//! naming rules, soft-delete columns, partition strategies, etc.
//!
//! Discovery is lazy and per-read, modelled on Claude Code:
//!
//! - Startup seeds the cache with the AGENTS.md sitting directly at
//!   `project_root` (the directory rowdy was launched from). Nothing
//!   above the cwd is walked.
//! - Whenever the chat agent calls a filesystem read tool
//!   (`read_file` / `list_directory` / `grep_files`), the touched
//!   directory's chain *up to project_root* is walked, and any
//!   AGENTS.md files in directories we haven't visited yet get loaded
//!   and added to the rendered prompt slice.
//! - A directory is scanned at most once per session. Files are read
//!   at most once. `:source` clears state and re-seeds from
//!   `project_root`.
//!
//! Output shape (each loaded file becomes a section in the system
//! prompt, with a path-tagged header so the LLM can ground its
//! references):
//!
//! ```text
//! # AGENTS.md (./AGENTS.md)
//! …repo-wide instructions…
//!
//! # AGENTS.md (src/llm/AGENTS.md)
//! …local module instructions…
//! ```
//!
//! Limits exist purely to keep the system prompt sane: 64 KB per file
//! and 128 KB combined. Per-file truncation produces a loud
//! `[truncated — N more bytes]` marker; the combined cap clips inside
//! the last rendered section so earlier sections always survive in
//! full. Non-UTF-8 files are skipped with a logger warn rather than
//! failing the whole load.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::log::Logger;

const FILENAME: &str = "AGENTS.md";

/// Per-file cap. Anything bigger gets truncated with a marker so the
/// model knows it's incomplete.
const MAX_PER_FILE_BYTES: usize = 64 * 1024;
/// Combined cap across all loaded sections. Enforced in `rendered()`
/// — the last rendered section is truncated to fit; earlier sections
/// always come through in full.
const MAX_TOTAL_BYTES: usize = 128 * 1024;

/// Lazy AGENTS.md collection. Populated by:
/// - [`AgentsMdCache::seed_root`] at app startup and after `:source`.
/// - [`AgentsMdCache::discover_for`] when the chat agent reads from a
///   subdirectory.
///
/// Rendered into a single string by [`AgentsMdCache::rendered`] each
/// time the system prompt is rebuilt — so newly discovered AGENTS.md
/// content shows up on the next chat turn automatically.
pub struct AgentsMdCache {
    /// Directories already scanned for an AGENTS.md (canonical, abs).
    visited: HashSet<PathBuf>,
    /// AGENTS.md files we've loaded, in load order. Iteration order
    /// of this Vec is the order sections appear in `rendered()`.
    loaded: Vec<LoadedAgentsMd>,
}

#[derive(Debug)]
struct LoadedAgentsMd {
    /// Header used when rendering: e.g. `./AGENTS.md` or
    /// `src/llm/AGENTS.md`. Always relative to `project_root` to keep
    /// absolute paths out of the prompt.
    rel_header: String,
    /// File body, already clipped to `MAX_PER_FILE_BYTES`.
    body: String,
    /// Bytes dropped by the per-file clip. >0 means render a marker.
    truncated_bytes: usize,
}

impl Default for AgentsMdCache {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentsMdCache {
    pub fn new() -> Self {
        Self {
            visited: HashSet::new(),
            loaded: Vec::new(),
        }
    }

    /// Scan `project_root` itself for an `AGENTS.md`. Idempotent —
    /// calling it twice with the same root is a no-op on the second
    /// call. Used at startup, and again by `:source` after
    /// [`clear`](Self::clear).
    ///
    /// Returns the relative-path headers (e.g. `./AGENTS.md`) of any
    /// AGENTS.md files freshly loaded by this call so the caller can
    /// surface a chat-history notice. Empty when nothing new was
    /// loaded.
    pub fn seed_root(&mut self, project_root: &Path, log: &Logger) -> Vec<String> {
        let Ok(canonical_root) = project_root.canonicalize() else {
            return Vec::new();
        };
        let mut loaded = Vec::new();
        if let Some(header) = self.scan_dir(&canonical_root, &canonical_root, log) {
            loaded.push(header);
        }
        loaded
    }

    /// Walk `target_dir` upward to `project_root` (inclusive on both
    /// ends), loading any AGENTS.md found in directories we haven't
    /// yet visited. Caller passes the *resolved* canonical directory
    /// the fs tool actually operated on; out-of-jail callers (target
    /// not inside `project_root`) are silently ignored.
    ///
    /// Returns the relative-path headers (e.g. `src/llm/AGENTS.md`)
    /// of any AGENTS.md files freshly loaded by this call, in load
    /// order (parent → child). Empty when the walk found nothing new.
    pub fn discover_for(
        &mut self,
        project_root: &Path,
        target_dir: &Path,
        log: &Logger,
    ) -> Vec<String> {
        let Ok(canonical_root) = project_root.canonicalize() else {
            return Vec::new();
        };
        let canonical_target = target_dir
            .canonicalize()
            .ok()
            .unwrap_or_else(|| target_dir.to_path_buf());
        if !canonical_target.starts_with(&canonical_root) {
            return Vec::new();
        }

        // Collect ancestors from the target up to (and including) the
        // project root. Walk shallowest-first when actually loading
        // so prompt order stays parent → child.
        let mut chain: Vec<PathBuf> = Vec::new();
        let mut cursor: PathBuf = canonical_target.clone();
        loop {
            chain.push(cursor.clone());
            if cursor == canonical_root {
                break;
            }
            match cursor.parent() {
                Some(parent) if parent.starts_with(&canonical_root) => {
                    cursor = parent.to_path_buf();
                }
                _ => break,
            }
        }

        let mut loaded = Vec::new();
        for dir in chain.into_iter().rev() {
            if let Some(header) = self.scan_dir(&dir, &canonical_root, log) {
                loaded.push(header);
            }
        }
        loaded
    }

    /// Render the loaded sections as a single string suitable for
    /// injection into the chat system prompt. Returns `None` when
    /// nothing has been loaded — the caller omits the AGENTS.md block
    /// entirely in that case.
    ///
    /// Combined cap (`MAX_TOTAL_BYTES`) is enforced here: sections are
    /// emitted in load order, and once we'd exceed the cap the last
    /// section is clipped with a `[truncated]` marker. Earlier
    /// sections always come through in full so a flood of small
    /// subdirectory files can't drown out the root file.
    pub fn rendered(&self) -> Option<String> {
        if self.loaded.is_empty() {
            return None;
        }
        let mut out = String::new();
        let mut bytes_remaining = MAX_TOTAL_BYTES;

        for entry in &self.loaded {
            // Don't break when the budget hits zero — keep emitting
            // section headers + a truncation marker for each dropped
            // file so the model knows there's more it can't see, and
            // which files were affected.
            let body_cap = entry.body.len().min(bytes_remaining);
            let (body, total_clip_marker) = clip(&entry.body, body_cap);

            if !out.is_empty() {
                out.push_str("\n\n");
            }
            out.push_str(&format!("# AGENTS.md ({})\n", entry.rel_header));
            out.push_str(body);
            // Per-file truncation marker (set when the file itself was
            // clipped at load time) wins over the combined-cap marker
            // when both apply, so the user sees the larger missing
            // count.
            let dropped = if total_clip_marker {
                entry.truncated_bytes + (entry.body.len() - body.len())
            } else {
                entry.truncated_bytes
            };
            if dropped > 0 {
                out.push_str(&format!("\n[AGENTS.md truncated — {dropped} more bytes]"));
            }
            bytes_remaining = bytes_remaining.saturating_sub(body.len());
        }

        Some(out)
    }

    /// Wipe state so `:source` can start over. The next call to
    /// `seed_root` re-loads `project_root`'s AGENTS.md if present.
    pub fn clear(&mut self) {
        self.visited.clear();
        self.loaded.clear();
    }

    /// Number of loaded sections. Used by `:source` to decide whether
    /// the success message should mention `AGENTS.md`, and by tests.
    pub fn loaded_count(&self) -> usize {
        self.loaded.len()
    }

    /// Scan `dir` for an AGENTS.md, marking it visited and loading
    /// the file when present. Returns the section header
    /// (`./AGENTS.md`-style relative path) on a fresh load so the
    /// caller can surface a chat notice; `None` when the directory
    /// was already visited or no AGENTS.md was found.
    fn scan_dir(&mut self, dir: &Path, project_root: &Path, log: &Logger) -> Option<String> {
        if !self.visited.insert(dir.to_path_buf()) {
            return None;
        }
        let path = dir.join(FILENAME);
        if !path.exists() {
            return None;
        }
        let loaded = load_one(&path, project_root, log)?;
        let header = loaded.rel_header.clone();
        self.loaded.push(loaded);
        Some(header)
    }
}

/// Read one AGENTS.md file, applying the UTF-8 + per-file size guards.
/// Returns `None` (with a `log.warn`) on IO error or non-UTF-8.
fn load_one(path: &Path, project_root: &Path, log: &Logger) -> Option<LoadedAgentsMd> {
    let raw = match fs::read(path) {
        Ok(b) => b,
        Err(err) => {
            log.warn(
                "agents_md",
                format!("read {} failed: {err}", path.display()),
            );
            return None;
        }
    };
    let text = match String::from_utf8(raw) {
        Ok(s) => s,
        Err(_) => {
            log.warn(
                "agents_md",
                format!("{} is not UTF-8 — skipped", path.display()),
            );
            return None;
        }
    };

    let (body_slice, truncated) = clip(&text, MAX_PER_FILE_BYTES);
    let body = body_slice.to_string();
    let truncated_bytes = if truncated {
        text.len() - body.len()
    } else {
        0
    };
    let rel_header = relative_header(project_root, path);

    Some(LoadedAgentsMd {
        rel_header,
        body,
        truncated_bytes,
    })
}

/// Display path for the section header. Anchored at `project_root` so
/// no absolute paths leak into the prompt. The root file gets a
/// canonical `./AGENTS.md` so all sections have a path-style header.
fn relative_header(project_root: &Path, full: &Path) -> String {
    match full.strip_prefix(project_root) {
        Ok(rel) => {
            let s = rel.to_string_lossy().to_string();
            if s.is_empty() || s == FILENAME {
                "./AGENTS.md".to_string()
            } else {
                s
            }
        }
        Err(_) => full.to_string_lossy().to_string(),
    }
}

/// Truncate `text` to at most `cap` bytes on a UTF-8 char boundary.
/// Returns `(slice, truncated_flag)`.
fn clip(text: &str, cap: usize) -> (&str, bool) {
    if text.len() <= cap {
        return (text, false);
    }
    let mut end = cap;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    (&text[..end], true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tempdir() -> PathBuf {
        let p = std::env::temp_dir().join(format!("rowdy-agents-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&p).unwrap();
        p.canonicalize().unwrap()
    }

    fn write(path: &Path, body: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, body).unwrap();
    }

    #[test]
    fn seed_root_loads_root_file_only() {
        let root = tempdir();
        write(&root.join("AGENTS.md"), "root content");
        write(&root.join("sub").join("AGENTS.md"), "sub content");

        let mut cache = AgentsMdCache::new();
        let loaded = cache.seed_root(&root, &Logger::discard());

        let rendered = cache.rendered().expect("loaded");
        assert!(rendered.contains("root content"));
        assert!(!rendered.contains("sub content"));
        assert_eq!(cache.loaded_count(), 1);
        assert_eq!(loaded, vec!["./AGENTS.md".to_string()]);
    }

    #[test]
    fn seed_root_returns_none_when_root_has_no_file() {
        let root = tempdir();
        let mut cache = AgentsMdCache::new();
        let loaded = cache.seed_root(&root, &Logger::discard());
        assert!(cache.rendered().is_none());
        assert_eq!(cache.loaded_count(), 0);
        assert!(loaded.is_empty());
    }

    #[test]
    fn seed_root_second_call_returns_no_new_paths() {
        let root = tempdir();
        write(&root.join("AGENTS.md"), "root content");

        let mut cache = AgentsMdCache::new();
        let first = cache.seed_root(&root, &Logger::discard());
        let second = cache.seed_root(&root, &Logger::discard());
        assert_eq!(first, vec!["./AGENTS.md".to_string()]);
        assert!(second.is_empty(), "double seed must not re-load");
    }

    #[test]
    fn discover_for_loads_subdir_agents_md() {
        let root = tempdir();
        write(&root.join("AGENTS.md"), "root content");
        let api = root.join("api");
        write(&api.join("AGENTS.md"), "api content");

        let mut cache = AgentsMdCache::new();
        cache.seed_root(&root, &Logger::discard());
        let loaded = cache.discover_for(&root, &api, &Logger::discard());

        let rendered = cache.rendered().expect("loaded");
        let root_pos = rendered.find("root content").expect("root present");
        let api_pos = rendered.find("api content").expect("api present");
        assert!(root_pos < api_pos, "root section must come before sub");
        assert_eq!(cache.loaded_count(), 2);
        assert_eq!(loaded, vec!["api/AGENTS.md".to_string()]);
    }

    #[test]
    fn discover_for_is_idempotent() {
        let root = tempdir();
        write(&root.join("AGENTS.md"), "root");
        let api = root.join("api");
        write(&api.join("AGENTS.md"), "api");

        let mut cache = AgentsMdCache::new();
        cache.seed_root(&root, &Logger::discard());
        let first = cache.discover_for(&root, &api, &Logger::discard());
        let second = cache.discover_for(&root, &api, &Logger::discard());

        assert_eq!(cache.loaded_count(), 2, "double discover must not re-load");
        assert_eq!(first, vec!["api/AGENTS.md".to_string()]);
        assert!(second.is_empty(), "second walk must report no new paths");
    }

    #[test]
    fn discover_for_walks_up_to_project_root() {
        let root = tempdir();
        write(&root.join("AGENTS.md"), "root");
        let a = root.join("a");
        let b = a.join("b");
        let c = b.join("c");
        fs::create_dir_all(&c).unwrap();
        write(&a.join("AGENTS.md"), "a-content");
        write(&b.join("AGENTS.md"), "b-content");
        // No AGENTS.md at c — but c must still be on the visited list.

        let mut cache = AgentsMdCache::new();
        cache.seed_root(&root, &Logger::discard());
        cache.discover_for(&root, &c, &Logger::discard());

        let rendered = cache.rendered().expect("loaded");
        assert!(rendered.contains("root"));
        assert!(rendered.contains("a-content"));
        assert!(rendered.contains("b-content"));
        // Order is parent → child.
        let root_pos = rendered.find("root").unwrap();
        let a_pos = rendered.find("a-content").unwrap();
        let b_pos = rendered.find("b-content").unwrap();
        assert!(root_pos < a_pos);
        assert!(a_pos < b_pos);
    }

    #[test]
    fn discover_for_skips_paths_outside_root() {
        let root = tempdir();
        write(&root.join("AGENTS.md"), "root");
        let outside = tempdir();
        write(&outside.join("AGENTS.md"), "outside-secret");

        let mut cache = AgentsMdCache::new();
        cache.seed_root(&root, &Logger::discard());
        cache.discover_for(&root, &outside, &Logger::discard());

        let rendered = cache.rendered().expect("loaded");
        assert!(!rendered.contains("outside-secret"));
        assert_eq!(cache.loaded_count(), 1);
    }

    #[test]
    fn discover_for_skips_already_visited_dir() {
        let root = tempdir();
        write(&root.join("AGENTS.md"), "root");
        let api = root.join("api");
        let sub = api.join("sub");
        fs::create_dir_all(&sub).unwrap();
        write(&api.join("AGENTS.md"), "api");
        write(&sub.join("AGENTS.md"), "sub");

        let mut cache = AgentsMdCache::new();
        cache.seed_root(&root, &Logger::discard());
        cache.discover_for(&root, &api, &Logger::discard());
        // Now cache has: root, api visited and loaded. sub still unseen.
        assert_eq!(cache.loaded_count(), 2);
        // Discovering into sub must scan only sub — api was visited
        // already and must not be rescanned (which would no-op anyway,
        // but we want to confirm the walk doesn't re-add).
        cache.discover_for(&root, &sub, &Logger::discard());
        assert_eq!(cache.loaded_count(), 3);
    }

    #[test]
    fn clear_resets_cache() {
        let root = tempdir();
        write(&root.join("AGENTS.md"), "root");
        let mut cache = AgentsMdCache::new();
        cache.seed_root(&root, &Logger::discard());
        assert_eq!(cache.loaded_count(), 1);

        cache.clear();
        assert!(cache.rendered().is_none());
        assert_eq!(cache.loaded_count(), 0);

        // Re-seed picks the file up again.
        cache.seed_root(&root, &Logger::discard());
        assert_eq!(cache.loaded_count(), 1);
    }

    #[test]
    fn non_utf8_file_skipped_with_warn() {
        let root = tempdir();
        write(&root.join("AGENTS.md"), "valid");
        let sub = root.join("sub");
        fs::create_dir_all(&sub).unwrap();
        // 0xff is invalid in any UTF-8 sequence.
        fs::write(sub.join("AGENTS.md"), [0xff, 0xfe, 0xfd]).unwrap();

        let mut cache = AgentsMdCache::new();
        cache.seed_root(&root, &Logger::discard());
        cache.discover_for(&root, &sub, &Logger::discard());

        let rendered = cache.rendered().expect("root still loads");
        assert!(rendered.contains("valid"));
        assert!(!rendered.contains('\u{FFFD}'));
        // Only the root file made it in.
        assert_eq!(cache.loaded_count(), 1);
    }

    #[test]
    fn oversized_single_file_truncated_with_marker() {
        let root = tempdir();
        let huge = "a".repeat(MAX_PER_FILE_BYTES + 1024);
        write(&root.join("AGENTS.md"), &huge);

        let mut cache = AgentsMdCache::new();
        cache.seed_root(&root, &Logger::discard());

        let rendered = cache.rendered().expect("loaded");
        assert!(rendered.contains("[AGENTS.md truncated"));
    }

    #[test]
    fn combined_cap_truncates_in_render() {
        // Three files all at MAX_PER_FILE_BYTES — combined would be
        // 192 KB, exceeding the 128 KB combined cap. The last
        // section should be clipped with a marker; earlier sections
        // intact.
        let root = tempdir();
        write(&root.join("AGENTS.md"), &"r".repeat(MAX_PER_FILE_BYTES));
        let a = root.join("a");
        let b = a.join("b");
        fs::create_dir_all(&b).unwrap();
        write(&a.join("AGENTS.md"), &"a".repeat(MAX_PER_FILE_BYTES));
        write(&b.join("AGENTS.md"), &"b".repeat(MAX_PER_FILE_BYTES));

        let mut cache = AgentsMdCache::new();
        cache.seed_root(&root, &Logger::discard());
        cache.discover_for(&root, &b, &Logger::discard());

        let rendered = cache.rendered().expect("loaded");
        assert!(rendered.contains("[AGENTS.md truncated"));
        // The first two sections (r and a) are full-size — we should
        // still see all of their content.
        let r_count = rendered.matches('r').count();
        let a_count = rendered.matches('a').count();
        // `a` appears in the literal string "AGENTS.md", in
        // "truncated", in path headers, etc. — guard with > rather
        // than == to keep the assertion stable across header tweaks.
        assert!(r_count >= MAX_PER_FILE_BYTES);
        assert!(a_count >= MAX_PER_FILE_BYTES);
    }

    #[test]
    fn header_paths_are_relative_to_project_root() {
        let root = tempdir();
        write(&root.join("AGENTS.md"), "x");
        let api = root.join("services").join("api");
        fs::create_dir_all(&api).unwrap();
        write(&api.join("AGENTS.md"), "y");

        let mut cache = AgentsMdCache::new();
        cache.seed_root(&root, &Logger::discard());
        cache.discover_for(&root, &api, &Logger::discard());

        let rendered = cache.rendered().expect("loaded");
        let abs = root.to_string_lossy().to_string();
        assert!(
            !rendered.contains(&abs),
            "absolute root path leaked into prompt: {abs}"
        );
        assert!(rendered.contains("./AGENTS.md"));
        assert!(rendered.contains("services/api/AGENTS.md"));
    }

    #[test]
    fn clip_respects_char_boundaries() {
        let s = "aaaa\u{1F600}bbbb"; // emoji is 4 bytes
        let (slice, truncated) = clip(s, 6);
        assert!(truncated);
        assert!(slice.starts_with("aaaa"));
        let (slice, truncated) = clip(s, 100);
        assert!(!truncated);
        assert_eq!(slice, s);
    }
}
