<script>
  import { onMount, tick } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import Welcome from "./lib/Welcome.svelte";
  import Onboarding from "./lib/Onboarding.svelte";
  import Unlock from "./lib/Unlock.svelte";
  import Dashboard from "./lib/Dashboard.svelte";
  import { theme } from "./lib/theme.js";
  import { selectedNetwork } from "./lib/network.js";
  import { subscribeToLogs, logInfo, logWarn, logError } from "./lib/AppLogger.js";

  // States: "checking" | "welcome" | "onboarding" | "restore" | "unlock" | "dashboard"
  let appState = "checking";
  let pendingDeepLinkCode = null; // code from a deep link received before unlock

  // Logs drawer
  let showLogs = false;
  let liveLogs = [];
  let logFilter = "all"; // "all" | "warn" | "error"
  // Newest first (reverse chronological order)
  $: filteredLogs = (logFilter === "all" ? liveLogs
    : liveLogs.filter(l => l.level === logFilter || (logFilter === "warn" && l.level === "error"))
  ).slice().reverse();

  onMount(async () => {
    const unsub = subscribeToLogs(entries => { liveLogs = [...entries]; });

    // Forward Rust log events (tauri-plugin-log webview target) into AppLogger
    try {
      const { attachLogger } = await import("@tauri-apps/plugin-log");
      await attachLogger(({ level, message, location }) => {
        const mod = location?.file?.replace(/.*\//, "").replace(".rs", "") ?? "rust";
        if (level >= 4) logError(mod, message);      // ERROR = 4
        else if (level >= 3) logWarn(mod, message);  // WARN  = 3
        else logInfo(mod, message);                  // INFO  = 2, DEBUG = 1
      });
    } catch (e) {
      // In browser dev mode tauri-plugin-log is not available — ignore
    }

    return unsub;
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
      listen("autopay://open-subscription", async (event) => {
        const url = event.payload;
        console.log("[DeepLink] Event received, appState=", appState, url);
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
      document.addEventListener("keydown", (e) => {
        if (e.ctrlKey && e.shiftKey && e.key === "D") { appState = "dashboard"; e.preventDefault(); }
      });
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
    if (confirm("Effacer le wallet local ? Cette action est irréversible si vous n'avez pas votre seed.")) {
      invoke("reset_wallet").catch(() => {});
      appState = "welcome";
    }
  }
</script>

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
        if (address) console.log("[Unlock] Wallet address:", address.slice(0, 12) + "…");
        appState = "dashboard";
        await checkPendingDeepLink();
      }}
      on:reset={onReset}
    />

  {:else}
    <Dashboard externalCode={pendingDeepLinkCode} />
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
      {#if liveLogs.filter(l => l.level === "error").length > 0}
        <span class="logs-badge">{liveLogs.filter(l => l.level === "error").length}</span>
      {/if}
    </button>
  {/if}

  <!-- Logs drawer -->
  {#if showLogs}
    <div class="logs-overlay" role="button" tabindex="-1" aria-label="Fermer les logs" on:click={() => showLogs = false} on:keydown={e => e.key === 'Escape' && (showLogs = false)}></div>
    <div class="logs-drawer">
      <div class="logs-header">
        <span>📋 Logs <span style="font-size:10px;color:#aaa;font-weight:400">↑ plus récent en haut</span></span>
        <div class="logs-filters">
          <button class:active={logFilter==="all"} on:click={() => logFilter="all"}>Tous</button>
          <button class:active={logFilter==="warn"} on:click={() => logFilter="warn"}>⚠️ Warnings</button>
          <button class:active={logFilter==="error"} on:click={() => logFilter="error"}>🔴 Erreurs</button>
        </div>
        <button class="logs-close" on:click={() => showLogs = false}>✕</button>
      </div>
      <div class="logs-body">
        {#if filteredLogs.length === 0}
          <div class="logs-empty">Aucun log{logFilter !== "all" ? " de ce type" : ""}.</div>
        {:else}
          {#each filteredLogs as log}
            <div class="log-line log-{log.level}">
              <span class="log-time">{log.t ? log.t.slice(11,19) : "??:??:??"}</span>
              <span class="log-mod">[{log.module}]</span>
              <span class="log-msg">{log.msg}</span>
            </div>
          {/each}
        {/if}
      </div>
      <div class="logs-footer">
        <button on:click={() => {
          const txt = filteredLogs.map(l => `${l.t ? l.t.slice(11,19) : "??"} [${l.level}] [${l.module}] ${l.msg}`).join('\n');
          navigator.clipboard.writeText(txt).catch(() => {});
        }}>📋 Copier</button>
        <span class="logs-hint">Ces logs ne contiennent ni montant ni adresse.</span>
      </div>
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
  .log-line {
    display: flex; gap: 6px; line-height: 1.4; padding: 1px 0;
    border-bottom: 1px solid var(--border)08;
  }
  .log-time { color: var(--text-muted); flex-shrink: 0; font-size: 10px; padding-top: 1px; }
  .log-mod { color: var(--accent); flex-shrink: 0; }
  .log-msg { color: var(--text); word-break: break-all; }
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
