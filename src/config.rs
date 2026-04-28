use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::llm::LlmBackendKind;
use crate::ui::theme::ThemeKind;

pub const FILE_NAME: &str = "config.toml";

/// On-disk shape of `.rowdy/config.toml`.
///
/// The crypto block is present iff the store is in encrypted mode. Connection
/// entries carry either a plaintext `url` (plaintext store) or `nonce` +
/// `ciphertext` (encrypted store) — never both.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    /// `None` when the field is not pinned by the project config —
    /// defaults to user config (then compiled default) at App seed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<ThemeKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema_width: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crypto: Option<CryptoBlock>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub connections: Vec<ConnectionEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub llm_providers: Vec<LlmProviderEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CryptoBlock {
    /// Base64 of the 16-byte salt fed to argon2id.
    pub salt: String,
    /// Base64 of the 12-byte nonce used to encrypt the verifier blob.
    pub verifier_nonce: String,
    /// Base64 of the verifier ciphertext (AEAD tag included).
    pub verifier_ciphertext: String,
    #[serde(default = "default_m_cost")]
    pub m_cost: u32,
    #[serde(default = "default_t_cost")]
    pub t_cost: u32,
    #[serde(default = "default_p_cost")]
    pub p_cost: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionEntry {
    pub name: String,
    /// Plaintext URL (set in plaintext mode). Mutually exclusive with the
    /// encrypted fields below.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Base64 of the 12-byte nonce used for this entry's AEAD operation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nonce: Option<String>,
    /// Base64 of the AEAD ciphertext (tag included). AAD is the entry name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ciphertext: Option<String>,
}

/// On-disk LLM provider record. Mirrors [`ConnectionEntry`]'s plaintext-vs-
/// encrypted shape: either `api_key` is set, or `nonce` + `ciphertext` are.
/// The `crypto` block on [`Config`] gates the mode for both kinds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmProviderEntry {
    pub name: String,
    pub backend: LlmBackendKind,
    /// Default model the user picked for this provider (e.g. `gpt-4.1-mini`).
    pub model: String,
    /// Optional base URL override. Required for OpenRouter (and useful for
    /// any OpenAI-compatible proxy); ignored by backends that hard-code their
    /// endpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Plaintext API key (set in plaintext mode). Mutually exclusive with the
    /// encrypted fields below.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// Base64 of the 12-byte nonce used for this entry's AEAD operation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nonce: Option<String>,
    /// Base64 of the AEAD ciphertext (tag included). AAD is the entry name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ciphertext: Option<String>,
}

fn default_m_cost() -> u32 {
    crate::crypto::KdfParams::default().m_cost
}
fn default_t_cost() -> u32 {
    crate::crypto::KdfParams::default().t_cost
}
fn default_p_cost() -> u32 {
    crate::crypto::KdfParams::default().p_cost
}

/// Owns the on-disk config file. All mutators flush eagerly, but a vanilla
/// (untouched) run never creates the file.
pub struct ConfigStore {
    path: PathBuf,
    state: Config,
}

impl ConfigStore {
    /// Loads from `<dir>/config.toml` if present; returns defaults otherwise.
    /// A missing file is not an error — it means the user is on defaults.
    /// A malformed file *is* an error: silently using defaults would lose
    /// the user's saved connections.
    pub fn load(dir: &Path) -> io::Result<Self> {
        let path = dir.join(FILE_NAME);
        let state = match fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text).map_err(|e| {
                io::Error::new(io::ErrorKind::InvalidData, format!("invalid config: {e}"))
            })?,
            Err(err) if err.kind() == io::ErrorKind::NotFound => Config::default(),
            Err(err) => return Err(err),
        };
        Ok(Self { path, state })
    }

    pub fn state(&self) -> &Config {
        &self.state
    }

    pub fn crypto(&self) -> Option<&CryptoBlock> {
        self.state.crypto.as_ref()
    }

    pub fn connections(&self) -> &[ConnectionEntry] {
        &self.state.connections
    }

    pub fn connection(&self, name: &str) -> Option<&ConnectionEntry> {
        self.state.connections.iter().find(|c| c.name == name)
    }

    pub fn connection_names(&self) -> Vec<String> {
        self.state
            .connections
            .iter()
            .map(|c| c.name.clone())
            .collect()
    }

    // LLM-provider getters/mutators are wired up in phase 1 but their first
    // call sites land in phase 2/3 (settings overlay + chat worker).
    #[allow(dead_code)]
    pub fn llm_providers(&self) -> &[LlmProviderEntry] {
        &self.state.llm_providers
    }

    #[allow(dead_code)]
    pub fn llm_provider(&self, name: &str) -> Option<&LlmProviderEntry> {
        self.state.llm_providers.iter().find(|p| p.name == name)
    }

    #[allow(dead_code)]
    pub fn llm_provider_names(&self) -> Vec<String> {
        self.state
            .llm_providers
            .iter()
            .map(|p| p.name.clone())
            .collect()
    }

    pub fn is_encrypted(&self) -> bool {
        self.state.crypto.is_some()
    }

    pub fn set_theme(&mut self, theme: ThemeKind) -> io::Result<()> {
        if self.state.theme == Some(theme) {
            return Ok(());
        }
        self.state.theme = Some(theme);
        self.flush()
    }

    pub fn set_schema_width(&mut self, width: u16) -> io::Result<()> {
        if self.state.schema_width == Some(width) {
            return Ok(());
        }
        self.state.schema_width = Some(width);
        self.flush()
    }

    /// Installs (or replaces) the crypto block. Caller is responsible for
    /// re-encrypting any pre-existing connection entries to match the new
    /// key — Phase 1 only persists the block.
    pub fn set_crypto(&mut self, crypto: CryptoBlock) -> io::Result<()> {
        self.state.crypto = Some(crypto);
        self.flush()
    }

    /// Inserts `entry` if its name is new; otherwise overwrites the existing
    /// entry with the same name. (Edit == overwrite, per the agreed scope.)
    pub fn upsert_connection(&mut self, entry: ConnectionEntry) -> io::Result<()> {
        if let Some(existing) = self
            .state
            .connections
            .iter_mut()
            .find(|c| c.name == entry.name)
        {
            *existing = entry;
        } else {
            self.state.connections.push(entry);
        }
        self.flush()
    }

    /// Removes the named connection. Returns `true` if anything was removed.
    pub fn delete_connection(&mut self, name: &str) -> io::Result<bool> {
        let before = self.state.connections.len();
        self.state.connections.retain(|c| c.name != name);
        let removed = self.state.connections.len() != before;
        if removed {
            self.flush()?;
        }
        Ok(removed)
    }

    /// Inserts `entry` if its name is new; otherwise overwrites the matching
    /// entry. Mirrors [`Self::upsert_connection`] for LLM provider records.
    #[allow(dead_code)]
    pub fn upsert_llm_provider(&mut self, entry: LlmProviderEntry) -> io::Result<()> {
        if let Some(existing) = self
            .state
            .llm_providers
            .iter_mut()
            .find(|p| p.name == entry.name)
        {
            *existing = entry;
        } else {
            self.state.llm_providers.push(entry);
        }
        self.flush()
    }

    /// Removes the named LLM provider. Returns `true` if anything was removed.
    #[allow(dead_code)]
    pub fn delete_llm_provider(&mut self, name: &str) -> io::Result<bool> {
        let before = self.state.llm_providers.len();
        self.state.llm_providers.retain(|p| p.name != name);
        let removed = self.state.llm_providers.len() != before;
        if removed {
            self.flush()?;
        }
        Ok(removed)
    }

    fn flush(&self) -> io::Result<()> {
        let text = toml::to_string_pretty(&self.state)
            .map_err(|e| io::Error::other(format!("serialise config: {e}")))?;
        fs::write(&self.path, text)
    }
}

// ThemeKind needs serde derives so it can travel through Config. Lower-case
// in the file ("dark" / "light").
mod theme_serde {
    use super::ThemeKind;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    impl Serialize for ThemeKind {
        fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
            match self {
                ThemeKind::Dark => "dark",
                ThemeKind::Light => "light",
            }
            .serialize(s)
        }
    }

    impl<'de> Deserialize<'de> for ThemeKind {
        fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
            let s = String::deserialize(d)?;
            ThemeKind::parse(&s).ok_or_else(|| {
                serde::de::Error::custom(format!("unknown theme: {s} (use dark|light)"))
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(cfg: &Config) -> Config {
        let text = toml::to_string_pretty(cfg).unwrap();
        toml::from_str(&text).unwrap()
    }

    #[test]
    fn defaults_round_trip() {
        let cfg = Config::default();
        assert_eq!(round_trip(&cfg), cfg);
    }

    #[test]
    fn config_with_crypto_and_connections_round_trips() {
        let cfg = Config {
            theme: Some(ThemeKind::Light),
            schema_width: Some(50),
            crypto: Some(CryptoBlock {
                salt: "AAAA".into(),
                verifier_nonce: "BBBB".into(),
                verifier_ciphertext: "CCCC".into(),
                m_cost: 19_456,
                t_cost: 2,
                p_cost: 1,
            }),
            connections: vec![
                ConnectionEntry {
                    name: "local".into(),
                    url: Some("sqlite:./sample.db".into()),
                    nonce: None,
                    ciphertext: None,
                },
                ConnectionEntry {
                    name: "prod".into(),
                    url: None,
                    nonce: Some("DDDD".into()),
                    ciphertext: Some("EEEE".into()),
                },
            ],
            llm_providers: vec![
                LlmProviderEntry {
                    name: "gpt".into(),
                    backend: LlmBackendKind::Openai,
                    model: "gpt-4.1-mini".into(),
                    base_url: None,
                    api_key: Some("sk-PLAIN".into()),
                    nonce: None,
                    ciphertext: None,
                },
                LlmProviderEntry {
                    name: "router".into(),
                    backend: LlmBackendKind::Openrouter,
                    model: "anthropic/claude-3.5-sonnet".into(),
                    base_url: Some("https://openrouter.ai/api/v1".into()),
                    api_key: None,
                    nonce: Some("FFFF".into()),
                    ciphertext: Some("GGGG".into()),
                },
            ],
        };
        assert_eq!(round_trip(&cfg), cfg);
    }

    #[test]
    fn upsert_llm_provider_inserts_then_overwrites() {
        let dir = tempdir();
        let mut store = ConfigStore::load(&dir).unwrap();
        store
            .upsert_llm_provider(LlmProviderEntry {
                name: "gpt".into(),
                backend: LlmBackendKind::Openai,
                model: "gpt-4.1-mini".into(),
                base_url: None,
                api_key: Some("sk-1".into()),
                nonce: None,
                ciphertext: None,
            })
            .unwrap();
        assert_eq!(store.llm_providers().len(), 1);
        store
            .upsert_llm_provider(LlmProviderEntry {
                name: "gpt".into(),
                backend: LlmBackendKind::Openai,
                model: "gpt-4.1".into(),
                base_url: None,
                api_key: Some("sk-2".into()),
                nonce: None,
                ciphertext: None,
            })
            .unwrap();
        assert_eq!(store.llm_providers().len(), 1);
        assert_eq!(store.llm_provider("gpt").unwrap().model, "gpt-4.1");
    }

    #[test]
    fn delete_llm_provider_returns_whether_anything_was_removed() {
        let dir = tempdir();
        let mut store = ConfigStore::load(&dir).unwrap();
        store
            .upsert_llm_provider(LlmProviderEntry {
                name: "gpt".into(),
                backend: LlmBackendKind::Openai,
                model: "gpt-4.1-mini".into(),
                base_url: None,
                api_key: Some("sk".into()),
                nonce: None,
                ciphertext: None,
            })
            .unwrap();
        assert!(store.delete_llm_provider("gpt").unwrap());
        assert!(!store.delete_llm_provider("gpt").unwrap());
    }

    #[test]
    fn missing_optional_blocks_deserialize_to_none_and_empty() {
        let text = "theme = \"dark\"\nschema_width = 36\n";
        let cfg: Config = toml::from_str(text).unwrap();
        assert_eq!(cfg.theme, Some(ThemeKind::Dark));
        assert_eq!(cfg.schema_width, Some(36));
        assert!(cfg.crypto.is_none());
        assert!(cfg.connections.is_empty());
    }

    #[test]
    fn upsert_inserts_then_overwrites() {
        let dir = tempdir();
        let mut store = ConfigStore::load(&dir).unwrap();
        store
            .upsert_connection(ConnectionEntry {
                name: "x".into(),
                url: Some("sqlite::memory:".into()),
                nonce: None,
                ciphertext: None,
            })
            .unwrap();
        assert_eq!(store.connections().len(), 1);
        store
            .upsert_connection(ConnectionEntry {
                name: "x".into(),
                url: Some("sqlite:./other.db".into()),
                nonce: None,
                ciphertext: None,
            })
            .unwrap();
        assert_eq!(store.connections().len(), 1);
        assert_eq!(
            store.connection("x").unwrap().url.as_deref(),
            Some("sqlite:./other.db")
        );
    }

    #[test]
    fn delete_returns_whether_anything_was_removed() {
        let dir = tempdir();
        let mut store = ConfigStore::load(&dir).unwrap();
        store
            .upsert_connection(ConnectionEntry {
                name: "x".into(),
                url: Some("sqlite::memory:".into()),
                nonce: None,
                ciphertext: None,
            })
            .unwrap();
        assert!(store.delete_connection("x").unwrap());
        assert!(!store.delete_connection("x").unwrap());
    }

    fn tempdir() -> PathBuf {
        // Single-process test scope; collisions vanishingly unlikely.
        let p = std::env::temp_dir().join(format!("rowdy-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }
}
