use std::sync::{Mutex, Arc};
use std::sync::atomic::{AtomicBool, Ordering};
use std::path::PathBuf;
use serde::{Deserialize, Serialize};
use aes_gcm::{aead::{Aead, KeyInit, OsRng as AesOsRng}, Aes256Gcm, Key as AesKey, Nonce as AesNonce};
use rand::RngCore;
use tauri::{Manager, Emitter, Listener};

// ============================================================
//  Alert system
// ============================================================

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum AlertLevel { Orange, Red }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppAlert {
    pub level: AlertLevel,
    pub kind: String,            // "BALANCE_LOW", "PAYMENT_FAILED", etc.
    pub sub_id: Option<String>,  // related subscription id (no address/amount)
    pub message: String,         // human-readable, no secrets
    pub created_at: i64,         // Unix timestamp
}

use autopay_crypto::{
    SeedVault, EncryptedSeed,
    generate_seed as crypto_generate_seed,
    mnemonic_to_bytes,
    fix_seed_checksum,
};
use autopay_core::db::Database;
use autopay_core::models::{Subscription, SubscriptionStatus};

mod rpc_manager;
use rpc_manager::{RpcConfig, RpcManager, RpcStatus, PUBLIC_NODES};

// ============================================================
//  Application state
// ============================================================

/// Shared app state, protected by a Mutex.
pub struct AppState {
    /// Path to the encrypted seed vault file.
    pub vault_path: PathBuf,
    /// Path to the wallet file used with monero-wallet-rpc.
    pub wallet_file_path: PathBuf,
    /// Whether the wallet is currently unlocked (seed in memory).
    pub unlocked: bool,
    /// Wallet address (primary, set after first unlock/restore).
    pub wallet_address: Option<String>,
    /// Cached XMR balance (piconero units, refreshed at unlock time).
    pub balance_atomic: u64,
    /// Whether the last RPC session succeeded (used for UI status dot).
    pub rpc_connected: bool,
    /// Last RPC error message (None = no error or not yet tried).
    pub rpc_last_error: Option<String>,
    /// Wallet password stored in memory for this session (for balance refresh).
    /// Cleared when the app is locked.
    pub session_password: Option<String>,
    /// A deep-link URL waiting to be consumed by the frontend.
    /// Set when the app is launched/focused via a crypto-autopay: URI.
    pub pending_deep_link: Option<String>,
    /// Timestamp (Unix seconds) of the last outgoing payment — enforces 25-min Monero cooldown.
    pub last_payment_sent_at: Option<i64>,
    /// Last known wallet scan height (from get_height after refresh).
    pub last_wallet_height: u64,
    /// Last known daemon height (from get_daemon_block_height).
    pub last_daemon_height: u64,
    /// Unix timestamp of the last successful daemon contact (for offline alert).
    pub last_daemon_contact_at: Option<i64>,
    /// Active alerts — recomputed after each scheduler run / refresh.
    pub alerts: Vec<AppAlert>,
    /// RPC busy flag — only one command can use the wallet RPC at a time (port 18083).
    pub rpc_busy: Arc<AtomicBool>,
    /// RPC process manager.
    pub rpc: RpcManager,
    /// Alert config: minimum days of runway before BALANCE_LOW fires (default 7).
    pub balance_low_threshold_days: f64,
    /// Alert preferences: which alert kinds are enabled (all enabled by default).
    pub enabled_alerts: std::collections::HashSet<String>,
    /// AES-256-GCM key for payment history encryption (derived at unlock, cleared at lock).
    pub history_key: Option<[u8; 32]>,
    /// Path to the encrypted payment history file.
    pub history_path: PathBuf,
    /// Whether payment history recording is enabled.
    pub history_enabled: bool,
    /// Number of days to keep history entries (default: 365).
    pub history_days: u32,
    /// Semi-auto mode: if true, scheduler queues payments and waits for user approval.
    pub semi_auto_mode: bool,
    /// Payments awaiting user approval (semi-auto mode).
    pub pending_approvals: std::collections::HashMap<String, PendingApproval>,
    /// Payments approved by the user this session (scheduler picks these up).
    pub approved_payments: std::collections::HashSet<String>,
    /// Unix timestamp of last unlock — scheduler skips 45s after unlock to let initial sync complete.
    pub last_unlock_at: i64,
    /// Treasurer mode: allows importing and executing one-time PV (Procès-Verbal) payments.
    pub treasurer_mode: bool,
}

/// A payment pending user confirmation (semi-auto mode).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingApproval {
    pub sub_id: String,
    pub sub_label: String,
    pub amount_xmr: String,
    pub queued_at: String, // ISO 8601
}

// ============================================================
//  Tauri commands
// ============================================================

/// Return and clear any pending deep-link URL.
/// The frontend calls this after transitioning to the dashboard to catch
/// URLs received while the wallet was locked.
#[tauri::command]
async fn get_pending_deep_link(state: tauri::State<'_, Mutex<AppState>>) -> Result<Option<String>, String> {
    let mut s = state.lock().map_err(|e| e.to_string())?;
    Ok(s.pending_deep_link.take())
}

/// Lock the wallet session (clears password from memory, marks as locked).
/// Called by the frontend auto-lock timer after inactivity.
#[tauri::command]
async fn lock_wallet(state: tauri::State<'_, Mutex<AppState>>) -> Result<(), String> {
    let mut s = state.lock().map_err(|e| e.to_string())?;
    s.unlocked = false;
    s.session_password = None;
    s.history_key = None;  // clear derived key from memory
    // Keep wallet_address cached (it's in the .address file anyway)
    log::info!("[Security] Wallet locked (auto-lock or manual)");
    Ok(())
}

/// Generate a fresh 25-word Monero seed phrase.
/// Called by the frontend during onboarding (step 2).
#[tauri::command]
async fn generate_seed() -> Result<String, String> {
    crypto_generate_seed()
        .map(|s| s.to_string())
        .map_err(|e| e.to_string())
}

/// Check if a seed vault file already exists (determines welcome vs dashboard).
#[tauri::command]
async fn check_wallet_exists(state: tauri::State<'_, Mutex<AppState>>) -> Result<bool, String> {
    let state = state.lock().map_err(|e| e.to_string())?;
    Ok(state.vault_path.exists())
}

/// Create (encrypt + save) a new wallet from a seed phrase and password.
/// Called at the end of onboarding step 3.
#[tauri::command]
async fn create_wallet(
    seed: String,
    password: String,
    state: tauri::State<'_, Mutex<AppState>>,
) -> Result<(), String> {
    // Validate seed length
    let words: Vec<&str> = seed.split_whitespace().collect();
    if words.len() != 25 {
        return Err(format!("Invalid seed: expected 25 words, got {}", words.len()));
    }

    // Validate seed words can be decoded
    mnemonic_to_bytes(&seed).map_err(|e| e.to_string())?;

    // Encrypt seed
    let encrypted = SeedVault::encrypt(&seed, &password).map_err(|e| e.to_string())?;
    let vault_json = serde_json::to_string(&encrypted).map_err(|e| e.to_string())?;

    // Save vault file
    let vault_path = {
        let state = state.lock().map_err(|e| e.to_string())?;
        state.vault_path.clone()
    };

    if let Some(parent) = vault_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(&vault_path, vault_json).map_err(|e| e.to_string())?;

    let mut state = state.lock().map_err(|e| e.to_string())?;
    state.unlocked = false; // Will be unlocked on next unlock call
    Ok(())
}

/// Unlock the wallet: verify password by decrypting the vault.
/// - Returns Err ONLY if the password is wrong (or vault missing).
/// - Returns Ok(UnlockResult) in all other cases (even if RPC fails).
///   rpc_error / seed_invalid in the result indicate degraded mode.
#[tauri::command]
async fn unlock_wallet(
    password: String,
    state: tauri::State<'_, Mutex<AppState>>,
    app_handle: tauri::AppHandle,
) -> Result<UnlockResult, String> {
    let (vault_path, wallet_file_path) = {
        let s = state.lock().map_err(|e| e.to_string())?;
        (s.vault_path.clone(), s.wallet_file_path.clone())
    };

    let vault_json = std::fs::read_to_string(&vault_path)
        .map_err(|_| "Vault file not found — please create a wallet first.".to_string())?;

    let encrypted: EncryptedSeed = serde_json::from_str(&vault_json)
        .map_err(|e| format!("Invalid vault file: {e}"))?;

    // Decrypt — ONLY step that can return Err (wrong password)
    let seed = SeedVault::decrypt(&encrypted, &password)
        .map_err(|_| "Incorrect password.".to_string())?;

    log::info!("[Unlock] Vault decrypted successfully.");

    // Auto-migrate: fix checksum if seed was created with old buggy algorithm
    let seed_str = if let Some(fixed) = fix_seed_checksum(seed.as_str()) {
        log::warn!("[Unlock] Seed checksum was wrong (old app version) — auto-fixing and saving corrected vault.");
        // Re-encrypt with corrected seed and overwrite vault
        let corrected = fixed.to_string();
        match SeedVault::encrypt(&corrected, &password) {
            Ok(new_encrypted) => {
                match serde_json::to_string(&new_encrypted) {
                    Ok(json) => { let _ = std::fs::write(&vault_path, json); }
                    Err(e) => log::error!("[Unlock] Could not serialize corrected vault: {}", e),
                }
            }
            Err(e) => log::error!("[Unlock] Could not re-encrypt corrected vault: {}", e),
        }
        corrected
    } else {
        seed.to_string()
    };

    let keys_file = wallet_file_path.with_extension("keys");
    let wallet_exists = keys_file.exists();
    let wallet_name = wallet_file_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("autopay_wallet")
        .to_string();
    let wallet_dir = app_handle.path().app_data_dir().map_err(|e| e.to_string())?;

    // Fast-path: if we have a cached address + valid height marker, skip RPC entirely.
    // The background refresh (refresh_balance) will sync the balance.
    let addr_cache_file = wallet_dir.join(format!("{}.address", wallet_name));
    let height_marker_fast = wallet_dir.join(format!("{}.height", wallet_name));
    let saved_height_fast: u64 = std::fs::read_to_string(&height_marker_fast)
        .ok().and_then(|s| s.trim().parse().ok()).unwrap_or(0);

    if let Some(cached_addr) = std::fs::read_to_string(&addr_cache_file).ok()
        .map(|s| s.trim().to_string())
        .filter(|a| a.len() > 90 && saved_height_fast > 0)
    {
        log::info!("[Unlock] Fast path: using cached address {}…", &cached_addr[..12]);
        if let Ok(mut s) = state.lock() {
            s.unlocked = true;
            s.last_unlock_at = chrono::Utc::now().timestamp();
            s.session_password = Some(password.clone());
            s.history_key = Some(derive_history_key(&password));
            s.wallet_address = Some(cached_addr.clone());
            s.wallet_file_path = wallet_file_path.clone();
            s.rpc_connected = false;
            s.rpc_last_error = None;
            s.balance_atomic = 0;
        }
        return Ok(UnlockResult {
            address: cached_addr,
            balance_xmr: 0.0,
            rpc_error: None,
            seed_invalid: false,
        });
    }

    // Try RPC session — non-fatal on failure
    log::info!("[Unlock] wallet_dir={:?}, wallet_exists={}, wallet_name={}", wallet_dir, wallet_exists, wallet_name);
    let (wn, wd, sd, pw) = (wallet_name.clone(), wallet_dir.clone(), seed_str.clone(), password.clone());

    let rpc_result: Result<(String, f64), (String, bool)> = tokio::task::spawn_blocking(move || {
        log::info!("[Unlock/RPC] spawn_blocking started");
        let mut mgr = rpc_manager::RpcManager::new(rpc_manager::RpcConfig::default());
        log::info!("[Unlock/RPC] Using daemon: {}, port: {}", mgr.daemon_address(), mgr.config_port());

        mgr.start_for_dir(&wd).map_err(|e| {
            log::error!("[Unlock/RPC] start_for_dir failed: {}", e);
            (e, false)
        })?;

        log::info!("[Unlock/RPC] Process spawned, waiting for RPC to be ready…");
        mgr.wait_for_ready(std::time::Duration::from_secs(15)).map_err(|e| {
            log::error!("[Unlock/RPC] wait_for_ready timed out: {}", e);
            (e, false)
        })?;
        log::info!("[Unlock/RPC] RPC endpoint ready");

        // Compute a safe restore_height: current daemon height minus 24h buffer.
        // 720 blocks ≈ 24 hours. Avoids scan-from-genesis (millions of blocks).
        // Sufficient for recent deposits; users who had older funds will need a manual rescan.
        let daemon_height = rpc_manager::get_daemon_block_height(&mgr.daemon_address());
        let restore_height: u64 = if daemon_height > 720 { daemon_height - 720 } else { 0 };
        log::info!("[Unlock/RPC] daemon_height={}, restore_height={}", daemon_height, restore_height);

        // Cache file (no extension) and height-marker file paths.
        let cache_file = wd.join(&wn);
        let height_marker = wd.join(format!("{}.height", wn));
        let cache_exists = cache_file.exists();

        // Detect stale scan: if height_marker is missing or says "0", the wallet was created with
        // restore_height=0 (old bug) — we must delete and recreate from a recent height.
        let saved_height: u64 = std::fs::read_to_string(&height_marker)
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0);
        let stale_scan = saved_height == 0 && cache_exists; // was scanned from genesis

        let (address, balance) = if !wallet_exists || !cache_exists || stale_scan {
            // Force fresh restore: delete both files so restore_deterministic_wallet can create cleanly.
            if stale_scan {
                log::warn!("[Unlock/RPC] Detected stale restore_height=0 — resetting wallet cache to height {}", restore_height);
            } else if !wallet_exists {
                log::info!("[Unlock/RPC] No .keys — fresh restore at height {}", restore_height);
            } else {
                log::info!("[Unlock/RPC] Cache missing — restore at height {}", restore_height);
            }
            // Delete both wallet files (restore_deterministic_wallet fails if they exist)
            let _ = std::fs::remove_file(&cache_file);
            let _ = std::fs::remove_file(wd.join(format!("{}.keys", wn)));
            let addr = mgr.rpc_restore_wallet(&wn, &pw, &sd, restore_height).map_err(|e| {
                let is_seed = e.contains("word list") || e.contains("Electrum") || e.contains("verification");
                log::error!("[Unlock/RPC] restore failed (seed_invalid={}): {}", is_seed, e);
                (e, is_seed)
            })?;
            log::info!("[Unlock/RPC] Restore OK, address={}", &addr[..addr.len().min(12)]);
            // Save height marker so we know this wallet was created with a good restore_height
            let _ = std::fs::write(&height_marker, restore_height.to_string());
            // Balance is 0 immediately after restore — sync happens via refresh_balance later.
            let bal = 0.0_f64;
            (addr, bal)
        } else {
            log::info!("[Unlock/RPC] .keys + cache found (height={}) — calling open_wallet", saved_height);
            let open_resp = rpc_manager::ureq_post_json_pub(&mgr.rpc_url(), &serde_json::json!({
                "jsonrpc":"2.0","id":"open","method":"open_wallet",
                "params":{"filename":wn,"password":pw}
            })).map_err(|e| {
                log::error!("[Unlock/RPC] open_wallet failed: {}", e);
                (e, false)
            })?;
            if let Some(err) = open_resp.get("error") {
                let msg = format!("open_wallet RPC error: {}", err);
                log::error!("[Unlock/RPC] {}", msg);
                return Err((msg, false));
            }
            log::info!("[Unlock/RPC] open_wallet OK — fetching address");
            let addr_resp = rpc_manager::ureq_post_json_pub(&mgr.rpc_url(), &serde_json::json!({
                "jsonrpc":"2.0","id":"addr","method":"get_address",
                "params":{"account_index":0}
            })).map_err(|e| {
                log::error!("[Unlock/RPC] get_address failed: {}", e);
                (e, false)
            })?;
            let addr = addr_resp["result"]["address"].as_str().unwrap_or("").to_string();
            log::info!("[Unlock/RPC] Address retrieved ({}…)", &addr[..addr.len().min(12)]);
            let bal = mgr.rpc_get_balance().map(|(_, u)| u).unwrap_or_else(|e| {
                log::warn!("[Unlock/RPC] get_balance failed: {}", e);
                0.0
            });
            (addr, bal)
        };
        log::info!("[Unlock/RPC] Session complete — stopping wallet-rpc");
        mgr.stop();
        Ok::<(String, f64), (String, bool)>((address, balance))
    })
    .await
    .unwrap_or_else(|e| {
        log::error!("[Unlock/RPC] tokio spawn_blocking panicked: {}", e);
        Err((format!("Internal task error: {e}"), false))
    });

    let (address, balance_xmr, rpc_error, seed_invalid) = match rpc_result {
        Ok((addr, bal)) => {
            let prefix = if addr.len() > 12 { &addr[..12] } else { &addr };
            log::info!("[Unlock] RPC session OK. Address: {}…, balance: {} XMR", prefix, bal);
            (addr, bal, None, false)
        }
        Err((e, is_seed)) => {
            log::warn!("[Unlock] RPC failed (non-fatal, degraded mode). seed_invalid={}: {}", is_seed, e);
            (String::new(), 0.0, Some(e), is_seed)
        }
    };

    // Cache address on disk so next unlock is instant (fast-path above)
    if !address.is_empty() {
        let _ = std::fs::write(&addr_cache_file, &address);
        log::info!("[Unlock] Address cached to disk for fast future unlocks");
    }

    let mut s = state.lock().map_err(|e| e.to_string())?;
    s.unlocked = true;
    s.last_unlock_at = chrono::Utc::now().timestamp();
    s.history_key = Some(derive_history_key(&password));
    s.session_password = Some(password);
    s.wallet_address = if address.is_empty() { None } else { Some(address.clone()) };
    s.balance_atomic = (balance_xmr * 1e12) as u64;
    s.rpc_connected = rpc_error.is_none();
    s.rpc_last_error = rpc_error.clone();
    s.wallet_file_path = wallet_file_path.clone();

    log::info!("[Unlock] Done. rpc_connected={}, seed_invalid={}", s.rpc_connected, seed_invalid);
    Ok(UnlockResult { address, balance_xmr, rpc_error, seed_invalid })
}

/// Get full wallet+RPC info: balance, address, connection status, last error.
#[tauri::command]
async fn get_wallet_info(state: tauri::State<'_, Mutex<AppState>>) -> Result<serde_json::Value, String> {
    let s = state.lock().map_err(|e| e.to_string())?;
    Ok(serde_json::json!({
        "unlocked": s.unlocked,
        "address": s.wallet_address,
        "balance_xmr": s.balance_atomic as f64 / 1e12,
        "balance_atomic": s.balance_atomic,
        "rpc_connected": s.rpc_connected,
        "rpc_last_error": s.rpc_last_error,
    }))
}

/// Get the cached wallet balance (set at unlock time, refreshed by refresh_balance).
#[tauri::command]
async fn get_balance(state: tauri::State<'_, Mutex<AppState>>) -> Result<BalanceResponse, String> {
    let s = state.lock().map_err(|e| e.to_string())?;
    if !s.unlocked { return Err("Wallet is locked.".to_string()); }
    let atomic = s.balance_atomic;
    Ok(BalanceResponse { xmr: atomic as f64 / 1e12, atomic })
}

/// Send all funds (sweep_all) to a destination address.
/// Requires the wallet to be unlocked (session_password must be set).
/// Send a specific amount to a destination address (for scheduled payments).
/// Security: logs only the subscription ID — no amounts, no addresses in logs.
#[tauri::command]
async fn send_payment(
    destination_address: String,
    amount_xmr: f64,
    subscription_id: String,
    state: tauri::State<'_, Mutex<AppState>>,
    app_handle: tauri::AppHandle,
) -> Result<serde_json::Value, String> {
    let dest = destination_address.trim().to_string();
    if dest.len() < 90 || dest.len() > 110 {
        return Err("Invalid destination address".to_string());
    }
    if amount_xmr <= 0.0 || amount_xmr > 100.0 {
        return Err("Invalid amount".to_string());
    }
    let atomic_units = (amount_xmr * 1e12) as u64;

    let (wallet_name, wallet_dir, password, rpc_busy) = {
        let s = state.lock().map_err(|e| e.to_string())?;
        if !s.unlocked { return Err("Wallet is locked.".to_string()); }
        let wn = s.wallet_file_path.file_name()
            .and_then(|n| n.to_str()).unwrap_or("autopay_wallet").to_string();
        let wd = app_handle.path().app_data_dir().map_err(|e| e.to_string())?;
        let pw = s.session_password.clone().ok_or("No session password")?;
        let busy = Arc::clone(&s.rpc_busy);
        (wn, wd, pw, busy)
    };

    // Exclusive RPC access — reject if already in use (refresh_balance running)
    if rpc_busy.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).is_err() {
        return Err("RPC busy: balance refresh in progress. Retry in a moment.".to_string());
    }

    log::info!("[Payment] Sending payment for subscription #{}", subscription_id);

    let busy_clone = Arc::clone(&rpc_busy);
    let result = tokio::task::spawn_blocking(move || {
        // RAII guard: always clear rpc_busy when this closure exits
        struct RpcGuard(Arc<AtomicBool>);
        impl Drop for RpcGuard { fn drop(&mut self) { self.0.store(false, Ordering::SeqCst); } }
        let _guard = RpcGuard(Arc::clone(&busy_clone));

        let mut mgr = rpc_manager::RpcManager::new(rpc_manager::RpcConfig::default());
        mgr.start_for_dir(&wallet_dir).map_err(|e| e.to_string())?;
        mgr.wait_for_ready(std::time::Duration::from_secs(15)).map_err(|e| e.to_string())?;

        let open = rpc_manager::ureq_post_json_pub(&mgr.rpc_url(), &serde_json::json!({
            "jsonrpc":"2.0","id":"open","method":"open_wallet",
            "params":{"filename":wallet_name,"password":password}
        })).map_err(|e| e.to_string())?;
        if let Some(err) = open.get("error") {
            mgr.stop();
            return Err(format!("open_wallet: {}", err));
        }

        let resp = rpc_manager::ureq_post_json_pub(&mgr.rpc_url(), &serde_json::json!({
            "jsonrpc":"2.0","id":"tx","method":"transfer",
            "params":{
                "destinations":[{"amount": atomic_units, "address": dest}],
                "account_index": 0,
                "priority": 1,
                "get_tx_key": false
            }
        })).map_err(|e| e.to_string())?;

        mgr.stop();

        if let Some(err) = resp.get("error") {
            let msg = err.get("message").and_then(|m| m.as_str()).unwrap_or("unknown");
            log::error!("[Payment] sub #{} — RPC error: {}", subscription_id, msg);
            return Err(format!("transfer RPC error: {}", msg));
        }

        let tx_hash = resp["result"]["tx_hash"].as_str().unwrap_or("").to_string();
        if tx_hash.is_empty() {
            log::error!("[Payment] sub #{} — transfer returned no tx_hash. Full result: {:?}", subscription_id, resp["result"]);
            return Err("transfer returned empty tx_hash".to_string());
        }
        log::info!("[Payment] sub #{} — SUCCESS. tx_hash={}...", subscription_id, &tx_hash[..12.min(tx_hash.len())]);
        Ok(serde_json::json!({ "tx_hash": tx_hash }))
    })
    .await
    .map_err(|e| e.to_string())??;

    Ok(result)
}

/// Update a subscription's next_payment_at after a successful payment.
#[tauri::command]
async fn update_subscription_next_payment(
    id: String,
    next_payment_at: String,
    db_state: tauri::State<'_, Mutex<Database>>,
) -> Result<(), String> {
    let db = db_state.lock().map_err(|e| e.to_string())?;
    db.update_subscription_next_payment(&id, &next_payment_at)
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn sweep_all(
    destination: String,
    state: tauri::State<'_, Mutex<AppState>>,
    app_handle: tauri::AppHandle,
) -> Result<String, String> {
    // Validate destination looks like a Monero address (basic length check)
    let dest = destination.trim().to_string();
    if dest.len() < 90 || dest.len() > 110 {
        return Err("Invalid Monero address (expected 95-106 characters).".to_string());
    }

    let (wallet_name, wallet_dir, password) = {
        let s = state.lock().map_err(|e| e.to_string())?;
        if !s.unlocked { return Err("Wallet is locked.".to_string()); }
        let wn = s.wallet_file_path.file_name()
            .and_then(|n| n.to_str()).unwrap_or("autopay_wallet").to_string();
        let wd = app_handle.path().app_data_dir().map_err(|e| e.to_string())?;
        let pw = s.session_password.clone().ok_or("No session password")?;
        (wn, wd, pw)
    };

    log::info!("[Sweep] Initiating sweep_all to {}…", &dest[..dest.len().min(12)]);

    let tx_hash = tokio::task::spawn_blocking(move || {
        let mut mgr = rpc_manager::RpcManager::new(rpc_manager::RpcConfig::default());
        mgr.start_for_dir(&wallet_dir).map_err(|e| e.to_string())?;
        mgr.wait_for_ready(std::time::Duration::from_secs(15)).map_err(|e| e.to_string())?;

        // Open wallet
        let open = rpc_manager::ureq_post_json_pub(&mgr.rpc_url(), &serde_json::json!({
            "jsonrpc":"2.0","id":"open","method":"open_wallet",
            "params":{"filename":wallet_name,"password":password}
        })).map_err(|e| e.to_string())?;
        if let Some(err) = open.get("error") {
            return Err(format!("open_wallet error: {}", err));
        }

        // sweep_all
        let resp = rpc_manager::ureq_post_json_pub(&mgr.rpc_url(), &serde_json::json!({
            "jsonrpc":"2.0","id":"sweep","method":"sweep_all",
            "params":{
                "address": dest,
                "account_index": 0,
                "get_tx_hex": false
            }
        })).map_err(|e| e.to_string())?;

        mgr.stop();

        if let Some(err) = resp.get("error") {
            let msg = err.get("message").and_then(|m| m.as_str()).unwrap_or("unknown");
            return Err(format!("sweep_all RPC error: {}", msg));
        }

        // Return first tx hash
        let hash = resp["result"]["tx_hash_list"][0]
            .as_str()
            .unwrap_or("")
            .to_string();
        log::info!("[Sweep] Done. tx_hash={}", hash);
        Ok(hash)
    })
    .await
    .map_err(|e| e.to_string())??;

    Ok(tx_hash)
}

/// Refresh balance using the stored session password (called by frontend timer).
/// Opens a transient wallet-rpc session, fetches unlocked balance, stores it.
#[tauri::command]
async fn refresh_balance(
    state: tauri::State<'_, Mutex<AppState>>,
    app_handle: tauri::AppHandle,
) -> Result<BalanceResponse, String> {
    let (wallet_name, wallet_dir, password, rpc_busy) = {
        let s = state.lock().map_err(|e| e.to_string())?;
        if !s.unlocked { return Err("Locked".into()); }
        let wn = s.wallet_file_path.file_name()
            .and_then(|n| n.to_str()).unwrap_or("autopay_wallet").to_string();
        let wd = app_handle.path().app_data_dir().map_err(|e| e.to_string())?;
        let pw = s.session_password.clone().ok_or("No session")?;
        let busy = Arc::clone(&s.rpc_busy);
        (wn, wd, pw, busy)
    };

    // If a payment is in progress, skip this refresh rather than crashing the payment
    if rpc_busy.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).is_err() {
        log::warn!("[refresh_balance] RPC busy (payment in progress) — skipping refresh");
        return Err("RPC busy".into());
    }

    let (xmr, atomic, wallet_height) = tokio::task::spawn_blocking(move || {
        struct RpcGuard(Arc<AtomicBool>);
        impl Drop for RpcGuard { fn drop(&mut self) { self.0.store(false, Ordering::SeqCst); } }
        let _guard = RpcGuard(rpc_busy);

        let mut mgr = rpc_manager::RpcManager::new(rpc_manager::RpcConfig::default());
        mgr.start_for_dir(&wallet_dir).map_err(|e| e.to_string())?;
        mgr.wait_for_ready(std::time::Duration::from_secs(10)).map_err(|e| e.to_string())?;

        // Get wallet current height before sync (for progress reporting)
        let wallet_height_before = rpc_manager::ureq_post_json_pub(&mgr.rpc_url(), &serde_json::json!({
            "jsonrpc":"2.0","id":"h","method":"get_height","params":{}
        })).ok().and_then(|r| r["result"]["height"].as_u64()).unwrap_or(0);

        // Open wallet
        let open_resp = rpc_manager::ureq_post_json_pub(&mgr.rpc_url(), &serde_json::json!({
            "jsonrpc":"2.0","id":"open","method":"open_wallet",
            "params":{"filename":wallet_name,"password":password}
        })).map_err(|e| e.to_string())?;
        if let Some(err) = open_resp.get("error") {
            log::warn!("[refresh_balance] open_wallet error: {}", err);
        }

        // Trigger wallet refresh (sync with daemon) — 180s timeout (720 blocks ≈ 47s on remote node)
        log::info!("[refresh_balance] Calling wallet refresh (may take up to ~2 min for first sync)…");
        let refresh_resp = rpc_manager::ureq_post_json_long(&mgr.rpc_url(), &serde_json::json!({
            "jsonrpc":"2.0","id":"refresh","method":"refresh",
            "params":{"start_height":0}
        }), 180);
        let blocks_fetched = match &refresh_resp {
            Ok(r) => {
                let fetched = r["result"]["blocks_fetched"].as_u64().unwrap_or(0);
                let received = r["result"]["received_money"].as_bool().unwrap_or(false);
                log::info!("[refresh_balance] Sync done: blocks_fetched={}, received_money={}", fetched, received);
                fetched
            }
            Err(e) => { log::warn!("[refresh_balance] refresh failed (non-fatal, will retry): {}", e); 0 }
        };

        // Get updated wallet height after sync
        let wallet_height_after = rpc_manager::ureq_post_json_pub(&mgr.rpc_url(), &serde_json::json!({
            "jsonrpc":"2.0","id":"h2","method":"get_height","params":{}
        })).ok().and_then(|r| r["result"]["height"].as_u64()).unwrap_or(wallet_height_before + blocks_fetched);

        let (total, _unlocked) = mgr.rpc_get_balance().unwrap_or((0.0, 0.0));
        let atomic = (total * 1e12) as u64;
        log::info!("[refresh_balance] Total balance: {} XMR ({} pico)", total, atomic);
        mgr.stop();
        Ok::<(f64, u64, u64), String>((total, atomic, wallet_height_after))
    })
    .await
    .map_err(|e| e.to_string())??;

    // Get daemon height (non-blocking, direct REST call)
    let daemon_addr = state.lock().map_err(|e| e.to_string())?.rpc.daemon_address();
    let daemon_height = {
        let h = rpc_manager::get_daemon_block_height(&daemon_addr);
        if h > 0 { h } else { wallet_height }
    };

    if let Ok(mut s) = state.lock() {
        s.balance_atomic = atomic;
        s.last_wallet_height = wallet_height;
        s.last_daemon_height = daemon_height;
        if daemon_height > 0 {
            s.last_daemon_contact_at = Some(chrono::Utc::now().timestamp());
        }
    }

    // Recompute alerts immediately after balance update — don't wait for scheduler cycle
    if let (Some(state_rc), Some(db_rc)) = (
        app_handle.try_state::<Mutex<AppState>>(),
        app_handle.try_state::<Mutex<Database>>(),
    ) {
        if let (Ok(mut s), Ok(db)) = (state_rc.lock(), db_rc.lock()) {
            if let Ok(subs) = db.list_subscriptions() {
                let worst = recompute_alerts(&mut s, &subs, chrono::Utc::now().timestamp());
                let count = s.alerts.len();
                drop(s);
                update_tray_icon(&app_handle, worst, count);
            }
        }
        // Notify frontend to refresh alerts immediately
        app_handle.emit("alerts-updated", ()).ok();
    }

    Ok(BalanceResponse { xmr, atomic })
}

/// Return current active alerts (for frontend badge display).
#[tauri::command]
async fn get_alerts(state: tauri::State<'_, Mutex<AppState>>) -> Result<Vec<AppAlert>, String> {
    let s = state.lock().map_err(|e| e.to_string())?;
    Ok(s.alerts.clone())
}

/// Recompute all alerts based on current state and subscriptions.
/// Returns the worst AlertLevel, or None if clean.
fn recompute_alerts(
    state: &mut AppState,
    subs: &[Subscription],
    now_ts: i64,
) -> Option<AlertLevel> {
    let mut alerts: Vec<AppAlert> = Vec::new();

    let balance_xmr = state.balance_atomic as f64 / 1e12;

    // RED: wallet empty with active subscriptions (only after at least one successful sync)
    let has_synced = state.last_wallet_height > 0;
    if has_synced && balance_xmr < 0.000001 && subs.iter().any(|s| s.status == SubscriptionStatus::Active) {
        alerts.push(AppAlert {
            level: AlertLevel::Red,
            kind: "BALANCE_EMPTY".into(),
            sub_id: None,
            message: "Wallet balance is zero — recurring payments cannot be sent".into(),
            created_at: now_ts,
        });
    }

    // Estimate monthly spending (XMR/month) from active subs
    let monthly_xmr: f64 = subs.iter().filter(|s| s.status == SubscriptionStatus::Active).map(|s| {
        let amt: f64 = s.amount.parse().unwrap_or(0.0);
        let cron_parts: Vec<&str> = s.schedule.trim().split_whitespace().collect();
        let min_field = if cron_parts.len() >= 6 { cron_parts[1] } else { cron_parts.get(0).copied().unwrap_or("*") };
        let period_min = if min_field.starts_with("*/") {
            min_field[2..].parse::<f64>().unwrap_or(1440.0)
        } else if min_field == "0" { 60.0 } else { 1440.0 };
        let sends_per_month = 43200.0 / period_min; // 43200 min/month
        amt * sends_per_month
    }).sum();

    // ORANGE: balance will run out before threshold (default 7 days)
    let threshold_days = state.balance_low_threshold_days.max(1.0);
    if balance_xmr > 0.0 && monthly_xmr > 0.0 {
        let days_left = (balance_xmr / monthly_xmr) * 30.0;
        if days_left < threshold_days && days_left >= 0.0 {
            alerts.push(AppAlert {
                level: AlertLevel::Orange,
                kind: "BALANCE_LOW".into(),
                sub_id: None,
                message: if days_left < 1.0 {
                    format!("Portefeuille vide dans moins de 1 jour au rythme actuel (seuil : {:.0}j)", threshold_days)
                } else {
                    format!("Portefeuille vide dans ~{:.0} jour(s) au rythme actuel (seuil : {:.0}j)", days_left, threshold_days)
                },
                created_at: now_ts,
            });
        }
    }

    // RED: canary expired for any subscription
    for sub in subs {
        if sub.status == SubscriptionStatus::Active {
            if let Some(ref canary) = sub.canary_url {
                if !canary.is_empty() {
                    // Check last_canary_status from DB if available — simplified: flag if explicitly "Expired"
                    // TODO: use DB canary_status field when available
                }
            }
        }
    }

    // RED / ORANGE: overdue payments
    for sub in subs.iter().filter(|s| s.status == SubscriptionStatus::Active) {
        if let Some(next) = sub.next_payment_at {
            let overdue_secs = now_ts - next.timestamp();
            if overdue_secs > 24 * 3600 {
                // RED: overdue by more than 24h
                alerts.push(AppAlert {
                    level: AlertLevel::Red,
                    kind: "PAYMENT_OVERDUE_HARD".into(),
                    sub_id: Some(sub.id.clone()),
                    message: format!("Sub #{} overdue by >24h — possible payment failure", sub.id),
                    created_at: now_ts,
                });
            } else if overdue_secs > 3600 {
                // ORANGE: overdue by 1–24h
                alerts.push(AppAlert {
                    level: AlertLevel::Orange,
                    kind: "PAYMENT_OVERDUE_SOFT".into(),
                    sub_id: Some(sub.id.clone()),
                    message: format!("Sub #{} overdue by >1h", sub.id),
                    created_at: now_ts,
                });
            }
        }
    }

    // RED: offline >24h AND payments due in next 7 days
    if let Some(last_contact) = state.last_daemon_contact_at {
        let offline_secs = now_ts - last_contact;
        if offline_secs > 86_400 {
            let upcoming_in_7d = subs.iter().any(|s| {
                s.status == SubscriptionStatus::Active &&
                s.next_payment_at.map(|next| (next.timestamp() - now_ts) < 7 * 86_400).unwrap_or(false)
            });
            if upcoming_in_7d {
                alerts.push(AppAlert {
                    level: AlertLevel::Red,
                    kind: "OFFLINE_WITH_PENDING".into(),
                    sub_id: None,
                    message: format!(
                        "Hors ligne depuis {}h — paiements prevus dans les 7 prochains jours",
                        offline_secs / 3600
                    ),
                    created_at: now_ts,
                });
            }
        }
    }

    state.alerts = alerts;
    // Worst level
    if state.alerts.iter().any(|a| a.level == AlertLevel::Red) {
        Some(AlertLevel::Red)
    } else if state.alerts.iter().any(|a| a.level == AlertLevel::Orange) {
        Some(AlertLevel::Orange)
    } else {
        None
    }
}

// ============================================================
//  Payment history — encrypted storage
// ============================================================

/// A single payment history entry (stored encrypted).
/// Security: no wallet address, tx hash truncated to 12 chars.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub id: String,          // random UUID
    pub sub_id: String,      // subscription internal ID
    pub sub_label: String,   // human label from subscription
    pub amount_xmr: f64,     // amount sent (user wants this visible)
    pub sent_at: String,     // ISO 8601
    pub tx_prefix: String,   // first 12 chars of tx_hash only
}

/// Derive a 32-byte AES key from the master password for history encryption.
/// Uses Argon2id with lighter params (32MB, 2 iterations) since derived once at unlock.
fn derive_history_key(password: &str) -> [u8; 32] {
    use argon2::{Argon2, Params, Algorithm, Version};
    const HISTORY_SALT: &[u8] = b"autopay-history-v1-salt-2025"; // fixed purpose salt
    let params = Params::new(32_768, 2, 1, Some(32)).expect("valid params");
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut key = [0u8; 32];
    argon2.hash_password_into(password.as_bytes(), HISTORY_SALT, &mut key)
        .expect("Argon2id history key derivation failed");
    key
}

/// Encrypt a JSON payload with AES-256-GCM. Returns base64(NONCE | CIPHERTEXT).
fn aes_encrypt(key: &[u8; 32], plaintext: &[u8]) -> Result<String, String> {
    let cipher = Aes256Gcm::new(AesKey::<Aes256Gcm>::from_slice(key));
    let mut nonce_bytes = [0u8; 12];
    AesOsRng.fill_bytes(&mut nonce_bytes);
    let nonce = AesNonce::from_slice(&nonce_bytes);
    let ct = cipher.encrypt(nonce, plaintext).map_err(|e| e.to_string())?;
    let mut blob = nonce_bytes.to_vec();
    blob.extend_from_slice(&ct);
    Ok(base64_encode(&blob))
}

/// Decrypt a base64(NONCE | CIPHERTEXT) AES-256-GCM blob.
fn aes_decrypt(key: &[u8; 32], b64: &str) -> Result<Vec<u8>, String> {
    let blob = base64_decode(b64).map_err(|e| e.to_string())?;
    if blob.len() < 12 { return Err("history blob too short".to_string()); }
    let (nonce_bytes, ct) = blob.split_at(12);
    let cipher = Aes256Gcm::new(AesKey::<Aes256Gcm>::from_slice(key));
    let nonce = AesNonce::from_slice(nonce_bytes);
    cipher.decrypt(nonce, ct).map_err(|_| "history decryption failed".to_string())
}

fn base64_encode(data: &[u8]) -> String {
    use std::io::Write;
    let mut enc = Vec::new();
    {
        let mut b64 = base64::write::EncoderWriter::new(&mut enc, &base64::engine::general_purpose::STANDARD);
        b64.write_all(data).ok();
    }
    String::from_utf8(enc).unwrap_or_default()
}

fn base64_decode(s: &str) -> Result<Vec<u8>, base64::DecodeError> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.decode(s)
}

/// Load and decrypt history entries from disk.
fn load_history(key: &[u8; 32], path: &PathBuf) -> Vec<HistoryEntry> {
    match std::fs::read_to_string(path) {
        Ok(b64) if !b64.trim().is_empty() => {
            match aes_decrypt(key, b64.trim()) {
                Ok(plain) => serde_json::from_slice::<Vec<HistoryEntry>>(&plain)
                    .unwrap_or_default(),
                Err(e) => { log::error!("[History] Decrypt error: {}", e); vec![] }
            }
        }
        _ => vec![],
    }
}

/// Encrypt and save history entries to disk.
fn save_history(key: &[u8; 32], path: &PathBuf, entries: &[HistoryEntry]) -> Result<(), String> {
    let json = serde_json::to_vec(entries).map_err(|e| e.to_string())?;
    let b64 = aes_encrypt(key, &json)?;
    std::fs::write(path, b64).map_err(|e| e.to_string())
}

// ============================================================
//  Payment mode (semi-auto / auto)
// ============================================================

/// Set the payment mode. "semi-auto" = queue pending, "auto" = send directly.
#[tauri::command]
async fn set_payment_mode(
    semi_auto: bool,
    app_state: tauri::State<'_, Mutex<AppState>>,
) -> Result<(), String> {
    let mut s = app_state.lock().map_err(|e| e.to_string())?;
    s.semi_auto_mode = semi_auto;
    log::info!("[PaymentMode] Mode set to {}", if semi_auto { "semi-auto" } else { "auto" });
    Ok(())
}

/// Get current payment mode.
#[tauri::command]
async fn get_payment_mode(
    app_state: tauri::State<'_, Mutex<AppState>>,
) -> Result<bool, String> {
    let s = app_state.lock().map_err(|e| e.to_string())?;
    Ok(s.semi_auto_mode)
}

/// Approve a queued payment (semi-auto mode). Removes from pending list.
/// The scheduler will pick it up on its next cycle as approved.
#[tauri::command]
async fn approve_payment(
    sub_id: String,
    app_state: tauri::State<'_, Mutex<AppState>>,
) -> Result<(), String> {
    let mut s = app_state.lock().map_err(|e| e.to_string())?;
    // Remove from pending — scheduler will treat it as approved on next run
    // by checking `approved_payments` set
    s.pending_approvals.remove(&sub_id);
    s.approved_payments.insert(sub_id.clone());
    log::info!("[PaymentMode] Payment approved by user for sub {}", &sub_id[..8.min(sub_id.len())]);
    Ok(())
}

/// Reject a queued payment (semi-auto mode). Defers it to next occurrence.
#[tauri::command]
async fn reject_payment(
    sub_id: String,
    app_state: tauri::State<'_, Mutex<AppState>>,
) -> Result<(), String> {
    let mut s = app_state.lock().map_err(|e| e.to_string())?;
    s.pending_approvals.remove(&sub_id);
    log::info!("[PaymentMode] Payment rejected by user for sub {}", &sub_id[..8.min(sub_id.len())]);
    Ok(())
}

/// Get all payments currently pending user approval.
#[tauri::command]
async fn get_pending_approvals(
    app_state: tauri::State<'_, Mutex<AppState>>,
) -> Result<Vec<PendingApproval>, String> {
    let s = app_state.lock().map_err(|e| e.to_string())?;
    Ok(s.pending_approvals.values().cloned().collect())
}

/// Configure payment history (enabled toggle + retention days).
#[tauri::command]
async fn set_history_config(
    enabled: Option<bool>,
    days: Option<u32>,
    app_state: tauri::State<'_, Mutex<AppState>>,
) -> Result<(), String> {
    let mut s = app_state.lock().map_err(|e| e.to_string())?;
    if let Some(v) = enabled { s.history_enabled = v; }
    if let Some(d) = days { s.history_days = d.max(1).min(3650); }
    Ok(())
}

/// Append a new payment entry to the encrypted history file.
/// Called internally by the payment scheduler after a successful transfer.
fn append_history_entry(state: &AppState, entry: HistoryEntry) {
    if !state.history_enabled { return; }
    let key = match &state.history_key { Some(k) => k, None => return };
    let cutoff_days = state.history_days as i64;
    let mut entries = load_history(key, &state.history_path);
    // Prune old entries
    let cutoff = chrono::Utc::now() - chrono::Duration::days(cutoff_days);
    entries.retain(|e| {
        chrono::DateTime::parse_from_rfc3339(&e.sent_at)
            .map(|t| t.with_timezone(&chrono::Utc) > cutoff)
            .unwrap_or(true)
    });
    entries.push(entry);
    if let Err(e) = save_history(key, &state.history_path, &entries) {
        log::error!("[History] Save error: {}", e);
    }
}

/// Get the decrypted payment history (returns empty if locked or history disabled).
#[tauri::command]
async fn get_payment_history(
    app_state: tauri::State<'_, Mutex<AppState>>,
) -> Result<serde_json::Value, String> {
    let s = app_state.lock().map_err(|e| e.to_string())?;
    if !s.history_enabled {
        return Ok(serde_json::json!({ "enabled": false, "entries": [] }));
    }
    let key = match &s.history_key {
        Some(k) => k,
        None => return Ok(serde_json::json!({ "enabled": true, "entries": [], "locked": true })),
    };
    let entries = load_history(key, &s.history_path);
    Ok(serde_json::json!({ "enabled": true, "entries": entries }))
}

/// Clear all payment history from disk.
#[tauri::command]
async fn clear_payment_history(
    app_state: tauri::State<'_, Mutex<AppState>>,
) -> Result<(), String> {
    let s = app_state.lock().map_err(|e| e.to_string())?;
    if s.history_path.exists() {
        std::fs::remove_file(&s.history_path).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Get history config (enabled, days).
#[tauri::command]
async fn get_history_config(
    app_state: tauri::State<'_, Mutex<AppState>>,
) -> Result<serde_json::Value, String> {
    let s = app_state.lock().map_err(|e| e.to_string())?;
    Ok(serde_json::json!({ "enabled": s.history_enabled, "days": s.history_days }))
}

/// Update alert configuration (threshold and enabled kinds).
#[tauri::command]
async fn set_alert_config(
    balance_low_threshold_days: Option<f64>,
    enabled_alerts: Option<Vec<String>>,
    app_state: tauri::State<'_, Mutex<AppState>>,
) -> Result<(), String> {
    let mut s = app_state.lock().map_err(|e| e.to_string())?;
    if let Some(days) = balance_low_threshold_days {
        s.balance_low_threshold_days = days.max(1.0);
    }
    if let Some(kinds) = enabled_alerts {
        s.enabled_alerts = kinds.into_iter().collect();
    }
    Ok(())
}

/// Debug: return raw subscription data from DB for a given id.
#[tauri::command]
async fn get_subscription_debug(
    id: String,
    db_state: tauri::State<'_, Mutex<Database>>,
) -> Result<serde_json::Value, String> {
    let db = db_state.lock().map_err(|e| e.to_string())?;
    let subs = db.list_subscriptions().map_err(|e| e.to_string())?;
    let found = subs.iter().find(|s| s.id == id || s.id.contains(&id));
    match found {
        Some(s) => Ok(serde_json::json!({
            "id": s.id,
            "label": s.label,
            "status": format!("{:?}", s.status),
            "payments_sent": s.payments_sent,
            "next_payment_at": s.next_payment_at.map(|d| d.to_rfc3339()),
            "schedule": s.schedule,
            "amount": s.amount,
        })),
        None => Ok(serde_json::json!({"error": "not found", "all_ids": subs.iter().map(|s| &s.id).collect::<Vec<_>>()})),
    }
}

/// Return last known wallet/daemon heights for sync progress display.
#[tauri::command]
async fn get_sync_info(state: tauri::State<'_, Mutex<AppState>>) -> Result<serde_json::Value, String> {
    let s = state.lock().map_err(|e| e.to_string())?;
    let wallet_h = s.last_wallet_height;
    let daemon_h = s.last_daemon_height;
    let blocks_behind = if daemon_h > wallet_h { daemon_h - wallet_h } else { 0 };
    Ok(serde_json::json!({
        "wallet_height": wallet_h,
        "daemon_height": daemon_h,
        "blocks_behind": blocks_behind,
        "synced": blocks_behind < 5,
    }))
}

/// Get all subscriptions from the local SQLite database.
#[tauri::command]
async fn get_subscriptions(
    db_state: tauri::State<'_, Mutex<Database>>,
) -> Result<Vec<serde_json::Value>, String> {
    let db = db_state.lock().map_err(|e| e.to_string())?;
    let subs = db.list_subscriptions().map_err(|e| e.to_string())?;
    // Serialize to JSON values for the frontend
    let json_subs = subs.iter()
        .map(|s| serde_json::to_value(s).map_err(|e| e.to_string()))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(json_subs)
}

/// Parse and persist a subscription from a crypto-autopay: or monero-request: code.
#[tauri::command]
async fn add_subscription(
    code: String,
    db_state: tauri::State<'_, Mutex<Database>>,
) -> Result<serde_json::Value, String> {
    use autopay_crypto::{parse_code, AutoPayCode};
    use chrono::Utc;
    use uuid::Uuid;

    let parsed = parse_code(&code).map_err(|e| format!("Invalid code: {e:?}"))?;
    let AutoPayCode::V1(v1) = parsed;

    let now = Utc::now();
    // Parse start_date from code if present, otherwise use now
    let next_payment_at = chrono::DateTime::parse_from_rfc3339(&v1.start_date)
        .ok()
        .map(|d| d.with_timezone(&Utc));

    let sub = Subscription {
        id: Uuid::new_v4().to_string(),
        wallet_id: "default".to_string(),
        label: v1.custom_label.clone(),
        description: v1.description.clone(),
        destination_address: v1.sellers_wallet.clone(),
        payment_network: v1.payment_network.clone(),
        amount: v1.amount.clone(),
        currency: v1.currency.clone(),
        schedule: v1.schedule.clone(),
        number_of_payments: v1.number_of_payments,
        payments_sent: 0,
        next_payment_at,
        status: SubscriptionStatus::Active,
        grace_period_hours: v1.grace_period_hours.unwrap_or(24),
        canary_url: v1.canary_url.clone(),
        merchant_pubkey: None,
        last_canary_status: None,
        canary_last_ok: None,
        webhook_url: None,
        created_at: now,
        updated_at: now,
        raw_code: Some(code),
        notes: None,
    };

    {
        let db = db_state.lock().map_err(|e| e.to_string())?;
        db.insert_subscription(&sub).map_err(|e| e.to_string())?;
    }

    log::info!("Subscription '{}' added (id={})", sub.label, sub.id);
    serde_json::to_value(&sub).map_err(|e| e.to_string())
}

/// Save a manually-created subscription directly to DB (no code string needed).
/// Called from frontend mode="new" form submissions.
#[tauri::command]
async fn save_subscription(
    payload: serde_json::Value,
    db_state: tauri::State<'_, Mutex<Database>>,
) -> Result<serde_json::Value, String> {
    use chrono::Utc;
    use uuid::Uuid;

    let now = Utc::now();
    let label = payload["custom_label"].as_str().unwrap_or("—").to_string();
    let dest = payload["destination_address"].as_str().unwrap_or("").to_string();
    let amount = payload["amount"].as_str().unwrap_or("0").to_string();
    let currency = payload["currency"].as_str().unwrap_or("XMR").to_string();
    let schedule = payload["schedule"].as_str().unwrap_or("0 0 9 1 * *").to_string();
    let number_of_payments = payload["number_of_payments"].as_u64().unwrap_or(1);
    let start_immediately = payload["start_immediately"].as_bool().unwrap_or(true);
    let canary_url = payload["canary_url"].as_str().filter(|s| !s.is_empty()).map(|s| s.to_string());
    let description = payload["description"].as_str().unwrap_or("").to_string();

    // next_payment_at: now if start_immediately, else use start_date or compute from schedule
    let next_payment_at = if start_immediately {
        Some(now)
    } else if let Some(sd) = payload["start_date"].as_str().filter(|s| !s.is_empty()) {
        chrono::DateTime::parse_from_rfc3339(sd).ok().map(|d| d.with_timezone(&Utc))
            .or(Some(now + chrono::Duration::days(1)))
    } else {
        // Parse cron period for next occurrence
        let period_min = cron_period_minutes(&schedule);
        Some(now + chrono::Duration::minutes(period_min))
    };

    let sub = Subscription {
        id: Uuid::new_v4().to_string(),
        wallet_id: "default".to_string(),
        label: label.clone(),
        description: if description.is_empty() { None } else { Some(description) },
        destination_address: dest,
        payment_network: payload["network"].as_str().unwrap_or("monero").to_string(),
        amount,
        currency,
        schedule,
        number_of_payments,
        payments_sent: 0,
        next_payment_at,
        status: SubscriptionStatus::Active,
        grace_period_hours: 24,
        canary_url,
        merchant_pubkey: None,
        last_canary_status: None,
        canary_last_ok: None,
        webhook_url: None,
        created_at: now,
        updated_at: now,
        raw_code: None,
        notes: None,
    };

    {
        let db = db_state.lock().map_err(|e| e.to_string())?;
        db.insert_subscription(&sub).map_err(|e| e.to_string())?;
    }

    log::info!("Subscription '{}' saved (id={}, start_immediately={})", sub.label, sub.id, start_immediately);
    serde_json::to_value(&sub).map_err(|e| e.to_string())
}

/// Get the list of known public Monero nodes.
#[tauri::command]
fn get_public_nodes() -> Vec<String> {
    PUBLIC_NODES.iter().map(|s| s.to_string()).collect()
}

/// Check if a monero-wallet-rpc binary can be found.
#[tauri::command]
fn detect_rpc_binary(state: tauri::State<'_, Mutex<AppState>>) -> Option<String> {
    let s = state.lock().unwrap();
    s.rpc.detect_binary().map(|p| p.to_string_lossy().to_string())
}

/// Get current RPC status.
#[tauri::command]
fn get_rpc_status(state: tauri::State<'_, Mutex<AppState>>) -> RpcStatus {
    let s = state.lock().unwrap();
    s.rpc.status.clone()
}

/// Save RPC configuration (node, binary path, tolerance).
#[tauri::command]
fn save_rpc_config(
    config: RpcConfig,
    app_handle: tauri::AppHandle,
    state: tauri::State<'_, Mutex<AppState>>,
) -> Result<(), String> {
    let data_dir = app_handle.path().app_data_dir().map_err(|e| e.to_string())?;
    let path = data_dir.join("rpc_config.json");
    let json = serde_json::to_string_pretty(&config).map_err(|e| e.to_string())?;
    std::fs::write(&path, json).map_err(|e| e.to_string())?;
    // Update live config (takes effect on next start)
    let mut s = state.lock().unwrap();
    s.rpc = RpcManager::new(config);
    Ok(())
}

/// Start wallet-rpc (only called when a payment is due or user explicitly requests it).
#[tauri::command]
fn start_wallet_rpc(
    wallet_password: String,
    state: tauri::State<'_, Mutex<AppState>>,
) -> Result<String, String> {
    let mut s = state.lock().map_err(|e| e.to_string())?;
    let wallet_path = s.wallet_file_path.clone();
    s.rpc.start(&wallet_path, &wallet_password)?;
    Ok(s.rpc.rpc_url())
}

/// Stop wallet-rpc.
#[tauri::command]
fn stop_wallet_rpc(state: tauri::State<'_, Mutex<AppState>>) {
    let mut s = state.lock().unwrap();
    s.rpc.stop();
}

/// Delete a subscription by id.
#[tauri::command]
async fn delete_subscription(
    id: String,
    db_state: tauri::State<'_, Mutex<Database>>,
) -> Result<(), String> {
    let db = db_state.lock().map_err(|e| e.to_string())?;
    db.delete_subscription(&id).map_err(|e| e.to_string())
}

/// Pause or resume a subscription.
#[tauri::command]
async fn set_subscription_status(
    id: String,
    paused: bool,
    db_state: tauri::State<'_, Mutex<Database>>,
) -> Result<(), String> {
    let status = if paused { SubscriptionStatus::Paused } else { SubscriptionStatus::Active };
    let db = db_state.lock().map_err(|e| e.to_string())?;
    db.update_subscription_status(&id, &status).map_err(|e| e.to_string())
}

// ============================================================
//  Response types
// ============================================================

#[derive(Serialize, Deserialize)]
struct BalanceResponse {
    xmr: f64,
    atomic: u64,
}

/// Result returned by unlock_wallet — never Err on RPC failure, always Ok.
/// Password errors return Err("Incorrect password.").
#[derive(Serialize, Deserialize)]
struct UnlockResult {
    /// Monero address (empty string if RPC unavailable).
    address: String,
    /// XMR balance at unlock time (0.0 if RPC unavailable).
    balance_xmr: f64,
    /// If Some, an RPC/network error occurred but the wallet was still unlocked.
    rpc_error: Option<String>,
    /// True if the stored seed phrase failed checksum verification (old buggy version).
    seed_invalid: bool,
}

// ============================================================
//  App entry point
// ============================================================

/// Parse a CRON expression to its period in minutes (best effort).
/// Supports: "0 */N * * * *" (every N min), "0 0 * * * *" (hourly), "0 0 H * * *" (daily).
fn cron_period_minutes(cron: &str) -> i64 {
    let parts: Vec<&str> = cron.trim().split_whitespace().collect();
    // 6-field: sec min hour dom month dow
    let min_field = if parts.len() >= 6 { parts[1] } else if parts.len() >= 5 { parts[0] } else { "*" };
    let hour_field = if parts.len() >= 6 { parts[2] } else if parts.len() >= 5 { parts[1] } else { "*" };
    if min_field.starts_with("*/") {
        if let Ok(n) = min_field[2..].parse::<i64>() { return n.max(25); }
    }
    if min_field == "0" && hour_field == "*" { return 60; }      // hourly
    if min_field == "0" { return 24 * 60; }                      // daily
    30 * 24 * 60 // monthly fallback
}

/// Update the system tray icon based on current alert level.
fn update_tray_icon(app: &tauri::AppHandle, level: Option<AlertLevel>, alert_count: usize) {
    let icon_bytes: &[u8] = match level {
        Some(AlertLevel::Red)    => include_bytes!("../icons/tray-alert.png"),
        Some(AlertLevel::Orange) => include_bytes!("../icons/tray-warn.png"),
        None                     => include_bytes!("../icons/tray-ok.png"),
    };
    let tooltip = match level {
        Some(AlertLevel::Red)    => format!("Crypto AutoPay — {} alerte(s) critique(s)", alert_count),
        Some(AlertLevel::Orange) => format!("Crypto AutoPay — {} avertissement(s)", alert_count),
        None                     => "Crypto AutoPay — Tout va bien".to_string(),
    };
    if let Ok(img) = tauri::image::Image::from_bytes(icon_bytes) {
        if let Some(tray) = app.tray_by_id("main") {
            let _ = tray.set_icon(Some(img));
            let _ = tray.set_tooltip(Some(&tooltip));
        }
    }
}

// ============================================================
//  Treasurer mode commands
// ============================================================

#[tauri::command]
async fn set_treasurer_mode(
    state: tauri::State<'_, Mutex<AppState>>,
    enabled: bool,
) -> Result<(), String> {
    let mut s = state.lock().map_err(|e| e.to_string())?;
    s.treasurer_mode = enabled;
    Ok(())
}

#[tauri::command]
async fn get_treasurer_mode(
    state: tauri::State<'_, Mutex<AppState>>,
) -> Result<bool, String> {
    let s = state.lock().map_err(|e| e.to_string())?;
    Ok(s.treasurer_mode)
}

/// A Procès-Verbal payment order (JSON imported by the user).
/// Only XMR is supported in v1; Bitcoin address is stored for future.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PvPayment {
    pub id: String,
    pub recipient: String,          // human-readable label
    pub address: String,            // Monero address
    pub amount_xmr: f64,
    pub currency: String,           // "XMR" only for now
    pub execution: String,          // "immediate" or ISO-8601 datetime
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PvDocument {
    pub version: u32,
    pub id: String,
    pub created_at: String,
    pub committee: String,
    pub resolution_number: String,
    pub description: String,
    pub payments: Vec<PvPayment>,
}

/// Validate a PV JSON string and return parsed document (does NOT execute payments).
#[tauri::command]
async fn validate_pv(pv_json: String) -> Result<PvDocument, String> {
    serde_json::from_str::<PvDocument>(&pv_json)
        .map_err(|e| format!("Invalid PV format: {e}"))
}

/// Execute a single payment from a PV document (XMR only).
/// Returns the tx prefix hash.
#[tauri::command]
async fn execute_pv_payment(
    state: tauri::State<'_, Mutex<AppState>>,
    app: tauri::AppHandle,
    payment_json: String,
) -> Result<String, String> {
    let payment: PvPayment = serde_json::from_str(&payment_json)
        .map_err(|e| format!("Invalid payment JSON: {e}"))?;

    if payment.currency != "XMR" {
        return Err("Only XMR payments are supported in this version.".to_string());
    }

    let (wallet_name, wallet_dir, password, rpc_busy) = {
        let s = state.lock().map_err(|e| e.to_string())?;
        if !s.unlocked { return Err("Wallet is locked.".to_string()); }
        if !s.treasurer_mode { return Err("Treasurer mode is not enabled.".to_string()); }
        let busy = Arc::clone(&s.rpc_busy);
        let pw = s.session_password.clone().ok_or("No session password")?;
        let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
        let name = s.wallet_file_path.file_name()
            .and_then(|n| n.to_str()).unwrap_or("autopay_wallet").to_string();
        (name, dir, pw, busy)
    };

    if rpc_busy.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).is_err() {
        return Err("RPC busy — another operation is in progress. Try again in a moment.".to_string());
    }

    struct RpcGuard(Arc<AtomicBool>);
    impl Drop for RpcGuard { fn drop(&mut self) { self.0.store(false, Ordering::SeqCst); } }
    let _guard = RpcGuard(rpc_busy);

    let wallet_file = wallet_dir.join(&wallet_name);
    let rpc_config = {
        let s = state.lock().map_err(|e| e.to_string())?;
        s.rpc.config.clone()
    };

    let mut rpc = crate::rpc_manager::RpcManager::new(rpc_config);
    rpc.start(&wallet_file, &password)
        .map_err(|e| format!("Failed to start RPC: {e}"))?;

    let amount_pico = (payment.amount_xmr * 1e12) as u64;
    let rpc_url = rpc.rpc_url();

    // Open wallet
    let _ = rpc_manager::ureq_post_json_pub(&rpc_url, &serde_json::json!({
        "jsonrpc": "2.0", "id": "1", "method": "open_wallet",
        "params": { "filename": wallet_name, "password": password }
    }));

    // Refresh wallet (sync with daemon)
    let _ = rpc_manager::ureq_post_json_long(&rpc_url, &serde_json::json!({
        "jsonrpc": "2.0", "id": "1", "method": "refresh", "params": {}
    }), 120);

    let result = rpc_manager::ureq_post_json_pub(&rpc_url, &serde_json::json!({
        "jsonrpc": "2.0", "id": "1", "method": "transfer",
        "params": {
            "destinations": [{ "amount": amount_pico, "address": payment.address }],
            "priority": 0,
            "ring_size": 16,
            "get_tx_key": false,
        }
    }));

    rpc.stop();

    match result {
        Ok(v) => {
            let tx_hash = v["result"]["tx_hash"].as_str()
                .or_else(|| v["result"]["tx_hash_list"][0].as_str())
                .unwrap_or("unknown")
                .to_string();
            // Log without amount or address for security
            let prefix_len = std::cmp::min(8, tx_hash.len());
            log::info!("[Treasurer] PV payment executed: id={} tx_prefix={}…", payment.id, &tx_hash[..prefix_len]);
            Ok(tx_hash)
        }
        Err(e) => Err(format!("Payment failed: {e}")),
    }
}

/// Minimum interval between Monero sends (10 confirmations ≈ 20 min, use 25 min safely).
const MONERO_MIN_SEND_INTERVAL_SECS: u64 = 25 * 60;

/// Background scheduler: check for due subscriptions and send payments.
/// Called every 60 seconds from a tokio::spawn task.
async fn run_payment_scheduler(app: &tauri::AppHandle) {
    use chrono::Utc;

    let (session_pw, wallet_dir, wallet_name, last_sent) = {
        let state = match app.try_state::<Mutex<AppState>>() {
            Some(s) => s,
            None => return,
        };
        let s = match state.lock() {
            Ok(s) => s,
            Err(_) => return,
        };
        if !s.unlocked { return; }
        // Grace period: skip first 45s after unlock so the initial balance sync completes first
        if s.last_unlock_at > 0 {
            let since_unlock = Utc::now().timestamp() - s.last_unlock_at;
            if since_unlock < 45 {
                log::debug!("[Scheduler] Grace period: {}s since unlock, skipping this cycle", since_unlock);
                return;
            }
        }
        let pw = match &s.session_password {
            Some(p) => p.clone(),
            None => return,
        };
        let dir = match app.path().app_data_dir() {
            Ok(d) => d,
            Err(_) => return,
        };
        let name = s.wallet_file_path.file_name()
            .and_then(|n| n.to_str()).unwrap_or("autopay_wallet").to_string();
        let last = s.last_payment_sent_at;
        (pw, dir, name, last)
    };

    // Enforce 25-minute minimum between Monero sends
    if let Some(last) = last_sent {
        let elapsed = Utc::now().timestamp() - last;
        if elapsed < MONERO_MIN_SEND_INTERVAL_SECS as i64 {
            log::debug!("[Scheduler] Monero 25-min cooldown: {}s remaining", MONERO_MIN_SEND_INTERVAL_SECS as i64 - elapsed);
            return;
        }
    }

    // Read due subscriptions from DB
    let db_state = match app.try_state::<Mutex<Database>>() {
        Some(s) => s,
        None => return,
    };
    let subs = match db_state.lock() {
        Ok(db) => match db.list_subscriptions() {
            Ok(s) => s,
            Err(e) => { log::warn!("[Scheduler] DB read failed: {}", e); return; }
        },
        Err(_) => return,
    };

    let now = Utc::now();
    let due: Vec<_> = subs.into_iter().filter(|s| {
        if s.status != autopay_core::models::SubscriptionStatus::Active { return false; }
        // Check max_payments (stored in grace_period_hours field temporarily)
        // TODO: add proper max_payments field to DB
        if let Some(next) = s.next_payment_at {
            next <= now
        } else {
            false
        }
    }).collect();

    if due.is_empty() { return; }
    log::info!("[Scheduler] {} subscription(s) due", due.len());

    // Read semi-auto state once before the loop
    let (semi_auto, approved_ids) = {
        if let Some(state) = app.try_state::<Mutex<AppState>>() {
            if let Ok(mut s) = state.lock() {
                let semi = s.semi_auto_mode;
                let approved: std::collections::HashSet<String> = s.approved_payments.drain().collect();
                (semi, approved)
            } else { (false, Default::default()) }
        } else { (false, Default::default()) }
    };

    for sub in due {
        let dest = sub.destination_address.clone();
        let amount_pico = (sub.amount.parse::<f64>().unwrap_or(0.0) * 1e12) as u64;
        let sub_id = sub.id.clone();

        // Semi-auto mode: queue for user approval unless already approved this session
        if semi_auto && !approved_ids.contains(&sub_id) {
            log::info!("[Scheduler] Semi-auto: queuing payment for sub {} — awaiting user approval", &sub_id[..8.min(sub_id.len())]);
            if let Some(state) = app.try_state::<Mutex<AppState>>() {
                if let Ok(mut s) = state.lock() {
                    s.pending_approvals.insert(sub_id.clone(), PendingApproval {
                        sub_id: sub_id.clone(),
                        sub_label: sub.label.clone(),
                        amount_xmr: sub.amount.clone(),
                        queued_at: Utc::now().to_rfc3339(),
                    });
                }
            }
            // Compute last_payment human-readable
            let last_payment_iso = sub.next_payment_at
                .map(|next| {
                    let period = cron_period_minutes(&sub.schedule);
                    (next - chrono::Duration::minutes(period)).to_rfc3339()
                })
                .unwrap_or_default();
            app.emit("payment-pending", serde_json::json!({
                "sub_id": sub_id,
                "sub_label": sub.label,
                "amount_xmr": sub.amount,
                "schedule": sub.schedule,
                "payments_sent": sub.payments_sent,
                "last_payment_at": last_payment_iso,
            })).ok();
            continue; // skip to next sub — scheduler will retry next cycle
        }

        log::info!("[Scheduler] Sending payment for sub {}", sub_id);

        let wallet_dir2 = wallet_dir.clone();
        let wallet_name2 = wallet_name.clone();
        let pw2 = session_pw.clone();

        let result = tokio::task::spawn_blocking(move || {
            let mut mgr = rpc_manager::RpcManager::new(rpc_manager::RpcConfig::default());
            mgr.start_for_dir(&wallet_dir2).map_err(|e| e.to_string())?;
            mgr.wait_for_ready(std::time::Duration::from_secs(15)).map_err(|e| e.to_string())?;

            let open = rpc_manager::ureq_post_json_pub(&mgr.rpc_url(), &serde_json::json!({
                "jsonrpc":"2.0","id":"o","method":"open_wallet",
                "params":{"filename":wallet_name2,"password":pw2}
            })).map_err(|e| e.to_string())?;
            if let Some(err) = open.get("error") {
                return Err(format!("open_wallet: {}", err));
            }

            let resp = rpc_manager::ureq_post_json_pub(&mgr.rpc_url(), &serde_json::json!({
                "jsonrpc":"2.0","id":"tx","method":"transfer",
                "params":{
                    "destinations":[{"amount": amount_pico, "address": dest}],
                    "account_index": 0, "priority": 1
                }
            })).map_err(|e| e.to_string())?;
            mgr.stop();

            if let Some(err) = resp.get("error") {
                let msg = err.get("message").and_then(|m| m.as_str()).unwrap_or("?");
                return Err(format!("transfer error: {}", msg));
            }
            Ok(resp["result"]["tx_hash"].as_str().unwrap_or("").to_string())
        }).await;

        match result {
            Ok(Ok(tx_hash)) => {
                log::info!("[Scheduler] Payment sent for sub {} (tx={})", sub_id, tx_hash);
                // Update last_payment_sent_at in AppState
                if let Some(state) = app.try_state::<Mutex<AppState>>() {
                    if let Ok(mut s) = state.lock() {
                        s.last_payment_sent_at = Some(Utc::now().timestamp());
                    }
                }
                // Update next_payment_at in DB — compute next occurrence
                // For now, advance by the period using chrono; TODO: full CRON parsing
                if let Some(next) = sub.next_payment_at {
                    let period_min = cron_period_minutes(&sub.schedule);
                    let new_next = next + chrono::Duration::minutes(period_min);
                    if let Ok(db) = db_state.lock() {
                        if let Err(e) = db.update_next_payment(&sub_id, Some(new_next), sub.payments_sent + 1) {
                            log::error!("[Scheduler] DB update failed for sub {}: {}", sub_id, e);
                        } else {
                            log::info!("[Scheduler] next_payment_at advanced to {} for sub {}", new_next.to_rfc3339(), sub_id);
                        }
                    }
                }
                // Emit event so frontend can refresh balance and subscription list
                let new_next_iso = if let Some(next) = sub.next_payment_at {
                    let p = cron_period_minutes(&sub.schedule);
                    (next + chrono::Duration::minutes(p)).to_rfc3339()
                } else { String::new() };
                app.emit("payment-sent", serde_json::json!({"sub_id": sub_id, "tx_hash": tx_hash, "next_payment_at": new_next_iso})).ok();
                // Append to encrypted payment history
                if let Some(app_state) = app.try_state::<Mutex<AppState>>() {
                    if let Ok(s) = app_state.lock() {
                        let tx_prefix = if tx_hash.len() >= 12 { tx_hash[..12].to_string() } else { tx_hash.clone() };
                        let entry = HistoryEntry {
                            id: uuid::Uuid::new_v4().to_string(),
                            sub_id: sub_id.clone(),
                            sub_label: sub.label.clone(),
                            amount_xmr: sub.amount.parse::<f64>().unwrap_or(0.0),
                            sent_at: chrono::Utc::now().to_rfc3339(),
                            tx_prefix,
                        };
                        append_history_entry(&s, entry);
                    }
                }
            }
            Ok(Err(e)) => {
                let is_locked = e.contains("not enough unlocked") || e.contains("locked") || e.contains("unlocked money");
                if is_locked {
                    // Outputs locked: reschedule for 25 min later (one Monero block confirmation cycle)
                    log::warn!("[Scheduler] Sub {} — outputs still locked, rescheduling +25min", sub_id);
                    let retry_at = chrono::Utc::now() + chrono::Duration::minutes(25);
                    if let Some(db_rc) = app.try_state::<Mutex<Database>>() {
                        if let Ok(db) = db_rc.lock() {
                            if let Err(e) = db.update_next_payment(&sub_id, Some(retry_at), sub.payments_sent) {
                                log::error!("[Scheduler] DB retry-defer failed for sub {}: {}", sub_id, e);
                            } else {
                                log::info!("[Scheduler] next_payment_at deferred +25min to {} for sub {}", retry_at.to_rfc3339(), sub_id);
                            }
                        }
                    }
                    // Orange alert: outputs locked, retry scheduled
                    if let Some(state_rc) = app.try_state::<Mutex<AppState>>() {
                        if let Ok(mut s) = state_rc.lock() {
                            s.alerts.retain(|a| a.kind != "OUTPUTS_LOCKED" || a.sub_id.as_deref() != Some(&sub_id));
                            s.alerts.push(AppAlert {
                                level: AlertLevel::Orange,
                                kind: "OUTPUTS_LOCKED".into(),
                                sub_id: Some(sub_id.clone()),
                                message: format!("Sub #{} — fonds verrouilles (confirmation en cours), retry dans 25 min", sub_id),
                                created_at: chrono::Utc::now().timestamp(),
                            });
                            let count = s.alerts.len();
                            drop(s);
                            update_tray_icon(app, Some(AlertLevel::Orange), count);
                        }
                    }
                    app.emit("payment-locked", serde_json::json!({"sub_id": sub_id, "retry_in_min": 25, "next_payment_at": retry_at.to_rfc3339()})).ok();
                } else {
                    log::error!("[Scheduler] Payment failed for sub {}: {}", sub_id, e);
                    if let Some(state_rc) = app.try_state::<Mutex<AppState>>() {
                        if let Ok(mut s) = state_rc.lock() {
                            s.alerts.retain(|a| a.kind != "PAYMENT_FAILED" || a.sub_id.as_deref() != Some(&sub_id));
                            s.alerts.push(AppAlert {
                                level: AlertLevel::Orange,
                                kind: "PAYMENT_FAILED".into(),
                                sub_id: Some(sub_id.clone()),
                                message: format!("Payment failed for sub #{}: {}", sub_id, e),
                                created_at: chrono::Utc::now().timestamp(),
                            });
                            let count = s.alerts.len();
                            drop(s);
                            update_tray_icon(app, Some(AlertLevel::Orange), count);
                        }
                    }
                }
            }
            Err(e) => log::error!("[Scheduler] Task panic for sub {}: {}", sub_id, e),
        }
        // Only send one payment per cycle to respect 25-min Monero rule
        break;
    }

    // Recompute alerts + update tray icon after each scheduler run
    if let (Some(state_rc), Some(db_rc)) = (app.try_state::<Mutex<AppState>>(), app.try_state::<Mutex<Database>>()) {
        let subs = db_rc.lock().ok().and_then(|db| db.list_subscriptions().ok()).unwrap_or_default();
        if let Ok(mut s) = state_rc.lock() {
            let now_ts = chrono::Utc::now().timestamp();
            let worst = recompute_alerts(&mut s, &subs, now_ts);
            let count = s.alerts.len();
            drop(s);
            update_tray_icon(app, worst, count);
        }
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_deep_link::init())
        .plugin(
            tauri_plugin_single_instance::init(|app, args, _cwd| {
                // A second instance was launched (e.g. from a deep link click)
                // Forward any crypto-autopay: URL from its args to the running instance
                for arg in &args {
                    if arg.starts_with("crypto-autopay:") {
                        log::info!("Single-instance redirect: {}", arg);
                        // Store in state (in case frontend polls for it)
                        if let Some(state) = app.try_state::<Mutex<AppState>>() {
                            if let Ok(mut s) = state.lock() {
                                s.pending_deep_link = Some(arg.clone());
                            }
                        }
                        app.emit("autopay://open-subscription", arg.clone()).ok();
                    }
                }
                // Bring the existing window to front (show if hidden, then focus)
                if let Some(win) = app.get_webview_window("main") {
                    win.show().ok();
                    win.unminimize().ok();
                    win.set_focus().ok();
                }
            })
        )
        .setup(|app| {
            // Resolve app data directory
            let data_dir = app.path().app_data_dir()
                .expect("Failed to resolve app data dir");
            std::fs::create_dir_all(&data_dir).ok();

            let vault_path      = data_dir.join("seed.vault");
            let wallet_file_path = data_dir.join("autopay_wallet");
            let db_path          = data_dir.join("subscriptions.db");
            let history_path    = data_dir.join("history.vault");

            // Open the subscriptions SQLite database
            let db = Database::open(db_path.to_str().unwrap())
                .expect("Failed to open subscriptions DB");
            app.manage(Mutex::new(db));

            // Load RPC config from settings (or use defaults)
            let rpc_config_path = data_dir.join("rpc_config.json");
            let rpc_config: RpcConfig = std::fs::read_to_string(&rpc_config_path)
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default();

            app.manage(Mutex::new(AppState {
                vault_path,
                wallet_file_path,
                unlocked: false,
                wallet_address: None,
                balance_atomic: 0,
                rpc_connected: false,
                rpc_last_error: None,
                session_password: None,
                pending_deep_link: None,
                last_payment_sent_at: None,
                last_wallet_height: 0,
                last_daemon_height: 0,
                last_daemon_contact_at: None,
                alerts: Vec::new(),
                rpc_busy: Arc::new(AtomicBool::new(false)),
                rpc: RpcManager::new(rpc_config),
                balance_low_threshold_days: 7.0,
                enabled_alerts: {
                    let mut s = std::collections::HashSet::new();
                    for k in &["BALANCE_LOW","BALANCE_EMPTY","PAYMENT_FAILED","CANARY_EXPIRED",
                               "CANARY_UNREACHABLE","PAYMENT_OVERDUE_SOFT","PAYMENT_OVERDUE_HARD",
                               "WALLET_NOT_SYNCED","OUTPUTS_LOCKED","OFFLINE_WITH_PENDING"] {
                        s.insert(k.to_string());
                    }
                    s
                },
                history_key: None,
                history_path,
                history_enabled: true,
                history_days: 365,
                semi_auto_mode: true, // safe default: require user confirmation
                pending_approvals: std::collections::HashMap::new(),
                approved_payments: std::collections::HashSet::new(),
                last_unlock_at: 0,
                treasurer_mode: false,
            }));

            // Enable logging to both stdout and webview (always, not just debug)
            // The webview target lets the frontend receive Rust logs via @tauri-apps/plugin-log
            app.handle().plugin(
                tauri_plugin_log::Builder::default()
                    .level(log::LevelFilter::Info)
                    .targets([
                        tauri_plugin_log::Target::new(tauri_plugin_log::TargetKind::Stdout),
                        tauri_plugin_log::Target::new(tauri_plugin_log::TargetKind::Webview),
                    ])
                    .build(),
            )?;

            // Background payment scheduler — runs every 60s, independent of UI
            {
                let sched_handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    // Wait 30s before first check (let app fully initialize)
                    tauri::async_runtime::spawn(async {}).await.ok(); // yield to runtime
                    tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                    let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
                    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                    loop {
                        interval.tick().await;
                        run_payment_scheduler(&sched_handle).await;
                    }
                });
            }

            // Deep link handler — listen for scheme events, store and re-emit to frontend
            let handle = app.handle().clone();
            let handle2 = app.handle().clone(); // second clone for state access inside closure
            app.handle().listen("deep-link://new-url", move |event| {
                // Payload is a JSON array of URL strings: ["crypto-autopay:1:..."]
                let payload = event.payload().to_string();
                let raw = payload.trim();
                let url = if raw.starts_with('[') {
                    // JSON array: strip brackets and quotes
                    raw.trim_start_matches('[').trim_end_matches(']').trim().trim_matches('"').to_string()
                } else {
                    raw.to_string()
                };
                if url.starts_with("crypto-autopay:") {
                    log::info!("Deep link received: {}", url);
                    // Store so the frontend can poll for it after unlock
                    if let Some(state) = handle2.try_state::<Mutex<AppState>>() {
                        if let Ok(mut s) = state.lock() {
                            s.pending_deep_link = Some(url.clone());
                        }
                    }
                    handle.emit("autopay://open-subscription", url).ok();
                }
            });

            // System tray: minimize-to-tray with alert icon
            let tray_icon_bytes = include_bytes!("../icons/tray-ok.png");
            if let Ok(img) = tauri::image::Image::from_bytes(tray_icon_bytes) {
                // Build tray menu: Open + Quit
                use tauri::menu::{MenuBuilder, MenuItemBuilder};
                let menu_open = MenuItemBuilder::with_id("open", "Ouvrir Crypto AutoPay").build(app)?;
                let menu_quit = MenuItemBuilder::with_id("quit", "Quitter").build(app)?;
                let tray_menu = MenuBuilder::new(app)
                    .item(&menu_open)
                    .separator()
                    .item(&menu_quit)
                    .build()?;

                let _tray = tauri::tray::TrayIconBuilder::with_id("main")
                    .icon(img)
                    .tooltip("Crypto AutoPay")
                    .menu(&tray_menu)
                    .on_menu_event(|app, event| {
                        match event.id().as_ref() {
                            "open" => {
                                if let Some(win) = app.get_webview_window("main") {
                                    win.show().ok();
                                    win.unminimize().ok();
                                    win.set_focus().ok();
                                }
                            }
                            "quit" => {
                                log::info!("[Tray] User requested quit via tray menu");
                                app.exit(0);
                            }
                            _ => {}
                        }
                    })
                    .on_tray_icon_event(|tray, event| {
                        if let tauri::tray::TrayIconEvent::Click { .. } = event {
                            if let Some(win) = tray.app_handle().get_webview_window("main") {
                                win.show().ok();
                                win.unminimize().ok();
                                win.set_focus().ok();
                            }
                        }
                    })
                    .build(app)?;
                log::info!("[Tray] System tray initialized with menu");
            } else {
                log::warn!("[Tray] Could not load tray icon — tray not available");
            }

            Ok(())
        })
        .on_window_event(|window, event| {
            // Close button → minimize to tray instead of quitting
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_pending_deep_link,
            lock_wallet,
            generate_seed,
            check_wallet_exists,
            create_wallet,
            unlock_wallet,
            get_wallet_info,
            get_sync_info,
            get_alerts,
            set_alert_config,
            set_history_config,
            get_history_config,
            get_payment_history,
            clear_payment_history,
            set_payment_mode,
            get_payment_mode,
            approve_payment,
            reject_payment,
            get_pending_approvals,
            set_treasurer_mode,
            get_treasurer_mode,
            validate_pv,
            execute_pv_payment,
            save_subscription,
            get_subscription_debug,
            get_balance,
            refresh_balance,
            sweep_all,
            send_payment,
            update_subscription_next_payment,
            get_subscriptions,
            add_subscription,
            delete_subscription,
            set_subscription_status,
            get_public_nodes,
            detect_rpc_binary,
            get_rpc_status,
            save_rpc_config,
            start_wallet_rpc,
            stop_wallet_rpc,
        ])
        .run(tauri::generate_context!())
        .expect("error while building tauri application");
}
