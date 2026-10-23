/// Seed vault: Argon2id + AES-256-GCM encryption for Monero seed phrases.
///
/// Design:
/// - Argon2id (m=128MB, t=3, p=1) for key derivation — memory-hard, brute-force resistant
/// - AES-256-GCM for authenticated encryption (detects tampering)
/// - Seed is zeroized from memory immediately after encryption
/// - Stored on disk as: VERSION(1) | SALT(16) | NONCE(12) | CIPHERTEXT
use aes_gcm::{
    aead::{Aead, KeyInit, OsRng},
    Aes256Gcm, Key, Nonce,
};
use argon2::{Argon2, Params, Algorithm, Version};
use rand::RngCore;
use zeroize::Zeroizing;
use thiserror::Error;

const VAULT_VERSION: u8 = 0x01;
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 12;
const KEY_LEN: usize = 32;

// Argon2id parameters: 128 MB memory, 3 iterations, 1 thread
const ARGON2_M_COST: u32 = 131_072; // 128 MB in KB
const ARGON2_T_COST: u32 = 3;
const ARGON2_P_COST: u32 = 1;

#[derive(Debug, Error)]
pub enum VaultError {
    #[error("Encryption failed: {0}")]
    Encryption(String),
    #[error("Decryption failed — wrong password or corrupted data")]
    Decryption,
    #[error("Unknown vault version: {0:#x}")]
    UnknownVersion(u8),
    #[error("Corrupted vault data")]
    Corrupted,
    #[error("Key derivation failed: {0}")]
    Kdf(String),
}

/// An encrypted seed blob (VERSION | SALT | NONCE | CIPHERTEXT), hex-encoded for storage.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EncryptedSeed(pub String);

pub struct SeedVault;

impl SeedVault {
    /// Encrypt a seed phrase with a master password.
    /// The plaintext is zeroized from memory after encryption.
    pub fn encrypt(plaintext_seed: &str, password: &str) -> Result<EncryptedSeed, VaultError> {
        let mut salt = [0u8; SALT_LEN];
        OsRng.fill_bytes(&mut salt);

        let key = Self::derive_key(password, &salt)?;

        let mut nonce_bytes = [0u8; NONCE_LEN];
        OsRng.fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);

        let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key.as_ref()));
        let ciphertext = cipher
            .encrypt(nonce, plaintext_seed.as_bytes())
            .map_err(|e| VaultError::Encryption(e.to_string()))?;

        let mut blob = Vec::with_capacity(1 + SALT_LEN + NONCE_LEN + ciphertext.len());
        blob.push(VAULT_VERSION);
        blob.extend_from_slice(&salt);
        blob.extend_from_slice(&nonce_bytes);
        blob.extend_from_slice(&ciphertext);

        Ok(EncryptedSeed(hex::encode(blob)))
    }

    /// Decrypt a seed blob with the master password.
    /// Returns a Zeroizing<String> — seed is wiped when dropped.
    pub fn decrypt(
        encrypted: &EncryptedSeed,
        password: &str,
    ) -> Result<Zeroizing<String>, VaultError> {
        let blob = hex::decode(&encrypted.0).map_err(|_| VaultError::Corrupted)?;

        if blob.is_empty() {
            return Err(VaultError::Corrupted);
        }
        if blob[0] != VAULT_VERSION {
            return Err(VaultError::UnknownVersion(blob[0]));
        }

        let min_len = 1 + SALT_LEN + NONCE_LEN + 1;
        if blob.len() < min_len {
            return Err(VaultError::Corrupted);
        }

        let salt = &blob[1..1 + SALT_LEN];
        let nonce_bytes = &blob[1 + SALT_LEN..1 + SALT_LEN + NONCE_LEN];
        let ciphertext = &blob[1 + SALT_LEN + NONCE_LEN..];

        let key = Self::derive_key(password, salt)?;
        let nonce = Nonce::from_slice(nonce_bytes);
        let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key.as_ref()));

        let plaintext = cipher
            .decrypt(nonce, ciphertext)
            .map_err(|_| VaultError::Decryption)?;

        let seed_str = String::from_utf8(plaintext).map_err(|_| VaultError::Decryption)?;
        Ok(Zeroizing::new(seed_str))
    }

    fn derive_key(password: &str, salt: &[u8]) -> Result<Zeroizing<[u8; KEY_LEN]>, VaultError> {
        let params = Params::new(ARGON2_M_COST, ARGON2_T_COST, ARGON2_P_COST, Some(KEY_LEN))
            .map_err(|e| VaultError::Kdf(e.to_string()))?;

        let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
        let mut key = Zeroizing::new([0u8; KEY_LEN]);
        argon2
            .hash_password_into(password.as_bytes(), salt, key.as_mut())
            .map_err(|e| VaultError::Kdf(e.to_string()))?;

        Ok(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let seed = "hemlock lordship afoot stacking..."  ;
        let password = "hunter2";
        let encrypted = SeedVault::encrypt(seed, password).unwrap();
        let decrypted = SeedVault::decrypt(&encrypted, password).unwrap();
        assert_eq!(decrypted.as_str(), seed);
    }

    #[test]
    fn wrong_password_fails() {
        let seed = "some monero seed words here";
        let encrypted = SeedVault::encrypt(seed, "correct").unwrap();
        assert!(SeedVault::decrypt(&encrypted, "wrong").is_err());
    }

    #[test]
    fn tampered_data_fails() {
        let seed = "some monero seed words here";
        let encrypted = SeedVault::encrypt(seed, "password").unwrap();
        let mut tampered = encrypted.0.clone();
        // Flip a byte deep in the ciphertext
        let len = tampered.len();
        let mid = len / 2;
        let b = u8::from_str_radix(&tampered[mid..mid+2], 16).unwrap();
        tampered.replace_range(mid..mid+2, &format!("{:02x}", b ^ 0xff));
        assert!(SeedVault::decrypt(&EncryptedSeed(tampered), "password").is_err());
    }
}
