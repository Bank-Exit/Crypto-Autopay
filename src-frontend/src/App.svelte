<script>
  import { onMount, onDestroy, tick } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import Welcome from "./lib/Welcome.svelte";
  import Onboarding from "./lib/Onboarding.svelte";
  import Unlock from "./lib/Unlock.svelte";
  import Dashboard from "./lib/Dashboard.svelte";
  import { theme } from "./lib/theme.js";
  import { selectedNetwork } from "./lib/network.js";
  import { subscribeToLogs, logDebug, logInfo, logWarn, logError, setLogRetentionHours, seedHistory, formatLogTime, clearLogs, endBootPhase } from "./lib/AppLogger.js";
  import { t } from "./lib/i18n.js";

  // States: "checking" | "welcome" | "onboarding" | "restore" | "unlock" | "dashboard"
  let appState = "checking";
  let pendingDeepLinkCode = null; // code from a deep link received before unlock

  // ===== Auto-lock after 3 min of inactivity =====
  // UI-only: the backend session keeps syncing and paying while the screen is locked.
  const AUTO_LOCK_MS = 3 * 60 * 1000; // 3 minutes
  let autoLockTimer = null;

  function resetAutoLock() {
    if (appState !== "dashboard") return;
    if (autoLockTimer) clearTimeout(autoLockTimer);
    autoLockTimer = setTimeout(() => {
      if (appState !== "dashboard") return;
      appState = "unlock";
      logInfo("Security", "Auto-lock: screen locked after 3 min of inactivity (background sync continues)");
    }, AUTO_LOCK_MS);
  }

  $: if (appState === "dashboard") resetAutoLock();

  // Logs drawer. Technical logs are only shown in an unlocked session; locked screens
  // only get the startup journal (no amount, no address, deleted at unlock).
  let showLogs = false;
  $: logsUnlocked = appState === "dashboard";
  let startupLog = "";
  $: if (showLogs && !logsUnlocked) loadStartupLog();
  async function loadStartupLog() {
    try { startupLog = await invoke("get_startup_log"); } catch { startupLog = ""; }
  }

  let historySeeded = false;
  async function onSessionUnlocked() {
    endBootPhase();
    if (historySeeded) return;
    historySeeded = true;
    // Earlier lines (previous sessions, night runs) come back from the Rust log file.
    try { seedHistory(await invoke("get_log_history")); } catch { /* browser dev mode */ }
  }
  let liveLogs = [];
  let logFilter = "all"; // "all" | "warn" | "error"
  let showDebugLogs = localStorage.getItem("_autopay_logs_debug") === "1";
  $: localStorage.setItem("_autopay_logs_debug", showDebugLogs ? "1" : "0");
  // Newest first (reverse chronological order)
  $: filteredLogs = (logFilter === "all" ? liveLogs.filter(l => showDebugLogs || l.level !== "debug")
    : liveLogs.filter(l => l.level === logFilter || (logFilter === "warn" && l.level === "error"))
  ).slice().reverse();

  // onMount is async, so its returned cleanup would be ignored: detach listeners here,
  // otherwise every hot reload in dev stacks one more Rust log listener.
  let unsubLogs = null;
  let unlistenDeepLink = null;
  let devKeyHandler = null;
  onDestroy(() => {
    unsubLogs?.(); unlistenDeepLink?.();
    if (window.__autopayRustLogSink === forwardRustLog) window.__autopayRustLogSink = null;
    if (devKeyHandler) document.removeEventListener("keydown", devKeyHandler);
  });

  function forwardRustLog({ level, message, location }) {
    const mod = location?.file?.replace(/.*\//, "").replace(".rs", "") ?? "rust";
    if (level >= 5) logError(mod, message);      // ERROR = 5
    else if (level === 4) logWarn(mod, message); // WARN  = 4
    else if (level === 3) logInfo(mod, message); // INFO  = 3
    else logDebug(mod, message);                 // DEBUG = 2, TRACE = 1
  }

  onMount(async () => {
    unsubLogs = subscribeToLogs(entries => { liveLogs = [...entries]; });

    // Forward Rust log events (tauri-plugin-log webview target) into AppLogger.
    // A single listener per page, attached synchronously-guarded: hot-reloaded App instances
    // only swap the sink, so a slow attach can never leave a second listener behind.
    window.__autopayRustLogSink = forwardRustLog;
    if (!window.__autopayRustLogAttached) {
      window.__autopayRustLogAttached = true;
      import("@tauri-apps/plugin-log")
        .then(({ attachLogger }) => attachLogger((entry) => window.__autopayRustLogSink?.(entry)))
        .catch(() => { window.__autopayRustLogAttached = false; }); // browser dev mode: no backend
    }
    try {
      const r = await invoke("get_log_retention");
      setLogRetentionHours(r?.hours ?? 12);
    } catch (e) {
      // Browser dev mode: no backend
    }
  });

  /** Poll Rust state for any deep link that arrived before the frontend was ready */
  async function checkPendingDeepLink() {
    try {
      const code = await invoke("get_pending_deep_link");
      if (code) {
        console.log("[DeepLink] Pending from Rust state — resetting reactive first");
        pendingDeepLinkCode = null;
        await tick();
        pendingDeepLinkCode = code;
      }
    } catch(e) {
      console.warn("[DeepLink] get_pending_deep_link unavailable:", e);
    }
  }

  onMount(async () => {
    // Deep link real-time listener (Tauri events)
    try {
      const { listen } = await import("@tauri-apps/api/event");
      unlistenDeepLink = await listen("autopay://open-subscription", async (event) => {
        const url = event.payload;
        console.log("[DeepLink] Event received, appState=", appState);
        // Reset to null first so Svelte triggers reactivity even for same URL
        pendingDeepLinkCode = null;
        await tick();
        pendingDeepLinkCode = url;
        if (appState === "welcome") appState = "unlock";
      });
    } catch {}

    document.documentElement.setAttribute("data-theme", $theme);
    document.documentElement.setAttribute("data-network", $selectedNetwork);

    // Dev shortcut: Ctrl+Shift+D → jump to dashboard (browser-only, no Tauri)
    if (import.meta.env.DEV) {
      devKeyHandler = (e) => {
        if (e.ctrlKey && e.shiftKey && e.key === "D") { appState = "dashboard"; e.preventDefault(); }
      };
      document.addEventListener("keydown", devKeyHandler);
    }

    try {
      const exists = await invoke("check_wallet_exists");
      appState = exists ? "unlock" : "welcome";
    } catch (e) {
      // Tauri not available (browser dev preview) → go to welcome
      console.warn("check_wallet_exists unavailable:", e);
      appState = "welcome";
    }
  });

  // Keep data-network in sync with the store
  selectedNetwork.subscribe(v => {
    document.documentElement.setAttribute("data-network", v);
  });

  function onReset() {
    // TODO: show confirmation dialog before deleting vault
    if (confirm($t.resetConfirm)) {
      invoke("reset_wallet").catch(() => {});
      appState = "welcome";
    }
  }
</script>

<!-- Reset auto-lock on any user activity -->
<svelte:window
  on:mousemove={resetAutoLock}
  on:keydown={resetAutoLock}
  on:click={resetAutoLock}
  on:scroll={resetAutoLock}
/>

<div class="app-root">
  {#if appState === "checking"}
    <!-- Minimal splash while checking vault existence -->
    <div class="splash">
      <svg width="32" height="32" viewBox="0 0 72 72" fill="none">
        <path d="M36 8 A28 28 0 1 1 8 36" stroke="var(--accent)" stroke-width="5" stroke-linecap="round" fill="none"/>
        <polyline points="6,24 8,36 20,34" stroke="var(--accent)" stroke-width="5" stroke-linecap="round" stroke-linejoin="round" fill="none"/>
        <circle cx="24" cy="36" r="5" fill="var(--accent)" opacity="0.9"/>
        <circle cx="48" cy="36" r="5" fill="var(--accent)" opacity="0.5"/>
        <line x1="31" y1="36" x2="41" y2="36" stroke="var(--accent)" stroke-width="2.5" stroke-linecap="round"/>
        <polyline points="38,32 42,36 38,40" stroke="var(--accent)" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round" fill="none"/>
      </svg>
    </div>

  {:else if appState === "welcome"}
    <Welcome
      on:start={() => { appState = "onboarding"; }}
      on:restore={() => { appState = "restore"; }}
      on:skip={() => { appState = "dashboard"; }}
    />

  {:else if appState === "onboarding"}
    <Onboarding
      mode="new"
      on:complete={() => { appState = "unlock"; }}
      on:back={() => { appState = "welcome"; }}
    />

  {:else if appState === "restore"}
    <Onboarding
      mode="restore"
      on:complete={() => { appState = "unlock"; }}
      on:back={() => { appState = "welcome"; }}
    />

  {:else if appState === "unlock"}
    <Unlock
      on:unlocked={async (e) => {
        const { address, balance, rpcError } = e.detail || {};
        if (rpcError) console.warn("[Unlock] RPC unavailable (offline mode):", rpcError);
        appState = "dashboard";
        onSessionUnlocked();
        await checkPendingDeepLink();
      }}
      on:reset={onReset}
    />

  {:else}
    <Dashboard externalCode={pendingDeepLinkCode} on:lock={() => { appState = "unlock"; }} />
  {/if}

  <!-- Floating Logs button (always visible, bottom-right) -->
  {#if appState !== "checking"}
    <button class="logs-fab" on:click={() => showLogs = !showLogs} title="Logs techniques">
      <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
        <path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z"/>
        <polyline points="14 2 14 8 20 8"/>
        <line x1="16" y1="13" x2="8" y2="13"/>
        <line x1="16" y1="17" x2="8" y2="17"/>
        <polyline points="10 9 9 9 8 9"/>
      </svg>
      Logs
      {#if logsUnlocked && liveLogs.filter(l => l.level === "error").length > 0}
        <span class="logs-badge">{liveLogs.filter(l => l.level === "error").length}</span>
      {/if}
    </button>
  {/if}

  <!-- Logs drawer -->
  {#if showLogs}
    <div class="logs-overlay" role="button" tabindex="-1" aria-label={$t.logsDrawerClose} on:click={() => showLogs = false} on:keydown={e => e.key === 'Escape' && (showLogs = false)}></div>
    <div class="logs-drawer">
      {#if !logsUnlocked}
      <div class="logs-header">
        <span>📋 {$t.logsStartupTitle}</span>
        <button class="logs-close" style="margin-left:auto" on:click={() => showLogs = false}>✕</button>
      </div>
      <pre class="logs-body logs-startup">{startupLog || $t.logsStartupEmpty}</pre>
      <div class="logs-footer">
        <button disabled={!startupLog} on:click={() => navigator.clipboard.writeText(startupLog).catch(() => {})}>📋 Copier</button>
        <span class="logs-hint">{$t.logsLockedNote}</span>
      </div>
      {:else}
      <div class="logs-header">
        <span>📋 Logs <span style="font-size:10px;color:#aaa;font-weight:400">{$t.logsDrawerNewest}</span></span>
        <div class="logs-filters">
          <button class:active={logFilter==="all"} on:click={() => logFilter="all"}>{$t.logsDrawerAll}</button>
          <button class:active={showDebugLogs} title={$t.logsDrawerDetailsTip} on:click={() => showDebugLogs = !showDebugLogs}>🔧 {$t.logsDrawerDetails}</button>
          <button class:active={logFilter==="warn"} on:click={() => logFilter="warn"}>⚠️ {$t.logsDrawerWarnings}</button>
          <button class:active={logFilter==="error"} on:click={() => logFilter="error"}>🔴 {$t.logsDrawerErrors}</button>
        </div>
        <button class="logs-close" on:click={() => showLogs = false}>✕</button>
      </div>
      <div class="logs-body">
        {#if filteredLogs.length === 0}
          <div class="logs-empty">Aucun log{logFilter !== "all" ? " de ce type" : ""}.</div>
        {:else}
          {#each filteredLogs as log (log.id)}
            <div class="log-line log-{log.level}">
              <span class="log-time">{formatLogTime(log.t)}</span>
              <span class="log-mod">[{log.module}]</span>
              <span class="log-msg">{log.msg}</span>
            </div>
          {/each}
        {/if}
      </div>
      <div class="logs-footer">
        <button on:click={() => {
          const txt = filteredLogs.map(l => `${formatLogTime(l.t)} [${l.level}] [${l.module}] ${l.msg}`).join('\n');
          navigator.clipboard.writeText(txt).catch(() => {});
        }}>📋 Copier</button>
        <button title="Vide l'affichage ; le fichier de logs reste conservé selon la durée choisie" on:click={clearLogs}>🗑 {$t.commonClear}</button>
        <span class="logs-hint">Ces logs ne contiennent ni montant ni adresse.</span>
      </div>
      {/if}
    </div>
  {/if}
</div>

<style>
  .app-root {
    width: 100vw;
    min-height: 100vh;
    overflow: auto;
  }

  .splash {
    width: 100vw; height: 100vh;
    display: flex; align-items: center; justify-content: center;
    background: var(--bg);
    animation: fade-in 0.3s ease;
  }

  @keyframes fade-in { from { opacity: 0; } to { opacity: 1; } }

  /* Floating Logs button */
  .logs-fab {
    position: fixed; bottom: 16px; right: 16px; z-index: 900;
    display: flex; align-items: center; gap: 6px;
    background: var(--bg-card); border: 1px solid var(--border);
    border-radius: 20px; padding: 7px 13px;
    font-size: 12px; color: var(--text-muted);
    cursor: pointer; box-shadow: 0 2px 8px #0002;
    transition: background 0.15s, color 0.15s;
  }
  .logs-fab:hover { background: var(--bg-hover); color: var(--text); }
  .logs-badge {
    background: #ef4444; color: white; border-radius: 9px;
    font-size: 10px; font-weight: 700; padding: 1px 5px; min-width: 16px; text-align: center;
  }

  /* Logs drawer */
  .logs-overlay {
    position: fixed; inset: 0; background: #0004; z-index: 901;
  }
  .logs-drawer {
    position: fixed; bottom: 0; right: 0; width: min(520px, 96vw);
    height: min(420px, 70vh);
    background: var(--bg-card); border: 1px solid var(--border);
    border-radius: 12px 12px 0 0; z-index: 902;
    display: flex; flex-direction: column;
    box-shadow: 0 -4px 24px #0003;
  }
  .logs-header {
    display: flex; align-items: center; gap: 8px; padding: 10px 14px;
    border-bottom: 1px solid var(--border); flex-shrink: 0;
    font-size: 13px; font-weight: 600;
  }
  .logs-filters {
    display: flex; gap: 4px; margin-left: auto;
  }
  .logs-filters button {
    border: 1px solid var(--border); background: var(--bg);
    border-radius: 6px; padding: 2px 8px; font-size: 11px;
    cursor: pointer; color: var(--text-muted);
  }
  .logs-filters button.active {
    background: var(--accent); color: white; border-color: var(--accent);
  }
  .logs-close {
    border: none; background: none; cursor: pointer; color: var(--text-muted);
    font-size: 14px; padding: 2px 6px; border-radius: 4px;
  }
  .logs-close:hover { background: var(--bg-hover); }
  .logs-body {
    flex: 1; overflow-y: auto; padding: 8px 12px;
    font-family: monospace; font-size: 11px;
    display: flex; flex-direction: column; gap: 2px;
  }
  .logs-empty { color: var(--text-muted); padding: 20px 0; text-align: center; }
  .logs-startup { margin: 0; display: block; white-space: pre-wrap; word-break: break-all; color: var(--text); }
  .log-line {
    display: flex; gap: 6px; line-height: 1.4; padding: 1px 0;
    border-bottom: 1px solid var(--border)08;
  }
  .log-time { color: var(--text-muted); flex-shrink: 0; font-size: 10px; padding-top: 1px; }
  .log-mod { color: var(--accent); flex-shrink: 0; }
  .log-msg { color: var(--text); word-break: break-all; }
  .log-debug .log-msg { color: var(--text-muted); }
  .log-warn .log-msg { color: #d97706; }
  .log-error .log-msg { color: #ef4444; }
  .logs-footer {
    padding: 8px 14px; border-top: 1px solid var(--border);
    display: flex; align-items: center; gap: 12px; flex-shrink: 0;
  }
  .logs-footer button {
    border: 1px solid var(--border); background: var(--bg);
    border-radius: 6px; padding: 4px 10px; font-size: 11px; cursor: pointer;
  }
  .logs-hint { font-size: 10px; color: var(--text-muted); margin-left: auto; }
</style>
