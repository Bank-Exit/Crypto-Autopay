use std::sync::{Mutex, Arc};
use std::sync::atomic::{AtomicBool, Ordering};
use std::path::PathBuf;
use serde::{Deserialize, Serialize};
use aes_gcm::{aead::{Aead, KeyInit, OsRng as AesOsRng}, Aes256Gcm, Key as AesKey, Nonce as AesNonce};
use rand::RngCore;
use zeroize::Zeroize;
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
mod payment_policy;
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
    /// Node used by the last successful node selection (tried first next time).
    pub active_node: Option<String>,
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
    /// Unlocked exceptions to the payment safety limits (persisted).
    pub payment_limits: payment_policy::PaymentLimits,
}

/// A payment pending user confirmation (semi-auto mode).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingApproval {
    pub sub_id: String,
    pub sub_label: String,
    pub amount_xmr: String,
    pub queued_at: String, // ISO 8601
    /// Why confirmation is required: "semi_auto", "first_payment", "terms_changed", "high_amount".
    #[serde(default)]
    pub reasons: Vec<String>,
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
    // Overwrite secrets before dropping them (copies held by in-flight operations
    // are dropped when those operations end).
    if let Some(mut pw) = s.session_password.take() { pw.zeroize(); }
    if let Some(mut key) = s.history_key.take() { key.zeroize(); }
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
        log::info!("[Unlock] Fast path: using cached address");
        if let Ok(mut s) = state.lock() {
            s.unlocked = true;
            s.last_unlock_at = chrono::Utc::now().timestamp();
            s.session_password = Some(password.clone());
            s.history_key = Some(history_key_with_migration(&password, &s.history_path));
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
    let app_rpc = app_handle.clone();

    let rpc_result: Result<(String, f64), (String, bool)> = tokio::task::spawn_blocking(move || {
        log::info!("[Unlock/RPC] spawn_blocking started");
        let (mut mgr, _) = live_rpc_manager(&app_rpc);
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
            log::info!("[Unlock/RPC] Restore OK");
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
            log::info!("[Unlock/RPC] Address retrieved");
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
    s.history_key = Some(history_key_with_migration(&password, &s.history_path));
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
    Ok(BalanceResponse { xmr: atomic as f64 / 1e12, atomic, daemon_reachable: s.last_daemon_contact_at.is_some() })
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
    let app_rpc = app_handle.clone();
    let result = tokio::task::spawn_blocking(move || {
        // RAII guard: always clear rpc_busy when this closure exits
        struct RpcGuard(Arc<AtomicBool>);
        impl Drop for RpcGuard { fn drop(&mut self) { self.0.store(false, Ordering::SeqCst); } }
        let _guard = RpcGuard(Arc::clone(&busy_clone));

        let (mut mgr, _) = live_rpc_manager(&app_rpc);
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

        if let Err(e) = rpc_manager::ureq_post_json_long(&mgr.rpc_url(), &serde_json::json!({
            "jsonrpc":"2.0","id":"r","method":"refresh","params":{}
        }), 600) {
            log::warn!("[Payment] refresh before transfer failed (continuing): {}", e);
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

    log::info!("[Sweep] Initiating sweep_all");

    let app_rpc = app_handle.clone();
    let tx_hash = tokio::task::spawn_blocking(move || {
        let (mut mgr, _) = live_rpc_manager(&app_rpc);
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

    let app_rpc = app_handle.clone();
    let rpc_result = tokio::task::spawn_blocking(move || {
        struct RpcGuard(Arc<AtomicBool>);
        impl Drop for RpcGuard { fn drop(&mut self) { self.0.store(false, Ordering::SeqCst); } }
        let _guard = RpcGuard(rpc_busy);

        let progress = |phase: &str, extra: serde_json::Value| {
            let mut payload = serde_json::json!({ "phase": phase });
            if let (Some(p), Some(e)) = (payload.as_object_mut(), extra.as_object()) {
                p.extend(e.clone());
            }
            app_rpc.emit("wallet-sync", payload).ok();
        };

        progress("node", serde_json::json!({}));
        let (mut mgr, daemon_height_before) = live_rpc_manager(&app_rpc);
        progress("starting", serde_json::json!({ "node": mgr.daemon_address() }));
        mgr.start_for_dir(&wallet_dir).map_err(|e| e.to_string())?;
        mgr.wait_for_ready(std::time::Duration::from_secs(10)).map_err(|e| e.to_string())?;

        progress("opening", serde_json::json!({}));
        let open_resp = rpc_manager::ureq_post_json_pub(&mgr.rpc_url(), &serde_json::json!({
            "jsonrpc":"2.0","id":"open","method":"open_wallet",
            "params":{"filename":wallet_name,"password":password}
        })).map_err(|e| e.to_string())?;
        if let Some(err) = open_resp.get("error") {
            log::warn!("[refresh_balance] open_wallet error: {}", err);
        }

        // wallet-rpc starts its own background refresh right after open_wallet and then
        // answers nothing until it is done, so get_height often times out here.
        // Fall back to the height saved after the previous successful sync.
        let synced_height_file = wallet_dir.join(format!("{}.synced_height", wallet_name));
        let height_body = serde_json::json!({ "jsonrpc":"2.0","id":"h","method":"get_height","params":{} });
        let start_height = rpc_manager::ureq_post_json_long(&mgr.rpc_url(), &height_body, 3)
            .ok().and_then(|r| r["result"]["height"].as_u64())
            .filter(|h| *h > 0)
            .or_else(|| std::fs::read_to_string(&synced_height_file).ok()
                .and_then(|s| s.trim().parse::<u64>().ok()));

        // wallet-rpc does not answer any call while `refresh` runs, so only the gap
        // to catch up is known up front; the UI derives an estimate from it.
        let blocks_behind = match start_height {
            Some(h) if daemon_height_before > 0 => Some(daemon_height_before.saturating_sub(h)),
            _ => None,
        };
        log::info!("[refresh_balance] Calling wallet refresh ({} blocks behind)…",
            blocks_behind.map(|b| b.to_string()).unwrap_or_else(|| "unknown".into()));
        progress("syncing", serde_json::json!({ "blocks_behind": blocks_behind }));

        // A wallet that was closed for days can need several minutes on a remote node.
        let refresh_resp = rpc_manager::ureq_post_json_long(&mgr.rpc_url(), &serde_json::json!({
            "jsonrpc":"2.0","id":"refresh","method":"refresh",
            "params":{"start_height":0}
        }), 600);
        progress("reading", serde_json::json!({}));
        match &refresh_resp {
            Ok(r) => {
                let fetched = r["result"]["blocks_fetched"].as_u64().unwrap_or(0);
                let received = r["result"]["received_money"].as_bool().unwrap_or(false);
                log::info!("[refresh_balance] Sync done: blocks_fetched={}, received_money={}", fetched, received);
            }
            Err(e) => log::warn!("[refresh_balance] refresh failed (non-fatal, will retry): {}", e),
        }

        let wallet_height = rpc_manager::ureq_post_json_pub(&mgr.rpc_url(), &serde_json::json!({
            "jsonrpc":"2.0","id":"h2","method":"get_height","params":{}
        })).ok().and_then(|r| r["result"]["height"].as_u64());
        if refresh_resp.as_ref().map(|r| r.get("error").is_none()).unwrap_or(false) {
            if let Some(h) = wallet_height {
                let _ = std::fs::write(&synced_height_file, h.to_string());
            }
        }

        let (total, _unlocked) = mgr.rpc_get_balance()?;
        let atomic = (total * 1e12) as u64;
        log::info!("[refresh_balance] Balance fetched");
        let node = mgr.daemon_address();
        mgr.stop();
        Ok::<(f64, u64, Option<u64>, String, u64), String>((total, atomic, wallet_height, node, daemon_height_before))
    })
    .await
    .map_err(|e| e.to_string())
    .and_then(|r| r);

    let (xmr, atomic, wallet_height, daemon_addr, daemon_height_before) = match rpc_result {
        Ok(v) => v,
        Err(e) => {
            if let Ok(mut s) = state.lock() {
                s.rpc_connected = false;
                s.rpc_last_error = Some(e.clone());
            }
            return Err(e);
        }
    };

    // 0 means the remote node did not answer: keep it as-is, never substitute the wallet height,
    // otherwise offline detection and OFFLINE_WITH_PENDING can never trigger.
    // No node answered before the session: skip a second probe that would only time out again.
    let daemon_height = if daemon_height_before == 0 { 0 } else {
        tokio::task::spawn_blocking(move || rpc_manager::get_daemon_block_height(&daemon_addr))
            .await
            .unwrap_or(0)
    };
    let daemon_reachable = daemon_height > 0;

    if let Ok(mut s) = state.lock() {
        s.balance_atomic = atomic;
        s.rpc_connected = true;
        s.rpc_last_error = None;
        if let Some(h) = wallet_height { s.last_wallet_height = h; }
        if daemon_reachable {
            s.last_daemon_height = daemon_height;
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

    Ok(BalanceResponse { xmr, atomic, daemon_reachable })
}

/// Return current active alerts (for frontend badge display).
#[tauri::command]
async fn get_alerts(state: tauri::State<'_, Mutex<AppState>>) -> Result<Vec<AppAlert>, String> {
    let s = state.lock().map_err(|e| e.to_string())?;
    Ok(s.alerts.clone())
}

/// Recompute all alerts based on current state and subscriptions.
/// Returns the worst AlertLevel, or None if clean.
/// Alerts raised by an event (a failed or blocked payment) rather than derived from the
/// current state: recompute_alerts keeps them for EVENT_ALERT_TTL_SECS unless the
/// subscription has been paid since.
const EVENT_ALERT_KINDS: &[&str] = &["PAYMENT_FAILED", "OUTPUTS_LOCKED", "PAYMENT_BLOCKED_LIMITS"];
const EVENT_ALERT_TTL_SECS: i64 = 24 * 3600;

/// Record an event alert for a subscription (replaces the previous one of the same kind).
fn push_alert(app: &tauri::AppHandle, level: AlertLevel, kind: &str, sub_id: &str, message: String) {
    if let Some(state_rc) = app.try_state::<Mutex<AppState>>() {
        if let Ok(mut s) = state_rc.lock() {
            s.alerts.retain(|a| a.kind != kind || a.sub_id.as_deref() != Some(sub_id));
            s.alerts.push(AppAlert {
                level,
                kind: kind.into(),
                sub_id: Some(sub_id.to_string()),
                message,
                created_at: chrono::Utc::now().timestamp(),
            });
        }
    }
}

fn recompute_alerts(
    state: &mut AppState,
    subs: &[Subscription],
    now_ts: i64,
) -> Option<AlertLevel> {
    let mut alerts: Vec<AppAlert> = state.alerts.iter()
        .filter(|a| EVENT_ALERT_KINDS.contains(&a.kind.as_str()) && now_ts - a.created_at < EVENT_ALERT_TTL_SECS)
        .cloned()
        .collect();

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
    let monthly_xmr: f64 = subs.iter().filter(|s| s.status == SubscriptionStatus::Active)
        .map(|s| payment_policy::monthly_projection(&s.amount, &s.schedule))
        .sum();

    // ORANGE: payments due within 48h that the current balance cannot cover.
    // One grouped alert rather than one per subscription.
    if has_synced && balance_xmr >= 0.000001 {
        let short = subs.iter()
            .filter(|s| s.status == SubscriptionStatus::Active)
            .filter(|s| s.next_payment_at.map(|n| n.timestamp() - now_ts < 48 * 3600).unwrap_or(false))
            .filter(|s| payment_policy::parse_amount(&s.amount)
                .map(|a| a + payment_policy::FEE_MARGIN_XMR > balance_xmr).unwrap_or(false))
            .count();
        if short > 0 {
            alerts.push(AppAlert {
                level: AlertLevel::Orange,
                kind: "BALANCE_SHORT_UPCOMING".into(),
                sub_id: None,
                message: format!("{} payment(s) due within 48h exceed the balance", short),
                created_at: now_ts,
            });
        }
    }

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
fn derive_history_key(password: &str, salt: &[u8]) -> [u8; 32] {
    use argon2::{Argon2, Params, Algorithm, Version};
    let params = Params::new(32_768, 2, 1, Some(32)).expect("valid params");
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut key = [0u8; 32];
    argon2.hash_password_into(password.as_bytes(), salt, &mut key)
        .expect("Argon2id history key derivation failed");
    key
}

/// Salt used by versions that predate the per-install random salt.
const LEGACY_HISTORY_SALT: &[u8] = b"autopay-history-v1-salt-2025";

/// History key derived with this install's random salt (`history.salt`).
/// On first use, an existing history encrypted with the legacy fixed salt is
/// re-encrypted; if that fails the legacy key is kept so no entry is lost.
fn history_key_with_migration(password: &str, history_path: &std::path::Path) -> [u8; 32] {
    let salt_path = history_path.with_extension("salt");
    if let Some(salt) = std::fs::read_to_string(&salt_path).ok()
        .and_then(|b| base64_decode(b.trim()).ok())
        .filter(|s| s.len() == 16)
    {
        return derive_history_key(password, &salt);
    }

    let mut salt = [0u8; 16];
    AesOsRng.fill_bytes(&mut salt);
    let new_key = derive_history_key(password, &salt);

    let existing = std::fs::read_to_string(history_path).ok().filter(|b| !b.trim().is_empty());
    if let Some(blob) = existing {
        let mut old_key = derive_history_key(password, LEGACY_HISTORY_SALT);
        let migrated = aes_decrypt(&old_key, blob.trim())
            .and_then(|plain| aes_encrypt(&new_key, &plain))
            .and_then(|b64| {
                // Write the salt first: if the history write then fails, the next unlock
                // would derive the new key over an old blob, so roll the salt back.
                std::fs::write(&salt_path, base64_encode(&salt)).map_err(|e| e.to_string())?;
                std::fs::write(history_path, b64).map_err(|e| {
                    let _ = std::fs::remove_file(&salt_path);
                    e.to_string()
                })
            });
        match migrated {
            Ok(()) => {
                old_key.zeroize();
                log::info!("[History] Re-encrypted with a per-install salt");
                new_key
            }
            Err(e) => {
                log::warn!("[History] Salt migration skipped, keeping legacy key: {}", e);
                old_key
            }
        }
    } else {
        if let Err(e) = std::fs::write(&salt_path, base64_encode(&salt)) {
            log::warn!("[History] Could not save history salt, using legacy salt: {}", e);
            return derive_history_key(password, LEGACY_HISTORY_SALT);
        }
        new_key
    }
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
    db_state: tauri::State<'_, Mutex<Database>>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    {
        let mut s = app_state.lock().map_err(|e| e.to_string())?;
        // Remove from pending — scheduler will treat it as approved on next run
        // by checking `approved_payments` set
        s.pending_approvals.remove(&sub_id);
        s.approved_payments.insert(sub_id.clone());
    }
    // Remember the exact terms the user agreed to: any later change of amount,
    // address or cadence will require a new confirmation.
    let sub = db_state.lock().map_err(|e| e.to_string())?
        .list_subscriptions().map_err(|e| e.to_string())?
        .into_iter().find(|s| s.id == sub_id);
    if let Some(sub) = sub {
        let dir = app_handle.path().app_data_dir().map_err(|e| e.to_string())?;
        let mut terms = payment_policy::load_approved_terms(&dir);
        terms.insert(sub.id.clone(), sub_fingerprint(&sub));
        payment_policy::save_approved_terms(&dir, &terms);
    }
    log::info!("[PaymentMode] Payment approved by user for sub {}", &sub_id[..8.min(sub_id.len())]);
    Ok(())
}

fn sub_fingerprint(sub: &Subscription) -> String {
    payment_policy::terms_fingerprint(&sub.destination_address, &sub.amount, &sub.currency, &sub.schedule)
}

/// Enforce amount / cadence limits on a new subscription, and ask for an explicit
/// confirmation when it brings the projected monthly total above the threshold.
fn check_new_subscription(
    address: &str,
    currency: &str,
    amount: &str,
    schedule: &str,
    limits: &payment_policy::PaymentLimits,
    existing: &[Subscription],
    confirm_monthly: bool,
) -> Result<(), String> {
    payment_policy::validate_currency(currency).map_err(|e| e.to_string())?;
    payment_policy::validate_address(address).map_err(|e| e.to_string())?;
    payment_policy::validate_terms(amount, schedule, limits).map_err(|e| e.to_string())?;
    let projected: f64 = existing.iter()
        .filter(|s| s.status == SubscriptionStatus::Active)
        .map(|s| payment_policy::monthly_projection(&s.amount, &s.schedule))
        .sum::<f64>()
        + payment_policy::monthly_projection(amount, schedule);
    if projected > payment_policy::MONTHLY_CONFIRM_XMR && !confirm_monthly {
        return Err(payment_policy::PolicyError::MonthlyTotal(projected).to_string());
    }
    Ok(())
}

/// Current payment limits and the fixed thresholds (for the Settings page).
#[tauri::command]
async fn get_payment_limits(app_state: tauri::State<'_, Mutex<AppState>>) -> Result<serde_json::Value, String> {
    let s = app_state.lock().map_err(|e| e.to_string())?;
    Ok(serde_json::json!({
        "allow_large_amounts": s.payment_limits.allow_large_amounts,
        "allow_short_cadence": s.payment_limits.allow_short_cadence,
        "min_amount_xmr": payment_policy::MIN_AMOUNT_XMR,
        "high_amount_xmr": payment_policy::HIGH_AMOUNT_XMR,
        "max_amount_xmr": payment_policy::MAX_AMOUNT_XMR,
        "min_cadence_days": payment_policy::MIN_CADENCE_MINUTES / (24 * 60),
        "monthly_confirm_xmr": payment_policy::MONTHLY_CONFIRM_XMR,
    }))
}

/// Unlock or re-lock the exceptions to the payment limits.
/// Unlocking requires the master password; re-locking never does.
#[tauri::command]
async fn set_payment_limits(
    allow_large_amounts: bool,
    allow_short_cadence: bool,
    password: Option<String>,
    app_state: tauri::State<'_, Mutex<AppState>>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    let mut s = app_state.lock().map_err(|e| e.to_string())?;
    let unlocking = (allow_large_amounts && !s.payment_limits.allow_large_amounts)
        || (allow_short_cadence && !s.payment_limits.allow_short_cadence);
    if unlocking {
        let session = s.session_password.as_deref().ok_or("Wallet is locked.")?;
        let mut given = password.unwrap_or_default();
        let same = given.len() == session.len()
            && given.bytes().zip(session.bytes()).fold(0u8, |acc, (a, b)| acc | (a ^ b)) == 0;
        given.zeroize();
        if !same { return Err("Incorrect password.".into()); }
    }
    let limits = payment_policy::PaymentLimits { allow_large_amounts, allow_short_cadence };
    let dir = app_handle.path().app_data_dir().map_err(|e| e.to_string())?;
    limits.save(&dir)?;
    log::info!("[Policy] Limits updated: large_amounts={}, short_cadence={}", allow_large_amounts, allow_short_cadence);
    s.payment_limits = limits;
    Ok(())
}

/// Make the data dir 0700 and everything in it 0700 (dirs) / 0600 (files): files created
/// by older versions were group/world readable. No-op outside Unix.
fn restrict_permissions(dir: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fn walk(p: &std::path::Path, depth: u8) {
            let Ok(meta) = std::fs::symlink_metadata(p) else { return };
            if meta.file_type().is_symlink() { return; }
            let mode = if meta.is_dir() { 0o700 } else { 0o600 };
            if meta.permissions().mode() & 0o777 != mode {
                let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode));
            }
            if meta.is_dir() && depth < 6 {
                if let Ok(entries) = std::fs::read_dir(p) {
                    for e in entries.flatten() { walk(&e.path(), depth + 1); }
                }
            }
        }
        walk(dir, 0);
    }
    #[cfg(not(unix))]
    let _ = dir;
}

/// Delay applied by "Reporter" in the semi-auto popup.
const POSTPONE_MINUTES: i64 = 60;

/// Decline a queued payment (semi-auto mode) and move its due date, otherwise the
/// scheduler would queue it again on the next 60s tick.
/// `skip = false`: postpone by POSTPONE_MINUTES. `skip = true`: drop this occurrence
/// and wait for the next one of the schedule. Returns the new due date (RFC 3339).
#[tauri::command]
async fn reject_payment(
    sub_id: String,
    skip: Option<bool>,
    app_state: tauri::State<'_, Mutex<AppState>>,
    db_state: tauri::State<'_, Mutex<Database>>,
) -> Result<String, String> {
    app_state.lock().map_err(|e| e.to_string())?.pending_approvals.remove(&sub_id);

    let db = db_state.lock().map_err(|e| e.to_string())?;
    let sub = db.list_subscriptions().map_err(|e| e.to_string())?
        .into_iter().find(|s| s.id == sub_id)
        .ok_or("Subscription not found")?;
    let now = chrono::Utc::now();
    let new_next = if skip.unwrap_or(false) {
        next_occurrence(&sub.schedule, sub.next_payment_at.unwrap_or(now).max(now))
    } else {
        now + chrono::Duration::minutes(POSTPONE_MINUTES)
    };
    db.update_next_payment(&sub_id, Some(new_next), sub.payments_sent).map_err(|e| e.to_string())?;
    log::info!("[PaymentMode] Payment {} by user for sub {}",
        if skip.unwrap_or(false) { "skipped" } else { "postponed" }, &sub_id[..8.min(sub_id.len())]);
    Ok(new_next.to_rfc3339())
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
    confirm_monthly: Option<bool>,
    db_state: tauri::State<'_, Mutex<Database>>,
    app_state: tauri::State<'_, Mutex<AppState>>,
) -> Result<serde_json::Value, String> {
    use autopay_crypto::{parse_code, AutoPayCode};
    use chrono::Utc;
    use uuid::Uuid;

    let parsed = parse_code(&code).map_err(|e| format!("Invalid code: {e:?}"))?;
    let AutoPayCode::V1(v1) = parsed;

    {
        let limits = app_state.lock().map_err(|e| e.to_string())?.payment_limits.clone();
        let existing = db_state.lock().map_err(|e| e.to_string())?.list_subscriptions().map_err(|e| e.to_string())?;
        check_new_subscription(&v1.sellers_wallet, &v1.currency, &v1.amount, &v1.schedule,
            &limits, &existing, confirm_monthly.unwrap_or(false))?;
    }

    let now = Utc::now();
    // Parse start_date from code if present, otherwise use now
    let next_payment_at = chrono::DateTime::parse_from_rfc3339(&v1.start_date)
        .ok()
        .map(|d| d.with_timezone(&Utc));

    let sub = Subscription {
        id: Uuid::new_v4().to_string(),
        wallet_id: "default".to_string(),
        label: payment_policy::clean_text(&v1.custom_label, 80),
        description: v1.description.as_deref().map(|d| payment_policy::clean_text(d, 500)),
        destination_address: v1.sellers_wallet.trim().to_string(),
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
    confirm_monthly: Option<bool>,
    db_state: tauri::State<'_, Mutex<Database>>,
    app_state: tauri::State<'_, Mutex<AppState>>,
) -> Result<serde_json::Value, String> {
    use chrono::Utc;
    use uuid::Uuid;

    let now = Utc::now();
    let label = payment_policy::clean_text(payload["custom_label"].as_str().unwrap_or("—"), 80);
    let dest = payload["destination_address"].as_str().unwrap_or("").trim().to_string();
    let amount = payload["amount"].as_str().unwrap_or("0").to_string();
    let currency = payload["currency"].as_str().unwrap_or("XMR").to_string();
    let schedule = payload["schedule"].as_str().unwrap_or("0 0 9 1 * *").to_string();
    let number_of_payments = payload["number_of_payments"].as_u64().unwrap_or(1);
    let start_immediately = payload["start_immediately"].as_bool().unwrap_or(true);
    let canary_url = payload["canary_url"].as_str().filter(|s| !s.is_empty()).map(|s| s.to_string());
    let description = payment_policy::clean_text(payload["description"].as_str().unwrap_or(""), 500);

    {
        let limits = app_state.lock().map_err(|e| e.to_string())?.payment_limits.clone();
        let existing = db_state.lock().map_err(|e| e.to_string())?.list_subscriptions().map_err(|e| e.to_string())?;
        check_new_subscription(&dest, &currency, &amount, &schedule,
            &limits, &existing, confirm_monthly.unwrap_or(false))?;
    }

    // next_payment_at: now if start_immediately, else use start_date or compute from schedule
    let next_payment_at = if start_immediately {
        Some(now)
    } else if let Some(sd) = payload["start_date"].as_str().filter(|s| !s.is_empty()) {
        chrono::DateTime::parse_from_rfc3339(sd).ok().map(|d| d.with_timezone(&Utc))
            .or(Some(now + chrono::Duration::days(1)))
    } else {
        Some(next_occurrence(&schedule, now))
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

/// Build an RpcManager from the saved settings on a node that currently answers
/// (automatic failover when no node is pinned). Blocking: probes nodes over the network.
/// Also returns the selected node's height (0 if no node answered).
fn live_rpc_manager(app: &tauri::AppHandle) -> (RpcManager, u64) {
    let (config, last_working) = match app.try_state::<Mutex<AppState>>() {
        Some(st) => match st.lock() {
            Ok(s) => (s.rpc.config.clone(), s.active_node.clone()),
            Err(_) => (RpcConfig::default(), None),
        },
        None => (RpcConfig::default(), None),
    };
    let (config, node, height) = rpc_manager::select_live_node(config, last_working.as_deref());
    if height > 0 {
        if let Some(st) = app.try_state::<Mutex<AppState>>() {
            if let Ok(mut s) = st.lock() { s.active_node = Some(node); }
        }
    }
    (RpcManager::new(config), height)
}

/// Saved RPC settings plus the node actually in use.
#[tauri::command]
fn get_rpc_config(state: tauri::State<'_, Mutex<AppState>>) -> Result<serde_json::Value, String> {
    let s = state.lock().map_err(|e| e.to_string())?;
    Ok(serde_json::json!({
        "config": s.rpc.config,
        "active_node": s.active_node,
    }))
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
    let mut config = config;
    config.daemon_address = config.manual_node();
    if let Some(node) = &config.daemon_address {
        let valid = match node.rsplit_once(':') {
            Some((host, port)) => !host.is_empty()
                && !host.contains("://")
                && !host.contains(char::is_whitespace)
                && port.parse::<u16>().map(|p| p > 0).unwrap_or(false),
            None => false,
        };
        if !valid {
            return Err("Invalid node address (expected host:port, e.g. node.example.org:18089).".into());
        }
    }
    let data_dir = app_handle.path().app_data_dir().map_err(|e| e.to_string())?;
    let path = data_dir.join("rpc_config.json");
    let json = serde_json::to_string_pretty(&config).map_err(|e| e.to_string())?;
    std::fs::write(&path, json).map_err(|e| e.to_string())?;
    // Update live config (takes effect on next start)
    let mut s = state.lock().unwrap();
    if let Some(node) = config.manual_node() { s.active_node = Some(node); }
    s.rpc = RpcManager::new(config);
    Ok(())
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
    /// false when the remote Monero node could not be reached during the last refresh
    /// (balance is then the wallet's cached value, not a fresh one).
    daemon_reachable: bool,
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
    payment_policy::average_interval_minutes(cron).unwrap_or(30 * 24 * 60)
}

/// Next due date strictly after `after`, following the calendar (a monthly "1st at 9:00"
/// stays on the 1st). Falls back to one month if the expression cannot be evaluated.
fn next_occurrence(schedule: &str, after: chrono::DateTime<chrono::Utc>) -> chrono::DateTime<chrono::Utc> {
    payment_policy::next_after(schedule, after).unwrap_or_else(|| {
        log::warn!("[Scheduler] Unsupported schedule expression, falling back to +30 days");
        after + chrono::Duration::days(30)
    })
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

    let amount_pico = (payment.amount_xmr * 1e12) as u64;
    let app_rpc = app.clone();
    let pv_id = payment.id.clone();
    let dest = payment.address.clone();

    // Same flow as send_payment: wallet-dir mode + open_wallet, so the password
    // never appears on the wallet-rpc command line (visible to any local user via `ps`).
    tokio::task::spawn_blocking(move || {
        struct RpcGuard(Arc<AtomicBool>);
        impl Drop for RpcGuard { fn drop(&mut self) { self.0.store(false, Ordering::SeqCst); } }
        let _guard = RpcGuard(rpc_busy);

        let (mut mgr, _) = live_rpc_manager(&app_rpc);
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

        if let Err(e) = rpc_manager::ureq_post_json_long(&mgr.rpc_url(), &serde_json::json!({
            "jsonrpc":"2.0","id":"refresh","method":"refresh","params":{}
        }), 600) {
            log::warn!("[Treasurer] refresh failed (continuing): {}", e);
        }

        let resp = rpc_manager::ureq_post_json_pub(&mgr.rpc_url(), &serde_json::json!({
            "jsonrpc":"2.0","id":"tx","method":"transfer",
            "params":{
                "destinations":[{"amount": amount_pico, "address": dest}],
                "account_index": 0,
                "priority": 1,
                "get_tx_key": false
            }
        })).map_err(|e| e.to_string());
        mgr.stop();
        let resp = resp?;

        if let Some(err) = resp.get("error") {
            let msg = err.get("message").and_then(|m| m.as_str()).unwrap_or("unknown");
            log::error!("[Treasurer] PV id={} — transfer error: {}", pv_id, msg);
            return Err(format!("Payment failed: {}", msg));
        }
        let tx_hash = resp["result"]["tx_hash"].as_str().unwrap_or("").to_string();
        if tx_hash.is_empty() {
            return Err("Payment failed: transfer returned no tx_hash".to_string());
        }
        log::info!("[Treasurer] PV payment executed: id={} tx_prefix={}…", pv_id, &tx_hash[..8.min(tx_hash.len())]);
        Ok(tx_hash)
    })
    .await
    .map_err(|e| e.to_string())?
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
    let (semi_auto, approved_ids, limits, balance_known) = {
        if let Some(state) = app.try_state::<Mutex<AppState>>() {
            if let Ok(mut s) = state.lock() {
                let semi = s.semi_auto_mode;
                let approved: std::collections::HashSet<String> = s.approved_payments.drain().collect();
                let balance = if s.rpc_connected { Some(s.balance_atomic) } else { None };
                (semi, approved, s.payment_limits.clone(), balance)
            } else { return; }
        } else { return; }
    };
    // Approvals not consumed by an actual send this cycle must survive to the next one.
    let restore_approvals = |ids: &std::collections::HashSet<String>| {
        if let Some(state) = app.try_state::<Mutex<AppState>>() {
            if let Ok(mut s) = state.lock() { s.approved_payments.extend(ids.iter().cloned()); }
        }
    };

    let data_dir = match app.path().app_data_dir() { Ok(d) => d, Err(_) => return };
    let mut approved_terms = payment_policy::load_approved_terms(&data_dir);

    for sub in due {
        let dest = sub.destination_address.clone();
        let amount_xmr = payment_policy::parse_amount(&sub.amount).unwrap_or(0.0);
        let amount_pico = (amount_xmr * 1e12) as u64;
        let sub_id = sub.id.clone();
        let short_id = sub_id[..8.min(sub_id.len())].to_string();

        // Limits are re-checked at payment time: they may have been re-locked since the
        // subscription was added, or the DB may have been modified outside the app.
        if let Err(e) = payment_policy::validate_currency(&sub.currency)
            .and_then(|_| payment_policy::validate_address(&sub.destination_address))
            .and_then(|_| payment_policy::validate_terms(&sub.amount, &sub.schedule, &limits))
        {
            log::warn!("[Scheduler] Sub {} blocked by payment limits ({})", short_id, e);
            push_alert(app, AlertLevel::Orange, "PAYMENT_BLOCKED_LIMITS", &sub_id, e.to_string());
            continue;
        }

        // Not enough funds: no point spawning wallet-rpc for a transfer that will fail.
        let needed = ((amount_xmr + payment_policy::FEE_MARGIN_XMR) * 1e12) as u64;
        if let Some(balance) = balance_known {
            if balance < needed {
                log::info!("[Scheduler] Sub {} due but balance too low — waiting for funds", short_id);
                continue;
            }
        }

        // Subscriptions paid before this check existed are trusted as-is.
        let fingerprint = sub_fingerprint(&sub);
        if sub.payments_sent > 0 && !approved_terms.contains_key(&sub_id) {
            approved_terms.insert(sub_id.clone(), fingerprint.clone());
            payment_policy::save_approved_terms(&data_dir, &approved_terms);
        }
        let mut reasons: Vec<String> = payment_policy::confirmation_reasons(
            &sub.amount, &fingerprint, approved_terms.get(&sub_id),
        ).into_iter().map(String::from).collect();
        if semi_auto { reasons.insert(0, "semi_auto".into()); }

        // Queue for user approval unless already approved this session
        if !reasons.is_empty() && !approved_ids.contains(&sub_id) {
            log::info!("[Scheduler] Sub {} queued for user approval ({})", short_id, reasons.join(","));
            if let Some(state) = app.try_state::<Mutex<AppState>>() {
                if let Ok(mut s) = state.lock() {
                    s.pending_approvals.insert(sub_id.clone(), PendingApproval {
                        sub_id: sub_id.clone(),
                        sub_label: sub.label.clone(),
                        amount_xmr: sub.amount.clone(),
                        queued_at: Utc::now().to_rfc3339(),
                        reasons: reasons.clone(),
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
                "reasons": reasons,
            })).ok();
            continue; // skip to next sub — scheduler will retry next cycle
        }

        // Without the lock, start_for_dir would kill the wallet-rpc of a running refresh (or payment).
        let rpc_busy = match app.try_state::<Mutex<AppState>>()
            .and_then(|st| st.lock().ok().map(|s| Arc::clone(&s.rpc_busy)))
        {
            Some(b) => b,
            None => return,
        };
        if rpc_busy.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).is_err() {
            log::info!("[Scheduler] RPC busy — payment for sub {} retried next cycle", short_id);
            restore_approvals(&approved_ids);
            return;
        }

        log::info!("[Scheduler] Sending payment for sub {}", &sub_id[..8.min(sub_id.len())]);

        let wallet_dir2 = wallet_dir.clone();
        let wallet_name2 = wallet_name.clone();
        let pw2 = session_pw.clone();
        let app_rpc = app.clone();

        let result = tokio::task::spawn_blocking(move || {
            struct RpcGuard(Arc<AtomicBool>);
            impl Drop for RpcGuard { fn drop(&mut self) { self.0.store(false, Ordering::SeqCst); } }
            let _guard = RpcGuard(rpc_busy);

            let (mut mgr, _) = live_rpc_manager(&app_rpc);
            mgr.start_for_dir(&wallet_dir2).map_err(|e| e.to_string())?;
            mgr.wait_for_ready(std::time::Duration::from_secs(15)).map_err(|e| e.to_string())?;

            let open = rpc_manager::ureq_post_json_pub(&mgr.rpc_url(), &serde_json::json!({
                "jsonrpc":"2.0","id":"o","method":"open_wallet",
                "params":{"filename":wallet_name2,"password":pw2}
            })).map_err(|e| e.to_string())?;
            if let Some(err) = open.get("error") {
                return Err(format!("open_wallet: {}", err));
            }

            // Spend from an up-to-date wallet, otherwise stale outputs make the transfer fail.
            if let Err(e) = rpc_manager::ureq_post_json_long(&mgr.rpc_url(), &serde_json::json!({
                "jsonrpc":"2.0","id":"r","method":"refresh","params":{}
            }), 600) {
                log::warn!("[Scheduler] refresh before transfer failed (continuing): {}", e);
            }

            let resp = rpc_manager::ureq_post_json_pub(&mgr.rpc_url(), &serde_json::json!({
                "jsonrpc":"2.0","id":"tx","method":"transfer",
                "params":{
                    "destinations":[{"amount": amount_pico, "address": dest}],
                    "account_index": 0, "priority": 1
                }
            })).map_err(|e| e.to_string());
            mgr.stop();
            let resp = resp?;

            if let Some(err) = resp.get("error") {
                let msg = err.get("message").and_then(|m| m.as_str()).unwrap_or("?");
                return Err(format!("transfer error: {}", msg));
            }
            Ok(resp["result"]["tx_hash"].as_str().unwrap_or("").to_string())
        }).await;

        match result {
            Ok(Ok(tx_hash)) => {
                log::info!("[Scheduler] Payment sent for sub {} (tx={}…)", &sub_id[..8.min(sub_id.len())], &tx_hash[..8.min(tx_hash.len())]);
                // Update last_payment_sent_at in AppState
                if let Some(state) = app.try_state::<Mutex<AppState>>() {
                    if let Ok(mut s) = state.lock() {
                        s.last_payment_sent_at = Some(Utc::now().timestamp());
                        s.alerts.retain(|a| a.sub_id.as_deref() != Some(sub_id.as_str()));
                    }
                }
                // Next occurrence after now: a long-overdue subscription must not
                // trigger a burst of catch-up payments.
                let new_next = next_occurrence(&sub.schedule, Utc::now());
                {
                    if let Ok(db) = db_state.lock() {
                        if let Err(e) = db.update_next_payment(&sub_id, Some(new_next), sub.payments_sent + 1) {
                            log::error!("[Scheduler] DB update failed for sub {}: {}", sub_id, e);
                        } else {
                            log::info!("[Scheduler] next_payment_at advanced to {} for sub {}", new_next.to_rfc3339(), sub_id);
                        }
                    }
                }
                // Emit event so frontend can refresh balance and subscription list
                let new_next_iso = new_next.to_rfc3339();
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
    // Files created from now on (vault, DB and its WAL, logs, wallet cache, webview storage)
    // are private to the current user.
    #[cfg(unix)]
    unsafe { libc::umask(0o077); }

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_deep_link::init())
        .plugin(
            tauri_plugin_single_instance::init(|app, args, _cwd| {
                // A second instance was launched (e.g. from a deep link click)
                // Forward any crypto-autopay: URL from its args to the running instance
                for arg in &args {
                    if arg.starts_with("crypto-autopay:") {
                        log::info!("Single-instance redirect: deep link forwarded");
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
            restrict_permissions(&data_dir);

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
                               "WALLET_NOT_SYNCED","OUTPUTS_LOCKED","OFFLINE_WITH_PENDING",
                               "PAYMENT_BLOCKED_LIMITS","BALANCE_SHORT_UPCOMING"] {
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
                active_node: None,
                payment_limits: payment_policy::PaymentLimits::load(&data_dir),
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
                    log::info!("Deep link received");
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
            get_payment_limits,
            set_payment_limits,
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
            get_rpc_config,
            detect_rpc_binary,
            get_rpc_status,
            save_rpc_config,
            stop_wallet_rpc,
        ])
        .run(tauri::generate_context!())
        .expect("error while building tauri application");
}
