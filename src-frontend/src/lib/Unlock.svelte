<script>
  import { createEventDispatcher } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { t, currentLang, LANGUAGES } from "./i18n.js";
  import { theme, toggleTheme } from "./theme.js";
  import { logInfo, logWarn, logError } from "./AppLogger.js";

  const dispatch = createEventDispatcher();

  let password = "";
  let error = "";
  let rpcWarning = null; // non-blocking warning after unlock
  let loading = false;
  let loadingMsg = "Déverrouillage…";

  async function unlock() {
    if (!password) return;
    loading = true;
    error = "";
    rpcWarning = null;
    loadingMsg = "Déverrouillage…";
    logInfo("Unlock", "unlock_wallet invoked");
    try {
      const result = await invoke("unlock_wallet", { password });
      logInfo("Unlock", `unlock_wallet returned: address=${result.address ? result.address.slice(0,12)+"…" : "none"}, balance=${result.balance_xmr}, seed_invalid=${result.seed_invalid}, rpc_error=${result.rpc_error ?? "none"}`);

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
      error = msg.includes("Incorrect") ? "Mot de passe incorrect." : msg;
      loading = false;
    }
  }

  function friendlyRpcError(w) {
    if (!w) return "";
    if (w.type === "seed_invalid") {
      return "⚠️ Votre phrase secrète semble corrompue (ancienne version de l'app). Vos abonnements sont accessibles, mais le solde Monero ne peut pas être affiché. Pour corriger : réinitialisez votre wallet (bouton ci-dessous) puis recréez-le avec les 25 mots notés.";
    }
    if (w.type === "network") {
      return "⚠️ Impossible de joindre le réseau Monero. Vérifiez votre connexion internet. Vos abonnements restent accessibles, le solde sera affiché à la prochaine connexion.";
    }
    return "⚠️ Erreur réseau : " + w.raw;
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
      <svg width="36" height="36" viewBox="0 0 72 72" fill="none">
        <path d="M36 8 A28 28 0 1 1 8 36" stroke="var(--accent)" stroke-width="5" stroke-linecap="round" fill="none"/>
        <polyline points="6,24 8,36 20,34" stroke="var(--accent)" stroke-width="5" stroke-linecap="round" stroke-linejoin="round" fill="none"/>
        <circle cx="24" cy="36" r="5" fill="var(--accent)" opacity="0.9"/>
        <circle cx="48" cy="36" r="5" fill="var(--accent)" opacity="0.5"/>
        <line x1="31" y1="36" x2="41" y2="36" stroke="var(--accent)" stroke-width="2.5" stroke-linecap="round"/>
        <polyline points="38,32 42,36 38,40" stroke="var(--accent)" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round" fill="none"/>
      </svg>
      <div>
        <div class="app-name">Crypto AutoPay</div>
        <div class="lock-hint">{$t.unlockHint || "Entrez votre mot de passe pour déverrouiller"}</div>
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
      {$t.masterPassword || "Mot de passe"}
      <div class="pw-row">
        <!-- svelte-ignore a11y-autofocus -->
        <input
          type="password"
          bind:value={password}
          on:keydown={onKey}
          placeholder="••••••••••••"
          autocomplete="current-password"
          autofocus
        />
        {#if import.meta.env.DEV}
          <!-- Dev bypass: skip password check but still load wallet info -->
          <button
            class="dev-bypass"
            type="button"
            title="[DEV] bypass (loads wallet info)"
            on:click={async () => {
              logInfo("Unlock", "[DEV] Bypass activated — loading wallet info");
              try {
                const { invoke: inv } = await import("@tauri-apps/api/core");
                const info = await inv("get_wallet_info");
                logInfo("Unlock", `[DEV] Wallet info: rpc_ok=${info?.rpc_connected}, balance=${info?.balance_xmr}`);
                dispatch("unlocked", { address: info?.address ?? "", balance: info?.balance_xmr ?? 0 });
              } catch (e) {
                logWarn("Unlock", `[DEV] get_wallet_info unavailable: ${e}`);
                dispatch("unlocked", { address: "", balance: 0 });
              }
            }}
          >▶</button>
        {/if}
      </div>
    </label>

    {#if error}
      <div class="error-msg">{error}</div>
    {/if}

    {#if rpcWarning}
      <div class="rpc-warning">
        <p>{friendlyRpcError(rpcWarning)}</p>
        {#if rpcWarning.type === "seed_invalid"}
          <button class="btn-link warn-reset" on:click={() => dispatch("reset")}>
            → Réinitialiser mon wallet
          </button>
        {/if}
        <button class="btn-link warn-close" on:click={() => dispatch("unlocked", { address: "", balance: 0 })}>
          → Continuer quand même
        </button>
      </div>
    {:else}
      <button
        class="btn-primary btn-full"
        on:click={unlock}
        disabled={!password || loading}
      >
        {#if loading}
          <span class="spinner"></span> {loadingMsg}
        {:else}
          {$t.unlock || "Déverrouiller"} →
        {/if}
      </button>

      <button class="btn-link" on:click={() => dispatch("reset")}>
        {$t.resetWallet || "Réinitialiser le wallet (seed perdu)"}
      </button>
    {/if}
  </div>
</div>

<style>
  /* Password row with optional dev bypass */
  .pw-row { display: flex; align-items: center; gap: 6px; }
  .pw-row input { flex: 1; }
  .dev-bypass {
    width: 22px; height: 22px; min-width: 22px;
    font-size: 9px; padding: 0;
    background: #f59e0b22; border: 1px solid #f59e0b66;
    color: #f59e0b; border-radius: 4px; cursor: pointer;
    flex-shrink: 0; line-height: 1;
  }
  .dev-bypass:hover { background: #f59e0b44; }

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
  .app-name { font-size: 16px; font-weight: 700; }
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
