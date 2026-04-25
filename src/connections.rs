use std::fmt;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use zeroize::Zeroizing;

use crate::config::{ConnectionEntry, CryptoBlock};
use crate::crypto::{self, DerivedKey, KdfParams, NONCE_LEN, SALT_LEN};

#[derive(Debug)]
pub enum ConnectionError {
    /// The on-disk entry doesn't match the store mode (encrypted entry but
    /// no key, or vice-versa).
    ModeMismatch {
        name: String,
        expected_encrypted: bool,
    },
    /// Stored bytes (salt/nonce/ciphertext) couldn't be decoded.
    Decode { what: &'static str, msg: String },
    /// Stored bytes decoded but had the wrong length for their slot.
    InvalidLength {
        what: &'static str,
        expected: usize,
        got: usize,
    },
    /// Decryption failed — wrong key or tampered ciphertext.
    Crypto(crypto::CryptoError),
    /// Decrypted plaintext wasn't valid UTF-8.
    Utf8(String),
}

impl fmt::Display for ConnectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ModeMismatch {
                name,
                expected_encrypted,
            } => write!(
                f,
                "connection {name:?}: store is {} but entry is the opposite",
                if *expected_encrypted {
                    "encrypted"
                } else {
                    "plaintext"
                }
            ),
            Self::Decode { what, msg } => write!(f, "could not base64-decode {what}: {msg}"),
            Self::InvalidLength {
                what,
                expected,
                got,
            } => {
                write!(f, "{what} has wrong length: expected {expected}, got {got}")
            }
            Self::Crypto(e) => write!(f, "{e}"),
            Self::Utf8(msg) => write!(f, "decrypted url is not valid utf-8: {msg}"),
        }
    }
}

impl std::error::Error for ConnectionError {}

pub type ConnectionResult<T> = Result<T, ConnectionError>;

/// Wraps the optional decryption key. `None` means the store is in plaintext
/// mode; entries are returned as-is. `Some(key)` means every entry on disk is
/// expected to be encrypted with `key`.
pub struct ConnectionStore {
    key: Option<DerivedKey>,
}

impl ConnectionStore {
    pub fn plaintext() -> Self {
        Self { key: None }
    }

    pub fn encrypted(key: DerivedKey) -> Self {
        Self { key: Some(key) }
    }

    pub fn is_encrypted(&self) -> bool {
        self.key.is_some()
    }

    /// Builds an entry from `name` + `url`, encrypting if a key is present.
    /// Caller persists the result via `ConfigStore::upsert_connection`.
    pub fn make_entry(&self, name: String, url: &str) -> ConnectionResult<ConnectionEntry> {
        match &self.key {
            None => Ok(ConnectionEntry {
                name,
                url: Some(url.to_string()),
                nonce: None,
                ciphertext: None,
            }),
            Some(key) => {
                let nonce = crypto::random_nonce().map_err(ConnectionError::Crypto)?;
                let ct = crypto::encrypt(key, &nonce, name.as_bytes(), url.as_bytes())
                    .map_err(ConnectionError::Crypto)?;
                Ok(ConnectionEntry {
                    name,
                    url: None,
                    nonce: Some(B64.encode(nonce)),
                    ciphertext: Some(B64.encode(ct)),
                })
            }
        }
    }

    /// Returns the cleartext URL for `entry`. The result is `Zeroizing` so
    /// the URL doesn't linger in memory after the caller is done with it.
    pub fn lookup(&self, entry: &ConnectionEntry) -> ConnectionResult<Zeroizing<String>> {
        match (
            &self.key,
            &entry.url,
            entry.nonce.as_ref(),
            entry.ciphertext.as_ref(),
        ) {
            (None, Some(url), None, None) => Ok(Zeroizing::new(url.clone())),
            (Some(key), None, Some(nonce_b64), Some(ct_b64)) => {
                let nonce = decode_array::<NONCE_LEN>("nonce", nonce_b64)?;
                let ct = decode_vec("ciphertext", ct_b64)?;
                let pt = crypto::decrypt(key, &nonce, entry.name.as_bytes(), &ct)
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

/// Builds a `CryptoBlock` for a fresh encrypted store. Generates a random
/// salt, derives the key, and writes a verifier blob. Returns the block plus
/// the derived key so the caller can immediately encrypt their first entry
/// without re-deriving.
pub fn initialise_crypto(password: &str) -> ConnectionResult<(CryptoBlock, DerivedKey)> {
    initialise_crypto_with(password, &KdfParams::default())
}

pub fn initialise_crypto_with(
    password: &str,
    params: &KdfParams,
) -> ConnectionResult<(CryptoBlock, DerivedKey)> {
    let salt = crypto::random_salt().map_err(ConnectionError::Crypto)?;
    let key = crypto::derive_key(password, &salt, params).map_err(ConnectionError::Crypto)?;
    let (vnonce, vct) = crypto::build_verifier(&key).map_err(ConnectionError::Crypto)?;
    let block = CryptoBlock {
        salt: B64.encode(salt),
        verifier_nonce: B64.encode(vnonce),
        verifier_ciphertext: B64.encode(vct),
        m_cost: params.m_cost,
        t_cost: params.t_cost,
        p_cost: params.p_cost,
    };
    Ok((block, key))
}

/// Verifies `password` against the stored crypto block. On success returns
/// the derived key, ready for encryption/decryption.
pub fn unlock(password: &str, block: &CryptoBlock) -> ConnectionResult<DerivedKey> {
    let salt = decode_array::<SALT_LEN>("salt", &block.salt)?;
    let nonce = decode_array::<NONCE_LEN>("verifier_nonce", &block.verifier_nonce)?;
    let ct = decode_vec("verifier_ciphertext", &block.verifier_ciphertext)?;
    let params = KdfParams {
        m_cost: block.m_cost,
        t_cost: block.t_cost,
        p_cost: block.p_cost,
    };
    let key = crypto::derive_key(password, &salt, &params).map_err(ConnectionError::Crypto)?;
    if !crypto::check_verifier(&key, &nonce, &ct) {
        return Err(ConnectionError::Crypto(crypto::CryptoError::Aead));
    }
    Ok(key)
}

fn decode_array<const N: usize>(what: &'static str, encoded: &str) -> ConnectionResult<[u8; N]> {
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

fn decode_vec(what: &'static str, encoded: &str) -> ConnectionResult<Vec<u8>> {
    B64.decode(encoded).map_err(|e| ConnectionError::Decode {
        what,
        msg: e.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fast_params() -> KdfParams {
        KdfParams {
            m_cost: 8,
            t_cost: 1,
            p_cost: 1,
        }
    }

    #[test]
    fn plaintext_round_trip() {
        let store = ConnectionStore::plaintext();
        let entry = store
            .make_entry("local".into(), "sqlite:./sample.db")
            .unwrap();
        assert!(entry.ciphertext.is_none());
        assert_eq!(entry.url.as_deref(), Some("sqlite:./sample.db"));
        let url = store.lookup(&entry).unwrap();
        assert_eq!(url.as_str(), "sqlite:./sample.db");
    }

    #[test]
    fn encrypted_round_trip() {
        let (_block, key) = initialise_crypto_with("hunter2", &fast_params()).unwrap();
        let store = ConnectionStore::encrypted(key);
        let entry = store
            .make_entry("prod".into(), "postgres://u:p@h/db")
            .unwrap();
        assert!(entry.ciphertext.is_some());
        assert!(entry.url.is_none());
        let url = store.lookup(&entry).unwrap();
        assert_eq!(url.as_str(), "postgres://u:p@h/db");
    }

    #[test]
    fn unlock_with_correct_password_succeeds() {
        let (block, _key) = initialise_crypto_with("hunter2", &fast_params()).unwrap();
        let key = unlock("hunter2", &block).unwrap();
        // Verifier passed; the returned key must encrypt + decrypt cleanly.
        let store = ConnectionStore::encrypted(key);
        let entry = store.make_entry("e".into(), "url://x").unwrap();
        assert_eq!(store.lookup(&entry).unwrap().as_str(), "url://x");
    }

    #[test]
    fn unlock_with_wrong_password_fails() {
        let (block, _key) = initialise_crypto_with("hunter2", &fast_params()).unwrap();
        let err = unlock("wrong", &block).unwrap_err();
        assert!(matches!(
            err,
            ConnectionError::Crypto(crypto::CryptoError::Aead)
        ));
    }

    #[test]
    fn entry_renamed_after_encrypt_fails_to_decrypt() {
        // The connection name is part of the AAD, so renaming the entry on
        // disk (without re-encrypting) must invalidate it.
        let (_block, key) = initialise_crypto_with("pw", &fast_params()).unwrap();
        let store = ConnectionStore::encrypted(key);
        let mut entry = store.make_entry("prod".into(), "url://x").unwrap();
        entry.name = "dev".into();
        let err = store.lookup(&entry).unwrap_err();
        assert!(matches!(err, ConnectionError::Crypto(_)));
    }

    #[test]
    fn mode_mismatch_when_lookup_disagrees_with_store() {
        let (_block, key) = initialise_crypto_with("pw", &fast_params()).unwrap();
        let encrypted_store = ConnectionStore::encrypted(key);
        let plaintext_entry = ConnectionEntry {
            name: "x".into(),
            url: Some("sqlite::memory:".into()),
            nonce: None,
            ciphertext: None,
        };
        assert!(matches!(
            encrypted_store.lookup(&plaintext_entry),
            Err(ConnectionError::ModeMismatch { .. })
        ));

        let plaintext_store = ConnectionStore::plaintext();
        let encrypted_entry = ConnectionEntry {
            name: "x".into(),
            url: None,
            nonce: Some("AAAAAAAAAAAAAAAA".into()),
            ciphertext: Some("BBBB".into()),
        };
        assert!(matches!(
            plaintext_store.lookup(&encrypted_entry),
            Err(ConnectionError::ModeMismatch { .. })
        ));
    }
}
