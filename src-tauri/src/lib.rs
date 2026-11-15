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
    normalize_mnemonic,
    fix_seed_checksum,
};
use autopay_core::db::Database;
use autopay_core::models::{Subscription, SubscriptionStatus};

mod rpc_manager;
mod payment_policy;
mod unlock_guard;
mod log_retention;
mod netcheck;
mod boot_log;
mod defender;
mod unattended;
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
    /// Spendable part of the balance: change from a recent send stays locked for 10 blocks.
    pub unlocked_balance_atomic: u64,
    /// Whether the last RPC session succeeded (used for UI status dot).
    pub rpc_connected: bool,
    /// Last RPC error message (None = no error or not yet tried).
    pub rpc_last_error: Option<String>,
    /// Master password, held while the interface is unlocked (vault, guard changes).
    pub session_password: Option<String>,
    /// The Monero wallet's own password, used to open it in wallet-rpc. Random for wallets
    /// migrated to it; the master password for older ones until their next opening.
    pub wallet_password: Option<String>,
    /// Payments keep running while the interface is locked (secrets are in the OS keyring).
    pub unattended_active: bool,
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
    /// Wallet-wide ceiling on automatic sends over a rolling period (persisted).
    pub wallet_cap: payment_policy::WalletCap,
    /// Sent (amounts + fees) over the cap period, as last read from the wallet.
    pub sent_in_period_atomic: Option<u64>,
    /// Subscriptions refused by the wallet cap, with the Unix time of the next attempt.
    pub cap_blocked_until: std::collections::HashMap<String, i64>,
    /// Fingerprints of the terms the user agreed to, per subscription (persisted in the policy vault).
    pub approved_terms: std::collections::HashMap<String, String>,
    /// Other sealed sending-policy items (pause, per-subscription caps, ...).
    pub policy_extras: PolicyExtras,
    /// Outgoing transfers over the last 24 h, as last read from the wallet.
    pub sends_last_day: Option<u32>,
    /// Last time the remote node's height moved (0 = unknown).
    pub daemon_height_changed_at: i64,
    /// Since when the wallet lags the node by more than WALLET_BEHIND_BLOCKS.
    pub wallet_behind_since: Option<i64>,
    /// Unix timestamp of the last wallet refresh attempt (UI or background).
    pub last_refresh_at: i64,
    /// Nodes found too slow or failing, with the Unix time until which they are avoided.
    pub slow_nodes: std::collections::HashMap<String, i64>,
    /// Last readiness verdict logged, so the log only records changes.
    pub ready_state: Option<&'static str>,
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
    if let Some(mut pw) = s.wallet_password.take() { pw.zeroize(); }
    if let Some(mut key) = s.history_key.take() { key.zeroize(); }
    // Keep wallet_address cached (it's in the .address file anyway)
    log::info!("[Security] Wallet locked — session ended");
    log_retention::set_viewable(false);
    drop(s);
    end_wallet_session("wallet locked");
    Ok(())
}

/// Screen lock (padlock, idle): the master password leaves memory and logs are hidden;
/// the payment session keeps running.
#[tauri::command]
fn lock_ui(state: tauri::State<'_, Mutex<AppState>>) -> Result<(), String> {
    let mut s = state.lock().map_err(|e| e.to_string())?;
    if let Some(mut pw) = s.session_password.take() {
        pw.zeroize();
        log::info!("[Security] Interface locked — scheduled payments continue");
    }
    log_retention::set_viewable(false);
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
/// `restore_from`: when restoring, the approximate wallet creation date (unix seconds, 0 =
/// unknown); the first unlock scans the chain from there instead of from today.
#[tauri::command]
async fn create_wallet(
    seed: String,
    password: String,
    restore_from: Option<i64>,
    state: tauri::State<'_, Mutex<AppState>>,
) -> Result<(), String> {
    if password.chars().count() < 12 {
        return Err("PASSWORD_TOO_SHORT".into());
    }
    let seed = seed.split_whitespace().map(str::to_lowercase).collect::<Vec<_>>().join(" ");
    let words = seed.split(' ').count();
    if words != 25 {
        return Err(format!("SEED_WORD_COUNT:{words}"));
    }
    let seed = normalize_mnemonic(&seed).map_err(|e| format!("SEED_INVALID:{e}"))?;

    let (vault_path, wallet_name) = {
        let state = state.lock().map_err(|e| e.to_string())?;
        (state.vault_path.clone(), wallet_name_of(&state.wallet_file_path))
    };
    if vault_path.exists() {
        return Err("WALLET_EXISTS".into());
    }
    let data_dir = vault_path.parent().ok_or("No data folder")?.to_path_buf();
    std::fs::create_dir_all(&data_dir).map_err(|e| e.to_string())?;
    // Leftovers of a previous wallet (cache, history keyed to another password) would mix in.
    archive_wallet_files(&data_dir, &wallet_name)?;
    tokio::task::spawn_blocking(unattended::forget).await.map_err(|e| e.to_string())?;

    let encrypted = SeedVault::encrypt(&seed, &password).map_err(|e| e.to_string())?;
    let vault_json = serde_json::to_string(&encrypted).map_err(|e| e.to_string())?;
    std::fs::write(&vault_path, vault_json).map_err(|e| e.to_string())?;
    if let Some(ts) = restore_from {
        let height = if ts > 0 { monero_height_at(ts) } else { 1 };
        std::fs::write(data_dir.join(format!("{wallet_name}.restore_height")), height.to_string())
            .map_err(|e| e.to_string())?;
        log::info!("[Wallet] Restored from seed, chain scan from height {}", height);
    } else {
        log::info!("[Wallet] New wallet created");
    }

    let mut state = state.lock().map_err(|e| e.to_string())?;
    state.unlocked = false; // Will be unlocked on next unlock call
    Ok(())
}

fn wallet_name_of(wallet_file_path: &std::path::Path) -> String {
    wallet_file_path.file_name().and_then(|n| n.to_str()).unwrap_or("autopay_wallet").to_string()
}

/// Approximate chain height at `ts`, one month early so no deposit is missed.
fn monero_height_at(ts: i64) -> u64 {
    // v0.18 fork: block 2,688,888 on 2022-08-13. Blocks have been 2 minutes since the
    // v2 fork (2016-03-23); before it they were 1 minute, so older dates scan from the start.
    const REF_HEIGHT: i64 = 2_688_888;
    const REF_TS: i64 = 1_660_348_800;
    const V2_FORK_TS: i64 = 1_458_691_200;
    const MARGIN_BLOCKS: i64 = 30 * 720;
    if ts < V2_FORK_TS {
        return 1;
    }
    (REF_HEIGHT + (ts - REF_TS) / 120 - MARGIN_BLOCKS).max(1) as u64
}

/// Files bound to one wallet and its master password. Subscriptions and settings stay.
fn wallet_bound_files(wallet_name: &str) -> Vec<String> {
    let mut names: Vec<String> = ["seed.vault", "history.vault", "history.salt", POLICY_VAULT_FILE, WALLET_PW_FILE, "unlock_guard.json"]
        .iter().map(|s| s.to_string()).collect();
    names.extend(["", ".keys", ".address", ".height", ".synced_height", ".restore_height"]
        .iter().map(|ext| format!("{wallet_name}{ext}")));
    names
}

/// Move the current wallet aside to `archive/<date>/` rather than deleting it: a wrong
/// "forgot password" click must stay recoverable.
fn archive_wallet_files(data_dir: &std::path::Path, wallet_name: &str) -> Result<usize, String> {
    let present: Vec<String> = wallet_bound_files(wallet_name).into_iter()
        .filter(|n| data_dir.join(n).exists()).collect();
    if present.is_empty() {
        return Ok(0);
    }
    let dest = data_dir.join("archive").join(chrono::Local::now().format("%Y%m%d-%H%M%S").to_string());
    std::fs::create_dir_all(&dest).map_err(|e| e.to_string())?;
    for n in &present {
        std::fs::rename(data_dir.join(n), dest.join(n)).map_err(|e| format!("{n}: {e}"))?;
    }
    log::warn!("[Wallet] Previous wallet moved to archive/ ({} files)", present.len());
    Ok(present.len())
}

/// "Forgot password": end the session and archive the local wallet so it can be restored
/// from its 25 words. Subscriptions are kept.
#[tauri::command]
async fn reset_wallet(state: tauri::State<'_, Mutex<AppState>>) -> Result<(), String> {
    let (data_dir, wallet_name) = {
        let mut s = state.lock().map_err(|e| e.to_string())?;
        s.unlocked = false;
        if let Some(mut pw) = s.session_password.take() { pw.zeroize(); }
        if let Some(mut pw) = s.wallet_password.take() { pw.zeroize(); }
        if let Some(mut key) = s.history_key.take() { key.zeroize(); }
        s.unattended_active = false;
        s.wallet_address = None;
        s.balance_atomic = 0;
        s.rpc_connected = false;
        s.rpc_last_error = None;
        let dir = s.vault_path.parent().ok_or("No data folder")?.to_path_buf();
        (dir, wallet_name_of(&s.wallet_file_path))
    };
    log_retention::set_viewable(false);
    WALLET_SESSION_INVALIDATED.store(true, Ordering::SeqCst);
    tokio::task::spawn_blocking(|| {
        stop_wallet_session("wallet reset");
        unattended::forget();
    }).await.map_err(|e| e.to_string())?;
    archive_wallet_files(&data_dir, &wallet_name)?;
    Ok(())
}

/// Unlock the wallet: verify password by decrypting the vault.
/// - Returns Err ONLY if the password is wrong (or vault missing).
/// - Returns Ok(UnlockResult) in all other cases (even if RPC fails).
///   rpc_error / seed_invalid in the result indicate degraded mode.
/// Whether AutoPay is registered to start with the user session.
#[tauri::command]
fn get_autostart(app: tauri::AppHandle) -> Result<bool, String> {
    use tauri_plugin_autostart::ManagerExt;
    app.autolaunch().is_enabled().map_err(|e| e.to_string())
}

#[tauri::command]
fn set_autostart(app: tauri::AppHandle, enabled: bool) -> Result<(), String> {
    use tauri_plugin_autostart::ManagerExt;
    if enabled && cfg!(debug_assertions) {
        return Err("Dev build: autostart would launch a binary that needs the Vite dev server".into());
    }
    let launcher = app.autolaunch();
    let res = if enabled { launcher.enable() } else { launcher.disable() };
    res.map_err(|e| e.to_string())?;
    log::info!("[Autostart] {}", if enabled { "enabled" } else { "disabled" });
    Ok(())
}

/// Seconds before the unlock screen accepts a new attempt (0 = now).
#[tauri::command]
fn unlock_lockout_remaining(state: tauri::State<'_, Mutex<AppState>>) -> Result<i64, String> {
    let vault_path = state.lock().map_err(|e| e.to_string())?.vault_path.clone();
    Ok(unlock_guard::UnlockGuard::load(&vault_path).remaining_secs(chrono::Utc::now().timestamp()))
}

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

    let now = chrono::Utc::now().timestamp();
    let mut guard = unlock_guard::UnlockGuard::load(&vault_path);
    let wait = guard.remaining_secs(now);
    if wait > 0 {
        return Err(format!("LOCKED_OUT:{wait}"));
    }

    // Decrypt — ONLY step that can return Err (wrong password)
    let seed = match SeedVault::decrypt(&encrypted, &password) {
        Ok(seed) => {
            guard.record_success();
            seed
        }
        Err(_) => {
            let wait = guard.record_failure(now);
            log::warn!("[Unlock] Wrong password (lockout: {}s)", wait);
            return Err(if wait > 0 {
                format!("LOCKED_OUT:{wait}")
            } else {
                format!("Incorrect password.;remaining={}", guard.attempts_before_lockout())
            });
        }
    };

    log::info!("[Unlock] Vault decrypted successfully.");

    // UI lock only: the session kept running in the background, nothing to reload.
    {
        let mut s = state.lock().map_err(|e| e.to_string())?;
        if s.unlocked && s.wallet_password.is_some() {
            log::info!("[Unlock] Session resumed (background session still active)");
            s.session_password = Some(password.clone());
            drop(s);
            sync_unattended(&app_handle);
            session_unlocked(&app_handle);
            let s = state.lock().map_err(|e| e.to_string())?;
            return Ok(UnlockResult {
                address: s.wallet_address.clone().unwrap_or_default(),
                balance_xmr: s.balance_atomic as f64 / 1e12,
                rpc_error: s.rpc_last_error.clone(),
                seed_invalid: false,
            });
        }
    }

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
            let key = history_key_with_migration(&password, &s.history_path);
            s.wallet_password = Some(read_wallet_password(&wallet_dir, &key).unwrap_or_else(|| password.clone()));
            s.history_key = Some(key);
            apply_policy_vault(&mut s);
            s.wallet_address = Some(cached_addr.clone());
            s.wallet_file_path = wallet_file_path.clone();
            s.rpc_connected = false;
            s.rpc_last_error = None;
            s.balance_atomic = 0;
        }
        refresh_pause_item(&app_handle);
        sync_unattended(&app_handle);
        session_unlocked(&app_handle);
        return Ok(UnlockResult {
            address: cached_addr,
            balance_xmr: 0.0,
            rpc_error: None,
            seed_invalid: false,
        });
    }

    // Try RPC session — non-fatal on failure
    log::debug!("[Unlock] wallet_dir={:?}, wallet_exists={}, wallet_name={}", wallet_dir, wallet_exists, wallet_name);
    let history_key = {
        let s = state.lock().map_err(|e| e.to_string())?;
        history_key_with_migration(&password, &s.history_path)
    };
    let known_wallet_pw = read_wallet_password(&wallet_dir, &history_key);
    let (wn, wd, sd, master) = (wallet_name.clone(), wallet_dir.clone(), seed_str.clone(), password.clone());
    let app_rpc = app_handle.clone();

    let configured_node = state.lock().map_err(|e| e.to_string())?.rpc.config.daemon_address.clone();
    let mut final_wallet_pw = known_wallet_pw.clone().unwrap_or_else(|| password.clone());
    let rpc_result: Result<(String, f64, String), (String, bool)> = if netcheck::offline(configured_node.as_deref()) {
        log::info!("[Network] No network connection — wallet opened without sync");
        Err((netcheck::OFFLINE.to_string(), false))
    } else { tokio::task::spawn_blocking(move || {
        log::debug!("[Unlock/RPC] spawn_blocking started");
        // The wallet files may be recreated below: no other process may hold them.
        stop_wallet_session("unlock");
        let (mut mgr, _) = live_rpc_manager(&app_rpc);
        log::debug!("[Unlock/RPC] Using daemon: {}, port: {}", mgr.daemon_address(), mgr.config_port());

        mgr.start_for_dir(&wd).map_err(|e| {
            log::error!("[Unlock/RPC] start_for_dir failed: {}", e);
            (e, false)
        })?;

        log::debug!("[Unlock/RPC] Process spawned, waiting for RPC to be ready…");
        mgr.wait_for_ready(std::time::Duration::from_secs(15)).map_err(|e| {
            log::error!("[Unlock/RPC] wait_for_ready timed out: {}", e);
            (e, false)
        })?;
        log::info!("[Unlock] Wallet RPC started");

        // Compute a safe restore_height: current daemon height minus 24h buffer.
        // 720 blocks ≈ 24 hours. Avoids scan-from-genesis (millions of blocks).
        // Sufficient for recent deposits; users who had older funds will need a manual rescan.
        let restore_marker = wd.join(format!("{}.restore_height", wn));
        let requested_height: Option<u64> = std::fs::read_to_string(&restore_marker).ok()
            .and_then(|s| s.trim().parse().ok());
        let restore_height: u64 = requested_height.unwrap_or_else(|| {
            rpc_manager::get_daemon_block_height(&mgr.daemon_address()).saturating_sub(720)
        });
        log::debug!("[Unlock/RPC] restore_height={} (requested: {})", restore_height, requested_height.is_some());

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

        let (address, balance, wallet_pw) = if !wallet_exists || !cache_exists || stale_scan {
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
            let pw = match known_wallet_pw {
                Some(pw) => pw,
                None => new_wallet_password(&wd, &history_key).map_err(|e| (e, false))?,
            };
            let addr = mgr.rpc_restore_wallet(&wn, &pw, &sd, restore_height).map_err(|e| {
                let is_seed = e.contains("word list") || e.contains("Electrum") || e.contains("verification");
                log::error!("[Unlock/RPC] restore failed (seed_invalid={}): {}", is_seed, e);
                (e, is_seed)
            })?;
            log::info!("[Unlock/RPC] Restore OK");
            // Save height marker so we know this wallet was created with a good restore_height
            let _ = std::fs::write(&height_marker, restore_height.max(1).to_string());
            let _ = std::fs::remove_file(&restore_marker);
            // Balance is 0 immediately after restore — sync happens via refresh_balance later.
            let bal = 0.0_f64;
            (addr, bal, pw)
        } else {
            log::info!("[Unlock] Opening wallet (cache height {})", saved_height);
            let tried = known_wallet_pw.unwrap_or_else(|| master.clone());
            let pw = open_wallet_rpc(&mgr, &wn, &tried, Some(&master), &wd, Some(&history_key)).map_err(|e| {
                log::error!("[Unlock/RPC] {}", e);
                (e, false)
            })?;
            log::debug!("[Unlock/RPC] open_wallet OK — fetching address");
            let addr_resp = rpc_manager::ureq_post_json_pub(&mgr.rpc_url(), &serde_json::json!({
                "jsonrpc":"2.0","id":"addr","method":"get_address",
                "params":{"account_index":0}
            })).map_err(|e| {
                log::error!("[Unlock/RPC] get_address failed: {}", e);
                (e, false)
            })?;
            let addr = addr_resp["result"]["address"].as_str().unwrap_or("").to_string();
            log::debug!("[Unlock/RPC] Address retrieved");
            let bal = mgr.rpc_get_balance().map(|(_, u)| u).unwrap_or_else(|e| {
                log::warn!("[Unlock/RPC] get_balance failed: {}", e);
                0.0
            });
            (addr, bal, pw)
        };
        log::debug!("[Unlock/RPC] Session complete — wallet-rpc kept for the session");
        return_wallet_session(mgr);
        Ok::<(String, f64, String), (String, bool)>((address, balance, wallet_pw))
    })
    .await
    .unwrap_or_else(|e| {
        log::error!("[Unlock/RPC] tokio spawn_blocking panicked: {}", e);
        Err((format!("Internal task error: {e}"), false))
    }) };

    let (address, balance_xmr, rpc_error, seed_invalid) = match rpc_result {
        Ok((addr, bal, wallet_pw)) => {
            log::info!("[Unlock] Wallet opened and synced");
            final_wallet_pw = wallet_pw;
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
        log::debug!("[Unlock] Address cached to disk for fast future unlocks");
    }

    let mut s = state.lock().map_err(|e| e.to_string())?;
    s.unlocked = true;
    s.last_unlock_at = chrono::Utc::now().timestamp();
    s.history_key = Some(history_key);
    apply_policy_vault(&mut s);
    s.session_password = Some(password);
    s.wallet_password = Some(final_wallet_pw);
    s.wallet_address = if address.is_empty() { None } else { Some(address.clone()) };
    s.balance_atomic = (balance_xmr * 1e12) as u64;
    s.rpc_connected = rpc_error.is_none();
    s.rpc_last_error = rpc_error.clone();
    s.wallet_file_path = wallet_file_path.clone();

    log::info!("[Unlock] Done. rpc_connected={}, seed_invalid={}", s.rpc_connected, seed_invalid);
    drop(s);
    refresh_pause_item(&app_handle);
    refresh_alerts_and_tray(&app_handle);
    sync_unattended(&app_handle);
    session_unlocked(&app_handle);
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

/// LibreMonero price route on its onion service; None until it is published.
/// Kept here rather than in the frontend so the webview cannot point the Tor client elsewhere.
const LIBREMONERO_ONION_PRICE_URL: Option<&str> = None;
/// Local Tor SOCKS ports: tor daemon, then Tor Browser.
const TOR_SOCKS_PORTS: [u16; 2] = [9050, 9150];

/// Fiat quotes through a local Tor client (opt-in fiat display only, never used for payments).
#[tauri::command]
async fn fetch_price_onion() -> Result<serde_json::Value, String> {
    fetch_json_via_tor(LIBREMONERO_ONION_PRICE_URL.ok_or("ONION_UNAVAILABLE")?).await
}

async fn fetch_json_via_tor(url: &str) -> Result<serde_json::Value, String> {
    let mut last_err = String::from("TOR_UNAVAILABLE");
    for port in TOR_SOCKS_PORTS {
        // socks5h: the .onion name is resolved by Tor, never by the system resolver.
        let client = reqwest::Proxy::all(format!("socks5h://127.0.0.1:{port}"))
            .and_then(|proxy| reqwest::Client::builder()
                .proxy(proxy)
                .timeout(std::time::Duration::from_secs(30))
                .build())
            .map_err(|e| e.to_string())?;
        match client.get(url).send().await {
            Ok(resp) if resp.status().is_success() => return resp.json().await.map_err(|e| e.to_string()),
            Ok(resp) => last_err = format!("HTTP {}", resp.status()),
            Err(e) => last_err = format!("TOR_UNAVAILABLE: {e}"),
        }
    }
    Err(last_err)
}

/// Next due date of a cron schedule (computed here so the frontend never re-implements cron).
#[tauri::command]
fn next_occurrence_of(schedule: String) -> String {
    next_occurrence(&schedule, chrono::Utc::now()).to_rfc3339()
}

/// Update a subscription's next_payment_at (deferred or skipped payment).
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

/// Send all funds (sweep_all) to a destination address.
/// Requires the wallet to be unlocked (session_password must be set).
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

    let (wallet_name, wallet_dir, password, rpc_busy) = {
        let s = state.lock().map_err(|e| e.to_string())?;
        if !s.unlocked { return Err("Wallet is locked.".to_string()); }
        let wn = s.wallet_file_path.file_name()
            .and_then(|n| n.to_str()).unwrap_or("autopay_wallet").to_string();
        let wd = app_handle.path().app_data_dir().map_err(|e| e.to_string())?;
        let pw = s.wallet_password.clone().ok_or("Wallet is locked.")?;
        (wn, wd, pw, Arc::clone(&s.rpc_busy))
    };
    if rpc_busy.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).is_err() {
        return Err(RPC_BUSY.into());
    }

    log::info!("[Sweep] Initiating sweep_all");

    let app_rpc = app_handle.clone();
    let tx_hash = tokio::task::spawn_blocking(move || {
        struct RpcGuard(Arc<AtomicBool>);
        impl Drop for RpcGuard { fn drop(&mut self) { self.0.store(false, Ordering::SeqCst); } }
        let _guard = RpcGuard(rpc_busy);
        let mgr = take_wallet_session(&app_rpc, &wallet_dir, &wallet_name, &password)?;

        // sweep_all
        let resp = rpc_manager::ureq_post_json_pub(&mgr.rpc_url(), &serde_json::json!({
            "jsonrpc":"2.0","id":"sweep","method":"sweep_all",
            "params":{
                "address": dest,
                "account_index": 0,
                "get_tx_hex": false
            }
        }))?;
        return_wallet_session(mgr);

        if let Some(err) = resp.get("error") {
            let msg = err.get("message").and_then(|m| m.as_str()).unwrap_or("unknown");
            return Err(format!("sweep_all RPC error: {}", msg));
        }

        // Return first tx hash
        let hash = resp["result"]["tx_hash_list"][0]
            .as_str()
            .unwrap_or("")
            .to_string();
        log::info!("[Sweep] ✅ Sweep transaction sent");
        Ok(hash)
    })
    .await
    .map_err(|e| e.to_string())??;

    Ok(tx_hash)
}

/// Refresh balance using the stored session password (UI request).
#[tauri::command]
async fn refresh_balance(app_handle: tauri::AppHandle) -> Result<BalanceResponse, String> {
    refresh_wallet(&app_handle).await
}

/// Sync the wallet through a transient wallet-rpc session and store the balance.
/// Shared by the UI and the background loop; emits `wallet-refreshed` when done.
async fn refresh_wallet(app_handle: &tauri::AppHandle) -> Result<BalanceResponse, String> {
    let state = app_handle.state::<Mutex<AppState>>();
    let (wallet_name, wallet_dir, password, rpc_busy, need_address) = {
        let mut s = state.lock().map_err(|e| e.to_string())?;
        if !s.unlocked { return Err("Locked".into()); }
        if netcheck::offline(s.rpc.config.daemon_address.as_deref()) {
            if s.rpc_last_error.as_deref() != Some(netcheck::OFFLINE) {
                log::info!("[Network] No network connection — sync paused until it comes back");
            }
            s.last_refresh_at = chrono::Utc::now().timestamp();
            s.rpc_connected = false;
            s.rpc_last_error = Some(netcheck::OFFLINE.into());
            s.ready_state = None;
            drop(s);
            refresh_alerts_and_tray(app_handle);
            app_handle.emit("wallet-refreshed", serde_json::json!({ "ok": false, "error": netcheck::OFFLINE })).ok();
            return Err(netcheck::OFFLINE.into());
        }
        if s.rpc_last_error.as_deref() == Some(netcheck::OFFLINE) {
            log::info!("[Network] Network connection is back — syncing");
        }
        let need_address = s.wallet_address.is_none();
        let wn = s.wallet_file_path.file_name()
            .and_then(|n| n.to_str()).unwrap_or("autopay_wallet").to_string();
        let wd = app_handle.path().app_data_dir().map_err(|e| e.to_string())?;
        let pw = s.wallet_password.clone().ok_or("Wallet is locked.")?;
        let busy = Arc::clone(&s.rpc_busy);
        (wn, wd, pw, busy, need_address)
    };
    if let Ok(mut s) = state.lock() { s.last_refresh_at = chrono::Utc::now().timestamp(); }

    // If a payment is in progress, skip this refresh rather than crashing the payment
    if rpc_busy.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).is_err() {
        log::debug!("[Sync] Wallet busy (sync or payment running) — refresh skipped");
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

        progress("opening", serde_json::json!({}));
        let mgr = take_wallet_session(&app_rpc, &wallet_dir, &wallet_name, &password)?;
        let daemon_height_before = rpc_manager::get_daemon_block_height(&mgr.daemon_address());

        // wallet-rpc refreshes on its own in the background and answers nothing meanwhile,
        // so get_height often times out here.
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
        let behind_txt = blocks_behind.map(|b| b.to_string()).unwrap_or_else(|| "unknown".into());
        if blocks_behind.is_none_or(|b| b > SYNC_LOG_MIN_BLOCKS) {
            log::info!("[Sync] Wallet sync started ({} blocks behind)", behind_txt);
        } else {
            log::debug!("[Sync] Wallet sync started ({} blocks behind)", behind_txt);
        }
        progress("syncing", serde_json::json!({ "blocks_behind": blocks_behind }));

        // A wallet that was closed for days can need several minutes on a remote node.
        let refresh_started = std::time::Instant::now();
        let refresh_resp = rpc_manager::ureq_post_json_long(&mgr.rpc_url(), &serde_json::json!({
            "jsonrpc":"2.0","id":"refresh","method":"refresh",
            "params":{"start_height":0}
        }), 600);
        progress("reading", serde_json::json!({}));
        match &refresh_resp {
            Ok(r) => {
                let fetched = r["result"]["blocks_fetched"].as_u64().unwrap_or(0);
                if fetched > SYNC_LOG_MIN_BLOCKS {
                    log::info!("[Sync] Wallet synced ({} new block(s))", fetched);
                } else {
                    log::debug!("[Sync] Wallet synced ({} new block(s))", fetched);
                }
            }
            Err(e) => log::warn!("[Sync] Wallet sync failed (will retry): {}", e),
        }

        let wallet_height = rpc_manager::ureq_post_json_pub(&mgr.rpc_url(), &serde_json::json!({
            "jsonrpc":"2.0","id":"h2","method":"get_height","params":{}
        })).ok().and_then(|r| r["result"]["height"].as_u64());
        if refresh_resp.as_ref().map(|r| r.get("error").is_none()).unwrap_or(false) {
            if let Some(h) = wallet_height {
                let _ = std::fs::write(&synced_height_file, h.to_string());
            }
        }

        let (total, unlocked) = mgr.rpc_get_balance()?;
        let unlocked_atomic = (unlocked * 1e12) as u64;
        let atomic = (total * 1e12) as u64;
        log::debug!("[Sync] Balance read");
        record_sent_in_period(&app_rpc, &mgr.rpc_url());

        // The address is normally read at unlock; recover it if the RPC was unavailable then.
        let address = if need_address {
            let addr = rpc_manager::ureq_post_json_pub(&mgr.rpc_url(), &serde_json::json!({
                "jsonrpc":"2.0","id":"addr","method":"get_address","params":{"account_index":0}
            })).ok()
                .and_then(|r| r["result"]["address"].as_str().map(str::to_string))
                .filter(|a| !a.is_empty());
            if let Some(a) = &addr {
                let _ = std::fs::write(wallet_dir.join(format!("{}.address", wallet_name)), a);
                log::info!("[refresh_balance] Address recovered and cached");
            }
            addr
        } else {
            None
        };

        let node = mgr.daemon_address();
        // A slow or failed sync comes from the node most of the time: avoid it for a while and
        // restart wallet-rpc on another one next time.
        let fetched = refresh_resp.as_ref().ok()
            .and_then(|r| r["result"]["blocks_fetched"].as_u64()).unwrap_or(0);
        let secs = refresh_started.elapsed().as_secs();
        let too_slow = secs > SLOW_SYNC_SECS && fetched < SLOW_SYNC_MAX_BLOCKS;
        if refresh_resp.is_err() || too_slow {
            if too_slow {
                log::warn!("[Node] 🐢 {} too slow ({}s for {} block(s)) — switching node", node, secs, fetched);
            } else {
                log::warn!("[Node] {} failed to sync — switching node", node);
            }
            if let Some(st) = app_rpc.try_state::<Mutex<AppState>>() {
                if let Ok(mut s) = st.lock() {
                    s.slow_nodes.insert(node.clone(), chrono::Utc::now().timestamp() + SLOW_NODE_AVOID_SECS);
                    if s.active_node.as_deref() == Some(node.as_str()) { s.active_node = None; }
                }
            }
            drop(mgr);
        } else {
            return_wallet_session(mgr);
        }
        Ok::<_, String>((total, atomic, unlocked_atomic, wallet_height, node, daemon_height_before, address))
    })
    .await
    .map_err(|e| e.to_string())
    .and_then(|r| r);

    let (xmr, atomic, unlocked_atomic, wallet_height, daemon_addr, daemon_height_before, address) = match rpc_result {
        Ok(v) => v,
        Err(e) => {
            log::warn!("[Sync] Wallet refresh failed: {}", e);
            if let Ok(mut s) = state.lock() {
                s.rpc_connected = false;
                s.rpc_last_error = Some(e.clone());
                s.ready_state = None;
            }
            refresh_alerts_and_tray(app_handle);
            app_handle.emit("wallet-refreshed", serde_json::json!({ "ok": false, "error": e })).ok();
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

    let mut stalled_node = None;
    if let Ok(mut s) = state.lock() {
        let now = chrono::Utc::now().timestamp();
        s.balance_atomic = atomic;
        s.unlocked_balance_atomic = unlocked_atomic;
        s.rpc_connected = true;
        s.rpc_last_error = None;
        if address.is_some() { s.wallet_address = address; }
        if let Some(h) = wallet_height { s.last_wallet_height = h; }
        if daemon_reachable {
            if daemon_height > s.last_daemon_height || s.daemon_height_changed_at == 0 {
                s.daemon_height_changed_at = now;
            }
            s.last_daemon_height = daemon_height;
            s.last_daemon_contact_at = Some(now);
            let behind = wallet_height.is_some_and(|h| daemon_height > h + WALLET_BEHIND_BLOCKS);
            s.wallet_behind_since = if behind { Some(s.wallet_behind_since.unwrap_or(now)) } else { None };
        }
        // A node whose height stops moving is stuck or feeding a stale chain: leave it
        // (a node pinned by the user is only reported).
        if daemon_reachable && s.daemon_height_changed_at > 0
            && now - s.daemon_height_changed_at > NODE_STALL_SECS
            && s.rpc.config.daemon_address.is_none()
        {
            if let Some(node) = s.active_node.take() {
                s.slow_nodes.insert(node.clone(), now + SLOW_NODE_AVOID_SECS);
                s.daemon_height_changed_at = now;
                stalled_node = Some(node);
            }
        }
    }
    if let Some(node) = stalled_node {
        log::warn!("[Node] {} has not moved for {} min — switching node", node, NODE_STALL_SECS / 60);
        end_wallet_session("node stalled");
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
                update_tray_icon(app_handle, worst, count);
            }
        }
        // Notify frontend to refresh alerts immediately
        app_handle.emit("alerts-updated", ()).ok();
    }

    log_readiness(app_handle, daemon_reachable, atomic);
    let resp = BalanceResponse { xmr, atomic, daemon_reachable };
    app_handle.emit("wallet-refreshed", serde_json::json!({ "ok": true, "balance": &resp })).ok();
    Ok(resp)
}

/// A sync slower than this with few blocks to fetch means the node is the bottleneck.
const SLOW_SYNC_SECS: u64 = 60;
const SLOW_SYNC_MAX_BLOCKS: u64 = 200;
/// How long a slow or failing node is avoided.
const SLOW_NODE_AVOID_SECS: i64 = 3600;

/// Routine syncs (a few blocks every 2 min) are only logged at debug level.
const SYNC_LOG_MIN_BLOCKS: u64 = 20;

/// Seconds between two background wallet syncs.
const BACKGROUND_REFRESH_SECS: i64 = 120;

/// Keep the wallet synced while the session is open, whatever the window state.
async fn background_refresh(app: &tauri::AppHandle) {
    let due = app.try_state::<Mutex<AppState>>()
        .and_then(|st| st.lock().ok().map(|s| s.unlocked
            && chrono::Utc::now().timestamp() - s.last_refresh_at >= BACKGROUND_REFRESH_SECS
            && !s.rpc_busy.load(Ordering::SeqCst)))
        .unwrap_or(false);
    if due {
        log::debug!("[Sync] Background refresh");
        let _ = refresh_wallet(app).await;
    }
}

/// Log the "ready to send" verdict after a refresh, only when it changes.
fn log_readiness(app_handle: &tauri::AppHandle, daemon_reachable: bool, balance_atomic: u64) {
    let next_amount = app_handle.try_state::<Mutex<Database>>()
        .and_then(|db| db.lock().ok().and_then(|db| db.list_subscriptions().ok()))
        .and_then(|subs| subs.into_iter()
            .filter(|s| s.status == SubscriptionStatus::Active && s.next_payment_at.is_some())
            .min_by_key(|s| s.next_payment_at)
            .map(|s| payment_policy::parse_amount(&s.amount).unwrap_or(0.0)));
    let verdict: &'static str = if !daemon_reachable {
        "offline"
    } else {
        match next_amount {
            Some(a) if balance_atomic < ((a + payment_policy::FEE_MARGIN_XMR) * 1e12) as u64 => "low",
            Some(_) => "ready",
            None => "idle",
        }
    };
    let Some(state) = app_handle.try_state::<Mutex<AppState>>() else { return };
    let Ok(mut s) = state.lock() else { return };
    if s.ready_state == Some(verdict) { return; }
    s.ready_state = Some(verdict);
    drop(s);
    match verdict {
        "ready" => log::info!("[Ready] 🟢 Wallet synced, node reachable, funds cover the next payment"),
        "idle" => log::info!("[Ready] 🟢 Wallet synced, node reachable (no active subscription)"),
        "low" => log::warn!("[Ready] 🟠 Wallet synced, but funds do not cover the next payment"),
        _ => log::warn!("[Ready] 🔴 Wallet synced locally, remote node unreachable"),
    }
}

/// Seconds between two plain-language status lines (day and night follow-up).
const STATUS_HEARTBEAT_SECS: i64 = 30 * 60;
/// Seconds between two passes of the age-based log cleanup.
const LOG_PRUNE_SECS: i64 = 3600;
static LAST_STATUS_AT: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
static LAST_LOG_PRUNE_AT: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

/// Every 30 min, one non-technical line telling whether AutoPay can pay.
/// Also runs the hourly log cleanup. No amounts, no addresses.
fn status_heartbeat(app: &tauri::AppHandle) {
    let now = chrono::Utc::now().timestamp();
    if now - LAST_LOG_PRUNE_AT.load(Ordering::Relaxed) >= LOG_PRUNE_SECS {
        LAST_LOG_PRUNE_AT.store(now, Ordering::Relaxed);
        if let Ok(log_dir) = app.path().app_log_dir() {
            log_retention::prune(&log_dir);
        }
    }
    if now - LAST_STATUS_AT.load(Ordering::Relaxed) < STATUS_HEARTBEAT_SECS {
        return;
    }
    LAST_STATUS_AT.store(now, Ordering::Relaxed);

    let Some((unlocked, ready, last_refresh)) = app.try_state::<Mutex<AppState>>()
        .and_then(|st| st.lock().ok().map(|s| (s.unlocked, s.ready_state, s.last_refresh_at)))
    else { return };
    if !unlocked {
        log::info!("[Status] 🔒 AutoPay locked — no payment until the wallet is unlocked");
        return;
    }
    let (active, next_at) = app.try_state::<Mutex<Database>>()
        .and_then(|db| db.lock().ok().and_then(|db| db.list_subscriptions().ok()))
        .map(|subs| {
            let active: Vec<_> = subs.iter().filter(|s| s.status == SubscriptionStatus::Active).collect();
            (active.len(), active.iter().filter_map(|s| s.next_payment_at).min())
        })
        .unwrap_or((0, None));
    let next = match next_at {
        Some(t) => {
            let mins = (t.timestamp() - now) / 60;
            if mins <= 0 { "next payment due now".to_string() } else { format!("next payment in {}", human_minutes(mins)) }
        }
        None => "no payment scheduled".to_string(),
    };
    let sync_age_min = (now - last_refresh) / 60;
    let stale = if last_refresh > 0 && sync_age_min >= 10 {
        format!(" (last sync {} ago)", human_minutes(sync_age_min))
    } else {
        String::new()
    };
    match ready {
        Some("ready") => log::info!("[Status] 🟢 AutoPay active — {} active subscription(s), {}{}", active, next, stale),
        Some("idle") => log::info!("[Status] 🟢 AutoPay active — no active subscription{}", stale),
        Some("low") => log::warn!("[Status] 🟠 AutoPay active but funds do not cover the next payment — {}{}", next, stale),
        Some(_) => log::warn!("[Status] 🔴 AutoPay cannot reach a remote node — {}{}", next, stale),
        None => log::info!("[Status] ⏳ AutoPay unlocked, wallet not synced yet — {}", next),
    }
}

fn human_minutes(mins: i64) -> String {
    match mins {
        m if m < 60 => format!("{m} min"),
        m if m < 48 * 60 => format!("{} h {:02} min", m / 60, m % 60),
        m => format!("{} days", m / (24 * 60)),
    }
}

/// Return current active alerts (for frontend badge display).
#[tauri::command]
async fn get_alerts(state: tauri::State<'_, Mutex<AppState>>) -> Result<Vec<AppAlert>, String> {
    let s = state.lock().map_err(|e| e.to_string())?;
    Ok(s.alerts.clone())
}

/// Recompute alerts from the current state and update the tray icon accordingly.
fn refresh_alerts_and_tray(app_handle: &tauri::AppHandle) {
    if let (Some(state_rc), Some(db_rc)) = (
        app_handle.try_state::<Mutex<AppState>>(),
        app_handle.try_state::<Mutex<Database>>(),
    ) {
        if let (Ok(mut s), Ok(db)) = (state_rc.lock(), db_rc.lock()) {
            if let Ok(subs) = db.list_subscriptions() {
                let worst = recompute_alerts(&mut s, &subs, chrono::Utc::now().timestamp());
                let count = s.alerts.len();
                drop(s);
                update_tray_icon(app_handle, worst, count);
            }
        }
    }
}

/// Recompute all alerts based on current state and subscriptions.
/// Returns the worst AlertLevel, or None if clean.
/// Alerts raised by an event (a failed or blocked payment) rather than derived from the
/// current state: recompute_alerts keeps them for EVENT_ALERT_TTL_SECS unless the
/// subscription has been paid since.
const EVENT_ALERT_KINDS: &[&str] = &["PAYMENT_FAILED", "OUTPUTS_LOCKED", "PAYMENT_BLOCKED_LIMITS"];
const EVENT_ALERT_TTL_SECS: i64 = 24 * 3600;
/// Without a new block for this long, the node is considered stuck (~15 expected blocks).
const NODE_STALL_SECS: i64 = 30 * 60;
/// Blocks of lag, and for how long, before the wallet is reported as not synced.
const WALLET_BEHIND_BLOCKS: u64 = 10;
const WALLET_BEHIND_SECS: i64 = 30 * 60;

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

    // ORANGE: the node's height stopped moving (blocks come every ~2 min).
    // Only while the node answers: an unreachable node is OFFLINE_WITH_PENDING's business.
    let node_answering = state.last_daemon_contact_at.is_some_and(|t| now_ts - t < 15 * 60);
    if state.unlocked && node_answering && state.daemon_height_changed_at > 0
        && now_ts - state.daemon_height_changed_at > NODE_STALL_SECS
    {
        alerts.push(AppAlert {
            level: AlertLevel::Orange,
            kind: "NODE_STALLED".into(),
            sub_id: None,
            message: format!("Node height unchanged for {} min", (now_ts - state.daemon_height_changed_at) / 60),
            created_at: now_ts,
        });
    }

    // ORANGE: the wallet stays behind the node.
    if state.wallet_behind_since.is_some_and(|t| now_ts - t > WALLET_BEHIND_SECS) {
        alerts.push(AppAlert {
            level: AlertLevel::Orange,
            kind: "WALLET_NOT_SYNCED".into(),
            sub_id: None,
            message: "Wallet behind the node".into(),
            created_at: now_ts,
        });
    }

    // RED: the Monero engine cannot start at all, nothing can be paid.
    if state.rpc_last_error.as_deref().is_some_and(|e| e.starts_with("RPC_BINARY_MISSING")) {
        alerts.push(AppAlert {
            level: AlertLevel::Red,
            kind: "RPC_BINARY_MISSING".into(),
            sub_id: None,
            message: "monero-wallet-rpc not found".into(),
            created_at: now_ts,
        });
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
//  Sending policy vault
// ============================================================

const POLICY_VAULT_FILE: &str = "policy.vault";

/// Everything that decides what may leave the wallet without asking. Sealed with the history
/// key (AES-256-GCM): an edited, forged or deleted file never loosens anything, it falls back
/// to the strict defaults.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct PolicyVault {
    semi_auto: bool,
    limits: payment_policy::PaymentLimits,
    wallet_cap: payment_policy::WalletCap,
    approved_terms: std::collections::HashMap<String, String>,
    #[serde(flatten)]
    extras: PolicyExtras,
}

impl Default for PolicyVault {
    fn default() -> Self {
        Self {
            semi_auto: true,
            limits: Default::default(),
            wallet_cap: Default::default(),
            approved_terms: Default::default(),
            extras: Default::default(),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct PolicyExtras {
    /// Per-subscription ceiling over payment_policy::SUB_CAP_DAYS, in XMR.
    pub sub_caps: std::collections::HashMap<String, f64>,
    /// address_key(address) -> Unix time the address was first approved.
    pub address_seen: std::collections::HashMap<String, i64>,
    /// No automatic payment at all while set.
    pub paused: bool,
    pub notify_on_send: bool,
    pub sound_on_send: bool,
    /// Last successful seed backup check.
    pub seed_checked_at: Option<i64>,
}

impl Default for PolicyExtras {
    fn default() -> Self {
        Self {
            sub_caps: Default::default(),
            address_seen: Default::default(),
            paused: false,
            notify_on_send: true,
            sound_on_send: true,
            seed_checked_at: None,
        }
    }
}

/// Address identifier stored in the vault (never the address itself).
fn address_key(address: &str) -> String {
    blake3::hash(address.trim().as_bytes()).to_hex().to_string()
}

/// Record that the user agreed to the current terms of `sub`.
fn approve_terms(s: &mut AppState, sub: &Subscription) {
    s.approved_terms.insert(sub.id.clone(), sub_fingerprint(sub));
    s.policy_extras.address_seen
        .entry(address_key(&sub.destination_address))
        .or_insert_with(|| chrono::Utc::now().timestamp());
}

/// Master password check for loosening a guard (constant-time comparison).
fn check_master_password(s: &AppState, password: Option<String>) -> Result<(), String> {
    let session = s.session_password.as_deref().ok_or("Wallet is locked.")?;
    let mut given = password.unwrap_or_default();
    let same = given.len() == session.len()
        && given.bytes().zip(session.bytes()).fold(0u8, |acc, (a, b)| acc | (a ^ b)) == 0;
    given.zeroize();
    if same { Ok(()) } else { Err("Incorrect password.".into()) }
}

fn write_sealed(key: &[u8; 32], path: &std::path::Path, plain: &[u8]) -> Result<(), String> {
    let blob = aes_encrypt(key, plain)?;
    let tmp = path.with_extension("tmp");
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    std::io::Write::write_all(&mut opts.open(&tmp).map_err(|e| e.to_string())?, blob.as_bytes())
        .map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}

/// Persist the policy currently in `s` (requires an unlocked session).
fn save_policy_vault(s: &AppState) -> Result<(), String> {
    let key = s.history_key.as_ref().ok_or("Wallet is locked.")?;
    let vault = PolicyVault {
        semi_auto: s.semi_auto_mode,
        limits: s.payment_limits.clone(),
        wallet_cap: s.wallet_cap.clone(),
        approved_terms: s.approved_terms.clone(),
        extras: s.policy_extras.clone(),
    };
    let plain = serde_json::to_vec(&vault).map_err(|e| e.to_string())?;
    write_sealed(key, &s.history_path.with_file_name(POLICY_VAULT_FILE), &plain)
}

/// Load the sealed policy into `s` at unlock, migrating the plain files of earlier versions.
fn apply_policy_vault(s: &mut AppState) {
    let Some(key) = s.history_key.as_ref() else { return };
    let path = s.history_path.with_file_name(POLICY_VAULT_FILE);
    let vault = match std::fs::read_to_string(&path) {
        Ok(b64) => aes_decrypt(key, b64.trim()).ok()
            .and_then(|plain| serde_json::from_slice::<PolicyVault>(&plain).ok())
            .unwrap_or_else(|| {
                log::error!("[Policy] Policy vault altered or unreadable — strict defaults applied");
                PolicyVault::default()
            }),
        Err(_) => {
            let dir = path.parent().map(PathBuf::from).unwrap_or_default();
            PolicyVault {
                semi_auto: load_legacy_semi_auto_mode(&dir),
                limits: payment_policy::PaymentLimits::load_legacy(&dir),
                wallet_cap: payment_policy::WalletCap::load_legacy(&dir),
                approved_terms: payment_policy::load_legacy_approved_terms(&dir),
                extras: PolicyExtras::default(),
            }
        }
    };
    let migrating = !path.exists();
    s.semi_auto_mode = vault.semi_auto;
    s.payment_limits = vault.limits;
    s.wallet_cap = vault.wallet_cap;
    s.approved_terms = vault.approved_terms;
    // A pause requested from the tray while locked survives the unlock.
    let paused_while_locked = s.policy_extras.paused;
    s.policy_extras = vault.extras;
    s.policy_extras.paused |= paused_while_locked;
    if migrating {
        match save_policy_vault(s) {
            Ok(()) => {
                if let Some(dir) = path.parent() {
                    payment_policy::remove_legacy_files(dir);
                    let _ = std::fs::remove_file(dir.join(PAYMENT_MODE_FILE));
                }
                log::info!("[Policy] Sending policy moved to the encrypted vault");
            }
            Err(e) => log::error!("[Policy] Could not create the policy vault: {}", e),
        }
    }
}

// ============================================================
//  Payment mode (semi-auto / auto)
// ============================================================

const PAYMENT_MODE_FILE: &str = "payment_mode.json";

/// Payment mode saved in plain text by earlier versions (migration only).
fn load_legacy_semi_auto_mode(data_dir: &std::path::Path) -> bool {
    std::fs::read_to_string(data_dir.join(PAYMENT_MODE_FILE)).ok()
        .and_then(|j| serde_json::from_str::<serde_json::Value>(&j).ok())
        .and_then(|v| v["semi_auto"].as_bool())
        .unwrap_or(true)
}

/// Set the payment mode. "semi-auto" = queue pending, "auto" = send directly. Persisted.
#[tauri::command]
async fn set_payment_mode(
    semi_auto: bool,
    app_state: tauri::State<'_, Mutex<AppState>>,
) -> Result<(), String> {
    let mut s = app_state.lock().map_err(|e| e.to_string())?;
    s.semi_auto_mode = semi_auto;
    save_policy_vault(&s)?;
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
        let mut s = app_state.lock().map_err(|e| e.to_string())?;
        approve_terms(&mut s, &sub);
        save_policy_vault(&s)?;
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

/// How long technical logs are kept on disk, in hours (0 = not written).
#[tauri::command]
async fn get_log_retention() -> Result<serde_json::Value, String> {
    Ok(serde_json::json!({
        "hours": log_retention::hours(),
        "max_hours": log_retention::MAX_HOURS,
    }))
}

#[tauri::command]
async fn set_log_retention(hours: u32, app_handle: tauri::AppHandle) -> Result<(), String> {
    let data_dir = app_handle.path().app_data_dir().map_err(|e| e.to_string())?;
    log_retention::save(&data_dir, hours)?;
    log::info!("[Logs] Retention set to {} h", hours);
    if let Ok(log_dir) = app_handle.path().app_log_dir() {
        log_retention::prune(&log_dir);
    }
    Ok(())
}

/// Log lines still inside the retention window, for the logs drawer.
#[tauri::command]
async fn get_log_history(app_handle: tauri::AppHandle) -> Result<Vec<log_retention::HistoryLine>, String> {
    if !log_retention::viewable() { return Err("Locked".into()); }
    let log_dir = app_handle.path().app_log_dir().map_err(|e| e.to_string())?;
    Ok(log_retention::history(&log_dir))
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
    s.payment_limits = payment_policy::PaymentLimits { allow_large_amounts, allow_short_cadence };
    save_policy_vault(&s)?;
    log::info!("[Policy] Limits updated: large_amounts={}, short_cadence={}", allow_large_amounts, allow_short_cadence);
    // Blocks raised under the previous limits are stale; the scheduler re-raises them if still relevant.
    s.alerts.retain(|a| a.kind != "PAYMENT_BLOCKED_LIMITS");
    drop(s);
    refresh_alerts_and_tray(&app_handle);
    Ok(())
}

/// Returned instead of sending while every automatic payment is paused.
const PAUSED: &str = "PAUSED";

/// Seconds a subscription refused by the wallet cap waits before the next attempt.
const CAP_RETRY_SECS: i64 = 3600;

/// Outgoing transfers as the wallet itself reports them, confirmed or pending (the app
/// history can be disabled or cleared, the wallet's transfers cannot).
fn wallet_out_stats(rpc_url: &str, period_start: i64, dest: Option<&str>) -> Result<payment_policy::OutStats, String> {
    let r = rpc_manager::ureq_post_json_pub(rpc_url, &serde_json::json!({
        "jsonrpc":"2.0","id":"tr","method":"get_transfers","params":{"out":true,"pending":true}
    }))?;
    if let Some(err) = r.get("error") {
        return Err(format!("get_transfers error: {}", err["message"].as_str().unwrap_or("?")));
    }
    let now = chrono::Utc::now().timestamp();
    let sub_start = now - payment_policy::SUB_CAP_DAYS * 86400;
    let mut st = payment_policy::OutStats::default();
    for kind in ["out", "pending"] {
        for t in r["result"][kind].as_array().into_iter().flatten() {
            // A transfer without timestamp counts: unknown means possibly recent.
            let ts = t["timestamp"].as_i64().unwrap_or(i64::MAX);
            if ts >= period_start {
                st.sent_period = st.sent_period
                    .saturating_add(t["amount"].as_u64().unwrap_or(0))
                    .saturating_add(t["fee"].as_u64().unwrap_or(0));
            }
            if ts >= now - 86400 { st.sends_last_day += 1; }
            if let Some(d) = dest {
                let to_dest: u64 = t["destinations"].as_array().into_iter().flatten()
                    .filter(|x| x["address"].as_str() == Some(d))
                    .map(|x| x["amount"].as_u64().unwrap_or(0))
                    .sum();
                if to_dest > 0 {
                    st.dest_paid_before = true;
                    if ts >= sub_start { st.to_dest_recent = st.to_dest_recent.saturating_add(to_dest); }
                }
            }
        }
    }
    Ok(st)
}

fn cap_period_start(cap: &payment_policy::WalletCap) -> i64 {
    chrono::Utc::now().timestamp() - cap.period_days as i64 * 86400
}

/// Refresh the usage figures shown on the Wallets page.
fn record_sent_in_period(app: &tauri::AppHandle, rpc_url: &str) {
    let Some(st) = app.try_state::<Mutex<AppState>>() else { return };
    let Some(cap) = st.lock().ok().map(|s| s.wallet_cap.clone()) else { return };
    let stats = wallet_out_stats(rpc_url, cap_period_start(&cap), None).ok();
    if let Ok(mut s) = st.lock() {
        s.sent_in_period_atomic = stats.map(|x| x.sent_period);
        s.sends_last_day = stats.map(|x| x.sends_last_day);
    };
}

/// Enforce the sending guards right before a transfer. Fails closed: if the wallet cannot tell
/// what was sent, nothing is sent.
fn check_send_guards(
    app: &tauri::AppHandle,
    mgr: &RpcManager,
    amount_atomic: u64,
    dest: &str,
    sub_id: Option<&str>,
) -> Result<(), String> {
    let st = app.try_state::<Mutex<AppState>>().ok_or("App state unavailable")?;
    let (cap, sub_cap_xmr, dest_first_seen) = {
        let s = st.lock().map_err(|e| e.to_string())?;
        (
            s.wallet_cap.clone(),
            sub_id.and_then(|id| s.policy_extras.sub_caps.get(id).copied()),
            s.policy_extras.address_seen.get(&address_key(dest)).copied(),
        )
    };
    let stats = wallet_out_stats(&mgr.rpc_url(), cap_period_start(&cap), Some(dest))?;
    let (total, _) = mgr.rpc_get_balance()?;
    if let Ok(mut s) = st.lock() {
        s.sent_in_period_atomic = Some(stats.sent_period);
        s.sends_last_day = Some(stats.sends_last_day);
    }
    cap.check_send(&payment_policy::SendContext {
        amount_atomic,
        balance_atomic: (total * 1e12) as u64,
        stats,
        sub_cap_xmr,
        dest_first_seen,
        now: chrono::Utc::now().timestamp(),
    }).map_err(|e| e.to_string())
}

/// Prepare the transfer without relaying it, check its fee against the cap, then relay.
/// A malicious or broken node cannot push AutoPay into paying an abnormal fee.
fn transfer_with_fee_cap(app: &tauri::AppHandle, mgr: &RpcManager, amount_atomic: u64, dest: &str) -> Result<String, String> {
    let cap = app.try_state::<Mutex<AppState>>().ok_or("App state unavailable")?
        .lock().map_err(|e| e.to_string())?.wallet_cap.clone();
    let rpc_error = |resp: &serde_json::Value| resp.get("error").map(|err| {
        format!("transfer error: {}", err.get("message").and_then(|m| m.as_str()).unwrap_or("?"))
    });

    let prepared = rpc_manager::ureq_post_json_pub(&mgr.rpc_url(), &serde_json::json!({
        "jsonrpc":"2.0","id":"tx","method":"transfer",
        "params":{
            "destinations":[{"amount": amount_atomic, "address": dest}],
            "account_index": 0, "priority": 1, "get_tx_key": false,
            "do_not_relay": true, "get_tx_metadata": true
        }
    }))?;
    if let Some(e) = rpc_error(&prepared) { return Err(e); }
    let fee = prepared["result"]["fee"].as_u64().ok_or("transfer error: no fee in the prepared transaction")?;
    if let Err(e) = cap.check_fee(fee) {
        log::warn!("[Payment] 🛑 Prepared transaction dropped: network fee above the cap — nothing sent");
        return Err(e.to_string());
    }
    let metadata = prepared["result"]["tx_metadata"].as_str().filter(|m| !m.is_empty())
        .ok_or("transfer error: no metadata in the prepared transaction")?;

    let relayed = rpc_manager::ureq_post_json_pub(&mgr.rpc_url(), &serde_json::json!({
        "jsonrpc":"2.0","id":"relay","method":"relay_tx","params":{"hex": metadata}
    }))?;
    if let Some(e) = rpc_error(&relayed) { return Err(e); }
    Ok(relayed["result"]["tx_hash"].as_str().unwrap_or("").to_string())
}

/// Wallet cap settings and current usage for the Wallets page.
#[tauri::command]
async fn get_wallet_cap(app_state: tauri::State<'_, Mutex<AppState>>) -> Result<serde_json::Value, String> {
    let s = app_state.lock().map_err(|e| e.to_string())?;
    let sent = s.sent_in_period_atomic;
    let cap_atomic = sent.and_then(|sent| s.wallet_cap.cap_atomic(s.balance_atomic, sent));
    Ok(serde_json::json!({
        "cap": s.wallet_cap,
        "sent_xmr": sent.map(|v| v as f64 / 1e12),
        "cap_xmr": cap_atomic.map(|v| v as f64 / 1e12),
        "remaining_xmr": cap_atomic.zip(sent).map(|(c, sent)| c.saturating_sub(sent) as f64 / 1e12),
        "sends_last_day": s.sends_last_day,
    }))
}

/// Change the wallet cap. Loosening it (higher, longer-reaching, disabled) needs the master password.
#[tauri::command]
async fn set_wallet_cap(
    cap: payment_policy::WalletCap,
    password: Option<String>,
    app_state: tauri::State<'_, Mutex<AppState>>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    cap.validate()?;
    let mut s = app_state.lock().map_err(|e| e.to_string())?;
    if cap.is_looser_than(&s.wallet_cap) {
        check_master_password(&s, password)?;
    }
    log::info!("[Policy] Wallet guards updated: cap={} ({:?}, {} days), floor={}, daily sends={}, new-address delay={} h, fee cap={}",
        cap.enabled, cap.mode, cap.period_days, cap.floor_xmr > 0.0, cap.max_sends_per_day,
        cap.new_address_delay_hours, cap.max_fee_xmr > 0.0);
    s.wallet_cap = cap;
    save_policy_vault(&s)?;
    s.cap_blocked_until.clear();
    s.alerts.retain(|a| a.kind != "PAYMENT_BLOCKED_LIMITS");
    drop(s);
    refresh_alerts_and_tray(&app_handle);
    Ok(())
}

/// Per-subscription caps (XMR over payment_policy::SUB_CAP_DAYS).
#[tauri::command]
async fn get_sub_caps(app_state: tauri::State<'_, Mutex<AppState>>) -> Result<std::collections::HashMap<String, f64>, String> {
    Ok(app_state.lock().map_err(|e| e.to_string())?.policy_extras.sub_caps.clone())
}

/// Set or remove a subscription's cap. Raising or removing it needs the master password.
#[tauri::command]
async fn set_sub_cap(
    sub_id: String,
    cap_xmr: Option<f64>,
    password: Option<String>,
    app_state: tauri::State<'_, Mutex<AppState>>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    if cap_xmr.is_some_and(|c| !(c.is_finite() && c > 0.0)) {
        return Err("Amount must be positive.".into());
    }
    let mut s = app_state.lock().map_err(|e| e.to_string())?;
    let looser = match (s.policy_extras.sub_caps.get(&sub_id).copied(), cap_xmr) {
        (Some(_), None) => true,
        (Some(cur), Some(new)) => new > cur,
        (None, _) => false,
    };
    if looser { check_master_password(&s, password)?; }
    match cap_xmr {
        Some(c) => { s.policy_extras.sub_caps.insert(sub_id.clone(), c); }
        None => { s.policy_extras.sub_caps.remove(&sub_id); }
    }
    save_policy_vault(&s)?;
    s.cap_blocked_until.remove(&sub_id);
    s.alerts.retain(|a| !(a.kind == "PAYMENT_BLOCKED_LIMITS" && a.sub_id.as_deref() == Some(sub_id.as_str())));
    log::info!("[Policy] Subscription cap {} for sub {}", if cap_xmr.is_some() { "set" } else { "removed" },
        &sub_id[..8.min(sub_id.len())]);
    drop(s);
    refresh_alerts_and_tray(&app_handle);
    Ok(())
}

/// Tray entry toggling the global pause (its label follows the state).
struct TrayPauseItem(tauri::menu::MenuItem<tauri::Wry>);
struct TrayBootLogItem(tauri::menu::MenuItem<tauri::Wry>);

/// Technical logs become readable, and the startup journal has done its job.
fn session_unlocked(app: &tauri::AppHandle) {
    log_retention::set_viewable(true);
    boot_log::finish();
    if let Some(item) = app.try_state::<TrayBootLogItem>() {
        let _ = item.0.set_enabled(false);
    }
}

/// Startup journal line from the frontend (only kept before the first unlock).
#[tauri::command]
fn boot_log_append(level: String, module: String, msg: String) {
    if !boot_log::active() { return; }
    let level = match level.as_str() { "error" => "ERROR", "warn" => "WARN", "debug" => "DEBUG", _ => "INFO" };
    let clip = |s: &str, n: usize| s.chars().take(n).collect::<String>();
    boot_log::write(level, &format!("ui:{}", clip(&module, 40)), &clip(&msg, 500));
}

#[tauri::command]
fn boot_frontend_ready() {
    boot_log::set_frontend_ready();
}

/// Startup journal for the locked screen's logs drawer (empty once unlocked).
#[tauri::command]
fn get_startup_log() -> String {
    boot_log::read()
}

const TRAY_PAUSE_LABEL: &str = "Mettre en pause les paiements";
const TRAY_RESUME_LABEL: &str = "Reprendre les paiements";

fn refresh_pause_item(app: &tauri::AppHandle) {
    let paused = app.try_state::<Mutex<AppState>>()
        .and_then(|st| st.lock().ok().map(|s| s.policy_extras.paused))
        .unwrap_or(false);
    if let Some(item) = app.try_state::<TrayPauseItem>() {
        let _ = item.0.set_text(if paused { TRAY_RESUME_LABEL } else { TRAY_PAUSE_LABEL });
    }
}

/// Pause or resume every automatic payment (persisted when the session is unlocked).
fn apply_pause(app: &tauri::AppHandle, paused: bool) {
    if let Some(st) = app.try_state::<Mutex<AppState>>() {
        if let Ok(mut s) = st.lock() {
            s.policy_extras.paused = paused;
            if s.history_key.is_some() {
                if let Err(e) = save_policy_vault(&s) { log::error!("[Policy] Could not save the pause: {}", e); }
            }
        }
    }
    log::info!("[Policy] {} All automatic payments {}", if paused { "⏸" } else { "▶" },
        if paused { "paused" } else { "resumed" });
    refresh_pause_item(app);
    app.emit("pause-changed", paused).ok();
}

#[tauri::command]
fn get_paused(app_state: tauri::State<'_, Mutex<AppState>>) -> bool {
    app_state.lock().map(|s| s.policy_extras.paused).unwrap_or(false)
}

#[tauri::command]
fn set_paused(paused: bool, app_handle: tauri::AppHandle) {
    apply_pause(&app_handle, paused);
}

/// System notification and sound after each successful send.
#[tauri::command]
fn get_send_feedback(app_state: tauri::State<'_, Mutex<AppState>>) -> Result<serde_json::Value, String> {
    let s = app_state.lock().map_err(|e| e.to_string())?;
    Ok(serde_json::json!({ "notify": s.policy_extras.notify_on_send, "sound": s.policy_extras.sound_on_send }))
}

#[tauri::command]
fn set_send_feedback(notify: bool, sound: bool, app_state: tauri::State<'_, Mutex<AppState>>) -> Result<(), String> {
    let mut s = app_state.lock().map_err(|e| e.to_string())?;
    s.policy_extras.notify_on_send = notify;
    s.policy_extras.sound_on_send = sound;
    save_policy_vault(&s)
}

/// Days between two seed backup checks.
const SEED_CHECK_EVERY_DAYS: i64 = 90;

#[tauri::command]
fn get_seed_check(app_state: tauri::State<'_, Mutex<AppState>>) -> Result<serde_json::Value, String> {
    let s = app_state.lock().map_err(|e| e.to_string())?;
    let last = s.policy_extras.seed_checked_at;
    let due = last.is_none_or(|t| chrono::Utc::now().timestamp() - t > SEED_CHECK_EVERY_DAYS * 86400);
    Ok(serde_json::json!({ "checked_at": last, "due": due }))
}

/// Check a few seed words typed from the user's backup (1-based positions).
#[tauri::command]
async fn verify_seed_words(
    positions: Vec<usize>,
    mut words: Vec<String>,
    app_state: tauri::State<'_, Mutex<AppState>>,
) -> Result<bool, String> {
    if positions.is_empty() || positions.len() != words.len() {
        return Err("Invalid request.".into());
    }
    let (vault_path, password) = {
        let s = app_state.lock().map_err(|e| e.to_string())?;
        (s.vault_path.clone(), s.session_password.clone().ok_or("Wallet is locked.")?)
    };
    let encrypted: EncryptedSeed = serde_json::from_str(
        &std::fs::read_to_string(&vault_path).map_err(|e| e.to_string())?,
    ).map_err(|e| e.to_string())?;
    let seed = tokio::task::spawn_blocking(move || SeedVault::decrypt(&encrypted, &password))
        .await.map_err(|e| e.to_string())?.map_err(|e| e.to_string())?;
    let list: Vec<&str> = seed.split_whitespace().collect();
    let ok = positions.iter().zip(&words).all(|(p, w)| {
        *p >= 1 && list.get(p - 1).is_some_and(|x| x.eq_ignore_ascii_case(w.trim()))
    });
    for w in words.iter_mut() { w.zeroize(); }
    log::info!("[Seed] Backup check {}", if ok { "passed" } else { "failed" });
    if ok {
        let mut s = app_state.lock().map_err(|e| e.to_string())?;
        s.policy_extras.seed_checked_at = Some(chrono::Utc::now().timestamp());
        save_policy_vault(&s)?;
    }
    Ok(ok)
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
    if let Some(d) = days { s.history_days = d.clamp(1, 3650); }
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
        Some(k) if s.session_password.is_some() => k,
        _ => return Ok(serde_json::json!({ "enabled": true, "entries": [], "locked": true })),
    };
    let entries = load_history(key, &s.history_path);
    Ok(serde_json::json!({ "enabled": true, "entries": entries }))
}

/// Bank-style statement read from the wallet itself: money in and out, pending and failed
/// sends, labelled with the subscription when the payment history knows the transaction.
#[tauri::command]
async fn get_account_statement(
    state: tauri::State<'_, Mutex<AppState>>,
    app: tauri::AppHandle,
) -> Result<Vec<serde_json::Value>, String> {
    let (wallet_name, wallet_dir, password, rpc_busy, labels) = {
        let s = state.lock().map_err(|e| e.to_string())?;
        if s.session_password.is_none() {
            return Err("Wallet is locked.".into());
        }
        let pw = s.wallet_password.clone().ok_or("Wallet is locked.")?;
        let labels: std::collections::HashMap<String, String> = match (&s.history_key, s.history_enabled) {
            (Some(key), true) => load_history(key, &s.history_path).into_iter()
                .filter(|e| !e.tx_prefix.is_empty())
                .map(|e| (e.tx_prefix, e.sub_label)).collect(),
            _ => Default::default(),
        };
        let wd = app.path().app_data_dir().map_err(|e| e.to_string())?;
        (wallet_name_of(&s.wallet_file_path), wd, pw, Arc::clone(&s.rpc_busy), labels)
    };
    if rpc_busy.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).is_err() {
        return Err("RPC busy".into());
    }
    let app_rpc = app.clone();
    let r = tokio::task::spawn_blocking(move || {
        struct RpcGuard(Arc<AtomicBool>);
        impl Drop for RpcGuard { fn drop(&mut self) { self.0.store(false, Ordering::SeqCst); } }
        let _guard = RpcGuard(rpc_busy);
        let mgr = take_wallet_session(&app_rpc, &wallet_dir, &wallet_name, &password)?;
        let r = rpc_manager::ureq_post_json_pub(&mgr.rpc_url(), &serde_json::json!({
            "jsonrpc":"2.0","id":"st","method":"get_transfers",
            "params":{"in":true,"out":true,"pending":true,"failed":true,"pool":true}
        }));
        return_wallet_session(mgr);
        r
    }).await.map_err(|e| e.to_string())??;
    if let Some(err) = r.get("error") {
        return Err(format!("get_transfers error: {}", err["message"].as_str().unwrap_or("?")));
    }
    let mut lines = Vec::new();
    for kind in ["in", "pool", "out", "pending", "failed"] {
        for t in r["result"][kind].as_array().into_iter().flatten() {
            let txid = t["txid"].as_str().unwrap_or("");
            let prefix: String = txid.chars().take(12).collect();
            lines.push(serde_json::json!({
                "kind": kind,
                "amount_xmr": t["amount"].as_u64().unwrap_or(0) as f64 / 1e12,
                "fee_xmr": t["fee"].as_u64().unwrap_or(0) as f64 / 1e12,
                "timestamp": t["timestamp"].as_i64().unwrap_or(0),
                "confirmations": t["confirmations"].as_u64().unwrap_or(0),
                "tx_prefix": prefix,
                "label": labels.get(&prefix),
            }));
        }
    }
    lines.sort_by_key(|l| std::cmp::Reverse(l["timestamp"].as_i64().unwrap_or(0)));
    Ok(lines)
}

/// Write the payment history as CSV into the Downloads folder (owner-only file).
/// Returns the file path. The export holds amounts by design: it is the user's own record.
#[tauri::command]
async fn export_payment_history(
    app_state: tauri::State<'_, Mutex<AppState>>,
    app_handle: tauri::AppHandle,
) -> Result<String, String> {
    let entries = {
        let s = app_state.lock().map_err(|e| e.to_string())?;
        let key = s.history_key.as_ref().ok_or("Locked")?;
        load_history(key, &s.history_path)
    };
    let csv_field = |v: &str| {
        // Leading =,+,-,@ would run as formulas in spreadsheet apps.
        let v = if v.starts_with(['=', '+', '-', '@']) { format!("'{v}") } else { v.to_string() };
        format!("\"{}\"", v.replace('"', "\"\""))
    };
    let mut csv = String::from("date,subscription,amount_xmr,tx_prefix\n");
    for e in &entries {
        csv.push_str(&format!("{},{},{:.12},{}\n",
            csv_field(&e.sent_at), csv_field(&e.sub_label), e.amount_xmr, csv_field(&e.tx_prefix)));
    }
    let dir = app_handle.path().download_dir()
        .or_else(|_| app_handle.path().home_dir())
        .map_err(|e| e.to_string())?;
    let path = dir.join(format!("crypto-autopay-history-{}.csv", chrono::Local::now().format("%Y-%m-%d_%H%M")));
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    use std::io::Write;
    opts.open(&path).and_then(|mut f| f.write_all(csv.as_bytes())).map_err(|e| e.to_string())?;
    log::info!("[History] Exported {} entr(y/ies) to the Downloads folder", entries.len());
    Ok(path.to_string_lossy().to_string())
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
    let blocks_behind = daemon_h.saturating_sub(wallet_h);
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
        website_url: v1.website_url.as_deref().and_then(payment_policy::clean_website_url),
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

    log::info!("Subscription added (id={})", sub.id);
    serde_json::to_value(&sub).map_err(|e| e.to_string())
}

/// Editable terms of a subscription, read from the frontend form payload.
struct SubTerms {
    label: String,
    dest: String,
    amount: String,
    currency: String,
    start_date: Option<chrono::DateTime<chrono::Utc>>,
    schedule: String,
    number_of_payments: u64,
    canary_url: Option<String>,
    description: Option<String>,
    website_url: Option<String>,
}

fn sub_terms_from_payload(payload: &serde_json::Value, now: chrono::DateTime<chrono::Utc>) -> Result<SubTerms, String> {
    use chrono::Datelike;
    let start_date = payload["start_date"].as_str().filter(|s| !s.is_empty())
        .and_then(|sd| chrono::DateTime::parse_from_rfc3339(sd).ok())
        .map(|d| d.with_timezone(&chrono::Utc));
    // Legacy payloads carry "every N years" in repeat_every: fold it into the schedule.
    let schedule = payment_policy::with_year_step(
        payload["schedule"].as_str().unwrap_or("0 0 9 1 * *"),
        payload["repeat_every"].as_u64().unwrap_or(1).min(u32::MAX as u64) as u32,
        start_date.unwrap_or(now).year(),
    ).map_err(|e| e.to_string())?;
    let description = payment_policy::clean_text(payload["description"].as_str().unwrap_or(""), 500);
    Ok(SubTerms {
        label: payment_policy::clean_text(payload["custom_label"].as_str().unwrap_or("—"), 80),
        dest: payload["destination_address"].as_str().unwrap_or("").trim().to_string(),
        amount: payload["amount"].as_str().unwrap_or("0").to_string(),
        currency: payload["currency"].as_str().unwrap_or("XMR").to_string(),
        start_date,
        schedule,
        // Absent means unlimited, as in AutoPay codes (the form omits 0).
        number_of_payments: payload["number_of_payments"].as_u64().unwrap_or(0),
        canary_url: payload["canary_url"].as_str().filter(|s| !s.is_empty()).map(|s| s.to_string()),
        description: (!description.is_empty()).then_some(description),
        website_url: payload["website_url"].as_str().and_then(payment_policy::clean_website_url),
    })
}

/// Edit an existing subscription. Same checks as a new one; changed terms (address, amount,
/// currency, schedule) no longer match the approved fingerprint, so the next payment asks again.
#[tauri::command]
async fn update_subscription(
    id: String,
    payload: serde_json::Value,
    confirm_monthly: Option<bool>,
    db_state: tauri::State<'_, Mutex<Database>>,
    app_state: tauri::State<'_, Mutex<AppState>>,
) -> Result<serde_json::Value, String> {
    let now = chrono::Utc::now();
    let terms = sub_terms_from_payload(&payload, now)?;
    let (mut sub, others) = {
        let db = db_state.lock().map_err(|e| e.to_string())?;
        let (this, others): (Vec<_>, Vec<_>) = db.list_subscriptions().map_err(|e| e.to_string())?
            .into_iter().partition(|s| s.id == id);
        (this.into_iter().next().ok_or("Subscription not found")?, others)
    };
    {
        let limits = app_state.lock().map_err(|e| e.to_string())?.payment_limits.clone();
        check_new_subscription(&terms.dest, &terms.currency, &terms.amount, &terms.schedule,
            &limits, &others, confirm_monthly.unwrap_or(false))?;
    }
    let reschedule = terms.schedule != sub.schedule || sub.next_payment_at.is_none();
    sub.label = terms.label;
    sub.description = terms.description;
    sub.website_url = terms.website_url;
    sub.destination_address = terms.dest;
    sub.amount = terms.amount;
    sub.currency = terms.currency;
    sub.schedule = terms.schedule;
    sub.number_of_payments = terms.number_of_payments;
    sub.canary_url = terms.canary_url;
    sub.updated_at = now;
    // More payments planned than sent reopens a completed subscription, and the reverse ends it.
    if sub.payments_remaining() == Some(0) {
        sub.status = SubscriptionStatus::Completed;
        sub.next_payment_at = None;
    } else {
        if sub.status == SubscriptionStatus::Completed { sub.status = SubscriptionStatus::Active; }
        if reschedule { sub.next_payment_at = Some(next_occurrence(&sub.schedule, now)); }
    }
    db_state.lock().map_err(|e| e.to_string())?.update_subscription_terms(&sub).map_err(|e| e.to_string())?;
    {
        // A pending or session approval was given for the old terms.
        let mut s = app_state.lock().map_err(|e| e.to_string())?;
        s.pending_approvals.remove(&id);
        s.approved_payments.remove(&id);
    }
    log::info!("Subscription updated (id={})", id);
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
    let SubTerms { label, dest, amount, currency, start_date, schedule, number_of_payments,
        canary_url, description, website_url } = sub_terms_from_payload(&payload, now)?;
    let start_immediately = payload["start_immediately"].as_bool().unwrap_or(true);

    {
        let limits = app_state.lock().map_err(|e| e.to_string())?.payment_limits.clone();
        let existing = db_state.lock().map_err(|e| e.to_string())?.list_subscriptions().map_err(|e| e.to_string())?;
        check_new_subscription(&dest, &currency, &amount, &schedule,
            &limits, &existing, confirm_monthly.unwrap_or(false))?;
    }

    // next_payment_at: now if start_immediately, else use start_date or compute from schedule
    let next_payment_at = if start_immediately {
        Some(now)
    } else if payload["start_date"].as_str().is_some_and(|s| !s.is_empty()) {
        start_date.or(Some(now + chrono::Duration::days(1)))
    } else {
        Some(next_occurrence(&schedule, now))
    };

    let sub = Subscription {
        id: Uuid::new_v4().to_string(),
        wallet_id: "default".to_string(),
        label,
        description,
        website_url,
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

    log::info!("Subscription saved (id={}, start_immediately={})", sub.id, start_immediately);
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
/// The session's wallet-rpc, kept running between operations (see the rpc_manager module docs).
static WALLET_SESSION: Mutex<Option<RpcManager>> = Mutex::new(None);
/// Set when the session must end while an operation is using it: stopped when handed back.
static WALLET_SESSION_INVALIDATED: AtomicBool = AtomicBool::new(false);

/// The session wallet-rpc with the wallet open, started if needed. Callers hold `rpc_busy`.
/// Hand it back with `return_wallet_session`; dropping it instead (error paths) stops it,
/// and the next operation starts a fresh one.
fn take_wallet_session(
    app: &tauri::AppHandle,
    wallet_dir: &std::path::Path,
    wallet_name: &str,
    password: &str,
) -> Result<RpcManager, String> {
    let existing = WALLET_SESSION.lock().ok().and_then(|mut g| g.take());
    if let Some(mut mgr) = existing {
        if mgr.is_alive() { return Ok(mgr); }
        log::warn!("[RPC] Wallet RPC exited unexpectedly — restarting it");
        mgr.stop();
    }
    WALLET_SESSION_INVALIDATED.store(false, Ordering::SeqCst);
    let (mut mgr, _) = live_rpc_manager(app);
    mgr.start_for_dir(wallet_dir)?;
    mgr.wait_for_ready(std::time::Duration::from_secs(15))?;
    let (master, key) = app.try_state::<Mutex<AppState>>()
        .and_then(|st| st.lock().ok().map(|s| (s.session_password.clone(), s.history_key)))
        .unwrap_or((None, None));
    let pw = open_wallet_rpc(&mgr, wallet_name, password, master.as_deref(), wallet_dir, key.as_ref())?;
    if pw != password {
        if let Some(st) = app.try_state::<Mutex<AppState>>() {
            if let Ok(mut s) = st.lock() { s.wallet_password = Some(pw); }
        }
        sync_unattended(app);
    }
    log::info!("[RPC] Wallet RPC started, wallet open (node {})", mgr.daemon_address());
    Ok(mgr)
}

const WALLET_PW_FILE: &str = "wallet_pw.vault";

fn read_wallet_password(data_dir: &std::path::Path, key: &[u8; 32]) -> Option<String> {
    let b64 = std::fs::read_to_string(data_dir.join(WALLET_PW_FILE)).ok()?;
    String::from_utf8(aes_decrypt(key, b64.trim()).ok()?).ok()
}

/// A random wallet password, saved (encrypted with the history key) before it is used.
fn new_wallet_password(data_dir: &std::path::Path, key: &[u8; 32]) -> Result<String, String> {
    let mut raw = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut raw);
    let pw: String = raw.iter().map(|b| format!("{b:02x}")).collect();
    raw.zeroize();
    std::fs::write(data_dir.join(WALLET_PW_FILE), aes_encrypt(key, pw.as_bytes())?)
        .map_err(|e| format!("Wallet password not saved: {e}"))?;
    Ok(pw)
}

fn rpc_open_wallet(mgr: &RpcManager, wallet_name: &str, password: &str) -> Result<(), String> {
    let resp = rpc_manager::ureq_post_json_pub(&mgr.rpc_url(), &serde_json::json!({
        "jsonrpc":"2.0","id":"open","method":"open_wallet",
        "params":{"filename":wallet_name,"password":password}
    }))?;
    match resp.get("error") {
        Some(err) => Err(format!("open_wallet error: {}", err["message"].as_str().unwrap_or("?"))),
        None => Ok(()),
    }
}

/// Open the wallet and return the password that opened it. A wallet still protected by the
/// master password (older versions) is switched to a random password, so the OS keyring
/// never has to hold the master password.
fn open_wallet_rpc(
    mgr: &RpcManager,
    wallet_name: &str,
    password: &str,
    master: Option<&str>,
    data_dir: &std::path::Path,
    key: Option<&[u8; 32]>,
) -> Result<String, String> {
    let opened_with = match rpc_open_wallet(mgr, wallet_name, password) {
        Ok(()) => password.to_string(),
        Err(e) => match master.filter(|m| *m != password) {
            // Interrupted migration: the saved random password was never applied.
            Some(m) => {
                rpc_open_wallet(mgr, wallet_name, m).map_err(|_| e)?;
                m.to_string()
            }
            None => return Err(e),
        },
    };
    let (Some(m), Some(key)) = (master, key) else { return Ok(opened_with) };
    if opened_with != m {
        return Ok(opened_with);
    }
    let new_pw = new_wallet_password(data_dir, key)?;
    let resp = rpc_manager::ureq_post_json_pub(&mgr.rpc_url(), &serde_json::json!({
        "jsonrpc":"2.0","id":"pw","method":"change_wallet_password",
        "params":{"old_password":m,"new_password":new_pw}
    }));
    match resp {
        Ok(r) if r.get("error").is_none() => {
            log::info!("[Wallet] Wallet password replaced by a random one (master password no longer used by the wallet)");
            Ok(new_pw)
        }
        other => {
            let _ = std::fs::remove_file(data_dir.join(WALLET_PW_FILE));
            let why = match other { Ok(r) => r["error"]["message"].as_str().unwrap_or("?").to_string(), Err(e) => e };
            log::warn!("[Wallet] Wallet password unchanged: {}", why);
            Ok(opened_with)
        }
    }
}

/// Keep the keyring entry in line with the setting, from an unlocked interface.
fn sync_unattended(app: &tauri::AppHandle) {
    let Some(st) = app.try_state::<Mutex<AppState>>() else { return };
    let Ok(s) = st.lock() else { return };
    let Some(data_dir) = s.vault_path.parent().map(|p| p.to_path_buf()) else { return };
    let (Some(wallet_pw), Some(key)) = (s.wallet_password.clone(), s.history_key) else { return };
    // Not migrated yet: storing it would put the master password in the keyring.
    if s.session_password.as_deref() == Some(wallet_pw.as_str()) || !unattended::enabled(&data_dir) {
        return;
    }
    drop(s);
    let app = app.clone();
    std::thread::spawn(move || {
        let secrets = unattended::Secrets { wallet_password: wallet_pw, history_key: key };
        let ok = match unattended::store(&secrets) {
            Ok(()) => true,
            Err(e) => {
                log::warn!("[Unattended] OS keyring unavailable, payments need the unlocked app: {}", e);
                false
            }
        };
        if let Some(st) = app.try_state::<Mutex<AppState>>() {
            if let Ok(mut s) = st.lock() {
                if ok && !s.unattended_active { log::info!("[Unattended] Payments will continue while locked and after a restart"); }
                s.unattended_active = ok;
            }
        }
    });
}

/// At launch, with the setting on: start the payment session from the keyring, the
/// interface staying locked.
fn start_unattended(app: &tauri::AppHandle) {
    let Some(st) = app.try_state::<Mutex<AppState>>() else { return };
    let (data_dir, wallet_name, vault_exists) = match st.lock() {
        Ok(s) => (s.vault_path.parent().map(|p| p.to_path_buf()), wallet_name_of(&s.wallet_file_path), s.vault_path.exists()),
        Err(_) => return,
    };
    let Some(data_dir) = data_dir.filter(|_| vault_exists) else { return };
    if !unattended::enabled(&data_dir) {
        return;
    }
    let app = app.clone();
    std::thread::spawn(move || {
        let secrets = match unattended::load() {
            Ok(Some(s)) => s,
            Ok(None) => return,
            Err(e) => {
                log::warn!("[Unattended] OS keyring unavailable, payments wait for the unlock: {}", e);
                return;
            }
        };
        if read_wallet_password(&data_dir, &secrets.history_key).as_deref() != Some(secrets.wallet_password.as_str()) {
            log::warn!("[Unattended] Keyring entry does not match this wallet — removed");
            unattended::forget();
            return;
        }
        let address = std::fs::read_to_string(data_dir.join(format!("{wallet_name}.address"))).ok()
            .map(|a| a.trim().to_string()).filter(|a| a.len() > 90);
        let Some(st) = app.try_state::<Mutex<AppState>>() else { return };
        let Ok(mut s) = st.lock() else { return };
        if s.unlocked { return; }
        s.unlocked = true;
        s.unattended_active = true;
        s.last_unlock_at = chrono::Utc::now().timestamp();
        s.wallet_password = Some(secrets.wallet_password.clone());
        s.history_key = Some(secrets.history_key);
        apply_policy_vault(&mut s);
        s.wallet_address = address;
        drop(s);
        log::info!("[Unattended] Payment session started — interface locked");
        refresh_pause_item(&app);
        refresh_alerts_and_tray(&app);
    });
}

/// Pay while locked: setting state, for the Settings page.
#[tauri::command]
fn get_unattended(state: tauri::State<'_, Mutex<AppState>>) -> Result<serde_json::Value, String> {
    let s = state.lock().map_err(|e| e.to_string())?;
    let dir = s.vault_path.parent().ok_or("No data folder")?;
    Ok(serde_json::json!({ "enabled": unattended::enabled(dir), "active": s.unattended_active }))
}

#[tauri::command]
async fn set_unattended(
    enabled: bool,
    state: tauri::State<'_, Mutex<AppState>>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let dir = {
        let s = state.lock().map_err(|e| e.to_string())?;
        if s.session_password.is_none() { return Err("Wallet is locked.".into()); }
        s.vault_path.parent().ok_or("No data folder")?.to_path_buf()
    };
    unattended::set_enabled(&dir, enabled)?;
    if enabled {
        sync_unattended(&app);
    } else {
        tokio::task::spawn_blocking(unattended::forget).await.map_err(|e| e.to_string())?;
        state.lock().map_err(|e| e.to_string())?.unattended_active = false;
        log::info!("[Unattended] Turned off — payments need the unlocked app");
    }
    Ok(())
}

fn return_wallet_session(mut mgr: RpcManager) {
    if WALLET_SESSION_INVALIDATED.swap(false, Ordering::SeqCst) {
        log::info!("[RPC] Stopping wallet RPC (session ended during the operation)");
        mgr.stop();
        return;
    }
    if let Ok(mut g) = WALLET_SESSION.lock() {
        if let Some(mut old) = g.replace(mgr) { old.stop(); }
    }
}

/// Save the wallet and stop the session wallet-rpc (full lock, node change, app exit).
/// Blocks while the wallet is saved.
fn stop_wallet_session(reason: &str) {
    let existing = WALLET_SESSION.lock().ok().and_then(|mut g| g.take());
    if let Some(mut mgr) = existing {
        log::info!("[RPC] Stopping wallet RPC ({})", reason);
        mgr.stop();
    }
}

/// End the session wallet-rpc without blocking the caller. One in use by an operation stops
/// when handed back; otherwise the flag is cleared by the next fresh start.
fn end_wallet_session(reason: &'static str) {
    WALLET_SESSION_INVALIDATED.store(true, Ordering::SeqCst);
    std::thread::spawn(move || stop_wallet_session(reason));
}

fn live_rpc_manager(app: &tauri::AppHandle) -> (RpcManager, u64) {
    let now = chrono::Utc::now().timestamp();
    let (config, last_working, avoid) = match app.try_state::<Mutex<AppState>>() {
        Some(st) => match st.lock() {
            Ok(mut s) => {
                s.slow_nodes.retain(|_, until| *until > now);
                let avoid: Vec<String> = s.slow_nodes.keys().cloned().collect();
                let last = s.active_node.clone().filter(|n| !avoid.contains(n));
                (s.rpc.config.clone(), last, avoid)
            }
            Err(_) => (RpcConfig::default(), None, Vec::new()),
        },
        None => (RpcConfig::default(), None, Vec::new()),
    };
    let (config, node, height) = rpc_manager::select_live_node(config, last_working.as_deref(), &avoid);
    if height > 0 {
        if let Some(st) = app.try_state::<Mutex<AppState>>() {
            if let Ok(mut s) = st.lock() {
                if s.active_node.as_deref() != Some(node.as_str()) || s.ready_state.is_none() {
                    log::info!("[Node] Remote node connected: {} (height {})", node, height);
                }
                s.active_node = Some(node);
            }
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

/// Windows: with the user's consent (UAC), exclude the bundled Monero engine from Defender
/// and restore it from quarantine. "restored", or "reinstall" when nothing was quarantined.
#[tauri::command]
async fn defender_allow_rpc() -> Result<String, String> {
    let result = tokio::task::spawn_blocking(defender::allow).await.map_err(|e| e.to_string())?;
    match result {
        Ok(defender::Allow::Restored) => {
            log::info!("[Defender] Monero engine excluded from scans and available");
            Ok("restored".into())
        }
        Ok(defender::Allow::ReinstallNeeded) => {
            log::warn!("[Defender] Monero engine excluded, but not found in quarantine: reinstall needed");
            Ok("reinstall".into())
        }
        Err(e) => {
            log::warn!("[Defender] Exclusion not applied: {}", e);
            Err(e)
        }
    }
}

/// The exact PowerShell run by `defender_allow_rpc`, shown to the user before running it.
#[tauri::command]
fn defender_script() -> Option<String> {
    defender::sidecar_path().map(|p| defender::allow_script(&p))
}

/// Check if a monero-wallet-rpc binary can be found.
#[tauri::command]
fn detect_rpc_binary(state: tauri::State<'_, Mutex<AppState>>) -> Option<String> {
    let s = state.lock().unwrap();
    s.rpc.detect_binary().map(|p| p.to_string_lossy().to_string())
}

/// Status of the session wallet-rpc (the one actually syncing and paying), not of `s.rpc`,
/// which is only kept for its config.
#[tauri::command]
fn get_rpc_status(state: tauri::State<'_, Mutex<AppState>>) -> RpcStatus {
    let s = state.lock().unwrap();
    // While an operation holds the session it is out of WALLET_SESSION but running.
    let in_use = s.rpc_busy.load(Ordering::SeqCst);
    let idle_alive = WALLET_SESSION.lock().ok()
        .is_some_and(|mut g| g.as_mut().is_some_and(|m| m.is_alive()));
    if in_use || idle_alive {
        RpcStatus::Running
    } else if let Some(e) = &s.rpc_last_error {
        RpcStatus::Error(e.clone())
    } else if s.unlocked {
        RpcStatus::Starting
    } else {
        RpcStatus::Stopped
    }
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
    drop(s);
    end_wallet_session("node settings changed");
    Ok(())
}

/// Stop wallet-rpc.
#[tauri::command]
fn stop_wallet_rpc(state: tauri::State<'_, Mutex<AppState>>) {
    let mut s = state.lock().unwrap();
    s.rpc.stop();
    drop(s);
    end_wallet_session("requested");
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

/// Pause, resume or archive a subscription.
#[tauri::command]
async fn set_subscription_status(
    id: String,
    paused: bool,
    archived: Option<bool>,
    db_state: tauri::State<'_, Mutex<Database>>,
) -> Result<(), String> {
    let status = if archived.unwrap_or(false) { SubscriptionStatus::Cancelled }
        else if paused { SubscriptionStatus::Paused } else { SubscriptionStatus::Active };
    let db = db_state.lock().map_err(|e| e.to_string())?;
    db.update_subscription_status(&id, &status).map_err(|e| e.to_string())?;
    // Resuming a subscription whose payments are all sent leaves it completed.
    db.complete_exhausted().map(|_| ()).map_err(|e| e.to_string())
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
        let pw = s.wallet_password.clone().ok_or("Wallet is locked.")?;
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

    // Same flow as execute_subscription_payment: wallet-dir mode + open_wallet, so the password
    // never appears on the wallet-rpc command line (visible to any local user via `ps`).
    tokio::task::spawn_blocking(move || {
        struct RpcGuard(Arc<AtomicBool>);
        impl Drop for RpcGuard { fn drop(&mut self) { self.0.store(false, Ordering::SeqCst); } }
        let _guard = RpcGuard(rpc_busy);

        let mgr = take_wallet_session(&app_rpc, &wallet_dir, &wallet_name, &password)?;

        if let Err(e) = rpc_manager::ureq_post_json_long(&mgr.rpc_url(), &serde_json::json!({
            "jsonrpc":"2.0","id":"refresh","method":"refresh","params":{}
        }), 600) {
            log::warn!("[Treasurer] refresh failed (continuing): {}", e);
        }
        if let Err(e) = check_send_guards(&app_rpc, &mgr, amount_pico, &dest, None) {
            return_wallet_session(mgr);
            log::warn!("[Treasurer] PV id={} held by a sending guard", pv_id);
            return Err(e);
        }

        let sent = transfer_with_fee_cap(&app_rpc, &mgr, amount_pico, &dest);
        return_wallet_session(mgr);
        let tx_hash = match sent {
            Ok(h) => h,
            Err(e) if e.starts_with("POLICY:") => return Err(e),
            Err(e) => {
                log::error!("[Treasurer] PV id={} — {}", pv_id, e);
                return Err(format!("Payment failed: {}", e));
            }
        };
        if tx_hash.is_empty() {
            return Err("Payment failed: transfer returned no tx_hash".to_string());
        }
        log::info!("[Treasurer] ✅ PV payment executed: id={}", pv_id);
        Ok(tx_hash)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Minimum interval between Monero sends (10 confirmations ≈ 20 min, use 25 min safely).
const MONERO_MIN_SEND_INTERVAL_SECS: u64 = 25 * 60;

/// Background scheduler: check for due subscriptions and send payments.
/// Called every 60 seconds from a tokio::spawn task.
const RPC_BUSY: &str = "RPC busy";

/// Pay a subscription right away at the user's explicit request (pending-payment popup or
/// "pay now"). Limits still apply; the click counts as approval of the current terms.
#[tauri::command]
async fn pay_subscription_now(sub_id: String, app_handle: tauri::AppHandle) -> Result<String, String> {
    let sub = app_handle.state::<Mutex<Database>>().lock().map_err(|e| e.to_string())?
        .list_subscriptions().map_err(|e| e.to_string())?
        .into_iter().find(|s| s.id == sub_id)
        .ok_or("Subscription not found")?;
    let limits = {
        let st = app_handle.state::<Mutex<AppState>>();
        let mut s = st.lock().map_err(|e| e.to_string())?;
        s.pending_approvals.remove(&sub_id);
        s.approved_payments.remove(&sub_id);
        s.payment_limits.clone()
    };
    payment_policy::validate_currency(&sub.currency)
        .and_then(|_| payment_policy::validate_address(&sub.destination_address))
        .and_then(|_| payment_policy::validate_terms(&sub.amount, &sub.schedule, &limits))
        .map_err(|e| e.to_string())?;

    {
        let mut s = app_handle.state::<Mutex<AppState>>().inner().lock().map_err(|e| e.to_string())?;
        approve_terms(&mut s, &sub);
        save_policy_vault(&s)?;
    }

    log::info!("[Payment] Manual payment requested for sub {}", &sub_id[..8.min(sub_id.len())]);
    let res = execute_subscription_payment(&app_handle, &sub).await;
    refresh_alerts_and_tray(&app_handle);
    res
}

/// Send one subscription payment and do all the bookkeeping (DB, history, alerts, events).
/// Shared by the scheduler and the manual "pay now" command so both behave the same.
async fn execute_subscription_payment(app: &tauri::AppHandle, sub: &Subscription) -> Result<String, String> {
    use chrono::Utc;
    if sub.payments_remaining() == Some(0) {
        return Err("All planned payments of this subscription have been sent.".into());
    }
    let (session_pw, wallet_dir, wallet_name) = {
        let st = app.try_state::<Mutex<AppState>>().ok_or("App state unavailable")?;
        let s = st.lock().map_err(|e| e.to_string())?;
        if !s.unlocked { return Err("Wallet is locked.".into()); }
        if s.policy_extras.paused { return Err(PAUSED.into()); }
        let pw = s.wallet_password.clone().ok_or("Wallet is locked.")?;
        let name = s.wallet_file_path.file_name()
            .and_then(|n| n.to_str()).unwrap_or("autopay_wallet").to_string();
        (pw, app.path().app_data_dir().map_err(|e| e.to_string())?, name)
    };
    let db_state = app.state::<Mutex<Database>>();
    let dest = sub.destination_address.clone();
    let amount_xmr = payment_policy::parse_amount(&sub.amount).unwrap_or(0.0);
    let amount_pico = (amount_xmr * 1e12) as u64;
    let sub_id = sub.id.clone();

    // Without the lock, start_for_dir would kill the wallet-rpc of a running refresh (or payment).
    let rpc_busy = match app.try_state::<Mutex<AppState>>()
        .and_then(|st| st.lock().ok().map(|s| Arc::clone(&s.rpc_busy)))
    {
        Some(b) => b,
        None => return Err("App state unavailable".into()),
    };
    if rpc_busy.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).is_err() {
        return Err(RPC_BUSY.into());
    }

    log::info!("[Payment] Sending payment for sub {}", &sub_id[..8.min(sub_id.len())]);

    let wallet_dir2 = wallet_dir.clone();
    let wallet_name2 = wallet_name.clone();
    let pw2 = session_pw.clone();
    let app_rpc = app.clone();
    let sub_id_guard = sub_id.clone();

    let result = tokio::task::spawn_blocking(move || {
        struct RpcGuard(Arc<AtomicBool>);
        impl Drop for RpcGuard { fn drop(&mut self) { self.0.store(false, Ordering::SeqCst); } }
        let _guard = RpcGuard(rpc_busy);

        let mgr = take_wallet_session(&app_rpc, &wallet_dir2, &wallet_name2, &pw2)?;

        // Spend from an up-to-date wallet, otherwise stale outputs make the transfer fail.
        if let Err(e) = rpc_manager::ureq_post_json_long(&mgr.rpc_url(), &serde_json::json!({
            "jsonrpc":"2.0","id":"r","method":"refresh","params":{}
        }), 600) {
            log::warn!("[Scheduler] refresh before transfer failed (continuing): {}", e);
        }
        if let Err(e) = check_send_guards(&app_rpc, &mgr, amount_pico, &dest, Some(&sub_id_guard)) {
            return_wallet_session(mgr);
            return Err(e);
        }

        let sent = transfer_with_fee_cap(&app_rpc, &mgr, amount_pico, &dest);
        return_wallet_session(mgr);
        sent
    }).await;

    match result {
        Ok(Ok(tx_hash)) => {
            let tx_hash_out = tx_hash.clone();
            log::info!("[Payment] ✅ Payment sent for sub {}", &sub_id[..8.min(sub_id.len())]);
            // Update last_payment_sent_at in AppState
            if let Some(state) = app.try_state::<Mutex<AppState>>() {
                if let Ok(mut s) = state.lock() {
                    s.last_payment_sent_at = Some(Utc::now().timestamp());
                    s.alerts.retain(|a| a.sub_id.as_deref() != Some(sub_id.as_str()));
                }
            }
            // Next occurrence after now: a long-overdue subscription must not
            // trigger a burst of catch-up payments. Slots inside the send
            // cooldown are skipped, otherwise the countdown shows a slot that
            // cannot be paid (e.g. "*/25" sent at :57 would be due at :00).
            let cooldown_end = Utc::now() + chrono::Duration::seconds(MONERO_MIN_SEND_INTERVAL_SECS as i64 - 60);
            let new_next = (sub.payments_remaining() != Some(1))
                .then(|| next_occurrence(&sub.schedule, cooldown_end));
            {
                if let Ok(db) = db_state.lock() {
                    if let Err(e) = db.update_next_payment(&sub_id, new_next, sub.payments_sent + 1) {
                        log::error!("[Scheduler] DB update failed for sub {}: {}", sub_id, e);
                    } else if let Some(next) = new_next {
                        log::info!("[Scheduler] next_payment_at advanced to {} for sub {}", next.to_rfc3339(), sub_id);
                    } else if let Err(e) = db.complete_exhausted() {
                        log::error!("[Scheduler] Could not mark sub {} as completed: {}", sub_id, e);
                    } else {
                        log::info!("[Scheduler] Last planned payment sent, sub {} completed", sub_id);
                    }
                }
            }
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
            // Emitted once the history entry exists, so the frontend reloads it right away.
            let new_next_iso = new_next.map(|t| t.to_rfc3339());
            app.emit("payment-sent", serde_json::json!({"sub_id": sub_id, "tx_hash": tx_hash, "next_payment_at": new_next_iso})).ok();
            Ok(tx_hash_out)
        }
        Ok(Err(e)) if e == PAUSED => Err(e),
        Ok(Err(e)) if e.starts_with("POLICY:") => {
            let short = &sub_id[..8.min(sub_id.len())];
            let retry_at = chrono::Utc::now().timestamp() + CAP_RETRY_SECS;
            if let Some(state_rc) = app.try_state::<Mutex<AppState>>() {
                if let Ok(mut s) = state_rc.lock() {
                    s.cap_blocked_until.insert(sub_id.clone(), retry_at);
                }
            }
            let already = app.try_state::<Mutex<AppState>>()
                .and_then(|st| st.lock().ok().map(|s| s.alerts.iter()
                    .any(|a| a.kind == "PAYMENT_BLOCKED_LIMITS" && a.sub_id.as_deref() == Some(sub_id.as_str()))))
                .unwrap_or(false);
            if !already {
                let code = e.split(':').nth(1).unwrap_or("?");
                log::warn!("[Payment] 🛑 Payment for sub {} held by a sending guard ({}) — nothing sent", short, code);
            }
            push_alert(app, AlertLevel::Orange, "PAYMENT_BLOCKED_LIMITS", &sub_id, e.clone());
            Err(e)
        }
        Ok(Err(e)) => {
            let is_locked = e.contains("not enough unlocked") || e.contains("locked") || e.contains("unlocked money");
            if is_locked {
                // Outputs locked: reschedule for 25 min later (one Monero block confirmation cycle)
                log::warn!("[Payment] ⏳ Funds still locked for sub {} — retry in 25 min", &sub_id[..8.min(sub_id.len())]);
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
                Err(format!("OUTPUTS_LOCKED: {e}"))
            } else {
                log::error!("[Payment] ❌ Payment failed for sub {}: {}", &sub_id[..8.min(sub_id.len())], e);
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
                Err(e)
            }
        }
        Err(e) => {
            log::error!("[Scheduler] Task panic for sub {}: {}", sub_id, e);
            Err(e.to_string())
        }
    }
}

async fn run_payment_scheduler(app: &tauri::AppHandle) {
    use chrono::Utc;

    let last_sent = {
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
        if s.wallet_password.is_none() { return; }
        if s.policy_extras.paused {
            log::debug!("[Scheduler] All payments paused — skipping");
            return;
        }
        if netcheck::offline(s.rpc.config.daemon_address.as_deref()) {
            log::debug!("[Scheduler] No network connection — skipping");
            return;
        }
        s.last_payment_sent_at
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
        if s.payments_remaining() == Some(0) { return false; }
        if let Some(next) = s.next_payment_at {
            next <= now
        } else {
            false
        }
    }).collect();

    if due.is_empty() { return; }
    log::debug!("[Scheduler] {} subscription(s) due", due.len());

    // Read semi-auto state once before the loop
    let (semi_auto, approved_ids, limits, balance_known, unlocked_known) = {
        if let Some(state) = app.try_state::<Mutex<AppState>>() {
            if let Ok(mut s) = state.lock() {
                let semi = s.semi_auto_mode;
                let approved: std::collections::HashSet<String> = s.approved_payments.drain().collect();
                let balance = if s.rpc_connected { Some(s.balance_atomic) } else { None };
                let unlocked = if s.rpc_connected { Some(s.unlocked_balance_atomic) } else { None };
                (semi, approved, s.payment_limits.clone(), balance, unlocked)
            } else { return; }
        } else { return; }
    };
    // Approvals not consumed by an actual send this cycle must survive to the next one.
    let restore_approvals = |ids: &std::collections::HashSet<String>| {
        if let Some(state) = app.try_state::<Mutex<AppState>>() {
            if let Ok(mut s) = state.lock() { s.approved_payments.extend(ids.iter().cloned()); }
        }
    };

    let approved_terms = match app.try_state::<Mutex<AppState>>()
        .and_then(|st| st.lock().ok().map(|s| s.approved_terms.clone()))
    {
        Some(t) => t,
        None => return,
    };

    for sub in due {
        let amount_xmr = payment_policy::parse_amount(&sub.amount).unwrap_or(0.0);
        let sub_id = sub.id.clone();
        let short_id = sub_id[..8.min(sub_id.len())].to_string();

        // Limits are re-checked at payment time: they may have been re-locked since the
        // subscription was added, or the DB may have been modified outside the app.
        if let Err(e) = payment_policy::validate_currency(&sub.currency)
            .and_then(|_| payment_policy::validate_address(&sub.destination_address))
            .and_then(|_| payment_policy::validate_terms(&sub.amount, &sub.schedule, &limits))
        {
            let already_alerted = app.try_state::<Mutex<AppState>>()
                .and_then(|st| st.lock().ok().map(|s| s.alerts.iter()
                    .any(|al| al.kind == "PAYMENT_BLOCKED_LIMITS" && al.sub_id.as_deref() == Some(sub_id.as_str()))))
                .unwrap_or(false);
            if !already_alerted {
                log::warn!("[Scheduler] Sub {} blocked by payment limits ({})", short_id, e);
            }
            push_alert(app, AlertLevel::Orange, "PAYMENT_BLOCKED_LIMITS", &sub_id, e.to_string());
            continue;
        }

        // Not enough funds: no point spawning wallet-rpc for a transfer that will fail.
        let needed = ((amount_xmr + payment_policy::FEE_MARGIN_XMR) * 1e12) as u64;
        if let Some(balance) = balance_known {
            if balance < needed {
                log::debug!("[Scheduler] Sub {} due but balance too low — waiting for funds", short_id);
                continue;
            }
        }
        let cap_wait = app.try_state::<Mutex<AppState>>()
            .and_then(|st| st.lock().ok().and_then(|s| s.cap_blocked_until.get(&sub_id).copied()))
            .is_some_and(|until| until > Utc::now().timestamp());
        if cap_wait {
            log::debug!("[Scheduler] Sub {} held by the wallet cap — next check later", short_id);
            continue;
        }

        // Enough funds but part of them still confirming (change of a recent send):
        // neither prompt nor send until they are spendable. Survives app restarts, unlike
        // the in-memory cooldown.
        if unlocked_known.is_some_and(|u| u < needed) {
            log::debug!("[Scheduler] Sub {} due but funds still confirming — waiting", short_id);
            restore_approvals(&approved_ids);
            continue;
        }

        // No recorded approval (new, or vault reset) means confirmation, never implicit trust:
        // the subscription DB is not authenticated, the vault is.
        let fingerprint = sub_fingerprint(&sub);
        let mut reasons: Vec<String> = payment_policy::confirmation_reasons(
            &sub.amount, &fingerprint, approved_terms.get(&sub_id),
        ).into_iter().map(String::from).collect();
        if semi_auto { reasons.insert(0, "semi_auto".into()); }

        // Queue for user approval unless already approved this session
        if !reasons.is_empty() && !approved_ids.contains(&sub_id) {
            if let Some(state) = app.try_state::<Mutex<AppState>>() {
                if let Ok(mut s) = state.lock() {
                    if !s.pending_approvals.contains_key(&sub_id) {
                        log::info!("[Scheduler] Sub {} waiting for user approval ({})", short_id, reasons.join(","));
                    }
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

        match execute_subscription_payment(app, &sub).await {
            Err(e) if e == RPC_BUSY => {
                log::debug!("[Scheduler] Wallet busy — payment for sub {} retried next cycle", short_id);
                restore_approvals(&approved_ids);
                return;
            }
            _ => {
                let others: std::collections::HashSet<String> =
                    approved_ids.iter().filter(|id| **id != sub_id).cloned().collect();
                restore_approvals(&others);
            }
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
/// After this long without the interface reporting in, the startup journal is shown instead.
const BOOT_UI_TIMEOUT_SECS: u64 = 30;

pub fn run() {
    // Files created from now on (vault, DB and its WAL, logs, wallet cache, webview storage)
    // are private to the current user.
    #[cfg(unix)]
    unsafe { libc::umask(0o077); }
    boot_log::init();

    tauri::Builder::default()
        .on_page_load(|_webview, payload| {
            boot_log::write("INFO", "boot", &format!("Page load {:?}: {}", payload.event(), payload.url().path()));
        })
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_autostart::init(tauri_plugin_autostart::MacosLauncher::LaunchAgent, None))
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
            boot_log::write("INFO", "boot", "Setup started");
            match tauri::webview_version() {
                Ok(v) => boot_log::write("INFO", "boot", &format!("WebView {v}")),
                Err(e) => boot_log::write("ERROR", "boot", &format!("WebView not found: {e}")),
            }
            // Resolve app data directory
            let data_dir = app.path().app_data_dir()
                .expect("Failed to resolve app data dir");
            std::fs::create_dir_all(&data_dir).ok();
            restrict_permissions(&data_dir);

            // Start with the user session by default (once, so a later opt-out sticks).
            // Skipped in debug builds to avoid registering the dev binary.
            let autostart_marker = data_dir.join("autostart_initialized");
            if !cfg!(debug_assertions) && !autostart_marker.exists() {
                use tauri_plugin_autostart::ManagerExt;
                match app.autolaunch().enable() {
                    Ok(()) => log::info!("[Autostart] Enabled on first run"),
                    Err(e) => log::warn!("[Autostart] Could not enable: {}", e),
                }
                let _ = std::fs::write(&autostart_marker, "1");
            }

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
                wallet_password: None,
                unattended_active: false,
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
                               "PAYMENT_BLOCKED_LIMITS","BALANCE_SHORT_UPCOMING","NODE_STALLED"] {
                        s.insert(k.to_string());
                    }
                    s
                },
                history_key: None,
                history_path,
                history_enabled: true,
                history_days: 365,
                semi_auto_mode: true,
                pending_approvals: std::collections::HashMap::new(),
                approved_payments: std::collections::HashSet::new(),
                last_unlock_at: 0,
                active_node: None,
                payment_limits: payment_policy::PaymentLimits::default(),
                wallet_cap: payment_policy::WalletCap::default(),
                approved_terms: std::collections::HashMap::new(),
                policy_extras: PolicyExtras::default(),
                sends_last_day: None,
                daemon_height_changed_at: 0,
                wallet_behind_since: None,
                sent_in_period_atomic: None,
                cap_blocked_until: std::collections::HashMap::new(),
                treasurer_mode: false,
                last_refresh_at: 0,
                slow_nodes: std::collections::HashMap::new(),
                unlocked_balance_atomic: 0,
                ready_state: None,
            }));

            // Logs go to stdout, to the webview (logs drawer, via @tauri-apps/plugin-log) and to a
            // rotating file in the app log dir that keeps recording while the window is closed.
            // Log lines must never contain amounts or addresses; the dir is owner-only.
            if let Ok(data_dir) = app.path().app_data_dir() {
                log_retention::load(&data_dir);
            }
            if let Ok(log_dir) = app.path().app_log_dir() {
                let _ = std::fs::create_dir_all(&log_dir);
                restrict_permissions(&log_dir);
                log_retention::prune(&log_dir);
            }
            app.handle().plugin(
                tauri_plugin_log::Builder::default()
                    .level(log::LevelFilter::Info)
                    .level_for("app_lib", log::LevelFilter::Debug)
                    .targets([
                        tauri_plugin_log::Target::new(tauri_plugin_log::TargetKind::Stdout),
                        // Nothing reaches the logs drawer before unlock.
                        tauri_plugin_log::Target::new(tauri_plugin_log::TargetKind::Webview)
                            .filter(|_| log_retention::viewable()),
                        tauri_plugin_log::Target::new(tauri_plugin_log::TargetKind::Dispatch(
                            tauri_plugin_log::fern::Dispatch::new().chain(tauri_plugin_log::fern::Output::call(
                                |record| boot_log::append(&record.args().to_string()),
                            )),
                        ))
                        .filter(|_| boot_log::active()),
                        tauri_plugin_log::Target::new(tauri_plugin_log::TargetKind::LogDir {
                            file_name: Some("autopay".into()),
                        })
                        .filter(|_| log_retention::file_enabled()),
                    ])
                    .max_file_size(2_000_000)
                    // Old files are removed by age (log_retention::prune), not by count.
                    .rotation_strategy(tauri_plugin_log::RotationStrategy::KeepAll)
                    .timezone_strategy(tauri_plugin_log::TimezoneStrategy::UseLocal)
                    .build(),
            )?;

            // Background wallet sync + payment scheduler — runs every 60s, independent of the UI
            // (window closed or UI locked).
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
                        background_refresh(&sched_handle).await;
                        run_payment_scheduler(&sched_handle).await;
                        status_heartbeat(&sched_handle);
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
                let menu_pause = MenuItemBuilder::with_id("pause", TRAY_PAUSE_LABEL).build(app)?;
                let menu_boot_log = MenuItemBuilder::with_id("boot_log", "Journal de démarrage / Startup log").build(app)?;
                let menu_quit = MenuItemBuilder::with_id("quit", "Quitter").build(app)?;
                let tray_menu = MenuBuilder::new(app)
                    .item(&menu_open)
                    .item(&menu_pause)
                    .item(&menu_boot_log)
                    .separator()
                    .item(&menu_quit)
                    .build()?;
                app.manage(TrayPauseItem(menu_pause));
                app.manage(TrayBootLogItem(menu_boot_log));

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
                            "pause" => {
                                let paused = app.try_state::<Mutex<AppState>>()
                                    .and_then(|st| st.lock().ok().map(|s| s.policy_extras.paused))
                                    .unwrap_or(false);
                                apply_pause(app, !paused);
                            }
                            "boot_log" => boot_log::open_in_viewer(),
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

            // A blank window gives the tester nothing to report: if the interface has not
            // come up after a while, show the startup journal in plain HTML instead.
            {
                let handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_secs(BOOT_UI_TIMEOUT_SECS)).await;
                    if boot_log::frontend_ready() || !boot_log::active() { return; }
                    log::error!("[Boot] Interface not displayed after {} s (blank window?)", BOOT_UI_TIMEOUT_SECS);
                    if cfg!(debug_assertions) {
                        log::error!("[Boot] Dev build: the interface comes from the Vite dev server (npm run dev), is it running?");
                    }
                    if let Some(win) = handle.get_webview_window("main") {
                        if let Err(e) = win.eval(boot_log::fallback_page_script()) {
                            log::error!("[Boot] Fallback page failed: {}", e);
                        }
                    }
                });
            }
            start_unattended(app.handle());
            boot_log::write("INFO", "boot", "Setup done");

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
            get_log_retention,
            set_log_retention,
            get_log_history,
            reset_wallet,
            lock_ui,
            get_account_statement,
            get_unattended,
            set_unattended,
            defender_allow_rpc,
            defender_script,
            boot_log_append,
            boot_frontend_ready,
            get_startup_log,
            get_pending_deep_link,
            lock_wallet,
            generate_seed,
            check_wallet_exists,
            create_wallet,
            unlock_wallet,
            unlock_lockout_remaining,
            pay_subscription_now,
            get_autostart,
            set_autostart,
            get_wallet_info,
            get_sync_info,
            get_alerts,
            set_alert_config,
            set_history_config,
            get_history_config,
            get_payment_history,
            export_payment_history,
            clear_payment_history,
            set_payment_mode,
            get_payment_mode,
            approve_payment,
            reject_payment,
            get_pending_approvals,
            get_payment_limits,
            set_payment_limits,
            get_wallet_cap,
            set_wallet_cap,
            get_sub_caps,
            set_sub_cap,
            get_paused,
            set_paused,
            get_send_feedback,
            set_send_feedback,
            get_seed_check,
            verify_seed_words,
            set_treasurer_mode,
            get_treasurer_mode,
            validate_pv,
            execute_pv_payment,
            save_subscription,
            get_subscription_debug,
            get_balance,
            refresh_balance,
            sweep_all,
            update_subscription_next_payment,
            next_occurrence_of,
            fetch_price_onion,
            update_subscription,
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
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| {
            if let tauri::RunEvent::Exit = event {
                stop_wallet_session("app exit");
                if let Ok(dir) = app.path().app_data_dir() {
                    rpc_manager::shutdown_leftover(&dir);
                }
            }
        });
}

#[cfg(test)]
mod tests {
    /// Needs a local Tor client: `cargo test -- --ignored tor_price`.
    #[tokio::test]
    #[ignore]
    async fn tor_price_route() {
        let data = super::fetch_json_via_tor("https://libremonero.com/api/v1/price").await.unwrap();
        assert!(data["xmr"]["eur"].as_f64().is_some_and(|p| p > 0.0));
    }

    #[test]
    fn restore_height_from_date() {
        use super::monero_height_at;
        // v2 fork (block 1,009,827) and 2024-01-01 (block ~3,054,000), minus the 1-month margin.
        assert!((monero_height_at(1_458_691_200) as i64 - (1_009_827 - 21_600)).abs() < 3_000);
        assert!((monero_height_at(1_704_067_200) as i64 - (3_054_000 - 21_600)).abs() < 5_000);
        assert_eq!(monero_height_at(1_400_000_000), 1);
    }

    #[test]
    fn archive_moves_only_wallet_files() {
        let dir = std::env::temp_dir().join(format!("autopay-archive-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for f in ["seed.vault", "autopay_wallet.keys", "history.vault", "subscriptions.db", "rpc_config.json"] {
            std::fs::write(dir.join(f), "x").unwrap();
        }
        assert_eq!(super::archive_wallet_files(&dir, "autopay_wallet").unwrap(), 3);
        assert!(!dir.join("seed.vault").exists() && !dir.join("autopay_wallet.keys").exists());
        assert!(dir.join("subscriptions.db").exists() && dir.join("rpc_config.json").exists());
        assert_eq!(std::fs::read_dir(dir.join("archive")).unwrap().count(), 1);
        assert_eq!(super::archive_wallet_files(&dir, "autopay_wallet").unwrap(), 0);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
