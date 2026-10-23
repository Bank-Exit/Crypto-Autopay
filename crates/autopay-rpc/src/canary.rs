/// Canary system — dead-man's-switch for subscription liveness.
///
/// The creator publishes a signed JSON at `canary_url`. AutoPay fetches it
/// periodically and checks:
/// 1. HTTP 200 reachable
/// 2. Ed25519 signature valid (same pubkey as in the subscription code)
/// 3. `wallet_address` matches `sellers_wallet` in the subscription
/// 4. `valid_until` is in the future
///
/// Any failure → subscription is suspended + red alert shown.
use reqwest::Client;
use serde::Deserialize;
use std::time::Duration;
use thiserror::Error;
use tracing::{debug, warn};

#[derive(Debug, Error)]
pub enum CanaryError {
    #[error("Canary URL unreachable: {0}")]
    Unreachable(String),
    #[error("Canary JSON invalid: {0}")]
    InvalidJson(String),
    #[error("Canary signature invalid")]
    SignatureInvalid,
    #[error("Canary wallet address mismatch (expected {expected}, got {actual})")]
    AddressMismatch { expected: String, actual: String },
    #[error("Canary expired at {0}")]
    Expired(String),
    #[error("Canary is marked inactive by creator")]
    MarkedInactive,
}

/// The JSON document published by the creator at `canary_url`.
#[derive(Debug, Deserialize)]
pub struct CanaryDocument {
    /// Must be true. If false, creator is explicitly signalling shutdown.
    pub active: bool,
    /// Must match `sellers_wallet` in the subscription code.
    pub wallet_address: String,
    /// RFC3339 timestamp — canary expires if not renewed before this date.
    pub valid_until: String,
    /// Human-readable message from the creator (optional).
    pub message: Option<String>,
    /// Ed25519 public key (hex) — must match `merchant_pubkey` in the subscription.
    pub pubkey: String,
    /// Ed25519 signature over the canonical JSON (all fields except `signature`).
    pub signature: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CanaryStatus {
    /// All checks passed.
    Ok,
    /// Canary URL not set — creator hasn't configured it (non-blocking warning).
    NotConfigured,
    /// HTTP fetch failed (network error, 404, etc.).
    Unreachable,
    /// Signature verification failed — possible tampering.
    SignatureInvalid,
    /// Wallet address in canary doesn't match subscription.
    AddressMismatch,
    /// Canary has expired — creator inactive or forgot to renew.
    Expired,
    /// Creator explicitly marked subscription as inactive.
    InactiveByCreator,
}

pub struct CanaryChecker {
    client: Client,
}

impl CanaryChecker {
    pub fn new() -> Self {
        Self {
            client: Client::builder()
                .timeout(Duration::from_secs(15))
                .build()
                .expect("failed to build canary HTTP client"),
        }
    }

    /// Check a canary URL against expected values from the subscription code.
    /// Returns CanaryStatus (never panics — all errors are mapped to status variants).
    pub async fn check(
        &self,
        canary_url: &str,
        expected_wallet: &str,
        expected_pubkey: Option<&str>,
    ) -> CanaryStatus {
        debug!("Checking canary at {canary_url}");

        let doc = match self.fetch_canary(canary_url).await {
            Ok(d) => d,
            Err(e) => {
                warn!("Canary fetch failed for {canary_url}: {e}");
                return CanaryStatus::Unreachable;
            }
        };

        if !doc.active {
            return CanaryStatus::InactiveByCreator;
        }

        // Wallet address check
        if doc.wallet_address != expected_wallet {
            warn!(
                "Canary wallet mismatch: expected {expected_wallet}, got {}",
                doc.wallet_address
            );
            return CanaryStatus::AddressMismatch;
        }

        // Expiry check
        let now = chrono::Utc::now().to_rfc3339();
        if doc.valid_until < now {
            warn!("Canary expired at {}", doc.valid_until);
            return CanaryStatus::Expired;
        }

        // Signature check (only if pubkey available from subscription code)
        let pubkey_to_use = expected_pubkey.unwrap_or(&doc.pubkey);
        if let Err(e) = self.verify_canary_signature(&doc, pubkey_to_use) {
            warn!("Canary signature invalid: {e}");
            return CanaryStatus::SignatureInvalid;
        }

        CanaryStatus::Ok
    }

    async fn fetch_canary(&self, url: &str) -> Result<CanaryDocument, CanaryError> {
        let resp = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|e| CanaryError::Unreachable(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(CanaryError::Unreachable(format!(
                "HTTP {}",
                resp.status()
            )));
        }

        resp.json::<CanaryDocument>()
            .await
            .map_err(|e| CanaryError::InvalidJson(e.to_string()))
    }

    fn verify_canary_signature(
        &self,
        doc: &CanaryDocument,
        pubkey_hex: &str,
    ) -> Result<(), CanaryError> {
        // Reconstruct the signed payload: JSON of the doc without the `signature` field.
        // We use a manual approach to avoid serde round-trip issues.
        let payload = serde_json::json!({
            "active": doc.active,
            "message": doc.message,
            "pubkey": doc.pubkey,
            "valid_until": doc.valid_until,
            "wallet_address": doc.wallet_address,
        });
        // Sort keys for canonical form
        let canonical = serde_json::to_string(&payload)
            .map_err(|e| CanaryError::InvalidJson(e.to_string()))?;

        autopay_crypto::signatures::verify_payload(
            pubkey_hex,
            canonical.as_bytes(),
            &doc.signature,
        )
        .map_err(|_| CanaryError::SignatureInvalid)
    }
}

impl Default for CanaryChecker {
    fn default() -> Self {
        Self::new()
    }
}
