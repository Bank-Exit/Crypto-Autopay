/// PaymentAdapter trait — network-agnostic interface for sending payments.
/// V1: MoneroAdapter. V3: BitcoinAdapter, LightningAdapter.
use async_trait::async_trait;
use thiserror::Error;

#[derive(Debug, Clone)]
pub struct TxId(pub String);

#[derive(Debug, Clone)]
pub struct Balance {
    /// Total balance in atomic units (piconero for XMR, satoshi for BTC).
    pub total: u64,
    /// Spendable (unlocked / confirmed) balance in atomic units.
    pub unlocked: u64,
    /// Human-readable ticker: "XMR", "BTC", etc.
    pub ticker: String,
}

#[derive(Debug, Error)]
pub enum AdapterError {
    #[error("RPC connection failed: {0}")]
    Connection(String),
    #[error("RPC error: {0}")]
    Rpc(String),
    #[error("Insufficient funds (need {needed}, have {available})")]
    InsufficientFunds { needed: u64, available: u64 },
    #[error("Invalid address: {0}")]
    InvalidAddress(String),
    #[error("Adapter not ready (wallet not synced or locked)")]
    NotReady,
    #[error("Unknown error: {0}")]
    Other(String),
}

#[async_trait]
pub trait PaymentAdapter: Send + Sync {
    /// Unique identifier for this network, e.g. "monero", "bitcoin".
    fn network_id(&self) -> &str;

    /// Human-readable display name, e.g. "Monero (XMR)".
    fn display_name(&self) -> &str;

    /// Whether the adapter is ready to send payments (synced, unlocked).
    async fn is_ready(&self) -> bool;

    /// Current balance (total and unlocked) in atomic units.
    async fn balance(&self) -> Result<Balance, AdapterError>;

    /// Generate a receive address for this wallet.
    async fn receive_address(&self) -> Result<String, AdapterError>;

    /// Send `amount_atomic` atomic units to `destination`.
    /// Returns the transaction ID on success.
    async fn send(
        &self,
        destination: &str,
        amount_atomic: u64,
        memo: Option<&str>,
    ) -> Result<TxId, AdapterError>;

    /// Convert a human-readable amount (e.g. "9.99" XMR) to atomic units.
    fn to_atomic(&self, amount: &str) -> Result<u64, AdapterError>;

    /// Convert atomic units back to a human-readable string.
    fn from_atomic(&self, amount_atomic: u64) -> String;
}
