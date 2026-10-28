# TODO — Crypto AutoPay

Items ordered by priority. Check `docs/` for detailed specs on implemented features.

---

## Critical / UX (grand public)

- [ ] **PRIORITY — Vulnerability disclosure policy**: publish a `SECURITY.md` (scope, safe harbour, response delay) with a dedicated security contact email, a bug bounty (amounts in XMR per severity), and the link to the GitLab repository / confidential issues. Must be online before the alpha with real funds.
- [ ] **RPC error UX**: All 10 error cases from `docs/RPC_states.md` must have friendly, actionable messages for non-technical users. Currently some show raw error strings. See Case A–J in RPC_states.md.
- [ ] **Tray zombie on crash**: If the app is force-killed (`kill -9`), the tray icon may remain. Add a PID lockfile or use the single-instance plugin's cleanup hook to remove the tray on startup if a stale one exists.
- [ ] **Window restore**: Single-instance plugin now correctly calls `show() + unminimize() + set_focus()`. Verify on all 3 platforms (Linux, Windows, macOS) that clicking the tray icon always brings the window to front.
- [ ] **System notifications (semi-auto)**: When a payment is pending approval (semi-auto mode), show a native OS notification (Ubuntu: libnotify, Windows: WinRT Toast, macOS: NSUserNotification) using `tauri-plugin-notification`. Currently only an in-app modal is shown — useless if the window is minimized to tray.
- [ ] **Review "AutoPay account" creation (UX + security)**: the notion of "account" makes no sense here (nothing is registered anywhere, it is a local encrypted vault + a Monero wallet). Find better wording ("coffre local", "profil sur cet appareil"...) and re-review the whole flow: seed generation, seed backup/verification, restoration from 25 words, reset when the seed is lost, master password change.
- [ ] **Multi-year subscriptions**: `repeat_every > 1` is refused (`POLICY:UNSUPPORTED_REPEAT`) because cron cannot express "every N years" and the scheduler would pay every year. Needs a dedicated column + scheduler support before re-enabling the "Pluriannuel" preset.
- [ ] **Persist subscription edits**: `onSubEdit` in `Dashboard.svelte` only updates the UI state; add an `update_subscription` command (going through `payment_policy`).
- [ ] **monero-wallet-rpc bundled**: Currently users must download and configure the binary separately. Bundle it inside the installer for one-click setup. Legal check: MIT license of monero-wallet-rpc allows bundling.

---

## Security

- [ ] **No amount/address in logs**: Already implemented — verify no regression with new code paths (scheduler, sweep, send_payment).
- [ ] **History encryption key rotation**: The history key is derived from the master password with a random per-install salt (`history.salt`). If the user changes their password, old entries become unreadable. Plan: re-encrypt on password change.
- [ ] **Encrypt subscriptions.db**: addresses, amounts and schedules are stored in clear (file is 0600). Plan: SQLCipher keyed from the master password, which means opening the DB only after unlock (today `Database` is opened at startup and shared everywhere).
- [ ] **Price lookup leaks the IP**: the code editor fetches `libremonero.com/api/price` directly. Route through Tor/proxy or make it opt-in.
- [ ] **Post-quantum consideration**: Current encryption is AES-256-GCM + Argon2id. AES-256 is considered quantum-resistant. For the future: evaluate CRYSTALS-Kyber (ML-KEM) for key encapsulation if a threat model requires it.
- [ ] **Audit**: Before any public release with real funds, a security audit of `autopay-crypto` crate is strongly recommended.

---

## Features (planned)

- [ ] **Android build**: Port to Android using `monero-rs` crate (Rust-native, no subprocess). Replace `monero-wallet-rpc` with direct Monero cryptography. Estimated effort: 10–15 focused sessions. See `docs/ANDROID_BUILD.md`.
- [ ] **Canary URL support**: Merchants can publish a signed canary file. If it expires or is unreachable, the subscription is suspended with a RED alert. Spec exists, integration partially done.
- [ ] **Max payments per subscription**: Field exists in DB schema (`grace_period_hours` repurposed), proper `max_payments` field needed. UI: show "X / Y payments done" progress bar.
- [ ] **Multi-wallet**: Currently only one wallet (principal). Architecture supports multiple wallets (wallets table in DB). UI: add/remove wallets, assign subs to wallets.
- [ ] **FIAT display**: Show XMR amounts alongside approximate FIAT value (using a privacy-respecting price API with caching). Already drafted in `docs/fiat-comparison.md`.
- [ ] **Export/import subscriptions**: Export subscription list as encrypted JSON. Import from another device. Useful for migration.
- [ ] **Payment retry config**: Currently fixed at 25-min retry for locked outputs. Allow user to configure retry interval and max retry count.

---

## Developer / Infrastructure

- [ ] **monero-wallet-rpc version pinning**: ship a pinned version with its SHA-256 (verified against binaryFate's signed `hashes.txt`), check `get_version` at startup and refuse/alert on an unknown major version. CI job watching Monero releases + hard-fork dates, run the RPC integration tests against the new binary, release before the fork height.
- [ ] **App update channel**: Tauri updater with a minisign public key embedded in the binary. Signed manifest fetched from the hardcoded domain, with Tor onion and IPFS mirrors as fallbacks; the signature is mandatory whatever the source. Notification only (no silent install), link to the official repository. Aim for reproducible builds.
- [ ] **Windows build**: Use GitHub Actions release workflow (`git tag v0.x.y && git push origin v0.x.y`). See `docs/WINDOWS_BUILD.md`. Cannot cross-compile from Linux.
- [ ] **macOS code signing**: Set `APPLE_CERTIFICATE`, `APPLE_SIGNING_IDENTITY`, `APPLE_ID`, `APPLE_PASSWORD`, `APPLE_TEAM_ID` secrets in GitHub repo settings. Without signing, macOS shows Gatekeeper warning.
- [ ] **Windows code signing**: Set `TAURI_SIGNING_PRIVATE_KEY` in GitHub secrets. Without signing, Windows Defender flags the installer.
- [ ] **Automated tests**: Add integration tests for the payment scheduler (mock monero-wallet-rpc HTTP server). Add unit tests for `recompute_alerts`.
- [ ] **Clippy clean**: Run `cargo clippy -D warnings` in CI and fix any remaining lint warnings.
- [ ] **DB migration system**: Currently schema is created fresh if DB doesn't exist. Need versioned migrations for schema updates (e.g. adding `max_payments` column).

---

## Known bugs / Regressions to verify

- [ ] **DEV bypass mode**: After `Ctrl+Shift+D` (dev bypass), `refresh_balance` fails with "Locked" because `session_password` is not set in Rust. This is expected in dev mode but confusing. Consider adding a dev-only mock balance.
- [ ] **Offline banner false positive**: Fixed in `5479a26` (now requires 3 consecutive `daemon_height=0` readings). Verify no regression after scheduler kills wallet-rpc.
- [ ] **Balance 0.0000 after lock/re-unlock**: Fixed in `04e1ebf`. Verify displayed as `-.----` until first successful refresh.
- [ ] **Tray icon color**: `tray-ok.png` / `tray-warn.png` / `tray-alert.png` use the old placeholder SVG colors. Replace with logo-derived icons once final logo is confirmed.

---

## Documentation

- [x] `docs/ALERTS.md` — all alert kinds, semi-auto mode, offline logic
- [x] `docs/RPC_states.md` — full state diagram, all error cases
- [x] `docs/WINDOWS_BUILD.md` — build options for Windows
- [x] `docs/ANDROID_BUILD.md` — Android architecture options
- [ ] `docs/ONBOARDING.md` — user guide for first setup (wallet creation, monero-wallet-rpc install, first subscription)
- [ ] `docs/SECURITY_MODEL.md` — threat model, key derivation, what is encrypted, what is in plaintext
- [ ] `CHANGELOG.md` — release notes per version
