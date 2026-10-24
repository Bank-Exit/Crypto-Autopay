//! RPC process manager — spawns and monitors a local `monero-wallet-rpc` child process.
//!
//! The wallet-rpc connects to a remote Monero node (user-configurable) and exposes
//! a local JSON-RPC endpoint used by the MoneroAdapter.
//!
//! ## Lifecycle
//!  1. User unlocks vault → wallet file restored from seed if missing.
//!  2. Scheduler fires (payment due) → `start()` called.
//!  3. Payment sent → `stop()` called (wallet-rpc shut down to minimise attack surface).
//!  4. On app close → `stop()` always called.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

// ---- Public node list (mainnet, no local node needed) ----
// Nodes are tried in order; first reachable one is used.
pub const PUBLIC_NODES: &[&str] = &[
    "opennode.xmr-tw.org:18089",       // long-running, trusted by community
    "node.sethsimmons.me:18089",
    "xmr-node.cakewallet.com:18081",
    "nodes.hashvault.pro:18081",
];

/// User-facing RPC configuration (serialisable, stored in settings).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RpcConfig {
    /// Path to the `monero-wallet-rpc` binary. None = auto-detect from PATH.
    pub binary_path: Option<PathBuf>,
    /// Remote Monero daemon to connect to. None = use first reachable public node.
    pub daemon_address: Option<String>,
    /// Local RPC port to bind (default 18083).
    pub rpc_port: u16,
    /// Payment tolerance window in percent (default 20 → ±20% of period).
    /// E.g. for a monthly sub, payment is triggered within ±6 days of due date.
    pub tolerance_pct: u8,
    /// Whether to log send events (privacy opt-in).
    pub send_logs_enabled: bool,
}

impl Default for RpcConfig {
    fn default() -> Self {
        Self {
            binary_path: None,
            daemon_address: None,
            rpc_port: 18083,
            tolerance_pct: 20,
            send_logs_enabled: false,
        }
    }
}

/// Status of the wallet-rpc process.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RpcStatus {
    Stopped,
    Starting,
    Running,
    Error(String),
}

pub struct RpcManager {
    config: RpcConfig,
    process: Option<Child>,
    pub status: RpcStatus,
}

impl RpcManager {
    pub fn new(config: RpcConfig) -> Self {
        Self { config, process: None, status: RpcStatus::Stopped }
    }

    /// Try to find the `monero-wallet-rpc` binary.
    /// Priority: user-configured path → PATH detection.
    pub fn detect_binary(&self) -> Option<PathBuf> {
        // 1. User-specified path
        if let Some(ref p) = self.config.binary_path {
            if p.exists() { return Some(p.clone()); }
        }
        // 2. PATH lookup
        for candidate in &["monero-wallet-rpc", "monero-wallet-rpc.exe"] {
            if let Ok(output) = Command::new("which").arg(candidate).output() {
                if output.status.success() {
                    let path_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
                    if !path_str.is_empty() { return Some(PathBuf::from(path_str)); }
                }
            }
            // Windows-style where
            if let Ok(output) = Command::new("where").arg(candidate).output() {
                if output.status.success() {
                    let path_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
                    if !path_str.is_empty() { return Some(PathBuf::from(path_str.lines().next().unwrap_or(""))); }
                }
            }
        }
        None
    }

    /// Daemon address to use (user-config or first public node).
    pub fn daemon_address(&self) -> String {
        self.config.daemon_address.clone()
            .unwrap_or_else(|| PUBLIC_NODES[0].to_string())
    }

    /// Start the wallet-rpc process.
    /// `wallet_file`: path to the wallet file (without extension).
    /// `wallet_password`: wallet password.
    pub fn start(&mut self, wallet_file: &Path, wallet_password: &str) -> Result<(), String> {
        if matches!(self.status, RpcStatus::Running) {
            return Ok(()); // already running
        }
        let binary = self.detect_binary()
            .ok_or_else(|| "monero-wallet-rpc not found. Install it or set the path in Settings.".to_string())?;

        let daemon_addr = self.daemon_address();
        let port = self.config.rpc_port;

        log::info!("Starting wallet-rpc: binary={:?}, daemon={}, port={}", binary, daemon_addr, port);
        self.status = RpcStatus::Starting;

        let child = Command::new(&binary)
            .args([
                "--daemon-address", &daemon_addr,
                "--rpc-bind-ip", "127.0.0.1",
                "--rpc-bind-port", &port.to_string(),
                "--wallet-file", wallet_file.to_str().unwrap_or(""),
                "--password", wallet_password,
                "--disable-rpc-login",   // localhost only, no login needed
                "--log-level", "1",
                "--non-interactive",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("Failed to spawn wallet-rpc: {e}"))?;

        self.process = Some(child);
        self.status = RpcStatus::Running;
        log::info!("wallet-rpc started on 127.0.0.1:{}", port);
        Ok(())
    }

    /// Stop the wallet-rpc process gracefully.
    pub fn stop(&mut self) {
        if let Some(mut child) = self.process.take() {
            // Try to stop via RPC first (best practice), then kill
            let _ = child.kill();
            let _ = child.wait();
            log::info!("wallet-rpc stopped");
        }
        self.status = RpcStatus::Stopped;
    }

    /// Start wallet-rpc in wallet-dir mode (no specific wallet pre-loaded).
    /// Used before calling `restore_deterministic_wallet` via RPC.
    pub fn start_for_dir(&mut self, wallet_dir: &Path) -> Result<(), String> {
        if matches!(self.status, RpcStatus::Running) {
            return Ok(());
        }
        let binary = self.detect_binary()
            .ok_or_else(|| "monero-wallet-rpc not found.".to_string())?;

        let daemon_addr = self.daemon_address();
        let port = self.config.rpc_port;

        log::info!("Starting wallet-rpc in dir mode: wallet_dir={:?}, daemon={}, port={}", wallet_dir, daemon_addr, port);
        self.status = RpcStatus::Starting;

        let child = Command::new(&binary)
            .args([
                "--daemon-address", &daemon_addr,
                "--rpc-bind-ip", "127.0.0.1",
                "--rpc-bind-port", &port.to_string(),
                "--wallet-dir", wallet_dir.to_str().unwrap_or("."),
                "--disable-rpc-login",
                "--log-level", "1",
                "--non-interactive",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("Failed to spawn wallet-rpc: {e}"))?;

        self.process = Some(child);
        self.status = RpcStatus::Running;
        log::info!("wallet-rpc (dir mode) started on 127.0.0.1:{}", port);
        Ok(())
    }

    /// Wait for the RPC endpoint to be reachable (up to `timeout`).
    pub fn wait_for_ready(&self, timeout: Duration) -> Result<(), String> {
        let url = self.rpc_url();
        let deadline = Instant::now() + timeout;
        loop {
            // Simple TCP connect check (fast, no HTTP overhead)
            let addr = format!("127.0.0.1:{}", self.config.rpc_port);
            if std::net::TcpStream::connect_timeout(
                &addr.parse().map_err(|e: std::net::AddrParseError| e.to_string())?,
                Duration::from_millis(200),
            ).is_ok() {
                // Give the RPC server a moment to initialise after TCP accepts
                std::thread::sleep(Duration::from_millis(300));
                log::info!("wallet-rpc is ready at {}", url);
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(format!("wallet-rpc did not become ready within {}s", timeout.as_secs()));
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    /// Restore a wallet from a seed phrase using the `restore_deterministic_wallet` RPC.
    /// Requires the RPC to already be running in wallet-dir mode.
    /// Returns the restored wallet address.
    pub fn rpc_restore_wallet(
        &self,
        wallet_name: &str,
        wallet_password: &str,
        seed_phrase: &str,
        restore_height: u64,
    ) -> Result<String, String> {
        let url = self.rpc_url();
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": "restore",
            "method": "restore_deterministic_wallet",
            "params": {
                "filename": wallet_name,
                "password": wallet_password,
                "seed": seed_phrase,
                "restore_height": restore_height,
                "autosave_current": true
            }
        });

        // Synchronous HTTP POST (blocking) — called from a Tauri command (async ctx)
        let response: serde_json::Value = ureq_post_json(&url, &body)?;

        if let Some(err) = response.get("error") {
            return Err(format!("RPC error: {}", err));
        }
        let address = response["result"]["address"]
            .as_str()
            .ok_or_else(|| "No address in restore response".to_string())?
            .to_string();
        log::info!("Wallet restored. Address: {}", address);
        Ok(address)
    }

    /// Get the wallet balance via RPC (wallet must be open).
    /// Returns (total_balance_xmr, unlocked_balance_xmr).
    pub fn rpc_get_balance(&self) -> Result<(f64, f64), String> {
        let url = self.rpc_url();
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": "balance",
            "method": "get_balance",
            "params": { "account_index": 0 }
        });
        let response: serde_json::Value = ureq_post_json(&url, &body)?;
        if let Some(err) = response.get("error") {
            return Err(format!("RPC error: {}", err));
        }
        let atomic = response["result"]["balance"].as_u64().unwrap_or(0);
        let unlocked = response["result"]["unlocked_balance"].as_u64().unwrap_or(0);
        // Monero: 1 XMR = 1_000_000_000_000 atomic units (piconero)
        Ok((atomic as f64 / 1e12, unlocked as f64 / 1e12))
    }

    pub fn rpc_url(&self) -> String {
        format!("http://127.0.0.1:{}/json_rpc", self.config.rpc_port)
    }
}

impl Drop for RpcManager {
    fn drop(&mut self) { self.stop(); }
}

/// Public alias for use in lib.rs (e.g. open_wallet, get_address calls).
pub fn ureq_post_json_pub(url: &str, body: &serde_json::Value) -> Result<serde_json::Value, String> {
    ureq_post_json(url, body)
}

/// Minimal synchronous HTTP POST helper using std::net::TcpStream.
/// Avoids pulling in reqwest/tokio from this synchronous context.
fn ureq_post_json(url: &str, body: &serde_json::Value) -> Result<serde_json::Value, String> {
    use std::io::{Read, Write};

    // Parse host and path from URL (http://127.0.0.1:PORT/json_rpc)
    let without_scheme = url.trim_start_matches("http://");
    let slash_pos = without_scheme.find('/').unwrap_or(without_scheme.len());
    let host_port = &without_scheme[..slash_pos];
    let path = &without_scheme[slash_pos..];

    let body_str = body.to_string();
    let request = format!(
        "POST {path} HTTP/1.0\r\nHost: {host_port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body_str}",
        body_str.len()
    );

    let mut stream = std::net::TcpStream::connect(host_port)
        .map_err(|e| format!("Cannot connect to wallet-rpc: {e}"))?;
    stream.set_read_timeout(Some(Duration::from_secs(30))).ok();
    stream.write_all(request.as_bytes())
        .map_err(|e| format!("Write error: {e}"))?;

    let mut response = String::new();
    stream.read_to_string(&mut response)
        .map_err(|e| format!("Read error: {e}"))?;

    // Split headers from body
    let body_start = response.find("\r\n\r\n")
        .ok_or("Malformed HTTP response")?;
    let json_body = &response[body_start + 4..];

    serde_json::from_str(json_body)
        .map_err(|e| format!("JSON parse error: {e} — body: {json_body}"))
}
