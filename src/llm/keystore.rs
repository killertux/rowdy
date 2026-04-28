#![allow(dead_code)]

//! Encrypted-at-rest storage for LLM provider API keys.
//!
//! Mirrors `crate::connections::ConnectionStore` field-for-field — same
//! AAD-bound AEAD shape, same plaintext-vs-encrypted dichotomy, same
//! `DerivedKey`. The two stores share their key (derived once during the
//! auth flow) so a user who locked their connection list also locks their
//! API keys, and vice-versa.
//!
//! Why a parallel store rather than reusing `ConnectionStore` directly? The
//! domains are different (an LLM "URL" is a base URL, an LLM "secret" is an
//! API key, both fit awkwardly into `ConnectionEntry`'s `url` slot) and
//! mixing them would force every caller to filter by entry kind. Keeping the
//! split clean costs ~30 lines of mostly-trivial mirroring.
//!
//! Reuses `crate::crypto::{encrypt, decrypt, random_nonce}`; the wire format
//! (nonce + ciphertext, both base64) matches `ConnectionStore` so future
//! re-encryption tooling can treat them uniformly.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use zeroize::Zeroizing;

use crate::config::LlmProviderEntry;
use crate::connections::ConnectionError;
use crate::crypto::{self, DerivedKey, NONCE_LEN};

pub type KeyResult<T> = Result<T, ConnectionError>;

pub struct LlmKeyStore {
    key: Option<DerivedKey>,
}

impl LlmKeyStore {
    pub fn plaintext() -> Self {
        Self { key: None }
    }

    pub fn encrypted(key: DerivedKey) -> Self {
        Self { key: Some(key) }
    }

    pub fn is_encrypted(&self) -> bool {
        self.key.is_some()
    }

    /// Encrypts `api_key` (if a key is present) and folds the rest of the
    /// provider metadata in around it. Caller persists the result via
    /// [`crate::config::ConfigStore::upsert_llm_provider`].
    ///
    /// AAD is the entry name, mirroring `ConnectionStore::make_entry` — so
    /// renaming an entry on disk invalidates its ciphertext.
    pub fn make_entry(
        &self,
        name: String,
        backend: crate::llm::LlmBackendKind,
        model: String,
        base_url: Option<String>,
        api_key: &str,
    ) -> KeyResult<LlmProviderEntry> {
        match &self.key {
            None => Ok(LlmProviderEntry {
                name,
                backend,
                model,
                base_url,
                api_key: Some(api_key.to_string()),
                nonce: None,
                ciphertext: None,
            }),
            Some(key) => {
                let nonce = crypto::random_nonce().map_err(ConnectionError::Crypto)?;
                let ct = crypto::encrypt(key, &nonce, name.as_bytes(), api_key.as_bytes())
                    .map_err(ConnectionError::Crypto)?;
                Ok(LlmProviderEntry {
                    name,
                    backend,
                    model,
                    base_url,
                    api_key: None,
                    nonce: Some(B64.encode(nonce)),
                    ciphertext: Some(B64.encode(ct)),
                })
            }
        }
    }

    /// Returns the cleartext API key for `entry`. Wrapped in `Zeroizing` so
    /// it doesn't linger in memory after the caller releases it.
    pub fn lookup(&self, entry: &LlmProviderEntry) -> KeyResult<Zeroizing<String>> {
        match (
            &self.key,
            &entry.api_key,
            entry.nonce.as_ref(),
            entry.ciphertext.as_ref(),
        ) {
            (None, Some(key), None, None) => Ok(Zeroizing::new(key.clone())),
            (Some(derived), None, Some(nonce_b64), Some(ct_b64)) => {
                let nonce = decode_array::<NONCE_LEN>("nonce", nonce_b64)?;
                let ct = decode_vec("ciphertext", ct_b64)?;
                let pt = crypto::decrypt(derived, &nonce, entry.name.as_bytes(), &ct)
                    .map_err(ConnectionError::Crypto)?;
                let s = std::str::from_utf8(&pt)
                    .map_err(|e| ConnectionError::Utf8(e.to_string()))?
                    .to_string();
                Ok(Zeroizing::new(s))
            }
            _ => Err(ConnectionError::ModeMismatch {
                name: entry.name.clone(),
                expected_encrypted: self.key.is_some(),
            }),
        }
    }
}

fn decode_array<const N: usize>(what: &'static str, encoded: &str) -> KeyResult<[u8; N]> {
    let bytes = decode_vec(what, encoded)?;
    if bytes.len() != N {
        return Err(ConnectionError::InvalidLength {
            what,
            expected: N,
            got: bytes.len(),
        });
    }
    let mut out = [0u8; N];
    out.copy_from_slice(&bytes);
    Ok(out)
}

fn decode_vec(what: &'static str, encoded: &str) -> KeyResult<Vec<u8>> {
    B64.decode(encoded).map_err(|e| ConnectionError::Decode {
        what,
        msg: e.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connections;
    use crate::crypto::KdfParams;
    use crate::llm::LlmBackendKind;

    fn fast_params() -> KdfParams {
        KdfParams {
            m_cost: 8,
            t_cost: 1,
            p_cost: 1,
        }
    }

    #[test]
    fn plaintext_round_trip() {
        let store = LlmKeyStore::plaintext();
        let entry = store
            .make_entry(
                "gpt".into(),
                LlmBackendKind::Openai,
                "gpt-4.1-mini".into(),
                None,
                "sk-PLAIN",
            )
            .unwrap();
        assert!(entry.ciphertext.is_none());
        assert_eq!(entry.api_key.as_deref(), Some("sk-PLAIN"));
        let key = store.lookup(&entry).unwrap();
        assert_eq!(key.as_str(), "sk-PLAIN");
    }

    #[test]
    fn encrypted_round_trip() {
        let (_block, dk) = connections::initialise_crypto_with("hunter2", &fast_params()).unwrap();
        let store = LlmKeyStore::encrypted(dk);
        let entry = store
            .make_entry(
                "gpt".into(),
                LlmBackendKind::Openai,
                "gpt-4.1-mini".into(),
                None,
                "sk-SECRET",
            )
            .unwrap();
        assert!(entry.api_key.is_none());
        assert!(entry.ciphertext.is_some());
        let key = store.lookup(&entry).unwrap();
        assert_eq!(key.as_str(), "sk-SECRET");
    }

    #[test]
    fn shared_derived_key_drives_both_stores() {
        // The whole point of having two stores: a single password unlocks
        // both. Round-trip a connection URL and an LLM key under one key.
        let (_block, dk) = connections::initialise_crypto_with("pw", &fast_params()).unwrap();
        let conn_store = connections::ConnectionStore::encrypted(dk.clone());
        let key_store = LlmKeyStore::encrypted(dk);

        let conn_entry = conn_store
            .make_entry("prod".into(), "postgres://u:p@h/db")
            .unwrap();
        let key_entry = key_store
            .make_entry(
                "gpt".into(),
                LlmBackendKind::Openai,
                "gpt-4.1".into(),
                None,
                "sk-COSHARED",
            )
            .unwrap();

        assert_eq!(
            conn_store.lookup(&conn_entry).unwrap().as_str(),
            "postgres://u:p@h/db"
        );
        assert_eq!(
            key_store.lookup(&key_entry).unwrap().as_str(),
            "sk-COSHARED"
        );
    }

    #[test]
    fn entry_renamed_after_encrypt_fails_to_decrypt() {
        let (_block, dk) = connections::initialise_crypto_with("pw", &fast_params()).unwrap();
        let store = LlmKeyStore::encrypted(dk);
        let mut entry = store
            .make_entry(
                "prod".into(),
                LlmBackendKind::Openai,
                "gpt-4.1".into(),
                None,
                "sk-X",
            )
            .unwrap();
        entry.name = "dev".into();
        let err = store.lookup(&entry).unwrap_err();
        assert!(matches!(err, ConnectionError::Crypto(_)));
    }

    #[test]
    fn mode_mismatch_when_lookup_disagrees_with_store() {
        let (_block, dk) = connections::initialise_crypto_with("pw", &fast_params()).unwrap();
        let encrypted_store = LlmKeyStore::encrypted(dk);
        let plaintext_entry = LlmProviderEntry {
            name: "x".into(),
            backend: LlmBackendKind::Openai,
            model: "gpt-4.1".into(),
            base_url: None,
            api_key: Some("sk".into()),
            nonce: None,
            ciphertext: None,
        };
        assert!(matches!(
            encrypted_store.lookup(&plaintext_entry),
            Err(ConnectionError::ModeMismatch { .. })
        ));

        let plaintext_store = LlmKeyStore::plaintext();
        let encrypted_entry = LlmProviderEntry {
            name: "x".into(),
            backend: LlmBackendKind::Openai,
            model: "gpt-4.1".into(),
            base_url: None,
            api_key: None,
            nonce: Some("AAAAAAAAAAAAAAAA".into()),
            ciphertext: Some("BBBB".into()),
        };
        assert!(matches!(
            plaintext_store.lookup(&encrypted_entry),
            Err(ConnectionError::ModeMismatch { .. })
        ));
    }
}
