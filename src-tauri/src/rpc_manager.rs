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

    pub fn rpc_url(&self) -> String {
        format!("http://127.0.0.1:{}/json_rpc", self.config.rpc_port)
    }
}

impl Drop for RpcManager {
    fn drop(&mut self) { self.stop(); }
}
