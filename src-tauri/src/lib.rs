use std::sync::Mutex;
use std::path::PathBuf;
use serde::{Deserialize, Serialize};
use tauri::{Manager, Emitter, Listener};

use autopay_crypto::{
    SeedVault, EncryptedSeed,
    generate_seed as crypto_generate_seed,
    mnemonic_to_bytes,
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
    /// A deep-link URL waiting to be consumed by the frontend.
    /// Set when the app is launched/focused via a crypto-autopay: URI.
    pub pending_deep_link: Option<String>,
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
/// If the wallet file doesn't exist yet, restore it from the seed via monero-wallet-rpc.
/// Returns Ok(address) on success.
#[tauri::command]
async fn unlock_wallet(
    password: String,
    state: tauri::State<'_, Mutex<AppState>>,
    app_handle: tauri::AppHandle,
) -> Result<String, String> {
    let (vault_path, wallet_file_path) = {
        let s = state.lock().map_err(|e| e.to_string())?;
        (s.vault_path.clone(), s.wallet_file_path.clone())
    };

    let vault_json = std::fs::read_to_string(&vault_path)
        .map_err(|_| "Vault file not found — please create a wallet first.".to_string())?;

    let encrypted: EncryptedSeed = serde_json::from_str(&vault_json)
        .map_err(|e| format!("Invalid vault file: {e}"))?;

    // Decrypt (validates the password)
    let seed = SeedVault::decrypt(&encrypted, &password)
        .map_err(|_| "Incorrect password.".to_string())?;

    log::info!("Vault decrypted successfully.");

    // Check if the wallet file exists (monero-wallet-rpc creates a .keys file)
    let keys_file = wallet_file_path.with_extension("keys");
    let wallet_exists = keys_file.exists();
    let wallet_name = wallet_file_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("autopay_wallet")
        .to_string();
    let wallet_dir = app_handle.path().app_data_dir().map_err(|e| e.to_string())?;

    let address = if !wallet_exists {
        // First unlock: restore wallet from seed via monero-wallet-rpc
        log::info!("Wallet file not found — restoring from seed…");

        // Start wallet-rpc in wallet-dir mode (blocking — runs in tokio task)
        let seed_str = seed.to_string();
        let password_clone = password.clone();
        let wallet_name_clone = wallet_name.clone();
        let wallet_dir_clone = wallet_dir.clone();

        // We need to run blocking code; tauri commands are async (tokio)
        let result = tokio::task::spawn_blocking(move || {
            let config = rpc_manager::RpcConfig::default();
            let mut mgr = rpc_manager::RpcManager::new(config);

            mgr.start_for_dir(&wallet_dir_clone)?;
            mgr.wait_for_ready(std::time::Duration::from_secs(10))?;

            let address = mgr.rpc_restore_wallet(
                &wallet_name_clone,
                &password_clone,
                &seed_str,
                0, // restore from genesis — safer for a new app
            )?;

            mgr.stop();
            Ok::<String, String>(address)
        })
        .await
        .map_err(|e| format!("Task error: {e}"))??;

        log::info!("Wallet restored. Address: {}", result);
        result
    } else {
        // Wallet exists — just open it via RPC to get the address
        log::info!("Wallet file found at {:?}", keys_file);
        let password_clone = password.clone();
        let wallet_name_clone = wallet_name.clone();
        let wallet_dir_clone = wallet_dir.clone();

        let result = tokio::task::spawn_blocking(move || {
            let config = rpc_manager::RpcConfig::default();
            let mut mgr = rpc_manager::RpcManager::new(config);

            // Start in wallet-dir mode then open the existing wallet
            mgr.start_for_dir(&wallet_dir_clone)?;
            mgr.wait_for_ready(std::time::Duration::from_secs(10))?;

            // Open the existing wallet
            let body = serde_json::json!({
                "jsonrpc": "2.0",
                "id": "open",
                "method": "open_wallet",
                "params": {
                    "filename": wallet_name_clone,
                    "password": password_clone
                }
            });
            let _ = rpc_manager::ureq_post_json_pub(&mgr.rpc_url(), &body)?;

            // Get address
            let addr_body = serde_json::json!({
                "jsonrpc": "2.0",
                "id": "addr",
                "method": "get_address",
                "params": { "account_index": 0 }
            });
            let resp = rpc_manager::ureq_post_json_pub(&mgr.rpc_url(), &addr_body)?;
            let address = resp["result"]["address"]
                .as_str()
                .unwrap_or("unknown")
                .to_string();

            mgr.stop();
            Ok::<String, String>(address)
        })
        .await
        .map_err(|e| format!("Task error: {e}"))??;

        result
    };

    let mut s = state.lock().map_err(|e| e.to_string())?;
    s.unlocked = true;
    log::info!("Wallet unlocked. Address: {}", address);
    Ok(address)
}

/// Get the current wallet balance.
/// NOTE: balance requires an open RPC session (password needed) — returns 0 for now,
/// will be extended when persistent RPC session is implemented.
#[tauri::command]
async fn get_balance(state: tauri::State<'_, Mutex<AppState>>) -> Result<BalanceResponse, String> {
    let unlocked = state.lock().map_err(|e| e.to_string())?.unlocked;
    if !unlocked {
        return Err("Wallet is locked.".to_string());
    }
    Ok(BalanceResponse { xmr: 0.0, atomic: 0 })
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

// ============================================================
//  App entry point
// ============================================================

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
                pending_deep_link: None,
                rpc: RpcManager::new(rpc_config),
            }));

            if cfg!(debug_assertions) {
                app.handle().plugin(
                    tauri_plugin_log::Builder::default()
                        .level(log::LevelFilter::Info)
                        .build(),
                )?;
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
            get_balance,
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
