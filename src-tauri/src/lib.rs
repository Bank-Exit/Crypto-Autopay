use std::sync::Mutex;
use std::path::PathBuf;
use serde::{Deserialize, Serialize};
use tauri::Manager;

use autopay_crypto::{
    SeedVault, EncryptedSeed,
    generate_seed as crypto_generate_seed,
    mnemonic_to_bytes,
};

// ============================================================
//  Application state
// ============================================================

/// Shared app state, protected by a Mutex.
pub struct AppState {
    /// Path to the encrypted seed vault file.
    pub vault_path: PathBuf,
    /// Whether the wallet is currently unlocked (seed in memory).
    pub unlocked: bool,
}

// ============================================================
//  Tauri commands
// ============================================================

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
/// Returns Ok(()) if password is correct.
#[tauri::command]
async fn unlock_wallet(
    password: String,
    state: tauri::State<'_, Mutex<AppState>>,
) -> Result<(), String> {
    let vault_path = {
        let state = state.lock().map_err(|e| e.to_string())?;
        state.vault_path.clone()
    };

    let vault_json = std::fs::read_to_string(&vault_path)
        .map_err(|_| "Vault file not found — please create a wallet first.".to_string())?;

    let encrypted: EncryptedSeed = serde_json::from_str(&vault_json)
        .map_err(|e| format!("Invalid vault file: {e}"))?;

    // Try to decrypt (this validates the password)
    let _seed = SeedVault::decrypt(&encrypted, &password).map_err(|_| {
        "Incorrect password.".to_string()
    })?;

    // TODO: spawn monero-wallet-rpc with the decrypted seed

    let mut state = state.lock().map_err(|e| e.to_string())?;
    state.unlocked = true;
    Ok(())
}

/// Get the current wallet balance (mock until wallet-rpc is connected).
#[tauri::command]
async fn get_balance(state: tauri::State<'_, Mutex<AppState>>) -> Result<BalanceResponse, String> {
    let state = state.lock().map_err(|e| e.to_string())?;
    if !state.unlocked {
        return Err("Wallet is locked.".to_string());
    }
    // TODO: call MoneroAdapter::balance() once wallet-rpc is spawned
    Ok(BalanceResponse { xmr: 0.0, atomic: 0 })
}

/// Get all subscriptions from the local SQLite database.
#[tauri::command]
async fn get_subscriptions() -> Result<Vec<serde_json::Value>, String> {
    // TODO: connect to DB and return real subscriptions
    Ok(vec![])
}

/// Parse and add a subscription from a monero-autopay code.
#[tauri::command]
async fn add_subscription(code: String) -> Result<String, String> {
    use autopay_crypto::parse_code;
    let parsed = parse_code(&code).map_err(|e| format!("Invalid code: {e:?}"))?;
    // TODO: insert into SQLite DB
    // Return parsed code as JSON for the frontend
    match parsed {
        autopay_crypto::AutoPayCode::V1(v1) => {
            Ok(format!("Subscription '{}' added", v1.custom_label))
        }
    }
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
        .setup(|app| {
            // Resolve vault path in app data directory
            let data_dir = app.path().app_data_dir()
                .expect("Failed to resolve app data dir");
            let vault_path = data_dir.join("seed.vault");

            app.manage(Mutex::new(AppState {
                vault_path,
                unlocked: false,
            }));

            if cfg!(debug_assertions) {
                app.handle().plugin(
                    tauri_plugin_log::Builder::default()
                        .level(log::LevelFilter::Info)
                        .build(),
                )?;
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            generate_seed,
            check_wallet_exists,
            create_wallet,
            unlock_wallet,
            get_balance,
            get_subscriptions,
            add_subscription,
        ])
        .run(tauri::generate_context!())
        .expect("error while building tauri application");
}
