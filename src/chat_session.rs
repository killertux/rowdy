//! Per-connection chat session persistence.
//!
//! Each connection's chat history lives at
//! `<data_dir>/chats/<sanitized_name>/session.jsonl` — one
//! `ChatMessage` per line, JSON-encoded, append-only. Append-only beats
//! rewriting an array because chat turns are written incrementally
//! (user submit, then assistant `Done`); the file always reflects the
//! latest stable state and a crash mid-turn can drop at most one line.
//!
//! We deliberately do *not* persist `ChatRole::System` messages — the
//! system prompt is rebuilt every turn from `llm::prompt::build_system_prompt`
//! and stitching a stale one back into history would cause drift.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use crate::state::chat::{ChatMessage, ChatRole};

const CHATS_DIR: &str = "chats";
const FILE_NAME: &str = "session.jsonl";

/// Path to the chat-session file for the given connection. Creates no
/// directories.
pub fn path_for(data_dir: &Path, connection_name: &str) -> PathBuf {
    data_dir
        .join(CHATS_DIR)
        .join(sanitize(connection_name))
        .join(FILE_NAME)
}

/// Read every persisted message for the connection. Missing file → empty
/// vec. Lines that fail to parse are skipped (best-effort: a corrupt
/// line shouldn't take down the whole history). Order is preserved.
pub fn load(path: &Path) -> io::Result<Vec<ChatMessage>> {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err),
    };
    let reader = BufReader::new(file);
    let mut out = Vec::new();
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<ChatMessage>(&line) {
            Ok(msg) => out.push(msg),
            Err(_) => continue,
        }
    }
    Ok(out)
}

/// Append one message as a single JSON line. Creates parent dirs and the
/// file itself on first call. System messages are skipped (see module
/// docs).
pub fn append(path: &Path, msg: &ChatMessage) -> io::Result<()> {
    if matches!(msg.role, ChatRole::System) {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    let line = serde_json::to_string(msg)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    file.write_all(line.as_bytes())?;
    file.write_all(b"\n")?;
    Ok(())
}

/// Wipe the session file. Missing file is a no-op so callers don't have
/// to special-case a never-saved connection.
pub fn clear(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

/// Replace anything outside `[A-Za-z0-9_.-]` with `_`. Different names
/// with the same sanitized form share the same chat file — same
/// trade-off `session::sanitize` makes for SQL sessions, accepted for
/// v1 because connection names are user-controlled and rare.
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
    use crate::state::chat::ChatBlock;

    #[test]
    fn sanitize_keeps_safe_characters() {
        assert_eq!(sanitize("local-dev_v2.0"), "local-dev_v2.0");
    }

    #[test]
    fn sanitize_replaces_path_unsafe_characters() {
        assert_eq!(sanitize("foo bar/baz"), "foo_bar_baz");
        assert_eq!(sanitize("../etc/passwd"), ".._etc_passwd");
    }

    #[test]
    fn sanitize_neutralises_dot_components() {
        assert_eq!(sanitize(""), "_");
        assert_eq!(sanitize("."), "_.");
        assert_eq!(sanitize(".."), "_..");
    }

    #[test]
    fn path_for_includes_chats_segment() {
        let p = path_for(Path::new("/tmp/.rowdy"), "my db");
        assert_eq!(p, Path::new("/tmp/.rowdy/chats/my_db/session.jsonl"));
    }

    #[test]
    fn load_returns_empty_when_missing() {
        let dir = tempdir();
        let p = dir.join("nope.jsonl");
        assert!(load(&p).unwrap().is_empty());
    }

    #[test]
    fn append_then_load_round_trips() {
        let dir = tempdir();
        let p = dir.join("chats").join("c").join("session.jsonl");
        append(&p, &ChatMessage::user_text("hi")).unwrap();
        append(&p, &ChatMessage::assistant_text("hello")).unwrap();
        let loaded = load(&p).unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].role, ChatRole::User);
        assert_eq!(loaded[1].role, ChatRole::Assistant);
        match &loaded[0].blocks[..] {
            [ChatBlock::Text(s)] => assert_eq!(s, "hi"),
            _ => panic!("expected single text block"),
        }
    }

    #[test]
    fn append_skips_system_messages() {
        let dir = tempdir();
        let p = dir.join("session.jsonl");
        append(&p, &ChatMessage::system_text("you are an llm")).unwrap();
        // File should not have been created — system messages don't persist.
        assert!(!p.exists());
        assert!(load(&p).unwrap().is_empty());
    }

    #[test]
    fn clear_removes_file_then_load_is_empty() {
        let dir = tempdir();
        let p = dir.join("session.jsonl");
        append(&p, &ChatMessage::user_text("doomed")).unwrap();
        assert_eq!(load(&p).unwrap().len(), 1);
        clear(&p).unwrap();
        assert!(load(&p).unwrap().is_empty());
        // Idempotent — clearing a missing file is fine.
        clear(&p).unwrap();
    }

    #[test]
    fn load_skips_corrupt_lines_but_keeps_valid_ones() {
        let dir = tempdir();
        let p = dir.join("session.jsonl");
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let mut f = File::create(&p).unwrap();
        let good = serde_json::to_string(&ChatMessage::user_text("ok")).unwrap();
        writeln!(f, "{good}").unwrap();
        writeln!(f, "{{not valid json").unwrap();
        let good2 = serde_json::to_string(&ChatMessage::assistant_text("still ok")).unwrap();
        writeln!(f, "{good2}").unwrap();
        let loaded = load(&p).unwrap();
        assert_eq!(loaded.len(), 2);
    }

    fn tempdir() -> PathBuf {
        let base = std::env::temp_dir().join(format!(
            "rowdy-chat-session-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&base).unwrap();
        base
    }
}
