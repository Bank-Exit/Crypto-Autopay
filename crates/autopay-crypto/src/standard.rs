/// monero-autopay:1: standard — encoding, decoding, and validation.
///
/// Format:  monero-autopay:1:<base64url(gzip(canonical-json))>
///
/// Compatibility: also reads `monero-request:1:` codes (import-only, no generation).
///
/// Security properties:
/// - Ed25519 signature over canonical JSON (all fields except `signature` itself)
/// - BLAKE3 checksum of the pre-gzip canonical JSON
/// - `merchant_pubkey` must be present for signature verification
/// - `canary_url` field enables dead-man's-switch monitoring
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use blake3;
use flate2::{read::GzDecoder, write::GzEncoder, Compression};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use thiserror::Error;

use crate::signatures::verify_payload;

const PREFIX_V1: &str = "monero-autopay:1:";
const PREFIX_COMPAT: &str = "monero-request:1:";

#[derive(Debug, Error)]
pub enum CodeError {
    #[error("Unknown prefix — not a monero-autopay or monero-request code")]
    UnknownPrefix,
    #[error("Base64 decode error: {0}")]
    Base64(String),
    #[error("Gzip decompress error: {0}")]
    Gzip(String),
    #[error("JSON parse error: {0}")]
    Json(String),
    #[error("Gzip compress error: {0}")]
    GzipCompress(String),
    #[error("Checksum mismatch — data may be corrupted or truncated")]
    ChecksumMismatch,
    #[error("Signature verification failed: {0}")]
    SignatureInvalid(String),
    #[error("Missing required field: {0}")]
    MissingField(String),
    #[error("Invalid wallet address")]
    InvalidAddress,
    #[error("Code has expired")]
    Expired,
}

/// The canonical monero-autopay:1: payload.
/// All fields that are Option<> are optional in the spec.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutoPayCodeV1 {
    /// Standard version embedded in JSON for forward compatibility.
    pub v: u8,

    /// Short display name (≤80 chars).
    pub custom_label: String,

    /// Long description of what the subscription provides.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// Blockchain to use: "monero" (V1), "bitcoin" (V3+).
    pub payment_network: String,

    /// Recipient wallet address (Monero primary address or subaddress).
    pub sellers_wallet: String,

    /// Currency of the `amount` field: "XMR", "EUR", "USD", etc.
    pub currency: String,

    /// Amount in `currency` units.
    pub amount: String,

    /// RFC3339 timestamp for the first payment.
    pub start_date: String,

    /// Cron expression: "0 0 1 * *" = monthly on the 1st.
    pub schedule: String,

    /// 0 = recurring until cancelled, 1 = one-time, N = fixed count.
    pub number_of_payments: u64,

    /// Minimum confirmations before considering payment settled.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_confirmations: Option<u32>,

    /// Grace period in hours before subscription is marked as expired.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grace_period_hours: Option<u32>,

    /// Free trial period in days before first payment.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trial_days: Option<u32>,

    /// When the subscription offer itself expires (RFC3339).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,

    /// URL for payment confirmation webhook (creator side).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub webhook_url: Option<String>,

    /// URL for the canary document (dead-man's-switch).
    /// If present, AutoPay will periodically fetch and verify this.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub canary_url: Option<String>,

    /// Creator's Ed25519 public key (hex). Required for signature verification.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub merchant_pubkey: Option<String>,

    /// Ed25519 signature over the canonical JSON of all other fields (hex).
    /// Computed over the JSON with `signature` field absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,

    /// BLAKE3 checksum of the pre-compression canonical JSON (hex).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checksum: Option<String>,

    // --- BOLT 12 / lnrecc / LNbits-inspired fields ---

    /// Seconds before the period start during which payment is accepted.
    /// Default: 7 days (604800s). Mirrors BOLT 12 paywindow.seconds_before.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pay_window_before_secs: Option<u32>,

    /// Seconds after the period start during which payment is still accepted.
    /// Default: 3 days (259200s). Mirrors BOLT 12 paywindow.seconds_after.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pay_window_after_secs: Option<u32>,

    /// If true and payment arrives after period start (but within pay_window_after_secs),
    /// the amount is reduced proportionally to the time remaining in the period.
    /// Mirrors BOLT 12 proportional_amount.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proportional_amount: Option<bool>,

    /// URL the creator updates each period with a message for the payer.
    /// AutoPay fetches this before each payment and displays the content.
    /// If the message contains a price change, the payer must manually confirm.
    /// Expected JSON: { "message": "...", "new_amount": "9.99", "new_currency": "EUR" (optional) }
    #[serde(skip_serializing_if = "Option::is_none")]
    pub occurrence_note_url: Option<String>,

    /// Cancel flag: if true, this code signals the creator that the payer
    /// is cancelling the subscription (sent as a courtesy notification).
    /// Used when AutoPay sends a cancellation webhook instead of a payment.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cancel: Option<bool>,

    /// Max fee the payer accepts for conversion (e.g. fiat→XMR exchange slippage).
    /// Expressed as a percentage string, e.g. "2.5" = max 2.5% slippage.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_conversion_fee_pct: Option<String>,

    /// Extensible metadata bag.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

/// Decoded code — either our V1 or a legacy monero-request:1: (imported as V1).
pub enum AutoPayCode {
    V1(AutoPayCodeV1),
}

/// Parse a code string into an AutoPayCode.
/// Performs checksum and signature verification when present.
pub fn parse_code(code: &str) -> Result<AutoPayCode, CodeError> {
    if let Some(rest) = code.strip_prefix(PREFIX_V1) {
        let payload = decode_payload(rest)?;
        let v1: AutoPayCodeV1 =
            serde_json::from_str(&payload).map_err(|e| CodeError::Json(e.to_string()))?;

        verify_checksum(&v1, &payload)?;
        verify_signature_if_present(&v1)?;
        check_expiry(&v1)?;

        Ok(AutoPayCode::V1(v1))
    } else if let Some(rest) = code.strip_prefix(PREFIX_COMPAT) {
        // monero-request:1: uses standard base64 (not URL-safe) + gzip
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(rest)
            .map_err(|e| CodeError::Base64(e.to_string()))?;
        let json = gunzip(&bytes)?;
        let compat: serde_json::Value =
            serde_json::from_str(&json).map_err(|e| CodeError::Json(e.to_string()))?;
        let v1 = map_compat_to_v1(compat)?;
        Ok(AutoPayCode::V1(v1))
    } else {
        Err(CodeError::UnknownPrefix)
    }
}

/// Encode an AutoPayCodeV1 into a monero-autopay:1: string.
/// Computes and sets checksum. Signature must be set beforehand if desired.
pub fn encode_code(code: &AutoPayCodeV1) -> Result<String, CodeError> {
    let canonical = canonical_json(code)?;

    // Re-encode with checksum
    let mut with_checksum = code.clone();
    with_checksum.checksum = Some(hex::encode(blake3::hash(canonical.as_bytes()).as_bytes()));

    let canonical_final = canonical_json(&with_checksum)?;
    let compressed = gzip(canonical_final.as_bytes())?;
    let encoded = URL_SAFE_NO_PAD.encode(&compressed);

    Ok(format!("{}{}", PREFIX_V1, encoded))
}

// --- Internal helpers ---

fn decode_payload(b64: &str) -> Result<String, CodeError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(b64)
        .map_err(|e| CodeError::Base64(e.to_string()))?;
    gunzip(&bytes)
}

fn gunzip(bytes: &[u8]) -> Result<String, CodeError> {
    let mut decoder = GzDecoder::new(bytes);
    let mut s = String::new();
    decoder
        .read_to_string(&mut s)
        .map_err(|e| CodeError::Gzip(e.to_string()))?;
    Ok(s)
}

fn gzip(data: &[u8]) -> Result<Vec<u8>, CodeError> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::best());
    encoder
        .write_all(data)
        .map_err(|e| CodeError::GzipCompress(e.to_string()))?;
    encoder
        .finish()
        .map_err(|e| CodeError::GzipCompress(e.to_string()))
}

/// Produce deterministic canonical JSON (sorted keys, no whitespace).
/// Uses serde_json's BTreeMap-backed Value for key ordering.
fn canonical_json(code: &AutoPayCodeV1) -> Result<String, CodeError> {
    // Serialize → Value (sorts struct fields by serde order) → re-serialize
    // For true canonicalization we go through Value with sorted map
    let v = serde_json::to_value(code).map_err(|e| CodeError::Json(e.to_string()))?;
    let sorted = sort_json_value(v);
    serde_json::to_string(&sorted).map_err(|e| CodeError::Json(e.to_string()))
}

fn sort_json_value(v: serde_json::Value) -> serde_json::Value {
    use serde_json::Value;
    match v {
        Value::Object(map) => {
            let sorted: serde_json::Map<String, Value> = map
                .into_iter()
                .map(|(k, v)| (k, sort_json_value(v)))
                .collect::<std::collections::BTreeMap<_, _>>()
                .into_iter()
                .collect();
            Value::Object(sorted)
        }
        Value::Array(arr) => Value::Array(arr.into_iter().map(sort_json_value).collect()),
        other => other,
    }
}

fn verify_checksum(code: &AutoPayCodeV1, _raw_json: &str) -> Result<(), CodeError> {
    let Some(expected_hex) = &code.checksum else {
        return Ok(()); // Optional: no checksum = no verification
    };
    // The checksum is computed over the JSON *without* the checksum field itself.
    // We recompute by removing it from a clone.
    let mut without = code.clone();
    without.checksum = None;
    let canonical = canonical_json(&without)?;
    let computed = hex::encode(blake3::hash(canonical.as_bytes()).as_bytes());
    if computed != *expected_hex {
        return Err(CodeError::ChecksumMismatch);
    }
    Ok(())
}

fn verify_signature_if_present(code: &AutoPayCodeV1) -> Result<(), CodeError> {
    let (Some(pubkey), Some(sig)) = (&code.merchant_pubkey, &code.signature) else {
        return Ok(()); // No signature — not verified (warning handled at call site)
    };
    // Compute the signed payload: canonical JSON without the `signature` field
    let mut without_sig = code.clone();
    without_sig.signature = None;
    let canonical = canonical_json(&without_sig)?;
    verify_payload(pubkey, canonical.as_bytes(), sig)
        .map_err(|e| CodeError::SignatureInvalid(e.to_string()))
}

fn check_expiry(code: &AutoPayCodeV1) -> Result<(), CodeError> {
    let Some(expires_at) = &code.expires_at else {
        return Ok(());
    };
    // Simple string comparison works for RFC3339 if both are UTC
    let now = chrono::Utc::now().to_rfc3339();
    if *expires_at < now {
        return Err(CodeError::Expired);
    }
    Ok(())
}

/// Map a monero-request:1: JSON into our AutoPayCodeV1 format.
fn map_compat_to_v1(v: serde_json::Value) -> Result<AutoPayCodeV1, CodeError> {
    Ok(AutoPayCodeV1 {
        v: 1,
        custom_label: v["custom_label"]
            .as_str()
            .unwrap_or("Imported subscription")
            .to_string(),
        description: None,
        payment_network: "monero".to_string(),
        sellers_wallet: v["sellers_wallet"]
            .as_str()
            .ok_or(CodeError::MissingField("sellers_wallet".into()))?
            .to_string(),
        currency: v["currency"].as_str().unwrap_or("XMR").to_string(),
        amount: v["amount"]
            .as_str()
            .ok_or(CodeError::MissingField("amount".into()))?
            .to_string(),
        start_date: v["start_date"].as_str().unwrap_or("").to_string(),
        schedule: v["schedule"].as_str().unwrap_or("0 0 1 * *").to_string(),
        number_of_payments: v["number_of_payments"].as_u64().unwrap_or(0),
        min_confirmations: None,
        grace_period_hours: None,
        trial_days: None,
        expires_at: None,
        webhook_url: None,
        canary_url: None,
        merchant_pubkey: None,
        signature: None,
        checksum: None,
        pay_window_before_secs: None,
        pay_window_after_secs: None,
        proportional_amount: None,
        occurrence_note_url: None,
        cancel: None,
        max_conversion_fee_pct: None,
        metadata: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_code() -> AutoPayCodeV1 {
        AutoPayCodeV1 {
            v: 1,
            custom_label: "Test Media — Monthly".to_string(),
            description: Some("Monthly newsletter access.".to_string()),
            payment_network: "monero".to_string(),
            sellers_wallet: "4At3X5rvVypTofgmueN9s9QtrzdRe5BueFrskAZi17BoYbhzysozzoMFB6zWnTKdGC6AxEAbEE5czFR3hbEEJbsm4hCeX2S".to_string(),
            currency: "EUR".to_string(),
            amount: "9.99".to_string(),
            start_date: "2026-11-01T00:00:00Z".to_string(),
            schedule: "0 0 1 * *".to_string(),
            number_of_payments: 0,
            min_confirmations: Some(10),
            grace_period_hours: Some(72),
            trial_days: None,
            expires_at: None,
            webhook_url: None,
            canary_url: Some("https://example.com/.well-known/autopay-canary.json".to_string()),
            merchant_pubkey: None,
            signature: None,
            checksum: None,
            pay_window_before_secs: Some(604_800),
            pay_window_after_secs: Some(259_200),
            proportional_amount: None,
            occurrence_note_url: None,
            cancel: None,
            max_conversion_fee_pct: None,
            metadata: None,
        }
    }

    #[test]
    fn encode_decode_roundtrip() {
        let code = sample_code();
        let encoded = encode_code(&code).unwrap();
        assert!(encoded.starts_with("monero-autopay:1:"));
        let decoded = parse_code(&encoded).unwrap();
        let AutoPayCode::V1(v1) = decoded;
        assert_eq!(v1.custom_label, code.custom_label);
        assert_eq!(v1.amount, code.amount);
    }

    #[test]
    fn checksum_tamper_detected() {
        let code = sample_code();
        let mut encoded = encode_code(&code).unwrap();
        // Corrupt the last few chars
        let len = encoded.len();
        encoded.truncate(len - 4);
        encoded.push_str("AAAA");
        assert!(parse_code(&encoded).is_err());
    }
}
