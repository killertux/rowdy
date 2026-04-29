//! Keybindings — sparse override layer over hardcoded defaults.
//!
//! `actions` defines the rebindable surface; `chord` parses the
//! key-notation strings; `keymap` holds the active mapping. Wired
//! into `event::translate` preludes for [`Context::GlobalImmediate`],
//! [`Context::Leader`], and [`Context::Schema`]. Result + Chat
//! contexts are populated for help-popover rendering but stay
//! hardcoded in `event.rs` because their keys depend on per-mode sub-state.

pub mod actions;
pub mod chord;
pub mod keymap;

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

pub const FILE_NAME: &str = "keybindings.toml";

/// On-disk shape. Each per-context table maps chord notation
/// (`<C-w>l`, `<Space>r`, `gg`, …) to action ID (`cancel-query`,
/// `run-statement-under-cursor`, …). Consumed by
/// [`keymap::Keymap::merge_overrides`].
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct KeybindingsFile {
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub global_immediate: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub leader: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub schema: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub result: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub chat_normal: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub chat_insert: BTreeMap<String, String>,
}

#[cfg(test)]
impl KeybindingsFile {
    pub fn is_empty(&self) -> bool {
        self.global_immediate.is_empty()
            && self.leader.is_empty()
            && self.schema.is_empty()
            && self.result.is_empty()
            && self.chat_normal.is_empty()
            && self.chat_insert.is_empty()
    }
}

/// Load `<dir>/keybindings.toml`. Missing file → defaults. Malformed
/// TOML → Err with the path baked into the message, mirroring the
/// project config loader policy.
pub fn load(dir: &Path) -> io::Result<KeybindingsFile> {
    let path = dir.join(FILE_NAME);
    match fs::read_to_string(&path) {
        Ok(text) => toml::from_str(&text).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid keybindings at {}: {e}", path.display()),
            )
        }),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(KeybindingsFile::default()),
        Err(err) => Err(err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir() -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("rowdy-keys-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn load_missing_returns_default() {
        let f = load(&tempdir()).unwrap();
        assert!(f.is_empty());
    }

    #[test]
    fn load_empty_file_returns_default() {
        let dir = tempdir();
        fs::write(dir.join(FILE_NAME), "").unwrap();
        let f = load(&dir).unwrap();
        assert!(f.is_empty());
    }

    #[test]
    fn load_malformed_returns_err() {
        let dir = tempdir();
        fs::write(dir.join(FILE_NAME), "this is = not [valid toml").unwrap();
        let err = load(&dir).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("invalid keybindings"));
    }

    #[test]
    fn round_trip_with_overrides() {
        let mut f = KeybindingsFile::default();
        f.leader.insert("r".into(), "cancel-query".into());
        f.leader
            .insert("R".into(), "run-statement-under-cursor".into());
        f.schema.insert("o".into(), "schema-toggle".into());

        let text = toml::to_string_pretty(&f).unwrap();
        let parsed: KeybindingsFile = toml::from_str(&text).unwrap();
        assert_eq!(parsed, f);
    }

    #[test]
    fn parses_documented_per_context_tables() {
        let text = r#"
[leader]
r = "cancel-query"

[schema]
o = "schema-toggle"

[global_immediate]
":" = "open-command"
"#;
        let f: KeybindingsFile = toml::from_str(text).unwrap();
        assert_eq!(f.leader.get("r").map(String::as_str), Some("cancel-query"));
        assert_eq!(f.schema.get("o").map(String::as_str), Some("schema-toggle"));
        assert_eq!(
            f.global_immediate.get(":").map(String::as_str),
            Some("open-command")
        );
        assert!(f.result.is_empty());
    }
}
