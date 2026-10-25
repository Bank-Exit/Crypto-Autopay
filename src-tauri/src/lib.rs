use std::sync::Mutex;
use std::path::PathBuf;
use serde::{Deserialize, Serialize};
use tauri::{Manager, Emitter, Listener};

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
    /// RPC process manager.
    pub rpc: RpcManager,
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
            s.session_password = Some(password.clone());
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
    subscription_id: i64,
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

    let (wallet_name, wallet_dir, password) = {
        let s = state.lock().map_err(|e| e.to_string())?;
        if !s.unlocked { return Err("Wallet is locked.".to_string()); }
        let wn = s.wallet_file_path.file_name()
            .and_then(|n| n.to_str()).unwrap_or("autopay_wallet").to_string();
        let wd = app_handle.path().app_data_dir().map_err(|e| e.to_string())?;
        let pw = s.session_password.clone().ok_or("No session password")?;
        (wn, wd, pw)
    };

    log::info!("[Payment] Sending payment for subscription #{}", subscription_id);

    let result = tokio::task::spawn_blocking(move || {
        let mut mgr = rpc_manager::RpcManager::new(rpc_manager::RpcConfig::default());
        mgr.start_for_dir(&wallet_dir).map_err(|e| e.to_string())?;
        mgr.wait_for_ready(std::time::Duration::from_secs(15)).map_err(|e| e.to_string())?;

        let open = rpc_manager::ureq_post_json_pub(&mgr.rpc_url(), &serde_json::json!({
            "jsonrpc":"2.0","id":"open","method":"open_wallet",
            "params":{"filename":wallet_name,"password":password}
        })).map_err(|e| e.to_string())?;
        if let Some(err) = open.get("error") {
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
        log::info!("[Payment] sub #{} — sent. tx_id={}", subscription_id, tx_hash);
        Ok(serde_json::json!({ "tx_hash": tx_hash }))
    })
    .await
    .map_err(|e| e.to_string())??;

    Ok(result)
}

/// Update a subscription's next_payment_at after a successful payment.
#[tauri::command]
async fn update_subscription_next_payment(
    id: i64,
    next_payment_at: String,
    db_state: tauri::State<'_, Mutex<Database>>,
) -> Result<(), String> {
    let db = db_state.lock().map_err(|e| e.to_string())?;
    db.update_subscription_next_payment(id, &next_payment_at)
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
    let (wallet_name, wallet_dir, password) = {
        let s = state.lock().map_err(|e| e.to_string())?;
        if !s.unlocked { return Err("Locked".into()); }
        let wn = s.wallet_file_path.file_name()
            .and_then(|n| n.to_str()).unwrap_or("autopay_wallet").to_string();
        let wd = app_handle.path().app_data_dir().map_err(|e| e.to_string())?;
        let pw = s.session_password.clone().ok_or("No session")?;
        (wn, wd, pw)
    };

    let (xmr, atomic) = tokio::task::spawn_blocking(move || {
        let mut mgr = rpc_manager::RpcManager::new(rpc_manager::RpcConfig::default());
        mgr.start_for_dir(&wallet_dir).map_err(|e| e.to_string())?;
        mgr.wait_for_ready(std::time::Duration::from_secs(10)).map_err(|e| e.to_string())?;

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
        match &refresh_resp {
            Ok(r) => {
                let fetched = r["result"]["blocks_fetched"].as_u64().unwrap_or(0);
                let received = r["result"]["received_money"].as_bool().unwrap_or(false);
                log::info!("[refresh_balance] Sync done: blocks_fetched={}, received_money={}", fetched, received);
            }
            Err(e) => log::warn!("[refresh_balance] refresh failed (non-fatal, will retry): {}", e),
        }

        let (total, _unlocked) = mgr.rpc_get_balance().unwrap_or((0.0, 0.0));
        let atomic = (total * 1e12) as u64;
        log::info!("[refresh_balance] Total balance: {} XMR ({} pico)", total, atomic);
        mgr.stop();
        Ok::<(f64, u64), String>((total, atomic))
    })
    .await
    .map_err(|e| e.to_string())??;

    if let Ok(mut s) = state.lock() {
        s.balance_atomic = atomic;
    }
    Ok(BalanceResponse { xmr, atomic })
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

    for sub in due {
        let dest = sub.destination_address.clone();
        let amount_pico = (sub.amount.parse::<f64>().unwrap_or(0.0) * 1e12) as u64;
        let sub_id = sub.id.clone();

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
                        let _ = db.update_next_payment(&sub_id, Some(new_next), sub.payments_sent + 1);
                    }
                }
                // Emit event so frontend can refresh balance and subscription list
                app.emit("payment-sent", serde_json::json!({"sub_id": sub_id})).ok();
            }
            Ok(Err(e)) => log::error!("[Scheduler] Payment failed for sub {}: {}", sub_id, e),
            Err(e) => log::error!("[Scheduler] Task panic for sub {}: {}", sub_id, e),
        }
        // Only send one payment per cycle to respect 25-min Monero rule
        break;
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
                // Bring the existing window to focus
                if let Some(win) = app.get_webview_window("main") {
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
                rpc: RpcManager::new(rpc_config),
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

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_pending_deep_link,
            generate_seed,
            check_wallet_exists,
            create_wallet,
            unlock_wallet,
            get_wallet_info,
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
