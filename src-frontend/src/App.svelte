<script>
  import { onMount } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import Welcome from "./lib/Welcome.svelte";
  import Onboarding from "./lib/Onboarding.svelte";
  import Unlock from "./lib/Unlock.svelte";
  import Dashboard from "./lib/Dashboard.svelte";
  import { theme } from "./lib/theme.js";
  import { selectedNetwork } from "./lib/network.js";

  // States: "checking" | "welcome" | "onboarding" | "restore" | "unlock" | "dashboard"
  let appState = "checking";
  let pendingDeepLinkCode = null; // code from a deep link received before unlock

  /** Poll Rust state for any deep link that arrived before the frontend was ready */
  async function checkPendingDeepLink() {
    try {
      const code = await invoke("get_pending_deep_link");
      if (code) {
        console.log("[DeepLink] Pending from Rust state:", code);
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
      listen("autopay://open-subscription", (event) => {
        const url = event.payload;
        console.log("[DeepLink] Event received, appState=", appState, url);
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
      on:unlocked={async () => { appState = "dashboard"; await checkPendingDeepLink(); }}
      on:reset={onReset}
    />

  {:else}
    <Dashboard externalCode={pendingDeepLinkCode} />
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
</style>
