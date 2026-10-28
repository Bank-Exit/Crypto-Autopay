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
- It emits a `payment-pending` event with: `sub_id`, `sub_label`, `amount_xmr`, `schedule`, `payments_sent`, `last_payment_at`.
- The frontend shows a **confirmation modal** with the payment details: sub label, amount, recurrence schedule, number of previous payments, and when the last payment was sent.
- The modal is only shown when:
  - The balance has been successfully loaded (`balanceXmr !== null`)
  - No sync is in progress (`balanceLoading === false`)
  - The app is not in offline mode
- User can **Confirm** (payment is queued for the next scheduler cycle) or **Defer** (payment skipped this cycle, retried next time).

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
`PAYMENT_OVERDUE_SOFT`, `PAYMENT_OVERDUE_HARD`, `WALLET_NOT_SYNCED`, `OUTPUTS_LOCKED`, `OFFLINE_WITH_PENDING`
