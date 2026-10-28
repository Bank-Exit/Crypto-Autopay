# Alert System — Crypto AutoPay

Alerts are recomputed after each scheduler run (~60s) and after each balance refresh.
The tray icon changes color accordingly. If both Orange and Red alerts exist, Red takes priority.

Alerts are displayed in two places:
- **Sidebar compact panel**: translated text, click to open overlay
- **Overlay modal**: full detail, dismissable

---

## Red alerts (critical — payments at risk)

| Kind | Trigger | Displayed message |
|---|---|---|
| `BALANCE_EMPTY` | Wallet balance = 0 XMR with at least one Active subscription | "Solde nul — les paiements récurrents ne peuvent pas être envoyés" |
| `CANARY_EXPIRED` | A subscription's canary URL returns `{"expired": true}` or HTTP 410 | "Canary expiré — {label}" |
| `PAYMENT_OVERDUE_HARD` | `next_payment_at` + 24h has passed and no payment confirmed | "Retard critique : {label} — tolérance dépassée" |
| `OFFLINE_WITH_PENDING` | No daemon contact for >24h AND at least one Active sub has `next_payment_at` within the next 7 days | "Hors ligne depuis {N}h — des paiements sont prévus dans les 7 prochains jours" |

**Red = tray icon shows red dot. App stays in tray. User must act.**

---

## Orange alerts (warnings — attention needed)

| Kind | Trigger | Displayed message |
|---|---|---|
| `BALANCE_LOW` | Balance runway < threshold (default 7 days) at current spending rate | "Portefeuille vide dans ~{N} j (seuil : {T}j)" |
| `CANARY_UNREACHABLE` | Canary URL returns HTTP 4xx/5xx or connection error | "Canary inaccessible — {label}" |
| `PAYMENT_OVERDUE_SOFT` | `next_payment_at` + 1h has passed (within 24h tolerance) | "Paiement en retard : {label} (dans la tolérance)" |
| `PAYMENT_FAILED` | `transfer` RPC call returned an error (e.g. "not enough money") | "Échec de paiement — {label}" |
| `OUTPUTS_LOCKED` | Wallet outputs locked (unconfirmed outputs), retry in 25 min | "Fonds verrouillés — {label}" |
| `WALLET_NOT_SYNCED` | `blocks_behind` > 1000 after a refresh | "Wallet non synchronisé avec la blockchain" |
| `PAYMENT_BLOCKED_LIMITS` | A due subscription breaks the payment limits (see below), e.g. after a limit was re-locked | "{label} — envoi suspendu. {raison}" |
| `BALANCE_SHORT_UPCOMING` | Payments due within 48 h exceed the known balance (one grouped alert) | "{N} paiement(s) prévu(s) dans les 48 h dépassent le solde disponible…" |

`PAYMENT_FAILED`, `OUTPUTS_LOCKED` and `PAYMENT_BLOCKED_LIMITS` are event alerts: they survive
`recompute_alerts` for 24 h and are cleared as soon as the subscription is paid.

---

## Payment limits (`payment_policy.rs`, enforced by the backend)

Checked when a subscription is added (`add_subscription`, `save_subscription`) and again by the
scheduler before every send. Errors are returned as `POLICY:<CODE>[:detail]` and translated by the frontend.

| Rule | Value | Code |
|---|---|---|
| Currency | XMR only (the scheduler always sends XMR) | `UNSUPPORTED_CURRENCY` |
| Address | Mainnet, base58, 95 chars (`4`/`8`) or 106 chars integrated (`4`) | `INVALID_ADDRESS` |
| Minimum per send | 0.00021 XMR | `AMOUNT_TOO_LOW` |
| Maximum per send | 5 XMR, unless "Montants élevés" is unlocked (password) | `AMOUNT_ABOVE_MAX` |
| Minimum cadence | 7 days, unless "Cadences très courtes" is unlocked (password) | `CADENCE_TOO_SHORT` |
| Absolute floor | 10 min between two cron occurrences (`*/25` has a 10 min gap); the 25 min send cooldown still applies | `CADENCE_BELOW_FLOOR` |
| Monthly total | Adding a sub that brings the projection above 1 XMR / 30 days needs `confirm_monthly` | `MONTHLY_TOTAL:<xmr>` |

Unlocks are stored in `payment_limits.json`; re-locking never needs the password.

Confirmation before a send (even in auto mode), via `payment-pending` + `approve_payment`:
- `first_payment`: no approved terms for this subscription yet;
- `terms_changed`: blake3 fingerprint of (address, amount, currency, schedule) differs from the approved one (`approved_terms.json`);
- `high_amount`: amount >= 1 XMR (every time);
- `semi_auto`: semi-auto mode is on.

Next due dates follow the cron calendar (`next_after`), not a fixed period.

---

## Offline >24h detection

The `OFFLINE_WITH_PENDING` alert fires when:
1. The last successful daemon contact (`last_daemon_contact_at`) was more than **24 hours ago**, AND
2. At least one Active subscription has `next_payment_at` within the next **7 days**.

This means: the app could not reach the Monero network for a full day, and a payment is coming up soon. The user must reconnect to allow the payment to go through.

The last daemon contact timestamp is updated every time `refresh_balance` receives a valid daemon block count > 0.

---

## Offline mode (frontend)

`refresh_balance` returns `daemon_reachable`, computed from a direct `get_info` call to the remote node after the wallet-rpc process has stopped. When it is `false`, the "Hors ligne" banner is shown instead of the sync progress. The next successful refresh with a reachable node clears it.

The RPC status dot changes to **grey / slow pulse** when offline (instead of green or amber).

Important: "Connexion refusée" on 127.0.0.1:18083 is NOT treated as offline — it means the local wallet-rpc process is restarting between scheduler runs (normal behaviour).

---

## Semi-auto payment mode

When enabled (default: **on**):
- The scheduler detects due payments but does NOT send them automatically.
- It emits a `payment-pending` event with: `sub_id`, `sub_label`, `amount_xmr`, `schedule`, `payments_sent`, `last_payment_at`, `reasons`.
- The frontend shows a **confirmation modal** with the payment details: sub label, amount, recurrence schedule, number of previous payments, and when the last payment was sent.
- The modal is only shown when:
  - The balance has been successfully loaded (`balanceXmr !== null`)
  - No sync is in progress (`balanceLoading === false`)
  - The app is not in offline mode
- User can **Confirm** (payment is queued for the next scheduler cycle), **Reporter d'1 h** (due date moved 60 min later in the DB) or **Passer cette échéance** (no send, next occurrence of the cadence).

When disabled ("auto" mode):
- Payments are sent immediately when due, without any user confirmation.

The mode is persisted in `localStorage` on the frontend and synchronised to the Rust `AppState.semi_auto_mode` on startup and every change.

---

## Alert configuration

Configurable in Settings > Alertes:
- `balance_low_threshold_days` (default: 7 days) — days of runway before BALANCE_LOW fires
- Per-kind enable/disable toggles (all enabled by default)

Default enabled kinds:
`BALANCE_LOW`, `BALANCE_EMPTY`, `PAYMENT_FAILED`, `CANARY_EXPIRED`, `CANARY_UNREACHABLE`,
`PAYMENT_OVERDUE_SOFT`, `PAYMENT_OVERDUE_HARD`, `WALLET_NOT_SYNCED`, `OUTPUTS_LOCKED`, `OFFLINE_WITH_PENDING`,
`PAYMENT_BLOCKED_LIMITS`, `BALANCE_SHORT_UPCOMING`
