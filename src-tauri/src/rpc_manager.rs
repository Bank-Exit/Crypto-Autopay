//! RPC process manager — spawns and monitors a local `monero-wallet-rpc` child process.
//!
//! The wallet-rpc connects to a remote Monero node (user-configurable) and exposes
//! a local JSON-RPC endpoint used by the MoneroAdapter.
//!
//! ## Lifecycle
//!  1. User unlocks vault → wallet file restored from seed if missing.
//!  2. First refresh / payment → `start_for_dir()` then `open_wallet` over JSON-RPC
//!     (the password is never passed on the command line). The process then stays up for
//!     the whole session: a fresh wallet-rpc has to download the whole mempool before its
//!     first refresh completes, which costs from seconds to minutes on a remote node.
//!  3. Every process gets random RPC credentials (HTTP digest auth), so other local
//!     programs cannot drive the wallet. They go through a short-lived 0600 config file,
//!     never the command line (visible in the process list).
//!  4. Full lock, node change or app close → `stop()` (wallet saved first).

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Build a command that never flashes a console window on Windows
/// (wallet-rpc, `where` and `taskkill` are console programs).
fn command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    #[allow(unused_mut)]
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

// ---- Public node list (mainnet, no local node needed) ----
// Nodes are tried in order; first reachable one is used.
// Order = preference; nodes found slow at runtime are skipped for a while (see lib.rs).
pub const PUBLIC_NODES: &[&str] = &[
    "xmr-node.cakewallet.com:18081",
    "nodes.hashvault.pro:18081",
    "opennode.xmr-tw.org:18089",       // round-robin DNS: some backends are very slow
    "node.sethsimmons.me:18089",
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

impl RpcConfig {
    /// Node pinned by the user, if any (empty string = automatic selection).
    pub fn manual_node(&self) -> Option<String> {
        self.daemon_address.as_ref()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }
}

/// Pick the daemon node for the next wallet-rpc session and return the config to use
/// along with the node's current height (0 if no node answered).
///
/// A pinned node is always used. In automatic mode, the last working node is tried first,
/// then the public list in order, so a node that starts refusing or rate-limiting us
/// is skipped transparently.
pub fn select_live_node(
    mut config: RpcConfig,
    last_working: Option<&str>,
    avoid: &[String],
) -> (RpcConfig, String, u64) {
    if let Some(node) = config.manual_node() {
        let height = probe_daemon_height(&node, Duration::from_secs(8)).unwrap_or(0);
        if height == 0 {
            log::warn!("[RPC] Pinned node {} did not answer", node);
        }
        config.daemon_address = Some(node.clone());
        return (config, node, height);
    }

    let mut candidates: Vec<String> = Vec::new();
    if let Some(n) = last_working { candidates.push(n.to_string()); }
    for n in PUBLIC_NODES {
        if !candidates.iter().any(|c| c == n) { candidates.push(n.to_string()); }
    }
    // Nodes recently found slow go last (still used if nothing else answers).
    candidates.sort_by_key(|c| avoid.contains(c));

    for node in &candidates {
        match probe_daemon_height(node, Duration::from_secs(6)) {
            Some(h) if h > 0 => {
                log::debug!("[RPC] Using node {} (height {})", node, h);
                config.daemon_address = Some(node.clone());
                return (config, node.clone(), h);
            }
            _ => log::warn!("[RPC] Node {} unavailable, trying next", node),
        }
    }

    let fallback = candidates.first().cloned().unwrap_or_else(|| PUBLIC_NODES[0].to_string());
    log::warn!("[RPC] No node answered — falling back to {}", fallback);
    config.daemon_address = Some(fallback.clone());
    (config, fallback, 0)
}

/// Ask a daemon for its height with a bounded total wait. None if unreachable, blocked or invalid.
pub fn probe_daemon_height(daemon_addr: &str, timeout: Duration) -> Option<u64> {
    let url = format!("http://{}/json_rpc", daemon_addr);
    let body = serde_json::json!({ "jsonrpc":"2.0","id":"h","method":"get_block_count" });
    match http_post_json(&url, &body, timeout, timeout) {
        Ok(r) => r["result"]["count"].as_u64().filter(|h| *h > 0),
        Err(e) => {
            log::debug!("[RPC] Probe {} failed: {}", daemon_addr, e);
            None
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
    /// PID file of the wallet-rpc spawned by this app (in the wallet dir).
    pid_file: Option<PathBuf>,
    /// Config file carrying the RPC credentials, deleted once wallet-rpc has read it.
    conf_file: Option<PathBuf>,
}

const PID_FILE_NAME: &str = "wallet-rpc.pid";
const CONF_FILE_NAME: &str = "wallet-rpc.conf";

/// RPC credentials of the wallet-rpc processes started by this app, by local port.
static LOCAL_AUTH: std::sync::Mutex<Vec<(u16, String, String)>> = std::sync::Mutex::new(Vec::new());

fn local_auth(port: u16) -> Option<(String, String)> {
    LOCAL_AUTH.lock().ok()?.iter().find(|(p, _, _)| *p == port).map(|(_, u, pw)| (u.clone(), pw.clone()))
}

fn set_local_auth(port: u16, creds: Option<(String, String)>) {
    if let Ok(mut v) = LOCAL_AUTH.lock() {
        v.retain(|(p, _, _)| *p != port);
        if let Some((u, pw)) = creds { v.push((port, u, pw)); }
    }
}

fn random_hex(bytes: usize) -> String {
    use rand::RngCore;
    let mut b = vec![0u8; bytes];
    rand::thread_rng().fill_bytes(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Write a file readable by the owner only.
fn write_private(path: &Path, content: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts.open(path)?.write_all(content.as_bytes())
}

fn is_wallet_rpc_cmd(cmd: &str) -> bool {
    cmd.contains("autopay-wallet-rpc") || cmd.contains("monero-wallet-rpc")
}

/// True if `pid` is a wallet-rpc started by this app (guards against PID reuse and
/// never matches another program's wallet-rpc).
fn is_our_wallet_rpc(pid: u32, wallet_dir: &std::path::Path) -> bool {
    #[cfg(target_os = "linux")]
    {
        std::fs::read(format!("/proc/{}/cmdline", pid))
            .map(|c| {
                let cmd = String::from_utf8_lossy(&c);
                is_wallet_rpc_cmd(&cmd) && cmd.contains(&*wallet_dir.to_string_lossy())
            })
            .unwrap_or(false)
    }
    #[cfg(target_os = "macos")]
    {
        command("ps").args(["-p", &pid.to_string(), "-o", "command="]).output()
            .map(|o| {
                let cmd = String::from_utf8_lossy(&o.stdout);
                is_wallet_rpc_cmd(&cmd) && cmd.contains(&*wallet_dir.to_string_lossy())
            })
            .unwrap_or(false)
    }
    #[cfg(windows)]
    {
        // tasklist exposes the image name only; the PID file already ties it to this app.
        let _ = wallet_dir;
        command("tasklist").args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"]).output()
            .map(|o| String::from_utf8_lossy(&o.stdout).contains("autopay-wallet-rpc"))
            .unwrap_or(false)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        let _ = (pid, wallet_dir);
        false
    }
}

fn port_in_use(port: u16) -> bool {
    std::net::TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
        Duration::from_millis(100),
    ).is_ok()
}

/// A free local port picked by the OS, used when the configured one is taken.
fn free_local_port() -> Option<u16> {
    std::net::TcpListener::bind("127.0.0.1:0").ok()?.local_addr().ok().map(|a| a.port())
}

/// wallet-rpc left behind by a previous run of this app: from the PID file, or on Linux
/// (runs older than the PID file) by scanning processes for our wallet dir.
fn find_stale_wallet_rpc(wallet_dir: &std::path::Path) -> Option<u32> {
    let from_file = std::fs::read_to_string(wallet_dir.join(PID_FILE_NAME)).ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
        .filter(|pid| is_our_wallet_rpc(*pid, wallet_dir));
    if from_file.is_some() { return from_file; }
    #[cfg(target_os = "linux")]
    {
        let me = std::process::id();
        std::fs::read_dir("/proc").ok()?.flatten()
            .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
            .find(|pid| *pid != me && is_our_wallet_rpc(*pid, wallet_dir))
    }
    #[cfg(not(target_os = "linux"))]
    None
}

/// Seconds wallet-rpc gets to save the wallet and exit after SIGTERM.
const TERM_GRACE_SECS: u64 = 15;

/// Stop a wallet-rpc process. On Unix, SIGTERM makes wallet-rpc abort a running refresh
/// and save the wallet before exiting; SIGKILL is only the fallback.
fn terminate_pid(pid: u32) {
    #[cfg(unix)]
    {
        let alive = || unsafe { libc::kill(pid as libc::pid_t, 0) == 0 };
        unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM); }
        let deadline = Instant::now() + Duration::from_secs(TERM_GRACE_SECS);
        while alive() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(200));
        }
        if alive() {
            log::warn!("[RPC] wallet-rpc (PID {}) ignored SIGTERM — killing it", pid);
            unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL); }
        }
    }
    #[cfg(windows)]
    {
        let _ = command("taskkill").args(["/PID", &pid.to_string(), "/F"]).status();
    }
}

/// Stop any wallet-rpc this app left running on `wallet_dir` (called when the app exits:
/// a session still syncing in a background thread would otherwise outlive the app).
pub fn shutdown_leftover(wallet_dir: &Path) {
    if let Some(pid) = find_stale_wallet_rpc(wallet_dir) {
        log::info!("[RPC] Stopping wallet-rpc before exit (wallet is saved)");
        terminate_pid(pid);
        let _ = std::fs::remove_file(wallet_dir.join(PID_FILE_NAME));
    }
}

impl RpcManager {
    pub fn new(config: RpcConfig) -> Self {
        Self { config, process: None, status: RpcStatus::Stopped, pid_file: None, conf_file: None }
    }

    /// Try to find the `monero-wallet-rpc` binary.
    /// Priority: user-configured path → bundled sidecar → PATH detection.
    pub fn detect_binary(&self) -> Option<PathBuf> {
        // 1. User-specified path
        if let Some(ref p) = self.config.binary_path {
            if p.exists() { return Some(p.clone()); }
        }
        // 2. Sidecar shipped with the installer (pinned, hash-checked version), placed
        //    next to the app executable by Tauri on every platform and in `tauri dev`.
        if let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(|p| p.to_path_buf())) {
            let sidecar = dir.join(if cfg!(windows) { "autopay-wallet-rpc.exe" } else { "autopay-wallet-rpc" });
            if sidecar.is_file() { return Some(sidecar); }
        }
        // 3. PATH lookup
        for candidate in &["monero-wallet-rpc", "monero-wallet-rpc.exe"] {
            #[cfg(not(windows))]
            if let Ok(output) = command("which").arg(candidate).output() {
                if output.status.success() {
                    let path_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
                    if !path_str.is_empty() { return Some(PathBuf::from(path_str)); }
                }
            }
            #[cfg(windows)]
            if let Ok(output) = command("where").arg(candidate).output() {
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
        self.config.manual_node()
            .unwrap_or_else(|| PUBLIC_NODES[0].to_string())
    }

    /// Expose local RPC port for logging.
    pub fn config_port(&self) -> u16 { self.config.rpc_port }

    /// Stop the wallet-rpc process.
    /// The open wallet is saved first: killing it unsaved loses the sync progress, and the
    /// next session has to fetch again every block since the last save.
    pub fn stop(&mut self) {
        if let Some(mut child) = self.process.take() {
            if matches!(child.try_wait(), Ok(None)) {
                // An idle wallet-rpc may ignore SIGTERM for seconds: save explicitly, then kill.
                // SIGTERM (which also saves) is the fallback when the RPC does not answer.
                let store = serde_json::json!({ "jsonrpc":"2.0","id":"store","method":"store","params":{} });
                match ureq_post_json_timeout(&self.rpc_url(), &store, 30) {
                    Ok(r) if r.get("error").is_none() => log::debug!("[RPC] Wallet saved"),
                    Ok(r) => log::debug!("[RPC] store skipped: {}", r["error"]["message"].as_str().unwrap_or("?")),
                    Err(e) => {
                        log::warn!("[RPC] Could not save the wallet before stopping: {}", e);
                        terminate_pid(child.id());
                    }
                }
            }
            let _ = child.kill();
            let _ = child.wait();
            log::debug!("[RPC] wallet-rpc stopped");
            if let Some(f) = self.pid_file.take() { let _ = std::fs::remove_file(f); }
        }
        if let Some(f) = self.conf_file.take() { let _ = std::fs::remove_file(f); }
        set_local_auth(self.config.rpc_port, None);
        self.status = RpcStatus::Stopped;
    }

    /// True while the spawned wallet-rpc process is alive.
    pub fn is_alive(&mut self) -> bool {
        self.process.as_mut().is_some_and(|c| matches!(c.try_wait(), Ok(None)))
    }

    /// Start wallet-rpc in wallet-dir mode (no specific wallet pre-loaded).
    /// Logs every step so failures are diagnosable.
    pub fn start_for_dir(&mut self, wallet_dir: &Path) -> Result<(), String> {
        if matches!(self.status, RpcStatus::Running) {
            log::debug!("[RPC] start_for_dir: already running, skipping");
            return Ok(());
        }

        // --- 1. Binary detection ---
        let binary = match self.detect_binary() {
            Some(b) => {
                log::debug!("[RPC] Binary found: {:?}", b);
                b
            }
            None => {
                let msg = "RPC_BINARY_MISSING: monero-wallet-rpc not found (bundled copy missing, \
                           not in PATH, no custom path configured).";
                log::error!("[RPC] {}", msg);
                return Err(msg.to_string());
            }
        };

        // --- 2. Port availability check ---
        // Only a wallet-rpc left behind by this app (crash, killed dev build) may be stopped;
        // a port held by any other program is left alone and another port is used instead.
        if port_in_use(self.config.rpc_port) {
            if let Some(pid) = find_stale_wallet_rpc(wallet_dir) {
                log::warn!("[RPC] Port {} held by a stale wallet-rpc from this app (PID {}) — stopping it",
                    self.config.rpc_port, pid);
                terminate_pid(pid);
                let _ = std::fs::remove_file(wallet_dir.join(PID_FILE_NAME));
                let deadline = Instant::now() + Duration::from_secs(3);
                while port_in_use(self.config.rpc_port) && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(200));
                }
            }
        }
        if port_in_use(self.config.rpc_port) {
            let fallback = free_local_port().ok_or_else(|| {
                let msg = format!("Port {} is in use and no free local port is available.", self.config.rpc_port);
                log::error!("[RPC] {}", msg);
                msg
            })?;
            log::warn!("[RPC] Port {} is used by another program — using free port {} for this session",
                self.config.rpc_port, fallback);
            self.config.rpc_port = fallback;
        }
        let port = self.config.rpc_port;

        // --- 3. Daemon address ---
        let daemon_addr = self.daemon_address();
        log::debug!("[RPC] Daemon node: {}", daemon_addr);

        // --- 4. Wallet dir ---
        log::debug!("[RPC] Wallet dir: {:?} (exists={})", wallet_dir, wallet_dir.exists());
        if !wallet_dir.exists() {
            if let Err(e) = std::fs::create_dir_all(wallet_dir) {
                log::error!("[RPC] Could not create wallet dir: {}", e);
                return Err(format!("Cannot create wallet dir: {e}"));
            }
        }

        // --- 5. Credentials ---
        let creds = (random_hex(8), random_hex(24));
        let conf_path = wallet_dir.join(CONF_FILE_NAME);
        write_private(&conf_path, &format!("rpc-login={}:{}\n", creds.0, creds.1)).map_err(|e| {
            let msg = format!("Cannot write wallet-rpc config: {e}");
            log::error!("[RPC] {}", msg);
            msg
        })?;
        self.conf_file = Some(conf_path.clone());

        // --- 6. Spawn ---
        let args = [
            "--daemon-address", &daemon_addr,
            "--rpc-bind-ip", "127.0.0.1",
            "--rpc-bind-port", &port.to_string(),
            "--wallet-dir", wallet_dir.to_str().unwrap_or("."),
            "--config-file", conf_path.to_str().unwrap_or(CONF_FILE_NAME),
            "--log-level", "0",        // suppress logs
            "--log-file", if cfg!(windows) { "NUL" } else { "/dev/null" }, // no 100MB+ log files
            "--non-interactive",
        ];
        log::debug!("[RPC] Spawning: {:?} {:?}", binary, args);
        self.status = RpcStatus::Starting;

        let child = command(&binary)
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| {
                if let Some(f) = self.conf_file.take() { let _ = std::fs::remove_file(f); }
                let msg = format!("Failed to spawn {:?}: {} — check binary permissions", binary, e);
                log::error!("[RPC] {}", msg);
                msg
            })?;
        set_local_auth(port, Some(creds));

        log::debug!("[RPC] Process spawned, PID={:?}", child.id());
        let pid_file = wallet_dir.join(PID_FILE_NAME);
        if std::fs::write(&pid_file, child.id().to_string()).is_ok() {
            self.pid_file = Some(pid_file);
        }
        self.process = Some(child);
        self.status = RpcStatus::Running;
        Ok(())
    }

    /// Wait for the local RPC endpoint to accept TCP connections.
    pub fn wait_for_ready(&self, timeout: Duration) -> Result<(), String> {
        let addr_str = format!("127.0.0.1:{}", self.config.rpc_port);
        let deadline = Instant::now() + timeout;
        let mut attempts = 0u32;

        log::debug!("[RPC] Waiting for local RPC on {} (timeout={}s)…", addr_str, timeout.as_secs());

        loop {
            attempts += 1;
            match std::net::TcpStream::connect_timeout(
                &addr_str.parse().map_err(|e: std::net::AddrParseError| e.to_string())?,
                Duration::from_millis(200),
            ) {
                Ok(_) => {
                    // Extra wait for the RPC server to fully initialise
                    std::thread::sleep(Duration::from_millis(400));
                    if let Some(f) = &self.conf_file { let _ = std::fs::remove_file(f); }
                    log::debug!("[RPC] Ready after {} attempt(s)", attempts);
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
                    if attempts.is_multiple_of(10) {
                        log::debug!("[RPC] Still waiting… attempt {}, error: {}", attempts, e);
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

        log::info!("[RPC] Wallet restored successfully");
        Ok(address)
    }

    /// Get the wallet balance via RPC (wallet must be open).
    /// Returns (total_balance_xmr, unlocked_balance_xmr).
    pub fn rpc_get_balance(&self) -> Result<(f64, f64), String> {
        let url = self.rpc_url();
        log::debug!("[RPC] Calling get_balance…");
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
        log::debug!("[RPC] get_balance OK");
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
    log::debug!("[RPC] Querying daemon block count from {}", daemon_addr);
    let count = probe_daemon_height(daemon_addr, Duration::from_secs(15)).unwrap_or(0);
    if count > 0 {
        log::debug!("[RPC] Daemon block count: {}", count);
    } else {
        log::warn!("[RPC] Could not get daemon height from {}", daemon_addr);
    }
    count
}

/// Minimal synchronous HTTP POST helper using std::net::TcpStream.
/// Avoids pulling in reqwest/tokio from this synchronous context.
fn ureq_post_json(url: &str, body: &serde_json::Value) -> Result<serde_json::Value, String> {
    ureq_post_json_timeout(url, body, 60)
}

fn ureq_post_json_timeout(url: &str, body: &serde_json::Value, timeout_secs: u64) -> Result<serde_json::Value, String> {
    http_post_json(url, body, Duration::from_secs(10), Duration::from_secs(timeout_secs))
}

/// HTTP/1.0 JSON POST with bounded connect and read timeouts
/// (a plain `TcpStream::connect` can hang for minutes on a node that drops packets).
/// Answers a digest challenge from a wallet-rpc started by this app.
fn http_post_json(
    url: &str,
    body: &serde_json::Value,
    connect_timeout: Duration,
    read_timeout: Duration,
) -> Result<serde_json::Value, String> {
    let method_hint = body.get("method").and_then(|m| m.as_str()).unwrap_or("?");
    log::debug!("[RPC/HTTP] POST {} method={}", url, method_hint);

    let without_scheme = url.trim_start_matches("http://");
    let slash_pos = without_scheme.find('/').unwrap_or(without_scheme.len());
    let host_port = &without_scheme[..slash_pos];
    let path = if slash_pos < without_scheme.len() { &without_scheme[slash_pos..] } else { "/" };
    let body_str = body.to_string();

    let mut conn = HttpConn::open(host_port, connect_timeout, read_timeout)?;
    let (mut status, headers, mut json_body) = conn.post(path, &body_str, None)?;
    if status == 401 {
        let creds = host_port.strip_prefix("127.0.0.1:")
            .and_then(|p| p.parse::<u16>().ok())
            .and_then(local_auth)
            .ok_or_else(|| "wallet-rpc requires authentication (not started by this session)".to_string())?;
        let challenge = digest_challenge(&headers)
            .ok_or_else(|| "wallet-rpc sent no usable digest challenge".to_string())?;
        let auth = digest_authorization(&challenge, &creds.0, &creds.1, path);
        // The nonce is bound to this connection: answer on the same one.
        (status, _, json_body) = conn.post(path, &body_str, Some(&auth))?;
        if status == 401 {
            log::error!("[RPC/HTTP] wallet-rpc rejected the session credentials");
            return Err("wallet-rpc authentication failed".into());
        }
    }

    // Bodies carry balances, addresses and tx hashes: only their size may be logged.
    log::debug!("[RPC/HTTP] {} response ({}B)", method_hint, json_body.len());

    serde_json::from_str(&json_body).map_err(|e| {
        let msg = format!("JSON parse error: {} ({}B body, HTTP {})", e, json_body.len(), status);
        log::error!("[RPC/HTTP] {}", msg);
        msg
    })
}

/// Keep-alive HTTP/1.1 connection (a digest challenge must be answered on the same one).
struct HttpConn {
    stream: std::net::TcpStream,
    host_port: String,
}

impl HttpConn {
    fn open(host_port: &str, connect_timeout: Duration, read_timeout: Duration) -> Result<Self, String> {
        use std::net::ToSocketAddrs;
        let addrs: Vec<std::net::SocketAddr> = host_port.to_socket_addrs()
            .map_err(|e| format!("Cannot resolve {}: {}", host_port, e))?
            .collect();
        let mut last_err = format!("No address for {}", host_port);
        for addr in &addrs {
            match std::net::TcpStream::connect_timeout(addr, connect_timeout) {
                Ok(stream) => {
                    stream.set_read_timeout(Some(read_timeout)).ok();
                    stream.set_write_timeout(Some(connect_timeout)).ok();
                    return Ok(Self { stream, host_port: host_port.to_string() });
                }
                Err(e) => last_err = e.to_string(),
            }
        }
        let msg = format!("TCP connect to {} failed: {} (is wallet-rpc running?)", host_port, last_err);
        log::error!("[RPC/HTTP] {}", msg);
        Err(msg)
    }

    /// One POST. Returns (status, raw headers, body).
    fn post(&mut self, path: &str, body: &str, authorization: Option<&str>) -> Result<(u16, String, String), String> {
        use std::io::{Read, Write};
        let host_port = self.host_port.clone();
        let fail = |what: &str, e: &dyn std::fmt::Display| {
            let msg = format!("{} {} failed: {}", what, host_port, e);
            log::error!("[RPC/HTTP] {}", msg);
            msg
        };

        let auth_line = authorization.map(|a| format!("Authorization: {a}\r\n")).unwrap_or_default();
        // Keep-alive only towards the local wallet-rpc; remote nodes (often behind a proxy that
        // may answer HTTP/1.1 with chunked bodies) get a plain HTTP/1.0 request.
        let (version, connection) = if host_port.starts_with("127.0.0.1:") {
            ("HTTP/1.1", "keep-alive")
        } else {
            ("HTTP/1.0", "close")
        };
        let request = format!(
            "POST {path} {version}\r\nHost: {host_port}\r\nContent-Type: application/json\r\n{auth_line}Content-Length: {}\r\nConnection: {connection}\r\n\r\n{body}",
            body.len()
        );
        self.stream.write_all(request.as_bytes()).map_err(|e| fail("Write to", &e))?;

        let mut buf: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 8192];
        let header_end = loop {
            if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") { break pos; }
            let n = self.stream.read(&mut chunk).map_err(|e| fail("Read from", &e))?;
            if n == 0 {
                let msg = format!("Malformed HTTP response (no header/body separator, {}B)", buf.len());
                log::error!("[RPC/HTTP] {}", msg);
                return Err(msg);
            }
            buf.extend_from_slice(&chunk[..n]);
        };
        let headers = String::from_utf8_lossy(&buf[..header_end]).to_string();
        let status = headers.split_whitespace().nth(1).and_then(|c| c.parse().ok()).unwrap_or(0);
        let content_length = headers.lines()
            .filter_map(|l| l.split_once(':'))
            .find(|(k, _)| k.trim().eq_ignore_ascii_case("content-length"))
            .and_then(|(_, v)| v.trim().parse::<usize>().ok());
        let mut body_bytes = buf[header_end + 4..].to_vec();
        loop {
            if content_length.is_some_and(|len| body_bytes.len() >= len) { break; }
            let n = self.stream.read(&mut chunk).map_err(|e| fail("Read from", &e))?;
            if n == 0 { break; }
            body_bytes.extend_from_slice(&chunk[..n]);
        }
        if let Some(len) = content_length { body_bytes.truncate(len); }
        Ok((status, headers, String::from_utf8_lossy(&body_bytes).to_string()))
    }
}

/// Parameters of the plain-MD5 digest challenge among the WWW-Authenticate headers.
fn digest_challenge(headers: &str) -> Option<Vec<(String, String)>> {
    headers.lines()
        .filter_map(|l| {
            let (name, value) = l.split_once(':')?;
            if !name.trim().eq_ignore_ascii_case("www-authenticate") { return None; }
            let value = value.trim();
            if !value.get(..7)?.eq_ignore_ascii_case("digest ") { return None; }
            Some(parse_auth_params(&value[7..]))
        })
        .find(|params| params.iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("algorithm"))
            .is_none_or(|(_, v)| v.eq_ignore_ascii_case("MD5")))
}

fn parse_auth_params(s: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = s.trim();
    while let Some((key, after)) = rest.split_once('=') {
        let key = key.trim().trim_start_matches(',').trim().to_string();
        let after = after.trim_start();
        let (value, next) = if let Some(quoted) = after.strip_prefix('"') {
            let end = quoted.find('"').unwrap_or(quoted.len());
            (quoted[..end].to_string(), quoted.get(end + 1..).unwrap_or(""))
        } else {
            let end = after.find(',').unwrap_or(after.len());
            (after[..end].trim().to_string(), &after[end..])
        };
        out.push((key, value));
        rest = next.trim_start_matches([',', ' ']);
    }
    out
}

fn md5_hex(data: &str) -> String {
    use md5::{Digest, Md5};
    Md5::digest(data.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

/// RFC 2617 digest response for a POST (qop=auth when offered).
fn digest_authorization(params: &[(String, String)], user: &str, pass: &str, uri: &str) -> String {
    let get = |k: &str| params.iter().find(|(pk, _)| pk.eq_ignore_ascii_case(k)).map(|(_, v)| v.as_str());
    let realm = get("realm").unwrap_or("");
    let nonce = get("nonce").unwrap_or("");
    let ha1 = md5_hex(&format!("{user}:{realm}:{pass}"));
    let ha2 = md5_hex(&format!("POST:{uri}"));
    let mut header = format!("Digest username=\"{user}\", realm=\"{realm}\", nonce=\"{nonce}\", uri=\"{uri}\", algorithm=MD5");
    if get("qop").is_some_and(|q| q.split(',').any(|v| v.trim() == "auth")) {
        let cnonce = random_hex(8);
        let nc = "00000001";
        let response = md5_hex(&format!("{ha1}:{nonce}:{nc}:{cnonce}:auth:{ha2}"));
        header.push_str(&format!(", response=\"{response}\", qop=auth, nc={nc}, cnonce=\"{cnonce}\""));
    } else {
        header.push_str(&format!(", response=\"{}\"", md5_hex(&format!("{ha1}:{nonce}:{ha2}"))));
    }
    if let Some(opaque) = get("opaque") {
        header.push_str(&format!(", opaque=\"{opaque}\""));
    }
    header
}
