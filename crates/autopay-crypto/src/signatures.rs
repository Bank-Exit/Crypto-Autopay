/// Ed25519 signatures for:
/// - monero-autopay:1: code integrity verification
/// - Canary document authentication
use ed25519_dalek::{Signer, SigningKey, Verifier, VerifyingKey};
use rand::rngs::OsRng;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SignatureError {
    #[error("Invalid public key bytes")]
    InvalidPublicKey,
    #[error("Invalid signature bytes")]
    InvalidSignature,
    #[error("Signature verification failed")]
    VerificationFailed,
    #[error("Hex decode error: {0}")]
    HexDecode(String),
}

/// An Ed25519 keypair for a creator/merchant.
pub struct KeyPair {
    signing_key: SigningKey,
}

impl KeyPair {
    /// Generate a new random keypair (for creator onboarding).
    pub fn generate() -> Self {
        let signing_key = SigningKey::generate(&mut OsRng);
        Self { signing_key }
    }

    /// Import from a 32-byte secret scalar (hex-encoded).
    pub fn from_secret_hex(hex_str: &str) -> Result<Self, SignatureError> {
        let bytes = hex::decode(hex_str)
            .map_err(|e| SignatureError::HexDecode(e.to_string()))?;
        let bytes: [u8; 32] = bytes.try_into().map_err(|_| SignatureError::InvalidPublicKey)?;
        Ok(Self {
            signing_key: SigningKey::from_bytes(&bytes),
        })
    }

    pub fn public_key_hex(&self) -> String {
        hex::encode(self.signing_key.verifying_key().to_bytes())
    }

    pub fn sign(&self, payload: &[u8]) -> String {
        let sig = self.signing_key.sign(payload);
        hex::encode(sig.to_bytes())
    }
}

/// Sign an arbitrary payload. Returns hex-encoded signature.
pub fn sign_payload(signing_key_hex: &str, payload: &[u8]) -> Result<String, SignatureError> {
    let kp = KeyPair::from_secret_hex(signing_key_hex)?;
    Ok(kp.sign(payload))
}

/// Verify a payload against a hex-encoded public key and signature.
pub fn verify_payload(
    pubkey_hex: &str,
    payload: &[u8],
    signature_hex: &str,
) -> Result<(), SignatureError> {
    let pubkey_bytes = hex::decode(pubkey_hex)
        .map_err(|e| SignatureError::HexDecode(e.to_string()))?;
    let pubkey_bytes: [u8; 32] = pubkey_bytes
        .try_into()
        .map_err(|_| SignatureError::InvalidPublicKey)?;
    let verifying_key =
        VerifyingKey::from_bytes(&pubkey_bytes).map_err(|_| SignatureError::InvalidPublicKey)?;

    let sig_bytes = hex::decode(signature_hex)
        .map_err(|e| SignatureError::HexDecode(e.to_string()))?;
    let sig_bytes: [u8; 64] = sig_bytes
        .try_into()
        .map_err(|_| SignatureError::InvalidSignature)?;
    let signature = ed25519_dalek::Signature::from_bytes(&sig_bytes);

    verifying_key
        .verify(payload, &signature)
        .map_err(|_| SignatureError::VerificationFailed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_and_verify() {
        let kp = KeyPair::generate();
        let payload = b"test payload for monero-autopay";
        let sig = kp.sign(payload);
        assert!(verify_payload(&kp.public_key_hex(), payload, &sig).is_ok());
    }

    #[test]
    fn tampered_payload_fails() {
        let kp = KeyPair::generate();
        let sig = kp.sign(b"original");
        assert!(verify_payload(&kp.public_key_hex(), b"tampered", &sig).is_err());
    }
}
