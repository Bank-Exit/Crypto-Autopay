/// Monero wallet-rpc adapter.
/// Communicates with a local monero-wallet-rpc process via JSON-RPC over HTTP.
/// The wallet process is spawned and managed by the Tauri app layer.
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::{debug, error, warn};

use super::adapter::{AdapterError, Balance, PaymentAdapter, TxId};

const PICONERO_PER_XMR: u64 = 1_000_000_000_000;

/// Monero hot-wallet safety cap: warn if balance exceeds this (in piconero).
const HOT_WALLET_WARNING_XMR: f64 = 1.5;
const HOT_WALLET_MAX_XMR: f64 = 2.0;

pub struct MoneroAdapter {
    rpc_url: String,
    client: Client,
    rpc_user: Option<String>,
    rpc_password: Option<String>,
}

impl MoneroAdapter {
    /// Create a new adapter pointing at a local wallet-rpc instance.
    /// `rpc_url` example: "http://127.0.0.1:18083/json_rpc"
    pub fn new(rpc_url: impl Into<String>) -> Self {
        Self {
            rpc_url: rpc_url.into(),
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .expect("failed to build HTTP client"),
            rpc_user: None,
            rpc_password: None,
        }
    }

    pub fn with_auth(mut self, user: impl Into<String>, password: impl Into<String>) -> Self {
        self.rpc_user = Some(user.into());
        self.rpc_password = Some(password.into());
        self
    }

    async fn rpc_call(&self, method: &str, params: Value) -> Result<Value, AdapterError> {
        let body = json!({
            "jsonrpc": "2.0",
            "id": "autopay",
            "method": method,
            "params": params,
        });

        debug!("Monero RPC → {method}");

        let mut req = self.client.post(&self.rpc_url).json(&body);
        if let (Some(user), Some(pass)) = (&self.rpc_user, &self.rpc_password) {
            req = req.basic_auth(user, Some(pass));
        }

        let resp = req
            .send()
            .await
            .map_err(|e| AdapterError::Connection(e.to_string()))?;

        let json: Value = resp
            .json()
            .await
            .map_err(|e| AdapterError::Rpc(format!("JSON parse: {e}")))?;

        if let Some(err) = json.get("error") {
            return Err(AdapterError::Rpc(err.to_string()));
        }

        Ok(json["result"].clone())
    }

    /// Check if wallet balance exceeds safety cap. Returns warning level.
    pub async fn check_hot_wallet_limit(&self) -> HotWalletStatus {
        match self.balance().await {
            Ok(bal) => {
                let xmr = bal.unlocked as f64 / PICONERO_PER_XMR as f64;
                if xmr >= HOT_WALLET_MAX_XMR {
                    HotWalletStatus::Critical(xmr)
                } else if xmr >= HOT_WALLET_WARNING_XMR {
                    HotWalletStatus::Warning(xmr)
                } else {
                    HotWalletStatus::Ok(xmr)
                }
            }
            Err(e) => {
                warn!("Could not check hot wallet limit: {e}");
                HotWalletStatus::Unknown
            }
        }
    }
}

#[derive(Debug)]
pub enum HotWalletStatus {
    Ok(f64),
    Warning(f64),
    Critical(f64),
    Unknown,
}

#[async_trait]
impl PaymentAdapter for MoneroAdapter {
    fn network_id(&self) -> &str {
        "monero"
    }

    fn display_name(&self) -> &str {
        "Monero (XMR)"
    }

    async fn is_ready(&self) -> bool {
        // Try a lightweight RPC call to check connectivity
        self.rpc_call("get_version", json!({})).await.is_ok()
    }

    async fn balance(&self) -> Result<Balance, AdapterError> {
        let result = self.rpc_call("get_balance", json!({ "account_index": 0 })).await?;
        let total = result["balance"].as_u64().unwrap_or(0);
        let unlocked = result["unlocked_balance"].as_u64().unwrap_or(0);
        Ok(Balance {
            total,
            unlocked,
            ticker: "XMR".to_string(),
        })
    }

    async fn receive_address(&self) -> Result<String, AdapterError> {
        let result = self
            .rpc_call("get_address", json!({ "account_index": 0 }))
            .await?;
        result["address"]
            .as_str()
            .map(str::to_string)
            .ok_or(AdapterError::Rpc("no address in response".into()))
    }

    async fn send(
        &self,
        destination: &str,
        amount_atomic: u64,
        memo: Option<&str>,
    ) -> Result<TxId, AdapterError> {
        // Validate destination address length as a basic sanity check
        if destination.len() < 95 {
            return Err(AdapterError::InvalidAddress(destination.to_string()));
        }

        // Check balance before attempting
        let bal = self.balance().await?;
        if bal.unlocked < amount_atomic {
            return Err(AdapterError::InsufficientFunds {
                needed: amount_atomic,
                available: bal.unlocked,
            });
        }

        let mut params = json!({
            "destinations": [{
                "address": destination,
                "amount": amount_atomic,
            }],
            "account_index": 0,
            "priority": 1,
            "get_tx_key": true,
        });

        if let Some(m) = memo {
            params["payment_id"] = Value::Null; // Use subaddress, not payment_id
            // Memo is stored locally only — Monero doesn't support tx memos on-chain
            let _ = m; // will be stored in local DB by the scheduler
        }

        let result = self.rpc_call("transfer", params).await?;
        let tx_hash = result["tx_hash"]
            .as_str()
            .ok_or(AdapterError::Rpc("no tx_hash in response".into()))?
            .to_string();

        Ok(TxId(tx_hash))
    }

    fn to_atomic(&self, amount: &str) -> Result<u64, AdapterError> {
        // Parse decimal XMR amount to piconero (12 decimal places)
        let parsed: f64 = amount
            .parse()
            .map_err(|_| AdapterError::Other(format!("invalid amount: {amount}")))?;
        if parsed <= 0.0 {
            return Err(AdapterError::Other("amount must be positive".into()));
        }
        Ok((parsed * PICONERO_PER_XMR as f64).round() as u64)
    }

    fn from_atomic(&self, amount_atomic: u64) -> String {
        format!("{:.12}", amount_atomic as f64 / PICONERO_PER_XMR as f64)
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_string()
    }
}
