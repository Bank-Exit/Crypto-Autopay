//! Progressive lockout against password brute force on the unlock screen.
//!
//! State is persisted next to the vault so restarting the app does not reset it.
//! This only slows down attempts through the UI: the vault itself relies on Argon2.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const FREE_ATTEMPTS: u32 = 4;
/// Lockout duration applied after each failure beyond the free attempts.
const LOCKOUT_STEPS_SECS: [i64; 5] = [60, 10 * 60, 60 * 60, 6 * 60 * 60, 24 * 60 * 60];

#[derive(Debug, Default, Serialize, Deserialize)]
struct GuardState {
    failures: u32,
    locked_until: i64,
}

pub struct UnlockGuard {
    path: PathBuf,
    state: GuardState,
}

impl UnlockGuard {
    pub fn load(vault_path: &Path) -> Self {
        let path = vault_path.with_file_name("unlock_guard.json");
        let state = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        Self { path, state }
    }

    /// Seconds left before a new attempt is allowed (0 = allowed now).
    pub fn remaining_secs(&self, now: i64) -> i64 {
        (self.state.locked_until - now).max(0)
    }

    /// Attempts left before the next lockout kicks in.
    pub fn attempts_before_lockout(&self) -> u32 {
        FREE_ATTEMPTS.saturating_sub(self.state.failures)
    }

    /// Record a failed attempt; returns the lockout now in force (seconds).
    pub fn record_failure(&mut self, now: i64) -> i64 {
        self.state.failures = self.state.failures.saturating_add(1);
        if self.state.failures > FREE_ATTEMPTS {
            let step = (self.state.failures - FREE_ATTEMPTS - 1) as usize;
            let secs = LOCKOUT_STEPS_SECS[step.min(LOCKOUT_STEPS_SECS.len() - 1)];
            self.state.locked_until = now + secs;
        }
        self.save();
        self.remaining_secs(now)
    }

    pub fn record_success(&mut self) {
        if self.state.failures > 0 || self.state.locked_until > 0 {
            self.state = GuardState::default();
            self.save();
        }
    }

    fn save(&self) {
        if let Ok(json) = serde_json::to_string(&self.state) {
            if let Err(e) = std::fs::write(&self.path, json) {
                log::error!("[UnlockGuard] Could not persist lockout state: {}", e);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escalates_after_free_attempts() {
        let dir = std::env::temp_dir().join(format!("autopay-guard-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut g = UnlockGuard::load(&dir.join("vault.json"));
        g.record_success();
        for _ in 0..FREE_ATTEMPTS {
            assert_eq!(g.record_failure(1000), 0);
        }
        assert_eq!(g.record_failure(1000), 60);
        assert_eq!(g.record_failure(2000), 600);
        assert_eq!(UnlockGuard::load(&dir.join("vault.json")).remaining_secs(2000), 600);
        g.record_success();
        assert_eq!(g.remaining_secs(2000), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
