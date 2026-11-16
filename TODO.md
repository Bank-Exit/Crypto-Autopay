# TODO — Crypto AutoPay

Items ordered by priority. Check `docs/` for detailed specs on implemented features.

---

## Critical / UX (grand public)

- [ ] **Settings > About**: shows the SHA-256 of the running program; the release workflow now uploads `program-sha256-<platform>.txt` (step "Publish program fingerprint", untested until the next release: check that `gh release upload` finds the draft and that the macOS hash matches the installed .app). The About text announces a bug bounty: `SECURITY.md` below must be online first.
- [ ] **PRIORITY — Vulnerability disclosure policy**: publish a `SECURITY.md` (scope, safe harbour, response delay) with a dedicated security contact email, a bug bounty (amounts in XMR per severity), and the link to the GitLab repository / confidential issues. Must be online before the alpha with real funds.
- [ ] **RPC error UX**: All 10 error cases from `docs/RPC_states.md` must have friendly, actionable messages for non-technical users. Currently some show raw error strings. See Case A–J in RPC_states.md.
- [ ] **Tray zombie on crash**: If the app is force-killed (`kill -9`), the tray icon may remain. Add a PID lockfile or use the single-instance plugin's cleanup hook to remove the tray on startup if a stale one exists.
- [ ] **Window restore**: Single-instance plugin now correctly calls `show() + unminimize() + set_focus()`. Verify on all 3 platforms (Linux, Windows, macOS) that clicking the tray icon always brings the window to front.
- [ ] **System notifications (semi-auto)**: `tauri-plugin-notification` is in place for sent payments (optional chime + notification, no amount). Remaining: notify when a payment is pending approval, useless otherwise if the window is minimized to tray.
- [ ] **Review "AutoPay account" creation (UX + security)**: the notion of "account" makes no sense here (nothing is registered anywhere, it is a local encrypted vault + a Monero wallet). Find better wording ("coffre local", "profil sur cet appareil"...). Done: restore flow (password first, 25 words + creation month/year, scan from that height), "Forgot password" on Unlock (archives the old files, nothing deleted), seed diagnosis button wired to it. Remaining: master password change (see Security).
- [ ] **End-to-end test in the real window** (not only the browser mock): create, restore from a real seed with a date, forgot password, legacy wallet migration to the random password, unattended payments after a reboot, Account statement with incoming/outgoing/pending transfers. Linux first, then Windows/macOS.
- [x] **Account page**: first menu entry, wallets + bank-style statement (in/out/pending/failed from `get_transfers`, labelled by subscription). The History page is reached from "Reports and export".
- [x] **French seeds**: restore accepts English and French 25-word seeds (French list, 4-letter prefixes), checked against monero-wallet-rpc.
- [ ] **Other seed languages** for restore: Spanish, German, Italian, Portuguese, Dutch, Russian, Japanese, Chinese, Esperanto, Lojban (add the official lists to `autopay-crypto`; non-Latin lists need Unicode prefix handling and NFKC normalisation).
- [ ] **"PayPal 3.0 souverain" strategy**: reorientation points pending (author's notes not transcribed yet).
- [x] Persist subscription edits (`update_subscription`, same checks as a new subscription; changed terms ask for a new confirmation).
- [x] **Duplicated log lines**: several Rust log listeners attached in the same page (async `onMount` race). One listener per page now, forwarding to the current sink.
- [ ] **Replace wallet-rpc with an in-process Rust wallet**: removes the sidecar process, its port, its RPC credentials and the cold-start mempool download. Evaluate maturity (monero-rs / monero-serai / monero-wallet) for scanning, transfer and fee estimation on mainnet before switching.
- [ ] **Node rotation with a pinned node**: a node pinned in Settings is never rotated, even when found slow; show a hint in Settings when the pinned node is slow.
- [x] **Sweep (empty the wallet)**: tested OK on mainnet (0.2.1, Windows). Balance and the Wallets page now share the same live value.
- [x] **Start with the session**: `tauri-plugin-autostart`, enabled once on first run (release builds only; debug builds refuse to register it), toggle in Settings.
- [x] **Unattended payments** (on by default, Settings > Startup): the Monero wallet has its own random password (`wallet_pw.vault`), never the master password; it is kept with the history key in the OS keyring so payments run after login and while the screen is locked (`lock_ui`). Keyring verified on Ubuntu (Secret Service). Remaining: verify on Windows (Credential Manager) and macOS (Keychain, prompt on unsigned builds?), and Linux sessions without Secret Service (KDE without kwallet bridge, minimal WMs): the app must fall back to "unlock at login" with a clear hint.
- [x] **monero-wallet-rpc bundled**: shipped as the `autopay-wallet-rpc` sidecar (renamed to avoid clashing with a distro `monero` package in /usr/bin). Pinned in `scripts/monero-cli.lock`, fetched and SHA-256 checked by `scripts/fetch-wallet-rpc.sh` (CI and local dev). Linux verified locally (.deb); Windows/macOS to verify on the next release run.

---

## Security

- [ ] **No amount/address in logs**: Already implemented — verify no regression with new code paths (scheduler, sweep, send_payment).
- [ ] **Master password change**: the history key is derived from the master password with a random per-install salt (`history.salt`) and encrypts `history.vault`, `policy.vault` and `wallet_pw.vault`. Changing the password must re-encrypt the seed vault and these three files atomically, then update the keyring entry. The Monero wallet password itself does not change.
- [ ] **Unattended payments threat model**: with the option on, the OS keyring holds the wallet password and the history key. Malware running in the user session can read it and, with the wallet file, spend the hot wallet (capped by the payment limits and the 2 XMR ceiling). Document it in `SECURITY_MODEL.md` and the Settings hint; consider a cap on what unattended payments may send per day.
- [ ] **Encrypt subscriptions.db**: addresses, amounts and schedules are stored in clear (file is 0600). Plan: SQLCipher keyed from the master password, which means opening the DB only after unlock (today `Database` is opened at startup and shared everywhere).
- [x] **Fiat price (LibreMonero only)**: `GET https://libremonero.com/api/v1/price`, opt-in, quotes older than 1 h rejected, 5 min cache. Display only, never used for payments.
- [ ] **Price over Tor**: set `LIBREMONERO_ONION_PRICE_URL` in `src-tauri/src/lib.rs` once LibreMonero publishes its onion service; the app then tries it first through a local Tor client (SOCKS 9050 / 9150) and falls back to clearnet. Test: `cargo test -p monero-autopay -- --ignored tor_price`.
- [ ] **Post-quantum consideration**: Current encryption is AES-256-GCM + Argon2id. AES-256 is considered quantum-resistant. For the future: evaluate CRYSTALS-Kyber (ML-KEM) for key encapsulation if a threat model requires it.
- [ ] **Audit**: Before any public release with real funds, a security audit of `autopay-crypto` crate is strongly recommended.

---

## Features (planned)

- [ ] **Android build**: Port to Android using `monero-rs` crate (Rust-native, no subprocess). Replace `monero-wallet-rpc` with direct Monero cryptography. Estimated effort: 10–15 focused sessions. See `docs/ANDROID_BUILD.md`.
- [ ] **Canary URL support**: Merchants can publish a signed canary file. If it expires or is unreachable, the subscription is suspended with a RED alert. The checker exists (`crates/autopay-rpc/src/canary.rs`) but is not wired into the Tauri scheduler, and `recompute_alerts` never emits `CANARY_EXPIRED`: the Settings toggle currently has no effect.
- [ ] **Max payments per subscription**: Field exists in DB schema (`grace_period_hours` repurposed), proper `max_payments` field needed. UI: show "X / Y payments done" progress bar.
- [ ] **Multi-wallet**: Currently only one wallet (principal). Architecture supports multiple wallets (wallets table in DB). UI: add/remove wallets, assign subs to wallets.
- [ ] **FIAT display**: Show XMR amounts alongside approximate FIAT value (using a privacy-respecting price API with caching). Already drafted in `docs/fiat-comparison.md`.
- [ ] **Export/import subscriptions**: Export subscription list as encrypted JSON. Import from another device. Useful for migration.
- [ ] **Payment retry config**: Currently fixed at 25-min retry for locked outputs. Allow user to configure retry interval and max retry count.

---

## V2 — Merchant side

- [ ] **Merchant tooling** (the adoption bottleneck of a two-sided market): one subaddress per subscriber, payment notifications (webhooks), plugins (BTCPay Server, WooCommerce, Ghost).
- [ ] **LibreMonero integration**: non-custodial subaddress creation and payment detection in under 5 seconds, embeddable with a single tag. Natural partner for the merchant side.
- [ ] **Mobile**: Android, then iOS.

## Reputation / Operations

- [ ] **Respond to security reports and alerts quickly**: one bug that sends funds wrongly is enough to ruin trust. Define a response time in `SECURITY.md` and stick to it.
- [ ] **Regular audit**: one external audit before the first release with real funds, then yearly (and after any change to signing, key handling or the scheduler).

## Developer / Infrastructure

- [ ] **monero-wallet-rpc version pinning**: pinning done (`scripts/update-monero-lock.sh <version>` verifies the GPG-signed hashes.txt against binaryFate's fingerprint). Remaining: check `get_version` at startup and alert on an unknown major version; CI job watching Monero releases and hard-fork dates; RPC integration tests against the new binary before each bump.
- [ ] **App update channel**: Tauri updater with a minisign public key embedded in the binary. Signed manifest fetched from the hardcoded domain, with Tor onion and IPFS mirrors as fallbacks; the signature is mandatory whatever the source. Notification only (no silent install), link to the official repository. Aim for reproducible builds.
- [ ] **Windows build**: Use GitHub Actions release workflow (`git tag v0.x.y && git push origin v0.x.y`). See `docs/WINDOWS_BUILD.md`. Cannot cross-compile from Linux.
- [ ] **macOS code signing**: set `APPLE_CERTIFICATE` (base64 .p12), `APPLE_CERTIFICATE_PASSWORD`, `APPLE_SIGNING_IDENTITY`, `APPLE_ID`, `APPLE_PASSWORD`, `APPLE_TEAM_ID` in the repo secrets (Apple Developer account). Until then builds are ad-hoc signed: users must right-click > Open, or run `xattr -cr`.
- [ ] **Release signature**: create the release OpenPGP key, publish `keys/releases.asc` and sign `SHA256SUMS` for each release (`docs/SIGNING_RELEASES.md`, user side `docs/VERIFY_DOWNLOAD.md`).
- [ ] **Windows code signing**: needs an Authenticode certificate (OV/EV) and a signing step; without it SmartScreen warns on first launch. Note: `TAURI_SIGNING_PRIVATE_KEY` is the updater key, unrelated.
- [ ] **Automated tests**: Add integration tests for the payment scheduler (mock monero-wallet-rpc HTTP server). Add unit tests for `recompute_alerts`.
- [ ] **Clippy clean**: Run `cargo clippy -D warnings` in CI and fix any remaining lint warnings.
- [ ] **DB migration system**: Currently schema is created fresh if DB doesn't exist. Need versioned migrations for schema updates (e.g. adding `max_payments` column).

---

## Known bugs / Regressions to verify

- [ ] **DEV bypass mode**: After `Ctrl+Shift+D` (dev bypass), `refresh_balance` fails with "Locked" because the wallet is not unlocked in Rust. This is expected in dev mode but confusing. Consider adding a dev-only mock balance.
- [ ] **Offline banner false positive**: Fixed in `5479a26` (now requires 3 consecutive `daemon_height=0` readings). Verify no regression after scheduler kills wallet-rpc.
- [ ] **Balance 0.0000 after lock/re-unlock**: Fixed in `04e1ebf`. Verify displayed as `-.----` until first successful refresh.
- [ ] **New logo**: `public/logo.png` and `icons/icon.png` have an opaque white background; make a transparent logo (SVG or 1024 px PNG), regenerate the icons (`cargo tauri icon`) and the tray icons (`tray-ok/warn/alert.png`).

---

## Documentation

- [x] `docs/ALERTS.md` — all alert kinds, semi-auto mode, offline logic
- [x] `docs/RPC_states.md` — full state diagram, all error cases
- [x] `docs/WINDOWS_BUILD.md` — build options for Windows
- [x] `docs/ANDROID_BUILD.md` — Android architecture options
- [ ] `docs/ONBOARDING.md` — user guide for first setup (wallet creation, monero-wallet-rpc install, first subscription)
- [ ] `docs/SECURITY_MODEL.md` — threat model, key derivation (master password → seed vault + history key → wallet password), what is encrypted, what is in plaintext, what the OS keyring holds
- [x] `docs/RPC_states.md` — updated for the two passwords, `lock_ui` / `lock_wallet` and unattended start
- [ ] `CHANGELOG.md` — release notes per version
