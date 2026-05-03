//! Auto-discovery of `AGENTS.md` project instructions.
//!
//! `AGENTS.md` is the community standard for "drop a markdown file in
//! your repo to teach AI agents project-specific conventions" — table
//! naming rules, soft-delete columns, partition strategies, etc. This
//! module walks the project tree at startup (and on `:source`),
//! collects every `AGENTS.md` from the discovered anchor down to the
//! current working directory, concatenates them, and hands the result
//! back as a single string for [`crate::llm::prompt::build_system_prompt`]
//! to inject into the chat system prompt.
//!
//! Anchor discovery: walk parent directories from `project_root` until
//! we find a `.git` entry (file or directory — the file form supports
//! git worktrees and submodules). If none is found before we hit the
//! filesystem root or `$HOME`, fall back to `project_root` alone. The
//! anchor is the *top* of the chain so a repo-wide `AGENTS.md` at the
//! repo root layers below a subproject-specific one.
//!
//! Output shape (each file is wrapped with a path-tagged header so the
//! LLM can ground its references):
//!
//! ```text
//! # AGENTS.md (./AGENTS.md)
//! …repo-wide instructions…
//!
//! # AGENTS.md (api/AGENTS.md)
//! …api-subproject instructions…
//! ```
//!
//! Limits exist purely to keep the system prompt sane: 64 KB per file
//! and 128 KB combined. Truncation is loud (a `[truncated — N more
//! bytes]` marker), never silent file drops. Non-UTF-8 files are
//! skipped with a logger warn rather than failing the whole load.

use std::fs;
use std::path::{Path, PathBuf};

use crate::log::Logger;

const FILENAME: &str = "AGENTS.md";

/// Per-file cap. Anything bigger gets truncated with a marker so the
/// model knows it's incomplete.
const MAX_PER_FILE_BYTES: usize = 64 * 1024;
/// Combined cap across the whole chain. Last file in the chain gets
/// truncated to fit; earlier files are always loaded in full.
const MAX_TOTAL_BYTES: usize = 128 * 1024;

/// Load and concatenate the AGENTS.md chain from `project_root`'s
/// project anchor down to `project_root` itself. Returns `None` when
/// no AGENTS.md is found anywhere in the chain.
///
/// Failures on individual files (non-UTF-8, IO error) are logged via
/// `log` and the file is skipped — other files in the chain still
/// load. A single bad file shouldn't lose the user's other AGENTS.md
/// content.
pub fn load(project_root: &Path, log: &Logger) -> Option<String> {
    let canonical_root = project_root.canonicalize().ok()?;
    let anchor = find_anchor(&canonical_root);
    let chain = chain_from(&anchor, &canonical_root);

    let mut out = String::new();
    let mut bytes_remaining = MAX_TOTAL_BYTES;

    for dir in chain {
        let path = dir.join(FILENAME);
        if !path.exists() {
            continue;
        }
        let raw = match fs::read(&path) {
            Ok(b) => b,
            Err(err) => {
                log.warn(
                    "agents_md",
                    format!("read {} failed: {err}", path.display()),
                );
                continue;
            }
        };
        let text = match String::from_utf8(raw) {
            Ok(s) => s,
            Err(_) => {
                log.warn(
                    "agents_md",
                    format!("{} is not UTF-8 — skipped", path.display()),
                );
                continue;
            }
        };

        let rel = relative_to(&anchor, &path);
        let body_cap = MAX_PER_FILE_BYTES.min(bytes_remaining);
        let (body, file_truncated) = clip(&text, body_cap);

        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(&format!("# AGENTS.md ({rel})\n"));
        out.push_str(body);
        if file_truncated {
            let dropped = text.len() - body.len();
            out.push_str(&format!("\n[AGENTS.md truncated — {dropped} more bytes]"));
        }

        bytes_remaining = bytes_remaining.saturating_sub(body.len());
        if bytes_remaining == 0 {
            break;
        }
    }

    if out.is_empty() { None } else { Some(out) }
}

/// Walk `start` upward looking for a `.git` entry (dir or file). Stop
/// at the filesystem root or `$HOME`, whichever comes first. Returns
/// the directory containing the `.git`; falls back to `start` itself
/// when no marker is found, which gives us a sane single-dir anchor
/// for the not-in-a-git-repo case.
fn find_anchor(start: &Path) -> PathBuf {
    let stop_at = home_dir_canonical();
    let mut cursor: &Path = start;
    loop {
        if cursor.join(".git").exists() {
            return cursor.to_path_buf();
        }
        if let Some(home) = stop_at.as_deref()
            && cursor == home
        {
            return start.to_path_buf();
        }
        match cursor.parent() {
            Some(p) => cursor = p,
            None => return start.to_path_buf(),
        }
    }
}

/// Build the ordered list of directories from `anchor` down to
/// `leaf`, inclusive on both ends. If `leaf` doesn't live under
/// `anchor` (canonicalisation drift, weird mount), fall back to
/// `[leaf]` so the user still gets the closest AGENTS.md they expect.
fn chain_from(anchor: &Path, leaf: &Path) -> Vec<PathBuf> {
    let Ok(rel) = leaf.strip_prefix(anchor) else {
        return vec![leaf.to_path_buf()];
    };
    let mut out: Vec<PathBuf> = vec![anchor.to_path_buf()];
    let mut cursor = anchor.to_path_buf();
    for seg in rel.components() {
        cursor.push(seg);
        out.push(cursor.clone());
    }
    out
}

/// Display path relative to `anchor`. Used in the section header so
/// the LLM sees `./AGENTS.md` and `api/AGENTS.md` rather than
/// `/home/me/repo/api/AGENTS.md`.
fn relative_to(anchor: &Path, full: &Path) -> String {
    match full.strip_prefix(anchor) {
        Ok(rel) => {
            let s = rel.to_string_lossy().to_string();
            // Anchor's own AGENTS.md gets a leading `./` so the LLM
            // sees a consistent path-style header for every file in
            // the chain, including the top one.
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

fn home_dir_canonical() -> Option<PathBuf> {
    #[allow(deprecated)] // re-stabilised on rust-version >= 1.86 (matches user_config).
    std::env::home_dir().and_then(|p| p.canonicalize().ok())
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

    fn fake_git(dir: &Path) {
        fs::create_dir_all(dir.join(".git")).unwrap();
    }

    #[test]
    fn no_agents_md_anywhere_returns_none() {
        let root = tempdir();
        fake_git(&root);
        assert!(load(&root, &Logger::discard()).is_none());
    }

    #[test]
    fn single_agents_md_at_anchor_loads_with_header() {
        let root = tempdir();
        fake_git(&root);
        fs::write(root.join("AGENTS.md"), "use snake_case for tables").unwrap();
        let loaded = load(&root, &Logger::discard()).expect("loaded");
        assert!(loaded.contains("# AGENTS.md (./AGENTS.md)"));
        assert!(loaded.contains("use snake_case for tables"));
    }

    #[test]
    fn anchor_walks_up_to_nearest_git_dir() {
        // Layout: anchor/.git, anchor/AGENTS.md, anchor/sub/api/AGENTS.md.
        // project_root = anchor/sub/api → both AGENTS.md files load,
        // anchor file first.
        let anchor = tempdir();
        fake_git(&anchor);
        fs::write(anchor.join("AGENTS.md"), "repo-wide rule").unwrap();
        let api = anchor.join("sub").join("api");
        fs::create_dir_all(&api).unwrap();
        fs::write(api.join("AGENTS.md"), "api subproject rule").unwrap();

        let loaded = load(&api, &Logger::discard()).expect("loaded");
        let repo_pos = loaded.find("repo-wide rule").expect("repo content");
        let api_pos = loaded.find("api subproject rule").expect("api content");
        assert!(repo_pos < api_pos, "anchor file should appear before leaf");
        assert!(loaded.contains("./AGENTS.md"));
        assert!(loaded.contains("sub/api/AGENTS.md") || loaded.contains("sub/api"));
    }

    #[test]
    fn no_git_marker_falls_back_to_project_root_only() {
        let root = tempdir();
        // No .git anywhere in the chain.
        fs::write(root.join("AGENTS.md"), "isolated").unwrap();
        let loaded = load(&root, &Logger::discard()).expect("loaded");
        assert!(loaded.contains("isolated"));
    }

    #[test]
    fn non_utf8_file_is_skipped_but_others_load() {
        let anchor = tempdir();
        fake_git(&anchor);
        // Anchor file is valid UTF-8.
        fs::write(anchor.join("AGENTS.md"), "valid utf8").unwrap();
        // Subdir file is invalid UTF-8.
        let sub = anchor.join("sub");
        fs::create_dir_all(&sub).unwrap();
        fs::write(sub.join("AGENTS.md"), [0xff, 0xfe, 0xfd]).unwrap();

        let loaded = load(&sub, &Logger::discard()).expect("anchor file still loads");
        assert!(loaded.contains("valid utf8"));
        // The non-UTF-8 file's content must NOT appear.
        assert!(!loaded.contains("\u{FFFD}"));
    }

    #[test]
    fn oversized_single_file_is_truncated_with_marker() {
        let root = tempdir();
        fake_git(&root);
        let huge = "a".repeat(MAX_PER_FILE_BYTES + 1024);
        fs::write(root.join("AGENTS.md"), &huge).unwrap();
        let loaded = load(&root, &Logger::discard()).expect("loaded");
        assert!(
            loaded.contains("[AGENTS.md truncated"),
            "expected truncation marker, got: {}",
            &loaded[..loaded.len().min(200)]
        );
    }

    #[test]
    fn combined_cap_truncates_last_file() {
        // Anchor file fills most of the budget; sub file gets truncated.
        let anchor = tempdir();
        fake_git(&anchor);
        let big_anchor = "a".repeat(MAX_PER_FILE_BYTES);
        fs::write(anchor.join("AGENTS.md"), &big_anchor).unwrap();
        let sub = anchor.join("sub");
        fs::create_dir_all(&sub).unwrap();
        let big_sub = "b".repeat(MAX_PER_FILE_BYTES);
        fs::write(sub.join("AGENTS.md"), &big_sub).unwrap();

        let loaded = load(&sub, &Logger::discard()).expect("loaded");
        // Both files referenced by header.
        assert!(loaded.contains("./AGENTS.md"));
        assert!(loaded.contains("sub/AGENTS.md") || loaded.contains("sub"));
        // The combined cap fits both fully (MAX_PER_FILE * 2 == MAX_TOTAL),
        // so no truncation. Bump the second file to force truncation.
        let bigger_sub = "c".repeat(MAX_PER_FILE_BYTES + 1024);
        fs::write(sub.join("AGENTS.md"), &bigger_sub).unwrap();
        let loaded = load(&sub, &Logger::discard()).expect("loaded");
        assert!(loaded.contains("[AGENTS.md truncated"));
    }

    #[test]
    fn header_paths_are_relative_to_anchor() {
        // Headers must not leak absolute paths — privacy and prompt
        // hygiene both want this.
        let anchor = tempdir();
        fake_git(&anchor);
        fs::write(anchor.join("AGENTS.md"), "x").unwrap();
        let api = anchor.join("services").join("api");
        fs::create_dir_all(&api).unwrap();
        fs::write(api.join("AGENTS.md"), "y").unwrap();

        let loaded = load(&api, &Logger::discard()).expect("loaded");
        let abs = anchor.to_string_lossy().to_string();
        assert!(
            !loaded.contains(&abs),
            "absolute anchor path leaked into prompt: {abs}"
        );
    }

    #[test]
    fn clip_respects_char_boundaries() {
        // Multi-byte char near the cap should not produce invalid UTF-8.
        let s = "aaaa\u{1F600}bbbb"; // emoji is 4 bytes
        // Cap mid-emoji.
        let (slice, truncated) = clip(s, 6);
        assert!(truncated);
        // slice must be valid UTF-8 (it is by construction since it's &str).
        assert!(slice.starts_with("aaaa"));
        // Cap larger than total returns full string.
        let (slice, truncated) = clip(s, 100);
        assert!(!truncated);
        assert_eq!(slice, s);
    }
}
