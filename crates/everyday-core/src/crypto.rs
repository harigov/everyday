//! Encryption envelope.
//!
//! # Design
//!
//! Every Day uses the standard two-key layout:
//!
//! ```text
//!   password ──Argon2id(salt, params)──▶ KEK ──unwraps──▶ DEK ──encrypts──▶ records + blobs
//! ```
//!
//! A random 256-bit **data encryption key** (DEK) encrypts everything in the
//! vault. The DEK itself is stored wrapped by a **key encryption key** (KEK)
//! derived from the user's password with Argon2id. Changing the password
//! re-wraps the DEK, which is O(1) — no re-encryption of the journal.
//!
//! Records are sealed with XChaCha20-Poly1305: a 24-byte random nonce makes
//! collisions negligible without a counter, and the Poly1305 tag makes
//! tampering detectable. Each record is sealed with **associated data** that
//! binds it to its own identity, so an attacker with write access to the
//! store cannot move entry A's ciphertext over entry B and have it decrypt.

use crate::error::{Error, Result};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, ZeroizeOnDrop};

pub const KEY_LEN: usize = 32;
pub const NONCE_LEN: usize = 24;
pub const SALT_LEN: usize = 16;

/// Identifier written into the vault header, so a future release can add a
/// suite without breaking existing vaults.
pub const SUITE_XCHACHA20_POLY1305: &str = "xchacha20poly1305";
pub const SUITE_NONE: &str = "none";

/// A 256-bit secret that is wiped from memory when dropped.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct SecretKey([u8; KEY_LEN]);

impl SecretKey {
    pub fn random() -> Self {
        let mut k = [0u8; KEY_LEN];
        rand::rng().fill_bytes(&mut k);
        Self(k)
    }

    pub fn from_bytes(b: [u8; KEY_LEN]) -> Self {
        Self(b)
    }

    pub fn expose(&self) -> &[u8; KEY_LEN] {
        &self.0
    }
}

impl std::fmt::Debug for SecretKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never let a key reach a log line.
        f.write_str("SecretKey(<redacted>)")
    }
}

/// Sealing and opening of record payloads.
///
/// Implementors must be cheap to clone-free share across threads: the store
/// holds one behind an `Arc` for the lifetime of an unlocked session.
pub trait Cipher: Send + Sync + std::fmt::Debug {
    /// Suite identifier recorded in the vault header.
    fn id(&self) -> &'static str;

    /// True if this cipher actually protects the data. The UI uses it to
    /// decide whether to show the "unencrypted vault" warning.
    fn is_encrypting(&self) -> bool {
        true
    }

    /// Seal `plaintext`, binding it to `aad`.
    fn seal(&self, aad: &[u8], plaintext: &[u8]) -> Result<Vec<u8>>;

    /// Open a sealed payload. Fails if `aad` differs from sealing time or if
    /// the ciphertext was modified.
    fn open(&self, aad: &[u8], sealed: &[u8]) -> Result<Vec<u8>>;
}

/// XChaCha20-Poly1305. Wire format is `nonce (24) || ciphertext || tag (16)`.
pub struct AeadCipher {
    inner: XChaCha20Poly1305,
}

impl AeadCipher {
    pub fn new(key: &SecretKey) -> Self {
        Self { inner: XChaCha20Poly1305::new(&Key::from(*key.expose())) }
    }
}

impl std::fmt::Debug for AeadCipher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AeadCipher(xchacha20poly1305)")
    }
}

impl Cipher for AeadCipher {
    fn id(&self) -> &'static str {
        SUITE_XCHACHA20_POLY1305
    }

    fn seal(&self, aad: &[u8], plaintext: &[u8]) -> Result<Vec<u8>> {
        let mut nonce = [0u8; NONCE_LEN];
        rand::rng().fill_bytes(&mut nonce);
        let ct = self
            .inner
            .encrypt(&XNonce::from(nonce), Payload { msg: plaintext, aad })
            .map_err(|_| Error::Invalid("encryption failed".into()))?;
        let mut out = Vec::with_capacity(NONCE_LEN + ct.len());
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&ct);
        Ok(out)
    }

    fn open(&self, aad: &[u8], sealed: &[u8]) -> Result<Vec<u8>> {
        if sealed.len() < NONCE_LEN {
            return Err(Error::Decrypt);
        }
        let (nonce, ct) = sealed.split_at(NONCE_LEN);
        let nonce: &XNonce = nonce.try_into().map_err(|_| Error::Decrypt)?;
        self.inner.decrypt(nonce, Payload { msg: ct, aad }).map_err(|_| Error::Decrypt)
    }
}

/// Pass-through "cipher" for unencrypted vaults.
///
/// This exists so that the storage backends have exactly one code path.
/// A plaintext vault is a legitimate choice — a Markdown vault inside an
/// already-encrypted home directory, say — but it is never the default.
#[derive(Debug, Default)]
pub struct NullCipher;

impl Cipher for NullCipher {
    fn id(&self) -> &'static str {
        SUITE_NONE
    }

    fn is_encrypting(&self) -> bool {
        false
    }

    fn seal(&self, _aad: &[u8], plaintext: &[u8]) -> Result<Vec<u8>> {
        Ok(plaintext.to_vec())
    }

    fn open(&self, _aad: &[u8], sealed: &[u8]) -> Result<Vec<u8>> {
        Ok(sealed.to_vec())
    }
}

/// Argon2id cost parameters, persisted in the vault header so that a vault
/// created on a fast desktop still opens on a phone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KdfParams {
    /// Memory cost in KiB.
    pub memory_kib: u32,
    /// Number of passes.
    pub iterations: u32,
    /// Degree of parallelism.
    pub lanes: u32,
}

impl Default for KdfParams {
    /// ~64 MiB / 3 passes. Comfortably above the OWASP 2024 floor for
    /// Argon2id (19 MiB, t=2) while still unlocking in well under a second
    /// on a laptop and remaining viable on mobile.
    fn default() -> Self {
        Self { memory_kib: 64 * 1024, iterations: 3, lanes: 4 }
    }
}

impl KdfParams {
    /// Deliberately weak parameters for tests, so the suite does not spend
    /// seconds burning memory. Never use outside `cfg(test)` code paths.
    pub fn insecure_fast() -> Self {
        Self { memory_kib: 8, iterations: 1, lanes: 1 }
    }
}

/// Derive the key-encryption key from a password.
pub fn derive_key(password: &str, salt: &[u8], params: KdfParams) -> Result<SecretKey> {
    use argon2::{Algorithm, Argon2, ParamsBuilder, Version};

    let params = ParamsBuilder::new()
        .m_cost(params.memory_kib)
        .t_cost(params.iterations)
        .p_cost(params.lanes)
        .output_len(KEY_LEN)
        .build()
        .map_err(|e| Error::Invalid(format!("invalid Argon2 parameters: {e}")))?;

    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut out = [0u8; KEY_LEN];
    argon
        .hash_password_into(password.as_bytes(), salt, &mut out)
        .map_err(|e| Error::Invalid(format!("key derivation failed: {e}")))?;
    Ok(SecretKey::from_bytes(out))
}

pub fn random_salt() -> [u8; SALT_LEN] {
    let mut s = [0u8; SALT_LEN];
    rand::rng().fill_bytes(&mut s);
    s
}

/// Domain separator for the wrapped-DEK ciphertext.
const WRAP_AAD: &[u8] = b"everyday.vault.dek.v1";

pub fn wrap_key(kek: &SecretKey, dek: &SecretKey) -> Result<Vec<u8>> {
    AeadCipher::new(kek).seal(WRAP_AAD, dek.expose())
}

pub fn unwrap_key(kek: &SecretKey, wrapped: &[u8]) -> Result<SecretKey> {
    let mut bytes = AeadCipher::new(kek).open(WRAP_AAD, wrapped).map_err(|_| Error::BadPassword)?;
    if bytes.len() != KEY_LEN {
        bytes.zeroize();
        return Err(Error::Decrypt);
    }
    let mut key = [0u8; KEY_LEN];
    key.copy_from_slice(&bytes);
    bytes.zeroize();
    Ok(SecretKey::from_bytes(key))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> SecretKey {
        SecretKey::from_bytes([7u8; KEY_LEN])
    }

    #[test]
    fn seal_open_round_trips() {
        let c = AeadCipher::new(&key());
        let sealed = c.seal(b"entry:1", b"dear diary").unwrap();
        assert_eq!(c.open(b"entry:1", &sealed).unwrap(), b"dear diary");
    }

    #[test]
    fn ciphertext_does_not_contain_the_plaintext() {
        let c = AeadCipher::new(&key());
        let sealed = c.seal(b"entry:1", b"dear diary").unwrap();
        assert!(!sealed.windows(10).any(|w| w == b"dear diary"));
    }

    #[test]
    fn same_plaintext_seals_to_different_ciphertexts() {
        let c = AeadCipher::new(&key());
        let a = c.seal(b"x", b"same").unwrap();
        let b = c.seal(b"x", b"same").unwrap();
        assert_ne!(a, b, "a random nonce must be used per seal");
    }

    #[test]
    fn open_rejects_a_different_aad() {
        // This is what stops an attacker swapping entry ciphertexts around.
        let c = AeadCipher::new(&key());
        let sealed = c.seal(b"entry:1", b"secret").unwrap();
        assert!(matches!(c.open(b"entry:2", &sealed), Err(Error::Decrypt)));
    }

    #[test]
    fn open_rejects_tampering() {
        let c = AeadCipher::new(&key());
        let mut sealed = c.seal(b"aad", b"secret").unwrap();
        let last = sealed.len() - 1;
        sealed[last] ^= 0x01;
        assert!(matches!(c.open(b"aad", &sealed), Err(Error::Decrypt)));
    }

    #[test]
    fn open_rejects_a_different_key() {
        let sealed = AeadCipher::new(&key()).seal(b"aad", b"secret").unwrap();
        let other = AeadCipher::new(&SecretKey::from_bytes([9u8; KEY_LEN]));
        assert!(matches!(other.open(b"aad", &sealed), Err(Error::Decrypt)));
    }

    #[test]
    fn open_rejects_truncated_input() {
        let c = AeadCipher::new(&key());
        assert!(matches!(c.open(b"aad", &[0u8; 4]), Err(Error::Decrypt)));
        assert!(matches!(c.open(b"aad", &[]), Err(Error::Decrypt)));
    }

    #[test]
    fn null_cipher_is_transparent_and_advertises_itself() {
        let c = NullCipher;
        assert!(!c.is_encrypting());
        assert_eq!(c.seal(b"a", b"plain").unwrap(), b"plain");
        assert_eq!(c.open(b"a", b"plain").unwrap(), b"plain");
    }

    #[test]
    fn derive_key_is_deterministic_and_salt_sensitive() {
        let p = KdfParams::insecure_fast();
        let salt = [1u8; SALT_LEN];
        let a = derive_key("hunter2", &salt, p).unwrap();
        let b = derive_key("hunter2", &salt, p).unwrap();
        let c = derive_key("hunter2", &[2u8; SALT_LEN], p).unwrap();
        assert_eq!(a.expose(), b.expose());
        assert_ne!(a.expose(), c.expose());
    }

    #[test]
    fn wrap_unwrap_round_trips_and_rejects_the_wrong_password() {
        let p = KdfParams::insecure_fast();
        let salt = random_salt();
        let kek = derive_key("correct horse", &salt, p).unwrap();
        let dek = SecretKey::random();

        let wrapped = wrap_key(&kek, &dek).unwrap();
        assert_eq!(unwrap_key(&kek, &wrapped).unwrap().expose(), dek.expose());

        let wrong = derive_key("battery staple", &salt, p).unwrap();
        assert!(matches!(unwrap_key(&wrong, &wrapped), Err(Error::BadPassword)));
    }

    #[test]
    fn secret_key_debug_does_not_leak_material() {
        let s = format!("{:?}", SecretKey::from_bytes([0xAB; KEY_LEN]));
        assert_eq!(s, "SecretKey(<redacted>)");
        assert!(!s.contains("ab"));
    }
}
