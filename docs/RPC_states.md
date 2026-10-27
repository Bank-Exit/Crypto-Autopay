# RPC States — Crypto AutoPay

This document maps every possible state of the RPC subsystem, their causes, implications for the UI, and recovery paths.

---

## 1. Architecture overview

```
┌─────────────────────────────────────────────────────────┐
│ App (Tauri/Rust)                                        │
│                                                         │
│  AppState                                               │
│  ├── unlocked: bool          ← session password in mem  │
│  ├── session_password: Option │                          │
│  ├── rpc_busy: AtomicBool    ← exclusive RPC lock       │
│  ├── rpc_connected: bool     ← last refresh result      │
│  └── rpc_last_error: Option                             │
│                                                         │
│  RpcManager (per command, ephemeral)                    │
│  ├── status: Stopped|Starting|Running|Error             │
│  ├── process: Option<Child>  ← monero-wallet-rpc PID   │
│  └── config: RpcConfig       ← binary path, port, node │
│                                                         │
│  Scheduler (Tokio task, every 60s)                      │
│  └── calls: spawn_blocking → RpcManager::start_for_dir  │
│                                                         │
│  ─── HTTP 127.0.0.1:18083 ───────────────────────────► │
│       monero-wallet-rpc (external process, short-lived) │
│       └── TCP → daemon node (opennode.xmr-tw.org:18089) │
└─────────────────────────────────────────────────────────┘
```

**Key architectural choice**: `monero-wallet-rpc` is **started on demand and stopped immediately after** each operation (balance refresh, payment). It is never kept running between operations. This minimises the attack surface but means every refresh spawns + kills a process.

---

## 2. State diagram

```
                     ┌──────────┐
           App start │          │ vault missing
          ──────────►│ CHECKING │──────────────► [Welcome / Onboarding]
                     │          │
                     └────┬─────┘
                          │ vault found
                          ▼
                     ┌──────────┐   wrong pw / error
                     │  LOCKED  │◄──────────────────────────────────────────┐
                     │          │                                            │
                     └────┬─────┘                                           │
                          │ correct password                                 │
                          │ unlock_wallet()                                  │
                          ▼                                                  │
              ┌───────────────────────┐                                      │
              │ UNLOCKED              │                                      │
              │ session_password: ✓   │                                      │
              │ rpc_connected: ?      │                                      │
              └─────────┬─────────────┘                                      │
                        │                                                    │
          ┌─────────────┼──────────────────────────────┐                    │
          │             │                              │                    │
          ▼             ▼                              ▼                    │
  ┌──────────────┐ ┌──────────────┐          ┌──────────────────┐          │
  │ BALANCE      │ │ SCHEDULER    │          │ USER LOCK /      │          │
  │ REFRESH      │ │ PAYMENT      │          │ AUTO-LOCK (2min) │──────────┘
  │ (refresh_    │ │ (every 60s)  │          └──────────────────┘
  │  balance)    │ │              │
  └──────┬───────┘ └──────┬───────┘
         │                │
         └────────┬────────┘
                  │  both use the same flow:
                  ▼
         ┌─────────────────┐
         │ CHECK rpc_busy  │
         │ AtomicBool      │
         └────┬────────────┘
              │
    ┌─────────┴──────────┐
    │ busy?              │ not busy
    ▼                    ▼
  [SKIP / RPC busy]  ┌─────────────────────────────────┐
  (log warn,         │ RpcManager::start_for_dir()     │
   return Err)       │ ┌─────────────────────────────┐ │
                     │ │ 1. detect binary (PATH/cfg) │ │
                     │ │ 2. kill stale PID on :18083 │ │
                     │ │ 3. spawn monero-wallet-rpc  │ │
                     │ │ 4. wait_for_ready (15s)     │ │
                     │ └─────────────────────────────┘ │
                     └────────────┬────────────────────┘
                                  │
                   ┌──────────────┴──────────────┐
                   │ ready?                      │ timeout / error
                   ▼                             ▼
          ┌──────────────────┐         ┌─────────────────────┐
          │ open_wallet()    │         │ RPC_ERROR state      │
          │ (JSON-RPC call)  │         │ rpc_connected=false  │
          └────────┬─────────┘         │ rpc_last_error=msg   │
                   │                   └─────────────────────┘
          ┌────────┴─────────┐
          │ ok?              │ error
          ▼                  ▼
  ┌──────────────┐  ┌──────────────────┐
  │ get_balance  │  │ RPC_ERROR state  │
  │ + get_height │  │ (open_wallet     │
  │ JSON-RPC     │  │  failed: wrong   │
  │              │  │  pw, corrupted)  │
  └──────┬───────┘  └──────────────────┘
         │
  ┌──────┴────────┐
  │ ok?           │ HTTP/TCP error (daemon unreachable)
  ▼               ▼
  ┌──────────┐  ┌──────────────────────────────────────┐
  │ SUCCESS  │  │ DAEMON_UNREACHABLE state             │
  │ emit     │  │ balance=0 (unknown, not real zero)   │
  │ alerts-  │  │ rpc_connected=false                  │
  │ updated  │  │ → may trigger isOffline on frontend  │
  └──────────┘  └──────────────────────────────────────┘
         │
  RpcManager::stop() called in all paths (RAII)
  monero-wallet-rpc process killed
  rpc_busy cleared
```

---

## 3. States and their meaning

### 3.1 AppState.unlocked

| Value | Meaning |
|---|---|
| `false` | Wallet locked. `session_password = None`. All RPC commands return `Err("Locked")`. |
| `true` | Wallet unlocked. `session_password = Some(pw)`. RPC commands may proceed. |

**Transitions**:
- `false → true` : `unlock_wallet()` command with correct password
- `true → false` : `lock_wallet()` command (manual or auto-lock after 2 min inactivity)

### 3.2 AppState.rpc_busy (AtomicBool)

| Value | Meaning |
|---|---|
| `false` | No RPC operation in progress. Commands may start one. |
| `true` | An RPC operation is running (balance refresh OR scheduler payment). Any new call returns `Err("RPC busy")`. |

**Critical**: Only ONE operation can hold this lock at a time. The scheduler and `refresh_balance` use `compare_exchange` — if the exchange fails, the caller silently skips. The lock is released in a RAII closure (always released, even on panic/error).

### 3.3 RpcStatus (per RpcManager instance)

| Status | Description |
|---|---|
| `Stopped` | No process running. Safe to start. |
| `Starting` | Process spawned, waiting for HTTP on :18083. |
| `Running` | HTTP :18083 responds. Can send JSON-RPC calls. |
| `Error(msg)` | Process exited or HTTP never responded within 15s. |

**Note**: Each command creates its own `RpcManager` instance. There is no global persistent RpcManager keeping the process alive. Every `refresh_balance` and every scheduler payment creates, uses, and destroys its own manager.

### 3.4 AppState.rpc_connected (frontend-facing)

| Value | Meaning for UI |
|---|---|
| `true` | Last balance refresh succeeded. Green dot in sidebar. |
| `false` | Last refresh failed or never tried. Amber/grey dot. |
| `null` (frontend) | Fresh app launch, never tried yet. Grey dot. |

---

## 4. All error cases and recovery

### Case A — Binary not found

**Trigger**: `detect_binary()` returns `None` (not in PATH and not configured).

**Rust**: Returns `Err("Binary not found: monero-wallet-rpc not in PATH …")`.

**Frontend**: `rpcDiagnosis()` matches `"not found"` / `"binary"` → shows "Outil Monero introuvable" + link to getmonero.org.

**Recovery**: User installs `monero-wallet-rpc` or sets path in Settings > RPC.

---

### Case B — Port 18083 already in use (stale process)

**Trigger**: A previous wallet-rpc process crashed and left the port bound, OR another app uses :18083.

**Rust**: `start_for_dir()` detects the port is bound → calls `kill_stale_process()` → kills any PID listening on :18083 → spawns new process.

**Frontend**: Transparent to the user in normal cases. If kill fails, `wait_for_ready()` times out after 15s → Case D.

**Recovery**: Automatic. If the stale process can't be killed (permissions), user must manually kill it.

---

### Case C — RPC busy (concurrent calls)

**Trigger**: A payment is running (scheduler) and the user clicks Refresh, OR two scheduler cycles overlap.

**Rust**: `compare_exchange(false, true)` fails → returns `Err("RPC busy (payment in progress) — skipping refresh")`.

**Frontend**: Caught in `refreshBalance()` catch block, logged as Info ("Balance refresh skipped — paiement en cours"), no UI change.

**Recovery**: Automatic. The next `refreshBalance()` call (2-min timer) will succeed once the lock is released.

---

### Case D — RPC process timeout (15s)

**Trigger**: `monero-wallet-rpc` spawned but :18083 never becomes reachable within 15s. Possible causes:
- Binary exists but crashes immediately (wrong version, missing libssl)
- Port blocked by firewall
- Very slow machine

**Rust**: `wait_for_ready()` returns `Err("not ready after 15s")`. RpcManager status → `Error`.

**Frontend**: `rpcDiagnosis()` matches `"timeout"` / `"not ready"` / `"attempt"` → shows "monero-wallet-rpc n'a pas démarré" + Retry button.

**Recovery**: User clicks Retry → `refreshBalance()` called. Persistent failure = check binary version (`monero-wallet-rpc --version` must be ≥ 0.18.3.4).

---

### Case E — open_wallet fails (wrong password / corrupted wallet file)

**Trigger**: `session_password` is set but wallet file was created with a different password, or file is corrupted.

**Rust**: `open_wallet` JSON-RPC returns `{"error": …}`.

**Frontend**: `rpcDiagnosis()` matches `"word list"` / `"Electrum"` / `"checksum"` → shows "Seed de récupération invalide" with reset option.

**Recovery**: Either restore from seed (re-onboarding) or let the app auto-fix the seed checksum and retry unlock.

---

### Case F — Daemon node unreachable (real offline)

**Trigger**: `monero-wallet-rpc` starts fine on :18083, opens the wallet, but the remote daemon (opennode.xmr-tw.org:18089) is unreachable.

**Rust**: `get_balance` JSON-RPC succeeds but returns `balance: 0` because the wallet hasn't synced. `get_daemon_block_height` returns 0.

**Frontend**: After 3 consecutive `pollSyncInfo` readings where `daemon_height = 0` and `wallet_height > 100_000`, `isOffline = true` → blue "Hors ligne" banner. Balance displayed as `-.----` if unknown, or greyed cached value if previously known.

**What NOT to confuse**: `Connexion refusée` from 127.0.0.1:18083 is Case G (local RPC restarting), not this.

**Recovery**: Internet reconnects → `pollSyncInfo` gets `daemon_height > 0` → `offlineConsecutive` resets → `isOffline = false` → normal.

---

### Case G — Local RPC connection refused (RPC restarting between operations)

**Trigger**: The scheduler has just killed `monero-wallet-rpc` after a payment. A `refresh_balance` call arrives in the milliseconds before the next one is spawned.

**Rust**: HTTP call to 127.0.0.1:18083 returns `os error 104` (connection reset) or `os error 111` (connection refused).

**Frontend**: This is `rpcOk = false` with a `"Connexion refusée"` error message. **Not treated as offline** — this is a transient local state. The 2-min auto-refresh timer will pick it up once the process restarts.

**Critical**: This was previously misidentified as offline. The fix: only `pollSyncInfo` daemon_height check sets `isOffline`, never error message strings.

---

### Case H — Wallet locked during refresh

**Trigger**: Auto-lock fires (2-min timer) while a refresh is queued, OR user manually locks while refresh is running.

**Rust**: `refresh_balance()` first checks `s.unlocked` → if `false`, returns `Err("Locked")` immediately, before spawning any process.

**Frontend**: Caught in catch block, branch `errStr.includes("Locked")` → `balanceXmr = null` → displays `-.---- XMR`. No error overlay shown.

**Recovery**: User unlocks → new `refresh_balance()` triggered by `onMount` after unlock.

---

### Case I — Semi-auto mode: payment queued but not sent

**Trigger**: `semi_auto_mode = true` AND a subscription is due AND it hasn't been approved this session.

**Rust**: Scheduler emits `payment-pending` event, stores in `pending_approvals`, skips the actual transfer.

**Frontend**: Shows confirmation modal (only when `balanceLoading = false` AND `balanceXmr !== null` AND not offline). User confirms → `approve_payment()` → `approved_payments` set → next scheduler cycle sends.

**Important**: The approval only lives in memory for the current session. If the app restarts, the pending approval is re-shown at next scheduler cycle.

---

### Case J — Balance = 0 vs balance = unknown

These two states look similar but mean very different things:

| Situation | `balanceXmr` | Displayed |
|---|---|---|
| RPC OK, wallet synced, truly empty | `0.0` | `0.0000 XMR` + "Wallet vide" hint |
| RPC failed or locked, balance never loaded | `null` | `-.---- XMR` |
| RPC failed, but previous value known and > 0 | `> 0.0` | greyed `X.XXXX XMR` |
| Offline, no cached value | `null` | `-.---- XMR` |
| Offline, cached value | `> 0.0` | greyed `X.XXXX XMR` + "non actualisé" hint |

**Rule**: Never display `0.0000 XMR` when RPC is down or locked. That value is meaningless — the real balance is unknown.

---

## 5. RPC operation sequence (happy path)

```
User action: click Refresh
  │
  ▼
refreshBalance() {
  balanceLoading = true
  tauriInvoke("refresh_balance")
  │
  ▼  [Rust]
  check unlocked → true
  compare_exchange rpc_busy false→true → ok
  │
  ├─ spawn_blocking {
  │    RpcManager::new(config)
  │    start_for_dir(wallet_dir)
  │      detect_binary()               → /usr/local/bin/monero-wallet-rpc
  │      kill stale :18083 if any
  │      Command::spawn(args)          → PID 3488406
  │      wait_for_ready(15s)           → ok after 9 attempts (~2s)
  │    open_wallet(name, password)     → ok
  │    get_balance()                   → { balance: 820000000000, ... }
  │    get_height()                    → { height: 3793484 }
  │    get_daemon_block_height()       → 3793484
  │    RpcManager::stop()              → kill PID 3488406
  │  }
  │
  ├─ AppState update:
  │    balance_atomic = 820000000000
  │    last_wallet_height = 3793484
  │    last_daemon_height = 3793484
  │    last_daemon_contact_at = now()
  │    rpc_busy = false
  │
  ├─ recompute_alerts(...)
  ├─ emit "alerts-updated"
  └─ return Ok { xmr: 0.82, ... }

  [Frontend]
  balanceXmr = 0.82
  rpcOk = true
  wallets[0].balance = "0.8200"
  balanceLoading = false
  refreshAlerts()
}
```

---

## 6. Scheduler payment sequence (with semi-auto)

```
Tokio timer fires (every 60s)
  │
  ▼
run_payment_scheduler() {
  check unlocked → true
  check rpc_busy → false
  load due subscriptions from DB
  for each due sub:
    if semi_auto_mode && not in approved_payments:
      emit "payment-pending" {sub_id, sub_label, amount_xmr, schedule, ...}
      store in pending_approvals
      → WAIT for user approval
    else:
      spawn_blocking {
        RpcManager::start_for_dir(wallet_dir)    → PID
        open_wallet()
        transfer({dest, amount_pico})
        RpcManager::stop()                        → kill PID
      }
      on success:
        update DB (next_payment_at, payments_sent)
        append_history_entry() (encrypted)
        emit "payment-sent" {sub_id, tx_hash, next_payment_at}
      on "not enough money":
        emit "payment-failed"
      on "outputs locked":
        reschedule +25 min
        emit "payment-locked" {sub_id, retry_in_min, next_payment_at}
}
```

---

## 7. Common misconceptions

| Misconception | Reality |
|---|---|
| "monero-wallet-rpc is always running" | It starts on demand and stops immediately after each operation |
| "Connexion refusée on :18083 = offline" | It means the local RPC process was just killed by the scheduler — normal |
| "balance = 0 means wallet is empty" | 0 could mean locked/RPC down — only meaningful when `rpc_connected = true` |
| "semi-auto blocks all payments" | It blocks automatic sends; approved payments go through on next 60s tick |
| "rpc_busy prevents payment AND refresh simultaneously" | Correct — this is intentional. One operation at a time. |
| "the 2-min auto-lock runs in Rust" | No — it runs on the frontend (Svelte timer). It calls `lock_wallet` which clears `session_password` in Rust. |
