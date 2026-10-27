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
    pub config: RpcConfig,
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

    /// Expose local RPC port for logging.
    pub fn config_port(&self) -> u16 { self.config.rpc_port }

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
                "--log-level", "0",      // suppress logs
                "--log-file", "/dev/null", // prevent 100MB+ log files in project dir
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
    /// Logs every step so failures are diagnosable.
    pub fn start_for_dir(&mut self, wallet_dir: &Path) -> Result<(), String> {
        if matches!(self.status, RpcStatus::Running) {
            log::info!("[RPC] start_for_dir: already running, skipping");
            return Ok(());
        }

        // --- 1. Binary detection ---
        let binary = match self.detect_binary() {
            Some(b) => {
                log::info!("[RPC] Binary found: {:?}", b);
                b
            }
            None => {
                let msg = "monero-wallet-rpc not found in PATH and no custom path configured. \
                           Install the Monero CLI tools or set the binary path in Settings.";
                log::error!("[RPC] {}", msg);
                return Err(msg.to_string());
            }
        };

        // --- 2. Port availability check ---
        let port = self.config.rpc_port;
        let addr_str = format!("127.0.0.1:{}", port);
        if std::net::TcpStream::connect_timeout(
            &addr_str.parse().unwrap(),
            Duration::from_millis(100),
        ).is_ok() {
            log::warn!("[RPC] Port {} already in use — killing stale process", port);
            // Try to kill whatever is on that port
            let _ = Command::new("fuser").args(["-k", &format!("{}/tcp", port)]).status();
            std::thread::sleep(Duration::from_millis(500));
        }

        // --- 3. Daemon address ---
        let daemon_addr = self.daemon_address();
        log::info!("[RPC] Daemon node: {}", daemon_addr);

        // --- 4. Wallet dir ---
        log::info!("[RPC] Wallet dir: {:?} (exists={})", wallet_dir, wallet_dir.exists());
        if !wallet_dir.exists() {
            if let Err(e) = std::fs::create_dir_all(wallet_dir) {
                log::error!("[RPC] Could not create wallet dir: {}", e);
                return Err(format!("Cannot create wallet dir: {e}"));
            }
        }

        // --- 5. Spawn ---
        let args = [
            "--daemon-address", &daemon_addr,
            "--rpc-bind-ip", "127.0.0.1",
            "--rpc-bind-port", &port.to_string(),
            "--wallet-dir", wallet_dir.to_str().unwrap_or("."),
            "--disable-rpc-login",
            "--log-level", "0",        // suppress logs
            "--log-file", "/dev/null", // prevent 100MB+ log files in project dir
            "--non-interactive",
        ];
        log::info!("[RPC] Spawning: {:?} {:?}", binary, args);
        self.status = RpcStatus::Starting;

        let child = Command::new(&binary)
            .args(&args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| {
                let msg = format!("Failed to spawn {:?}: {} — check binary permissions", binary, e);
                log::error!("[RPC] {}", msg);
                msg
            })?;

        log::info!("[RPC] Process spawned, PID={:?}", child.id());
        self.process = Some(child);
        self.status = RpcStatus::Running;
        Ok(())
    }

    /// Wait for the local RPC endpoint to accept TCP connections.
    pub fn wait_for_ready(&self, timeout: Duration) -> Result<(), String> {
        let addr_str = format!("127.0.0.1:{}", self.config.rpc_port);
        let deadline = Instant::now() + timeout;
        let mut attempts = 0u32;

        log::info!("[RPC] Waiting for local RPC on {} (timeout={}s)…", addr_str, timeout.as_secs());

        loop {
            attempts += 1;
            match std::net::TcpStream::connect_timeout(
                &addr_str.parse().map_err(|e: std::net::AddrParseError| e.to_string())?,
                Duration::from_millis(200),
            ) {
                Ok(_) => {
                    // Extra wait for the RPC server to fully initialise
                    std::thread::sleep(Duration::from_millis(400));
                    log::info!("[RPC] Ready after {} attempt(s)", attempts);
                    return Ok(());
                }
                Err(e) => {
                    if Instant::now() >= deadline {
                        let msg = format!(
                            "wallet-rpc not ready after {}s ({} attempts). Last TCP error: {}. \
                             Possible causes: binary crash, port conflict, missing lib.",
                            timeout.as_secs(), attempts, e
                        );
                        log::error!("[RPC] {}", msg);
                        return Err(msg);
                    }
                    if attempts % 10 == 0 {
                        log::warn!("[RPC] Still waiting… attempt {}, error: {}", attempts, e);
                    }
                    std::thread::sleep(Duration::from_millis(200));
                }
            }
        }
    }

    /// Restore a wallet from a seed phrase via the `restore_deterministic_wallet` RPC.
    pub fn rpc_restore_wallet(
        &self,
        wallet_name: &str,
        wallet_password: &str,
        seed_phrase: &str,
        restore_height: u64,
    ) -> Result<String, String> {
        let url = self.rpc_url();
        let word_count = seed_phrase.split_whitespace().count();
        log::info!("[RPC] restore_deterministic_wallet: name={}, restore_height={}, seed_words={}", wallet_name, restore_height, word_count);

        let body = serde_json::json!({
            "jsonrpc": "2.0", "id": "restore",
            "method": "restore_deterministic_wallet",
            "params": {
                "filename": wallet_name, "password": wallet_password,
                "seed": seed_phrase, "restore_height": restore_height,
                "autosave_current": true
            }
        });

        let response = ureq_post_json(&url, &body).map_err(|e| {
            let msg = format!("HTTP POST to {} failed: {}", url, e);
            log::error!("[RPC] {}", msg);
            msg
        })?;

        log::debug!("[RPC] restore response: {}", response);

        if let Some(err) = response.get("error") {
            let code = err.get("code").and_then(|c| c.as_i64()).unwrap_or(-1);
            let message = err.get("message").and_then(|m| m.as_str()).unwrap_or("unknown");
            let detail = if message.contains("word list") || message.contains("Electrum") {
                "Seed checksum invalid — seed was generated with an older/buggy app version. \
                 Reset your wallet and create a new one."
            } else if message.contains("already exists") {
                "Wallet file already exists. This is unexpected — it may have been partially created."
            } else {
                message
            };
            let msg = format!("RPC restore_deterministic_wallet failed (code {}): {} — {}", code, message, detail);
            log::error!("[RPC] {}", msg);
            return Err(msg);
        }

        let address = response["result"]["address"]
            .as_str()
            .ok_or_else(|| {
                let msg = format!("No 'address' in restore response. Full response: {}", response);
                log::error!("[RPC] {}", msg);
                msg
            })?
            .to_string();

        log::info!("[RPC] Wallet restored successfully. Address prefix: {}…", &address[..address.len().min(12)]);
        Ok(address)
    }

    /// Get the wallet balance via RPC (wallet must be open).
    /// Returns (total_balance_xmr, unlocked_balance_xmr).
    pub fn rpc_get_balance(&self) -> Result<(f64, f64), String> {
        let url = self.rpc_url();
        log::info!("[RPC] Calling get_balance…");
        let body = serde_json::json!({
            "jsonrpc": "2.0", "id": "balance",
            "method": "get_balance",
            "params": { "account_index": 0 }
        });
        let response = ureq_post_json(&url, &body).map_err(|e| {
            log::error!("[RPC] get_balance HTTP error: {}", e);
            e
        })?;
        if let Some(err) = response.get("error") {
            let msg = format!("get_balance RPC error: {}", err);
            log::error!("[RPC] {}", msg);
            return Err(msg);
        }
        let atomic   = response["result"]["balance"].as_u64().unwrap_or(0);
        let unlocked = response["result"]["unlocked_balance"].as_u64().unwrap_or(0);
        log::info!("[RPC] Balance: total={} pico, unlocked={} pico", atomic, unlocked);
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

/// Same as ureq_post_json_pub but with a custom read timeout (use for long-running RPC calls like `refresh`).
pub fn ureq_post_json_long(url: &str, body: &serde_json::Value, timeout_secs: u64) -> Result<serde_json::Value, String> {
    ureq_post_json_timeout(url, body, timeout_secs)
}

/// Query the Monero daemon directly for the current blockchain height.
/// Used to compute a sensible restore_height (recent) for new wallets.
/// Falls back to 0 on error — caller should handle gracefully.
pub fn get_daemon_block_height(daemon_addr: &str) -> u64 {
    let url = format!("http://{}/json_rpc", daemon_addr);
    log::info!("[RPC] Querying daemon block count from {}", daemon_addr);
    match ureq_post_json(&url, &serde_json::json!({
        "jsonrpc":"2.0","id":"h","method":"get_block_count"
    })) {
        Ok(r) => {
            let count = r["result"]["count"].as_u64().unwrap_or(0);
            log::info!("[RPC] Daemon block count: {}", count);
            count
        }
        Err(e) => {
            log::warn!("[RPC] Could not get daemon height: {} — restore_height will be 0", e);
            0
        }
    }
}

/// Minimal synchronous HTTP POST helper using std::net::TcpStream.
/// Avoids pulling in reqwest/tokio from this synchronous context.
fn ureq_post_json(url: &str, body: &serde_json::Value) -> Result<serde_json::Value, String> {
    ureq_post_json_timeout(url, body, 60)
}

/// Same as ureq_post_json but with a configurable read timeout.
fn ureq_post_json_timeout(url: &str, body: &serde_json::Value, timeout_secs: u64) -> Result<serde_json::Value, String> {
    use std::io::{Read, Write};

    let method_hint = body.get("method").and_then(|m| m.as_str()).unwrap_or("?");
    log::debug!("[RPC/HTTP] POST {} method={}", url, method_hint);

    // Parse host and path
    let without_scheme = url.trim_start_matches("http://");
    let slash_pos = without_scheme.find('/').unwrap_or(without_scheme.len());
    let host_port = &without_scheme[..slash_pos];
    let path = &without_scheme[slash_pos..];

    let body_str = body.to_string();
    let request = format!(
        "POST {path} HTTP/1.0\r\nHost: {host_port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body_str}",
        body_str.len()
    );

    let mut stream = std::net::TcpStream::connect(host_port).map_err(|e| {
        let msg = format!("TCP connect to {} failed: {} (is wallet-rpc running?)", host_port, e);
        log::error!("[RPC/HTTP] {}", msg);
        msg
    })?;
    stream.set_read_timeout(Some(Duration::from_secs(timeout_secs))).ok();

    stream.write_all(request.as_bytes()).map_err(|e| {
        let msg = format!("Write to {} failed: {}", host_port, e);
        log::error!("[RPC/HTTP] {}", msg);
        msg
    })?;

    let mut response = String::new();
    stream.read_to_string(&mut response).map_err(|e| {
        let msg = format!("Read from {} failed: {}", host_port, e);
        log::error!("[RPC/HTTP] {}", msg);
        msg
    })?;

    let body_start = response.find("\r\n\r\n").ok_or_else(|| {
        let msg = format!("Malformed HTTP response (no header/body separator). Raw: {}", &response[..response.len().min(200)]);
        log::error!("[RPC/HTTP] {}", msg);
        msg
    })?;
    let json_body = &response[body_start + 4..];

    log::debug!("[RPC/HTTP] Response body ({}B): {}", json_body.len(), &json_body[..json_body.len().min(300)]);

    serde_json::from_str(json_body).map_err(|e| {
        let msg = format!("JSON parse error: {} — body: {}", e, &json_body[..json_body.len().min(300)]);
        log::error!("[RPC/HTTP] {}", msg);
        msg
    })
}
