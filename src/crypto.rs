use std::fmt;

use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::aead::{Aead, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, KeyInit, Nonce};
use rand::TryRng;
use zeroize::Zeroizing;

pub const KEY_LEN: usize = 32;
pub const SALT_LEN: usize = 16;
pub const NONCE_LEN: usize = 12;

/// Argon2id parameters used to derive the 32-byte AEAD key from the user
/// password. Defaults track OWASP's "modest hardware" baseline; tunable per
/// store so we can lower them for tests without touching prod values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KdfParams {
    /// Memory cost in KiB.
    pub m_cost: u32,
    /// Number of iterations.
    pub t_cost: u32,
    /// Parallelism (lanes).
    pub p_cost: u32,
}

impl Default for KdfParams {
    fn default() -> Self {
        // OWASP password storage cheat sheet, May 2024: Argon2id with m=19MiB,
        // t=2, p=1.
        Self {
            m_cost: 19_456,
            t_cost: 2,
            p_cost: 1,
        }
    }
}

/// 32-byte AEAD key. Wrapped in `Zeroizing` so it's wiped on drop.
pub type DerivedKey = Zeroizing<[u8; KEY_LEN]>;

/// Constant plaintext encrypted into the verifier blob; decrypting it
/// successfully proves the user supplied the correct password before any
/// real ciphertext is touched.
pub const VERIFIER_PLAINTEXT: &[u8] = b"rowdy-verifier-v1";
pub const VERIFIER_AAD: &[u8] = b"rowdy:verifier";

#[derive(Debug)]
pub enum CryptoError {
    Argon2(String),
    Aead,
    Rng(String),
}

impl fmt::Display for CryptoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Argon2(msg) => write!(f, "argon2 kdf failed: {msg}"),
            Self::Aead => write!(f, "decryption failed (wrong password or corrupt data)"),
            Self::Rng(msg) => write!(f, "rng failed: {msg}"),
        }
    }
}

impl std::error::Error for CryptoError {}

pub type CryptoResult<T> = Result<T, CryptoError>;

pub fn random_salt() -> CryptoResult<[u8; SALT_LEN]> {
    let mut buf = [0u8; SALT_LEN];
    rand::rng()
        .try_fill_bytes(&mut buf)
        .map_err(|e| CryptoError::Rng(e.to_string()))?;
    Ok(buf)
}

pub fn random_nonce() -> CryptoResult<[u8; NONCE_LEN]> {
    let mut buf = [0u8; NONCE_LEN];
    rand::rng()
        .try_fill_bytes(&mut buf)
        .map_err(|e| CryptoError::Rng(e.to_string()))?;
    Ok(buf)
}

pub fn derive_key(
    password: &str,
    salt: &[u8; SALT_LEN],
    params: &KdfParams,
) -> CryptoResult<DerivedKey> {
    let argon_params = Params::new(params.m_cost, params.t_cost, params.p_cost, Some(KEY_LEN))
        .map_err(|e| CryptoError::Argon2(e.to_string()))?;
    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, argon_params);
    let mut key = Zeroizing::new([0u8; KEY_LEN]);
    argon
        .hash_password_into(password.as_bytes(), salt, key.as_mut_slice())
        .map_err(|e| CryptoError::Argon2(e.to_string()))?;
    Ok(key)
}

pub fn encrypt(
    key: &[u8; KEY_LEN],
    nonce: &[u8; NONCE_LEN],
    aad: &[u8],
    plaintext: &[u8],
) -> CryptoResult<Vec<u8>> {
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    cipher
        .encrypt(Nonce::from_slice(nonce), Payload { msg: plaintext, aad })
        .map_err(|_| CryptoError::Aead)
}

pub fn decrypt(
    key: &[u8; KEY_LEN],
    nonce: &[u8; NONCE_LEN],
    aad: &[u8],
    ciphertext: &[u8],
) -> CryptoResult<Zeroizing<Vec<u8>>> {
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    let pt = cipher
        .decrypt(
            Nonce::from_slice(nonce),
            Payload {
                msg: ciphertext,
                aad,
            },
        )
        .map_err(|_| CryptoError::Aead)?;
    Ok(Zeroizing::new(pt))
}

/// Encrypts `VERIFIER_PLAINTEXT` so the next launch can detect a wrong
/// password before any connection ciphertext is touched. Returns
/// `(nonce, ciphertext)`.
pub fn build_verifier(key: &[u8; KEY_LEN]) -> CryptoResult<([u8; NONCE_LEN], Vec<u8>)> {
    let nonce = random_nonce()?;
    let ct = encrypt(key, &nonce, VERIFIER_AAD, VERIFIER_PLAINTEXT)?;
    Ok((nonce, ct))
}

/// True iff `key` decrypts the verifier ciphertext into the expected plaintext.
pub fn check_verifier(key: &[u8; KEY_LEN], nonce: &[u8; NONCE_LEN], ciphertext: &[u8]) -> bool {
    decrypt(key, nonce, VERIFIER_AAD, ciphertext)
        .map(|pt| pt.as_slice() == VERIFIER_PLAINTEXT)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fast_params() -> KdfParams {
        // Ultra-light parameters so the test suite stays snappy. Production
        // defaults live in `KdfParams::default()`.
        KdfParams {
            m_cost: 8,
            t_cost: 1,
            p_cost: 1,
        }
    }

    #[test]
    fn round_trip_encrypt_decrypt() {
        let salt = random_salt().unwrap();
        let key = derive_key("hunter2", &salt, &fast_params()).unwrap();
        let nonce = random_nonce().unwrap();
        let aad = b"connection:prod";
        let plaintext = b"postgres://user:pass@host/db";
        let ct = encrypt(&key, &nonce, aad, plaintext).unwrap();
        let pt = decrypt(&key, &nonce, aad, &ct).unwrap();
        assert_eq!(pt.as_slice(), plaintext);
    }

    #[test]
    fn decrypt_with_wrong_key_fails() {
        let salt = random_salt().unwrap();
        let key1 = derive_key("good", &salt, &fast_params()).unwrap();
        let key2 = derive_key("bad", &salt, &fast_params()).unwrap();
        let nonce = random_nonce().unwrap();
        let ct = encrypt(&key1, &nonce, b"x", b"secret").unwrap();
        assert!(decrypt(&key2, &nonce, b"x", &ct).is_err());
    }

    #[test]
    fn decrypt_with_wrong_aad_fails() {
        let salt = random_salt().unwrap();
        let key = derive_key("pw", &salt, &fast_params()).unwrap();
        let nonce = random_nonce().unwrap();
        let ct = encrypt(&key, &nonce, b"connection:prod", b"secret").unwrap();
        // Cipher-text bound to one slot must not decrypt under another's AAD.
        assert!(decrypt(&key, &nonce, b"connection:dev", &ct).is_err());
    }

    #[test]
    fn verifier_roundtrip_succeeds() {
        let salt = random_salt().unwrap();
        let key = derive_key("pw", &salt, &fast_params()).unwrap();
        let (nonce, ct) = build_verifier(&key).unwrap();
        assert!(check_verifier(&key, &nonce, &ct));
    }

    #[test]
    fn verifier_rejects_wrong_password() {
        let salt = random_salt().unwrap();
        let right = derive_key("right", &salt, &fast_params()).unwrap();
        let wrong = derive_key("wrong", &salt, &fast_params()).unwrap();
        let (nonce, ct) = build_verifier(&right).unwrap();
        assert!(!check_verifier(&wrong, &nonce, &ct));
    }

    #[test]
    fn same_password_same_salt_yields_same_key() {
        let salt = random_salt().unwrap();
        let k1 = derive_key("pw", &salt, &fast_params()).unwrap();
        let k2 = derive_key("pw", &salt, &fast_params()).unwrap();
        assert_eq!(k1.as_slice(), k2.as_slice());
    }

    #[test]
    fn same_password_different_salt_yields_different_keys() {
        let s1 = random_salt().unwrap();
        let mut s2 = s1;
        s2[0] ^= 0xFF;
        let k1 = derive_key("pw", &s1, &fast_params()).unwrap();
        let k2 = derive_key("pw", &s2, &fast_params()).unwrap();
        assert_ne!(k1.as_slice(), k2.as_slice());
    }
}
