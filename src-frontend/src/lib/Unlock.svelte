<script>
  import { createEventDispatcher, onMount, onDestroy } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { t, currentLang, LANGUAGES } from "./i18n.js";
  import { theme, toggleTheme } from "./theme.js";
  import { logInfo, logWarn, logError } from "./AppLogger.js";

  const dispatch = createEventDispatcher();

  let password = "";
  let error = "";
  let rpcWarning = null; // non-blocking warning after unlock
  let loading = false;
  export let showForgot = false;
  let forgotBusy = false;
  let forgotError = "";

  // Archives the local wallet (kept in archive/, subscriptions stay) before restoring.
  async function restoreFromSeed() {
    forgotBusy = true;
    forgotError = "";
    try {
      await invoke("reset_wallet");
      logWarn("Unlock", "Local wallet archived for a restore from seed");
      dispatch("reset");
    } catch (e) {
      forgotError = String(e?.message ?? e);
      forgotBusy = false;
    }
  }

  // Progressive lockout, enforced by the backend; this only mirrors the countdown.
  let lockedSecs = 0;
  let lockTimer = null;

  function startLockout(secs) {
    clearInterval(lockTimer);
    lockedSecs = Math.max(0, Number(secs) || 0);
    if (lockedSecs > 0) {
      lockTimer = setInterval(() => {
        lockedSecs = Math.max(0, lockedSecs - 1);
        if (lockedSecs === 0) clearInterval(lockTimer);
      }, 1000);
    }
  }

  function formatWait(secs) {
    const h = Math.floor(secs / 3600), m = Math.floor((secs % 3600) / 60), sec = secs % 60;
    if (h > 0) return `${h} h ${String(m).padStart(2, "0")} min`;
    if (m > 0) return `${m} min ${String(sec).padStart(2, "0")} s`;
    return `${sec} s`;
  }

  onMount(() => { invoke("unlock_lockout_remaining").then(startLockout).catch(() => {}); });
  onDestroy(() => clearInterval(lockTimer));

  async function unlock() {
    if (!password || lockedSecs > 0) return;
    loading = true;
    error = "";
    rpcWarning = null;
    logInfo("Unlock", "unlock_wallet invoked");
    try {
      const result = await invoke("unlock_wallet", { password });
      logInfo("Unlock", `unlock_wallet returned: address=${result.address ? "set" : "none"}, seed_invalid=${result.seed_invalid}, rpc_error=${result.rpc_error ?? "none"}`);

      if (result.rpc_error) {
        if (result.seed_invalid) {
          logError("Unlock", `Seed invalid: ${result.rpc_error}`);
          rpcWarning = { type: "seed_invalid", raw: result.rpc_error };
        } else {
          logWarn("Unlock", `RPC unavailable (offline mode): ${result.rpc_error}`);
          rpcWarning = { type: "network", raw: result.rpc_error };
        }
        loading = false;
        dispatch("unlocked", { address: result.address, balance: result.balance_xmr, rpcError: result.rpc_error });
      } else {
        logInfo("Unlock", "Unlock OK — dispatching to dashboard");
        dispatch("unlocked", { address: result.address, balance: result.balance_xmr });
      }
    } catch (e) {
      const msg = typeof e === "string" ? e : String(e);
      logError("Unlock", `unlock_wallet threw: ${msg}`);
      if (msg.startsWith("LOCKED_OUT:")) {
        startLockout(parseInt(msg.slice("LOCKED_OUT:".length), 10));
        password = "";
        error = "";
      } else if (msg.includes("Incorrect")) {
        const left = msg.match(/remaining=(\d+)/);
        error = left && Number(left[1]) <= 2
          ? `${$t.unlockWrongPassword} ${$t.unlockAttemptsLeft.replace("{n}", left[1])}`
          : $t.unlockWrongPassword;
      } else {
        error = msg;
      }
      loading = false;
    }
  }

  function friendlyRpcError(w, tObj) {
    if (!w) return "";
    if (w.type === "seed_invalid") return tObj.unlockSeedInvalid;
    if (w.type === "network") return tObj.unlockNetworkWarn;
    return tObj.unlockNetworkError + w.raw;
  }

  function onKey(e) {
    if (e.key === "Enter") unlock();
  }
</script>

<div class="unlock-screen">
  <div class="top-controls">
    <div class="lang-wrapper">
      <svg class="globe" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"
           stroke-linecap="round" stroke-linejoin="round">
        <circle cx="12" cy="12" r="10"/>
        <line x1="2" y1="12" x2="22" y2="12"/>
        <path d="M12 2a15.3 15.3 0 0 1 4 10 15.3 15.3 0 0 1-4 10 15.3 15.3 0 0 1-4-10 15.3 15.3 0 0 1 4-10z"/>
      </svg>
      <select bind:value={$currentLang} class="lang-select">
        {#each LANGUAGES as lang}
          <option value={lang.code}>{lang.label}</option>
        {/each}
      </select>
    </div>
    <button class="theme-btn" on:click={toggleTheme}>
      {#if $theme === "light"}🌙{:else}☀️{/if}
    </button>
  </div>

  <div class="unlock-card card">
    <!-- Logo -->
    <div class="lock-logo">
      <img src="/logo.png" alt="Crypto AutoPay" width="140" height="140" style="border-radius:20px;display:block;margin:0 auto 16px"/>
      <div>
        <div class="lock-hint">{$t.unlockHint}</div>
      </div>
    </div>

    <div class="lock-icon-row">
      <svg width="28" height="28" viewBox="0 0 24 24" fill="none" stroke="var(--text-muted)" stroke-width="1.5"
           stroke-linecap="round" stroke-linejoin="round">
        <rect x="3" y="11" width="18" height="11" rx="2" ry="2"/>
        <path d="M7 11V7a5 5 0 0 1 10 0v4"/>
      </svg>
    </div>

    <label class="pw-label">
      {$t.masterPasswordMin}
      <div class="pw-row">
        <!-- svelte-ignore a11y-autofocus -->
        <input
          type="password"
          bind:value={password}
          on:keydown={onKey}
          placeholder="••••••••••••"
          autocomplete="current-password"
          autofocus
          disabled={lockedSecs > 0}
        />
      </div>
    </label>

    {#if lockedSecs > 0}
      <div class="error-msg">{$t.unlockLockedOut.replace("{time}", formatWait(lockedSecs))}</div>
    {:else if error}
      <div class="error-msg">{error}</div>
    {/if}

    {#if rpcWarning}
      <div class="rpc-warning">
        <p>{friendlyRpcError(rpcWarning, $t)}</p>
        {#if rpcWarning.type === "seed_invalid"}
          <button class="btn-link warn-reset" on:click={() => { rpcWarning = null; showForgot = true; }}>
            {$t.unlockResetMine}
          </button>
        {/if}
        <button class="btn-link warn-close" on:click={() => dispatch("unlocked", { address: "", balance: 0 })}>
          {$t.unlockContinueAnyway}
        </button>
      </div>
    {:else if showForgot}
      <div class="forgot-panel">
        <strong>{$t.forgotTitle}</strong>
        <p>{$t.forgotDetail}</p>
        {#if forgotError}<div class="error-msg">{forgotError}</div>{/if}
        <button class="btn-primary btn-full" disabled={forgotBusy} on:click={restoreFromSeed}>{$t.forgotRestore}</button>
        <button class="btn-link" on:click={() => showForgot = false}>{$t.back}</button>
      </div>
    {:else}
      <button
        class="btn-primary btn-full"
        on:click={unlock}
        disabled={!password || loading || lockedSecs > 0}
      >
        {#if loading}
          <span class="spinner"></span> {$t.unlockLoading}
        {:else}
          {$t.unlock} →
        {/if}
      </button>

      <button class="btn-link" on:click={() => showForgot = true}>
        {$t.forgotPassword}
      </button>
    {/if}
  </div>
</div>

<style>
  .pw-row { display: flex; align-items: center; gap: 6px; }
  .pw-row input { flex: 1; }

  .unlock-screen {
    width: 100vw; min-height: 100vh;
    display: flex; align-items: center; justify-content: center;
    background: var(--bg);
    position: relative;
  }

  .top-controls {
    position: fixed; top: 12px; right: 16px;
    display: flex; align-items: center; gap: 8px; z-index: 20;
  }
  .lang-wrapper {
    display: flex; align-items: center; gap: 5px;
    background: var(--bg-card); border: 1px solid var(--border);
    border-radius: 7px; padding: 5px 9px;
  }
  .globe { width: 13px; height: 13px; color: var(--text-muted); flex-shrink: 0; }
  .lang-select {
    border: none; background: transparent; color: var(--text);
    font-size: 12px; cursor: pointer; outline: none;
    -webkit-appearance: none; appearance: none;
  }
  .theme-btn {
    background: var(--bg-card); border: 1px solid var(--border);
    border-radius: 7px; padding: 5px 9px; font-size: 15px; cursor: pointer;
  }

  .unlock-card {
    width: 100%; max-width: 380px;
    padding: 32px 28px;
    display: flex; flex-direction: column; gap: 16px;
  }

  .lock-logo {
    display: flex; align-items: center; gap: 12px;
    margin-bottom: 4px;
  }
  .lock-hint { font-size: 11px; color: var(--text-muted); margin-top: 2px; }

  .lock-icon-row {
    display: flex; justify-content: center;
    padding: 8px 0 4px;
  }

  label {
    display: flex; flex-direction: column; gap: 6px;
    font-size: 12.5px; color: var(--text-muted);
  }

  .error-msg {
    background: var(--red-bg); color: var(--red);
    padding: 8px 12px; border-radius: var(--radius-sm); font-size: 13px;
  }

  .rpc-warning {
    background: #fff7ed; border: 1px solid #fb923c44;
    border-radius: 8px; padding: 12px 14px;
    display: flex; flex-direction: column; gap: 8px;
  }
  .rpc-warning p {
    margin: 0; font-size: 12.5px; color: #92400e; line-height: 1.5;
  }
  .warn-reset { color: var(--accent); font-weight: 600; }
  .forgot-panel { display: flex; flex-direction: column; gap: 10px; font-size: 13px; color: var(--text); }
  .forgot-panel p { margin: 0; line-height: 1.55; color: var(--text-muted); }
  .warn-close { color: var(--text-muted); }

  .btn-full { width: 100%; padding: 13px; font-size: 14px; }

  .btn-link {
    background: none; border: none;
    color: var(--text-muted); font-size: 11px;
    cursor: pointer; text-align: center;
    padding: 4px;
    text-decoration: underline;
    text-underline-offset: 2px;
  }
  .btn-link:hover { color: var(--red); }

  button:disabled { opacity: 0.4; cursor: not-allowed; }

  .spinner {
    width: 13px; height: 13px;
    border: 2px solid var(--border);
    border-top-color: white;
    border-radius: 50%;
    display: inline-block;
    animation: spin 0.7s linear infinite;
  }
  @keyframes spin { to { transform: rotate(360deg); } }
</style>
