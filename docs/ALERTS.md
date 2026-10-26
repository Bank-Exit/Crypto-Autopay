# Alert System — Crypto AutoPay

Alerts are recomputed after each scheduler run (~60s) and after each balance refresh.
The tray icon changes color accordingly. If both Orange and Red alerts exist, Red takes priority.

---

## Red alerts (critical — payments at risk)

| Kind | Trigger | Message shown |
|---|---|---|
| `BALANCE_EMPTY` | Wallet balance = 0 XMR with at least one Active subscription | "Wallet balance is zero — recurring payments cannot be sent" |
| `CANARY_EXPIRED` | A subscription's canary URL returns `{"expired": true}` or HTTP 410 | "Canary expired for sub #{id} — merchant may have closed their store" |
| `PAYMENT_OVERDUE_HARD` | `next_payment_at` + 24h has passed and no payment sent | "Sub #{id} overdue by >24h — possible payment failure" |

**Red = tray icon shows red dot. App stays in tray. User must act.**

---

## Orange alerts (warnings — attention needed)

| Kind | Trigger | Message shown |
|---|---|---|
| `BALANCE_LOW` | Balance will run out in < 30 days at current spending rate | "Wallet balance will run out in ~{N} days at current spending rate" |
| `PAYMENT_FAILED` | `send_payment` returned an RPC error for any subscription | "Payment failed for sub #{id}" (no amount, no address) |
| `CANARY_UNREACHABLE` | Canary URL returns non-200 or times out (3 consecutive checks) | "Canary unreachable for sub #{id} — merchant server may be down" |
| `PAYMENT_OVERDUE_SOFT` | `next_payment_at` + 1h has passed (soft deadline) | "Sub #{id} overdue by >1h" |
| `WALLET_NOT_SYNCED` | `last_wallet_height` has not changed in > 24h | "Wallet has not synced in over 24h — check network connection" |

**Orange = tray icon shows orange dot. Payments may still work.**

---

## Priority rules

- If any Red alert exists → tray = red, badge shows total alert count
- If only Orange alerts → tray = orange, badge shows count
- If no alerts → tray = normal icon, no badge

---

## Security note

Alert messages contain **no sensitive data**: no amounts, no destination addresses, no keys.
Only subscription internal IDs are referenced (not addresses).
Alerts are stored in memory only (not persisted to disk).

---

## Future alerts (planned)

- `MAX_PAYMENTS_REACHED`: subscription has hit its `number_of_payments` limit (informational, auto-pause)
- `CLOCK_DRIFT`: system clock is off by > 5 min (affects CRON scheduling)
- `DAEMON_UNREACHABLE`: Monero daemon node cannot be reached
- `LOW_DISK_SPACE`: app data directory has < 100MB free (wallet sync needs space)
