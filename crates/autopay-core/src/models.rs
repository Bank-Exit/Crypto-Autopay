use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// All possible states of a subscription.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubscriptionStatus {
    /// Active — payments are scheduled normally.
    Active,
    /// Paused by user — no payments sent, can be resumed.
    Paused,
    /// Past due — last payment failed, within grace period.
    PastDue,
    /// Cancelled by user — no further payments.
    Cancelled,
    /// Expired — grace period exhausted or canary failed.
    Expired,
    /// Pending first payment (newly added, not yet paid).
    Pending,
    /// Suspended by AutoPay due to canary failure or security alert.
    Suspended,
}

impl std::fmt::Display for SubscriptionStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", serde_json::to_value(self).unwrap().as_str().unwrap_or("unknown"))
    }
}

/// A recurring subscription managed by AutoPay.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Subscription {
    /// UUID v4, primary key in the local DB.
    pub id: String,

    /// Wallet ID this subscription uses for sending.
    pub wallet_id: String,

    /// Display name from the monero-autopay:1: code.
    pub label: String,

    /// Optional long description.
    pub description: Option<String>,

    /// Recipient address.
    pub destination_address: String,

    /// Payment network (matches adapter network_id): "monero", "bitcoin".
    pub payment_network: String,

    /// Amount string as specified in the code ("9.99").
    pub amount: String,

    /// Currency of the amount ("XMR", "EUR", "USD").
    pub currency: String,

    /// Cron expression for scheduling.
    pub schedule: String,

    /// 0 = infinite, N = fixed count, 1 = one-time.
    pub number_of_payments: u64,

    /// How many payments have been successfully sent.
    pub payments_sent: u64,

    /// RFC3339 — when the next payment should fire.
    pub next_payment_at: Option<DateTime<Utc>>,

    /// Current status.
    pub status: SubscriptionStatus,

    /// Grace period in hours before marking expired.
    pub grace_period_hours: u32,

    /// Canary URL (optional).
    pub canary_url: Option<String>,

    /// Creator's Ed25519 pubkey for canary verification.
    pub merchant_pubkey: Option<String>,

    /// Last canary check result (human-readable, for UI display).
    pub last_canary_status: Option<String>,

    /// When the canary was last successfully verified.
    pub canary_last_ok: Option<DateTime<Utc>>,

    /// Webhook URL to notify on payment.
    pub webhook_url: Option<String>,

    /// When this subscription was added.
    pub created_at: DateTime<Utc>,

    /// When this subscription was last modified.
    pub updated_at: DateTime<Utc>,

    /// The raw monero-autopay:1: code string (for display/export).
    pub raw_code: Option<String>,

    /// User notes.
    pub notes: Option<String>,
}

impl Subscription {
    pub fn is_active(&self) -> bool {
        matches!(self.status, SubscriptionStatus::Active | SubscriptionStatus::PastDue)
    }

    pub fn is_infinite(&self) -> bool {
        self.number_of_payments == 0
    }

    pub fn payments_remaining(&self) -> Option<u64> {
        if self.is_infinite() {
            None
        } else {
            Some(self.number_of_payments.saturating_sub(self.payments_sent))
        }
    }
}

/// A record of a single payment attempt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaymentRecord {
    pub id: String,
    pub subscription_id: String,
    pub wallet_id: String,
    pub destination_address: String,
    pub amount_atomic: u64,
    pub currency: String,
    pub tx_id: Option<String>,
    pub status: PaymentStatus,
    pub error_message: Option<String>,
    pub initiated_at: DateTime<Utc>,
    pub confirmed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaymentStatus {
    Pending,
    Sent,
    Confirmed,
    Failed,
    Retrying,
}

/// Occurrence note fetched from the creator's `occurrence_note_url` before each payment.
/// If `new_amount` is present and differs from the subscription amount,
/// AutoPay must pause and ask the payer for manual confirmation.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OccurrenceNote {
    /// Human-readable message from the creator (e.g. "Price increase starting next month").
    pub message: String,
    /// Optional new amount for this period (triggers manual confirmation).
    pub new_amount: Option<String>,
    /// Optional new currency for this period.
    pub new_currency: Option<String>,
    /// RFC3339 — when this note was published.
    pub published_at: Option<String>,
    /// Ed25519 signature over the canonical JSON (without `signature`).
    pub pubkey: Option<String>,
    pub signature: Option<String>,
}

/// Result of fetching an occurrence note.
#[derive(Debug, Clone)]
pub enum OccurrenceNoteResult {
    /// Note fetched, no price change — proceed automatically.
    NoChange(OccurrenceNote),
    /// Note fetched, price change detected — requires manual confirmation.
    PriceChangeRequiresConfirmation(OccurrenceNote),
    /// URL unreachable — proceed with original amount, log warning.
    Unreachable,
    /// No URL configured — proceed normally.
    NotConfigured,
}
