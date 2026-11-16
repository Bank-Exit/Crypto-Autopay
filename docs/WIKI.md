# Crypto AutoPay — Feature wiki

Compact reference of everything the app does (v0.2.x). Details: `docs/ALERTS.md` (alerts, limits,
modes), `docs/RPC_states.md` (wallet-rpc lifecycle), `SPEC.md` (code format).

**Contents:** [Wallet](#wallet) · [Subscriptions](#subscriptions) · [Payment engine](#payment-engine) ·
[Sending guards](#sending-guards) · [Modes & pause](#modes--pause) · [Alerts](#alerts) ·
[Node & sync](#node--sync) · [History & reports](#history--reports) · [Treasurer (PV)](#treasurer-pv) ·
[Merchant side](#merchant-side) · [Settings](#settings) · [Privacy & security](#privacy--security) ·
[Files](#files) · [Known limits](#known-limits)

---

## Wallet

| Feature | Notes |
|---|---|
| Create / restore | Welcome: "Create a wallet" or "Restore with my 25 words". Both ask the master password first. Restore accepts English or French seeds (any case; the first 3 letters, 4 in French, are enough) and asks the approximate creation month (or "I don't know" = full scan); the chain is scanned from one month before it |
| Master password | ≥ 12 chars; Argon2id → AES-256-GCM (`seed.vault`); unlock lockout after repeated failures |
| Seed backup check | Reminder every 90 days; asks 3 random words, compared with the vault |
| Lock | Sidebar padlock or 3 min idle: the master password leaves memory and technical logs are hidden; scheduled payments keep running |
| Pay even when locked | On by default. The wallet's own random password (never the master password) and the history key go to the OS keyring (Keychain / Credential Manager / Secret Service); at launch the payment session starts from it with the interface locked. Older wallets switch from the master password to a random one at their next opening. Off = keyring entry deleted. Trade-off: malware in the user session can read the keyring and spend up to the caps |
| Balance | Live balance, sync progress, "top up in ~N days" from the monthly spend |
| Sweep | Send all funds to an address (Wallets page), irreversible, confirmed |
| Forgot password | Unlock screen → restore from the 25 words with a new password. The previous wallet files move to `archive/<date>/` (never deleted); subscriptions and settings stay |

## Subscriptions

| Feature | Notes |
|---|---|
| Add | Manual form, import of a `crypto-autopay:1:` / `monero-request:1:` code, QR, or `crypto-autopay://` deep link |
| Schedules | Every 25 min (needs the "very short schedules" unlock) … hourly, daily, weekly, monthly, quarterly, yearly, every N years; cron calendar |
| Options | Start date, number of payments, description, website URL (shown and clickable next to the description), canary URL |
| Duplicate check | Exact (same address + schedule) or soft (same name, amount, currency) before import |
| Actions | Pause / resume, pay now, edit, details, archive, delete, per-subscription cap (⋯ menu) |
| Completion | Once all planned payments are sent the subscription becomes "Completed" (greyed, no next date). "Restart" (⋯ menu) reopens it for N more payments (or no end), same terms, same limit checks as a new subscription; the next payment follows the schedule, nothing is sent at once. In Edit, the number of payments is a total including those already sent (the form says how many) |
| Timestamps | Creation and last modification (edit, pause / resume, archive), shown in Edit and Details; payments do not count as modifications |
| Card | Status, payments sent, next date, pie countdown (`+N min` = overdue), cap chip. While the global pause is on (or the subscription is paused) the countdown is replaced by "Paused: nothing is sent"; at resume an overdue payment leaves once, then the schedule continues from that send |
| Refill banner | When the planned payments empty the wallet, from the real schedule and the remaining number of payments (a subscription with 2 payments left counts 2), fees included; amount planned over the next 30 days |
| Cash-flow forecast | Bottom of "My subscriptions": projected balance over 30/90/180/365 days, date the wallet runs out, spending per subscription. Cumulative spending only while the balance is not synced |

## Payment engine

- Scheduler every ~60 s while unlocked; one send per cycle, **25 min** minimum between two sends (Monero outputs lock ~10 blocks).
- Before each send: wallet refresh → sending guards → transfer **prepared without relay** → fee check → relay.
- Next due date: next cron slot after the cooldown (no catch-up burst after downtime).
- Locked funds → retry in 25 min (`OUTPUTS_LOCKED`); failure → `PAYMENT_FAILED`; guard hit → held 1 h, nothing sent.
- After a real send: discreet chime + system notification (subscription name only), both optional.

## Sending guards

All sealed in `policy.vault` (encrypted with the master password). **Tightening is free, loosening asks the password.** Altered/deleted vault → strict defaults.

| Guard | Default | Where |
|---|---|---|
| Wallet cap | 33 % of balance / 30 days (or fixed XMR, 7/30/90 days) | Wallets > click the wallet card |
| Balance floor | off | Wallets |
| Max sends / 24 h | off | Wallets |
| Max network fee | 0.001 XMR | Wallets |
| Delay before 1st payment to a new address | off (24 h / 48 h) | Wallets |
| Per-subscription cap | none (XMR / rolling 30 days) | Subscription ⋯ > Cap… |
| Amount per payment | 0.00021 … 5 XMR ("high amounts" unlock) | Settings > Security limits |
| Cadence | ≥ 7 days ("very short schedules" unlock, floor 25 min) | Settings > Security limits |
| Monthly projection | confirmation above 1 XMR / 30 days when adding | add form |
| Confirmation | first payment, changed terms (address/amount/currency/schedule fingerprint), ≥ 1 XMR | always, even in Auto |

## Modes & pause

- **Semi-auto** (default): every due payment waits for Confirm / Postpone 1 h / Skip.
- **Auto**: due payments leave without asking (except the confirmations above).
- **Global pause**: "AutoPay active / paused" switch at the top of the Account page, or the tray menu; banner with Resume; nothing leaves while paused.
- Mode and pause persist across restarts (vault).

## Alerts

Sidebar panel + overlay, tray icon colour (red > orange > ok). Full table: `docs/ALERTS.md`.

- **Red:** `BALANCE_EMPTY`, `PAYMENT_OVERDUE_HARD`, `OFFLINE_WITH_PENDING`, `RPC_BINARY_MISSING`.
- **Orange:** `BALANCE_LOW`, `BALANCE_SHORT_UPCOMING`, `PAYMENT_OVERDUE_SOFT`, `PAYMENT_FAILED`, `OUTPUTS_LOCKED`, `PAYMENT_BLOCKED_LIMITS`, `WALLET_NOT_SYNCED`, `NODE_STALLED`.

## Node & sync

- Bundled `autopay-wallet-rpc` (official Monero binary, pinned + SHA-256 checked), bound to `127.0.0.1`, random digest credentials per session.
- Public node list with automatic failover (slow / unreachable / stalled > 30 min); a pinned custom node is only alerted, never rotated.
- RPC status dot: connected / checking / offline / error.
- No network at all (cable out, Wi-Fi off) is detected locally — the OS routing table is asked, nothing is sent — so no node search or wallet opening is attempted; "no node reachable" is reported separately. Skipped when the pinned node is local / LAN.
- Windows: if Defender quarantined the engine, the "engine not found" screen offers "Allow in Windows Defender" (UAC prompt). It excludes only `autopay-wallet-rpc.exe` (not the folder) and restores that exact file from quarantine; otherwise it asks to rerun the installer. The PowerShell it runs can be shown first.

## Account & history

- "Account" is the first menu entry: balance, top-up, limits, and a bank-style statement read from the wallet itself (money in, sends, pending, failed, fees), labelled with the subscription when the history knows the transaction. Needs the unlocked interface and a connected engine.
- "Clear" (discreet, above the statement) hides every current line for good after a confirmation. The transactions stay on the chain: a restore from the 25 words shows them again.
- "Reports & export" (from the Account page) opens the encrypted subscription-payment history below.
- Encrypted payment history (`history.vault`), retention in days, can be turned off (privacy mode). Like the statement, it needs the unlocked interface.
- Grouped by period with relative dates, CSV export, clear.
- Monthly report card (this month, last month, balance) + a one-time summary at each new month.

## Treasurer (PV)

Settings toggle. Import a meeting resolution JSON (Procès-Verbal), validate, execute each payment
(immediate or deferred). Same sending guards as subscriptions.

## Merchant side

- **Create a code** page: amount, currency, schedule, canary → `crypto-autopay:1:` code, QR, HTML button snippet.
- **Find subscriptions**: open directory (Tor / IPFS mirrors), demo entries.

## Settings

Language (FR/EN/DE/ES), theme, timezone, network (Monero; Bitcoin UI only), payment mode,
alerts on/off + low-balance threshold, send chime / notification, security limits, history,
technical logs retention, RPC node / binary path / payment tolerance, start with the session (on by default; refused by dev builds), pay even when locked (on by default). About at the bottom: disclaimer, open source, bug bounty, version, year and SHA-256 of the running program (compare with the `program-sha256-<platform>.txt` of the release).

## Privacy & security

- No telemetry, no account, nothing sent anywhere except to the Monero node (and the opt-in fiat rate).
- Technical logs: **no amount, no address** (internal id + time only); file `0600`, sliding retention; drawer with copy / clear, **only in an unlocked session**.
- Startup log (`logs/startup.log`, previous launch in `startup.previous.log`): from process start to the first unlock, then deleted. Home folder shown as `~`. Readable from the locked screen's Logs button, from the tray menu ("Journal de démarrage", opens the text viewer), and shown in the window if the interface is not up after 30 s.
- Secrets zeroized on lock; constant-time password comparison.
- Threat model: RPC not reachable from the network; main risk is local malware (see `TODO.md` > Security).
- Releases: `SHA256SUMS` signed with the project key — `docs/SIGNING_RELEASES.md`, `docs/VERIFY_DOWNLOAD.md`.

## Files

`~/.local/share/com.libre-market.autopay/` (Linux; OS equivalent elsewhere), all `0600`:

| File | Content | Encrypted |
|---|---|---|
| `seed.vault` | wallet seed | yes (master password) |
| `policy.vault` | guards, approvals, mode, pause, caps | yes |
| `history.vault` | payment history | yes |
| `subscriptions.db` | subscriptions (SQLite) | **no** — edits trigger a confirmation, not a payment |
| `autopay_wallet*` | Monero wallet files | yes (wallet password) |
| `wallet_pw.vault` | the wallet's random password | yes (master password) |
| `unattended.json` | "pay even when locked" setting | no |
| `archive/<date>/` | wallet set aside by "forgot password" / a restore | as above |
| `logs/autopay.log` | technical logs | no (nothing sensitive by design) |
| `logs/startup.log` | startup log, deleted at unlock (macOS: `~/Library/Logs/com.libre-market.autopay/`, Windows: `%LOCALAPPDATA%\com.libre-market.autopay\logs\`) | no |

## Known limits

- Canary URLs are stored but not checked yet: `CANARY_*` alerts never fire.
- `subscriptions.db` is not encrypted (planned: SQLCipher).
- Bitcoin is display-only; one wallet only; no in-app updater.
- macOS 10.15+ (the arm64 build is Apple Silicon only, so macOS 11+).
