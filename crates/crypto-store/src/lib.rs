//! Versioned, application-layer encryption for profile data.
//!
//! The module deliberately does not persist keys or talk to Android APIs. A platform adapter owns
//! the wrapped vault key (Android Keystore on Android, an OS keychain on desktop). This keeps the
//! cryptographic format portable while keeping platform secrets out of the shared Rust core.

use std::{fmt, sync::Mutex};

use argon2::{Algorithm, Argon2, Params, Version};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use chacha20poly1305::{AeadInPlace, KeyInit, XChaCha20Poly1305, XNonce};
use getrandom::fill as fill_random;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use zeroize::{Zeroize, ZeroizeOnDrop};

pub const ENVELOPE_VERSION: u8 = 1;
pub const KEY_BYTES: usize = 32;
pub const SALT_BYTES: usize = 16;
pub const NONCE_BYTES: usize = 24;
pub const ARGON2_MEMORY_KIB: u32 = 64 * 1024;
pub const ARGON2_ITERATIONS: u32 = 3;
pub const ARGON2_PARALLELISM: u32 = 1;

#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct MasterPassword(String);

impl MasterPassword {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for MasterPassword {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

impl From<String> for MasterPassword {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

impl From<&str> for MasterPassword {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct VaultKey([u8; KEY_BYTES]);

impl VaultKey {
    pub fn generate() -> Result<Self, CryptoError> {
        let mut key = [0; KEY_BYTES];
        fill_random(&mut key).map_err(CryptoError::Randomness)?;
        Ok(Self(key))
    }

    #[must_use]
    pub fn from_bytes(bytes: [u8; KEY_BYTES]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8; KEY_BYTES] {
        &self.0
    }
}

impl fmt::Debug for VaultKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted vault key>")
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct VaultMetadata {
    pub version: u8,
    pub salt: [u8; SALT_BYTES],
    pub wrapped_key: EncryptedPayload,
}

impl VaultMetadata {
    pub fn create(master_password: &MasterPassword) -> Result<(Self, VaultKey), CryptoError> {
        ensure_master_password(master_password)?;
        let mut salt = [0; SALT_BYTES];
        fill_random(&mut salt).map_err(CryptoError::Randomness)?;
        let key = VaultKey::generate()?;
        let wrapping_key = derive_key(master_password, &salt)?;
        let wrapped_key = encrypt(&wrapping_key, b"remoteapp/vault-key/v1", key.as_bytes())?;
        Ok((
            Self {
                version: ENVELOPE_VERSION,
                salt,
                wrapped_key,
            },
            key,
        ))
    }

    pub fn unlock(&self, master_password: &MasterPassword) -> Result<VaultKey, CryptoError> {
        ensure_master_password(master_password)?;
        if self.version != ENVELOPE_VERSION {
            return Err(CryptoError::UnsupportedVersion(self.version));
        }
        let wrapping_key = derive_key(master_password, &self.salt)?;
        let plaintext = decrypt(&wrapping_key, b"remoteapp/vault-key/v1", &self.wrapped_key)?;
        let bytes: [u8; KEY_BYTES] = plaintext
            .as_slice()
            .try_into()
            .map_err(|_| CryptoError::InvalidVaultKey)?;
        Ok(VaultKey::from_bytes(bytes))
    }
}

/// Platform-owned storage for the vault key.
///
/// Android should implement this trait with an Android Keystore-backed key alias. Desktop
/// adapters can use the platform keychain. The shared crate intentionally does not persist the
/// key itself, so a database export or a copied profile cache cannot unlock RDP credentials.
pub trait KeyStorage: Send + Sync {
    fn save(&self, key: &VaultKey) -> Result<(), CryptoError>;
    fn load(&self) -> Result<Option<VaultKey>, CryptoError>;
    fn clear(&self) -> Result<(), CryptoError>;
}

/// Non-secure in-memory adapter for deterministic tests and the desktop preview.
///
/// This type must not be used as the production Android key store.
#[derive(Debug, Default)]
pub struct MemoryKeyStorage(Mutex<Option<VaultKey>>);

impl KeyStorage for MemoryKeyStorage {
    fn save(&self, key: &VaultKey) -> Result<(), CryptoError> {
        let mut slot = self
            .0
            .lock()
            .map_err(|_| CryptoError::KeyStorage("memory key storage lock poisoned".into()))?;
        *slot = Some(key.clone());
        Ok(())
    }

    fn load(&self) -> Result<Option<VaultKey>, CryptoError> {
        self.0
            .lock()
            .map(|slot| slot.clone())
            .map_err(|_| CryptoError::KeyStorage("memory key storage lock poisoned".into()))
    }

    fn clear(&self) -> Result<(), CryptoError> {
        let mut slot = self
            .0
            .lock()
            .map_err(|_| CryptoError::KeyStorage("memory key storage lock poisoned".into()))?;
        slot.take();
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct EncryptedPayload {
    pub version: u8,
    pub nonce: [u8; NONCE_BYTES],
    pub ciphertext: Vec<u8>,
}

impl EncryptedPayload {
    #[must_use]
    pub fn encoded(&self) -> String {
        let mut bytes = Vec::with_capacity(1 + NONCE_BYTES + self.ciphertext.len());
        bytes.push(self.version);
        bytes.extend_from_slice(&self.nonce);
        bytes.extend_from_slice(&self.ciphertext);
        BASE64.encode(bytes)
    }

    pub fn decode_encoded(value: &str) -> Result<Self, CryptoError> {
        let bytes = BASE64.decode(value).map_err(CryptoError::Base64)?;
        if bytes.len() < 1 + NONCE_BYTES {
            return Err(CryptoError::MalformedEnvelope);
        }
        let version = bytes[0];
        let nonce: [u8; NONCE_BYTES] = bytes[1..1 + NONCE_BYTES]
            .try_into()
            .map_err(|_| CryptoError::MalformedEnvelope)?;
        Ok(Self {
            version,
            nonce,
            ciphertext: bytes[1 + NONCE_BYTES..].to_vec(),
        })
    }
}

pub fn encrypt(
    key: &VaultKey,
    associated_data: &[u8],
    plaintext: &[u8],
) -> Result<EncryptedPayload, CryptoError> {
    let cipher =
        XChaCha20Poly1305::new_from_slice(key.as_bytes()).map_err(|_| CryptoError::InvalidKey)?;
    let mut nonce = [0; NONCE_BYTES];
    fill_random(&mut nonce).map_err(CryptoError::Randomness)?;
    let mut ciphertext = plaintext.to_vec();
    let tag = cipher
        .encrypt_in_place_detached(XNonce::from_slice(&nonce), associated_data, &mut ciphertext)
        .map_err(|_| CryptoError::EncryptionFailed)?;
    ciphertext.extend_from_slice(&tag);
    Ok(EncryptedPayload {
        version: ENVELOPE_VERSION,
        nonce,
        ciphertext,
    })
}

pub fn decrypt(
    key: &VaultKey,
    associated_data: &[u8],
    payload: &EncryptedPayload,
) -> Result<Vec<u8>, CryptoError> {
    if payload.version != ENVELOPE_VERSION {
        return Err(CryptoError::UnsupportedVersion(payload.version));
    }
    if payload.ciphertext.len() < 16 {
        return Err(CryptoError::MalformedEnvelope);
    }
    let cipher =
        XChaCha20Poly1305::new_from_slice(key.as_bytes()).map_err(|_| CryptoError::InvalidKey)?;
    let split_at = payload.ciphertext.len() - 16;
    let mut plaintext = payload.ciphertext[..split_at].to_vec();
    let tag = chacha20poly1305::Tag::from_slice(&payload.ciphertext[split_at..]);
    cipher
        .decrypt_in_place_detached(
            XNonce::from_slice(&payload.nonce),
            associated_data,
            &mut plaintext,
            tag,
        )
        .map_err(|_| CryptoError::AuthenticationFailed)?;
    Ok(plaintext)
}

fn derive_key(
    master_password: &MasterPassword,
    salt: &[u8; SALT_BYTES],
) -> Result<VaultKey, CryptoError> {
    let params = Params::new(
        ARGON2_MEMORY_KIB,
        ARGON2_ITERATIONS,
        ARGON2_PARALLELISM,
        Some(KEY_BYTES),
    )
    .map_err(|error| CryptoError::Kdf(error.to_string()))?;
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut output = [0; KEY_BYTES];
    argon2
        .hash_password_into(master_password.expose().as_bytes(), salt, &mut output)
        .map_err(|error| CryptoError::Kdf(error.to_string()))?;
    Ok(VaultKey::from_bytes(output))
}

fn ensure_master_password(password: &MasterPassword) -> Result<(), CryptoError> {
    if password.is_empty() {
        Err(CryptoError::EmptyMasterPassword)
    } else {
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum CryptoError {
    #[error("master password cannot be empty")]
    EmptyMasterPassword,
    #[error("unsupported encryption envelope version {0}")]
    UnsupportedVersion(u8),
    #[error("invalid vault key")]
    InvalidVaultKey,
    #[error("invalid encryption key")]
    InvalidKey,
    #[error("malformed encrypted envelope")]
    MalformedEnvelope,
    #[error("encryption failed")]
    EncryptionFailed,
    #[error("ciphertext authentication failed")]
    AuthenticationFailed,
    #[error("key derivation failed: {0}")]
    Kdf(String),
    #[error("randomness source failed: {0}")]
    Randomness(getrandom::Error),
    #[error("platform key storage failed: {0}")]
    KeyStorage(String),
    #[error("base64 decoding failed: {0}")]
    Base64(base64::DecodeError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vault_round_trip_and_wrong_password_fail() {
        let password = MasterPassword::new("correct horse battery staple");
        let (metadata, key) = VaultMetadata::create(&password).unwrap();
        let plaintext = br#"{"host":"192.0.2.10","password":"secret"}"#;
        let encrypted = encrypt(&key, b"profile/1/1", plaintext).unwrap();
        assert_eq!(
            decrypt(&key, b"profile/1/1", &encrypted).unwrap(),
            plaintext
        );
        assert!(decrypt(&key, b"profile/1/2", &encrypted).is_err());
        assert_eq!(
            metadata.unlock(&password).unwrap().as_bytes(),
            key.as_bytes()
        );
        assert!(metadata.unlock(&MasterPassword::new("wrong")).is_err());
    }

    #[test]
    fn encoded_envelope_round_trips() {
        let password = MasterPassword::new("test");
        let (_, key) = VaultMetadata::create(&password).unwrap();
        let encrypted = encrypt(&key, b"aad", b"payload").unwrap();
        let decoded = EncryptedPayload::decode_encoded(&encrypted.encoded()).unwrap();
        assert_eq!(decrypt(&key, b"aad", &decoded).unwrap(), b"payload");
    }

    #[test]
    fn debug_output_redacts_secrets() {
        let password = MasterPassword::new("top-secret");
        let key = VaultKey::generate().unwrap();
        assert!(!format!("{password:?}").contains("top-secret"));
        assert!(!format!("{key:?}").contains("top-secret"));
    }
}
