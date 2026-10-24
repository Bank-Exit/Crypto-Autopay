<script>
  import { t, currentLang, LANGUAGES } from "./i18n.js";
  import { theme } from "./theme.js";
  import { selectedNetwork, currentNetwork, NETWORKS } from "./network.js";
  import { paymentMode } from "./paymentMode.js";
  import SubscriptionList from "./SubscriptionList.svelte";
  import PaymentAnimation from "./PaymentAnimation.svelte";
  import SubscriptionCodeEditor from "./SubscriptionCodeEditor.svelte";
  import { scheduleToHuman } from "./autopay-encode.js";

  // External code pre-fill (from deep link)
  export let externalCode = null;

  // ---- Subscriptions list (mock data + live additions) ----
  let subscriptions = [
    {
      id: 1, label: "Le Média Libre",
      description: "Soutien mensuel au journalisme indépendant",
      amount: "0.05", currency: "XMR", schedule: "Mensuel",
      next_payment_at: "2026-11-01", status: "Active", canary_status: "Ok",
      payments_sent: 8, wallet_id: 1,
    },
    {
      id: 2, label: "Libre-Market épicerie",
      description: "Abonnement légumes de saison",
      amount: "0.12", currency: "XMR", schedule: "Bimensuel",
      next_payment_at: "2026-10-28", status: "Active", canary_status: "Ok",
      payments_sent: 3, wallet_id: 2,
    },
    {
      id: 3, label: "Radio Pirate FR",
      description: "Podcast libertaire hebdomadaire",
      amount: "0.02", currency: "XMR", schedule: "Mensuel",
      next_payment_at: null, status: "Suspended", canary_status: "Expired",
      payments_sent: 12, wallet_id: 1,
    },
  ];

  /** Convert a payload from SubscriptionCodeEditor into a subscription row */
  function payloadToSubscription(payload) {
    return {
      id: Date.now(),
      label: payload.custom_label || "—",
      description: payload.description || "",
      amount: String(payload.amount),
      currency: payload.currency || "XMR",
      schedule: scheduleToHuman(payload.schedule, payload.repeat_every || 1, $currentLang),
      next_payment_at: payload.start_date || nextOccurrence(payload.schedule),
      status: "Active",
      canary_status: payload.canary_url ? "Ok" : "NotConfigured",
      payments_sent: 0,
      wallet_id: wallets[0]?.id || 1,
      _code: payload._sourceCode || null, // keep for future backend sync
    };
  }

  /** Best-effort: compute next occurrence date from a 6-field cron preset */
  function nextOccurrence(schedule) {
    const now = new Date();
    const s = (schedule || "").trim();
    // Monthly (dom=1): next 1st of month
    if (s === "0 0 9 1 * *" || s === "0 9 1 * *") {
      const d = new Date(now.getFullYear(), now.getMonth() + 1, 1);
      return d.toISOString().split("T")[0];
    }
    // Weekly (Monday): next Monday
    if (s === "0 0 9 * * 1" || s === "0 9 * * 1") {
      const d = new Date(now);
      d.setDate(now.getDate() + ((8 - now.getDay()) % 7 || 7));
      return d.toISOString().split("T")[0];
    }
    // Daily
    if (s === "0 0 9 * * *" || s === "0 9 * * *") {
      const d = new Date(now);
      d.setDate(now.getDate() + 1);
      return d.toISOString().split("T")[0];
    }
    // Quarterly
    if (s.includes("*/3")) {
      const nextMonth = Math.ceil((now.getMonth() + 1) / 3) * 3;
      return new Date(now.getFullYear(), nextMonth, 1).toISOString().split("T")[0];
    }
    // Yearly
    if (s === "0 0 9 1 1 *" || s === "0 9 1 1 *") {
      return new Date(now.getFullYear() + 1, 0, 1).toISOString().split("T")[0];
    }
    return null;
  }

  // ---- New-subscription modal ----
  let showNewSubModal = false;
  let modalInitialCode = null; // pre-filled code from deep link

  // React to external code (deep link)
  $: if (externalCode) {
    modalInitialCode = externalCode;
    showNewSubModal = true;
  }

  function onSubGenerated({ detail }) {
    const sub = payloadToSubscription({ ...detail.payload, _sourceCode: detail.code });
    subscriptions = [...subscriptions, sub];
    showNewSubModal = false;
    page = "subscriptions"; // ensure we're on the right tab
  }

  function onSubImported({ detail }) {
    const sub = payloadToSubscription({ ...detail.payload, _sourceCode: detail.code });
    subscriptions = [...subscriptions, sub];
    showNewSubModal = false;
    page = "subscriptions";
  }

  let trayStatus = "green";

  // Mock wallets with monthly spend estimates
  let wallets = [
    { id: 1, name: "Wallet principal",   desc: "Médias & créateurs",       balance: "0.82", color: "#f26822", monthlyOut: 0.07 },
    { id: 2, name: "Wallet services",    desc: "VPN et hébergement",       balance: "1.21", color: "#2e7d32", monthlyOut: 0.04 },
  ];

  $: totalBalance = wallets.reduce((s, w) => s + parseFloat(w.balance), 0).toFixed(3);
  $: totalMonthlyOut = wallets.reduce((s, w) => s + (w.monthlyOut || 0), 0);

  let nextPayment = { label: "Le Média Libre", amount: "0.05", in_days: 8 };
  let page = "subscriptions";

  let editingWallet = null;
  function startEditWallet(w) { editingWallet = { ...w }; }
  function saveWallet() {
    wallets = wallets.map(w => w.id === editingWallet.id ? { ...editingWallet } : w);
    editingWallet = null;
  }

  // Notifications settings (mock)
  let notifSettings = {
    canary: true, lowBalance: true, paymentSent: true, paymentFailed: true,
    beforePayment: true, beforePaymentDays: 2,
  };

  // UTC offset helper
  function getTimezoneDisplay() {
    const tz = Intl.DateTimeFormat().resolvedOptions().timeZone;
    const offset = -new Date().getTimezoneOffset();
    const sign = offset >= 0 ? "+" : "-";
    const h = String(Math.floor(Math.abs(offset)/60)).padStart(2,"0");
    const m = String(Math.abs(offset)%60).padStart(2,"0");
    return `${tz} (UTC${sign}${h}:${m})`;
  }

  // Refill estimate: total balance / total monthly spend → human readable
  function refillEstimate(balanceStr, monthlyOut, t) {
    const bal = parseFloat(balanceStr);
    if (!monthlyOut || monthlyOut <= 0) return "—";
    const months = bal / monthlyOut;
    if (months >= 18) return `~${Math.round(months/12)} ${t.refillYears}`;
    if (months >= 4)  return `~${Math.round(months)} ${t.refillMonths}`;
    const weeks = months * 4.33;
    if (weeks >= 2)   return `~${Math.round(weeks)} ${t.refillWeeks}`;
    const days = months * 30;
    if (days >= 1)    return `~${Math.round(days)} ${t.refillDays}`;
    return `< 1 ${t.refillDays}`;
  }

  // Total refill estimate
  $: totalRefillText = (tStr) => refillEstimate(totalBalance, totalMonthlyOut, tStr);

  // Animation subjects (network-aware via PaymentAnimation internals)
  const animSubjects = [
    { toLabel: "Le Média Libre", xmrAmount: "0.05", btcAmount: "420 sats" },
    { toLabel: "VPN",            xmrAmount: "0.012", btcAmount: "120 sats" },
  ];
</script>

<div class="layout">
  <!-- Sidebar -->
  <aside class="sidebar">
    <div class="sidebar-header">
      <div class="sidebar-logo">
        <svg width="24" height="24" viewBox="0 0 72 72" fill="none">
          <path d="M36 8 A28 28 0 1 1 8 36" stroke="var(--accent)" stroke-width="5" stroke-linecap="round" fill="none"/>
          <polyline points="6,24 8,36 20,34" stroke="var(--accent)" stroke-width="5" stroke-linecap="round" stroke-linejoin="round" fill="none"/>
          <circle cx="24" cy="36" r="5" fill="var(--accent)" opacity="0.9"/>
          <circle cx="48" cy="36" r="5" fill="var(--accent)" opacity="0.5"/>
          <line x1="31" y1="36" x2="41" y2="36" stroke="var(--accent)" stroke-width="2.5" stroke-linecap="round"/>
          <polyline points="38,32 42,36 38,40" stroke="var(--accent)" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round" fill="none"/>
        </svg>
        <div>
          <div class="sidebar-appname">Monero AutoPay</div>
          <div class="tray-row">
            <span class="status-dot {trayStatus}"></span>
            <span class="tray-label">OK</span>
          </div>
          <div class="tray-row mode-tray">
            <span class="mode-tray-dot" class:semi={$paymentMode === 'semi-auto'}></span>
            <span class="tray-label">{$paymentMode === 'semi-auto' ? 'Semi-auto' : 'Auto'}</span>
          </div>
        </div>
      </div>
    </div>

    <nav class="sidebar-nav">
      <!-- No emojis except Trouver des souscriptions -->
      <button class="nav-item" class:active={page === "subscriptions"} on:click={() => page = "subscriptions"}>
        {$t.navSubscriptions}
      </button>
      <button class="nav-item" class:active={page === "wallets"} on:click={() => page = "wallets"}>
        {$t.navWallets}
      </button>
      <button class="nav-item" class:active={page === "history"} on:click={() => page = "history"}>
        {$t.navPaymentHistory}
      </button>
      <button class="nav-item" class:active={page === "archived"} on:click={() => page = "archived"}>
        {$t.navArchived}
      </button>
      <button class="nav-item find-nav" class:active={page === "find"} on:click={() => page = "find"}>
        {$t.findSubscriptions}
      </button>

      <div class="nav-divider"></div>

      <button class="nav-item" class:active={page === "howto"} on:click={() => page = "howto"}>
        {$t.navHowItWorks}
      </button>
      <button class="nav-item" class:active={page === "settings"} on:click={() => page = "settings"}>
        {$t.navSettings}
      </button>
    </nav>

    <!-- Total balance (all wallets) -->
    <div class="sidebar-balance card">
      <div class="balance-label">{$t.balance}</div>
      <div class="balance-amount text-accent">{totalBalance} {$currentNetwork.symbol}</div>
      <div class="balance-wallets">
        {#each wallets as w}
          <div class="balance-sub">
            <span class="w-dot" style="background:{w.color}"></span>
            <span class="w-name">{w.name}</span>
            <span class="w-val">{w.balance}</span>
          </div>
        {/each}
      </div>
    </div>
  </aside>

  <!-- Main content -->
  <main class="content">

    <!-- ====== SUBSCRIPTIONS ====== -->
    {#if page === "subscriptions"}
      <div class="page-header">
        <h1>{$currentNetwork.label} AutoPay</h1>
        <div class="page-actions">
          <button class="btn-ghost" on:click={() => page = "find"}>{$t.findSubscriptions}</button>
          <button class="btn-ghost">📥 {$t.importSubscription}</button>
          <button class="btn-primary" on:click={() => showNewSubModal = true}>＋ {$t.newSubscription}</button>
        </div>
      </div>

      <div class="next-banner card">
        <span class="text-muted">{$t.nextPayment}</span>
        <strong>{nextPayment.label}</strong>
        <span>{nextPayment.amount} {$currentNetwork.symbol}</span>
        <span class="text-muted">{$t.inPrefix} {nextPayment.in_days} {$t.inDaysSuffix}</span>
      </div>

      <!-- Refill estimate banner -->
      <div class="refill-banner card">
        <span class="text-muted">{$t.nextRefillLabel} :</span>
        <strong>{$t.refillIn} {totalRefillText($t)}</strong>
        <span class="text-muted refill-hint">({totalMonthlyOut.toFixed(3)} {$currentNetwork.symbol} {$t.walletMonthlyOut})</span>
      </div>

      <SubscriptionList {wallets} {subscriptions} />

    <!-- ====== WALLETS ====== -->
    {:else if page === "wallets"}
      <div class="page-header">
        <h1>{$t.walletsTitle}</h1>
        <div class="page-actions">
          <button class="btn-primary">＋ {$t.walletAdd}</button>
        </div>
      </div>

      {#if editingWallet}
        <!-- Edit wallet inline -->
        <div class="wallet-edit-panel card">
          <h2>Modifier le wallet</h2>
          <label>
            {$t.walletName}
            <input bind:value={editingWallet.name} />
          </label>
          <label>
            {$t.walletDesc}
            <input bind:value={editingWallet.desc} />
          </label>
          <div class="edit-actions">
            <button class="btn-primary" on:click={saveWallet}>Enregistrer</button>
            <button class="btn-ghost" on:click={() => editingWallet = null}>{$t.back}</button>
          </div>
        </div>
      {:else}
        <div class="wallet-list">
          {#each wallets as w}
            <div class="wallet-card card">
              <div class="wallet-color-bar" style="background:{w.color}"></div>
              <div class="wallet-body">
                <div class="wallet-top">
                  <div>
                    <div class="wallet-name">{w.name}</div>
                    {#if w.desc}<div class="wallet-desc text-muted">{w.desc}</div>{/if}
                  </div>
                  <button class="btn-ghost btn-sm" on:click={() => startEditWallet(w)}>{$t.walletEdit}</button>
                </div>
                <div class="wallet-balance">
                  <span class="balance-num text-accent">{w.balance} {$currentNetwork.symbol}</span>
                </div>
              <div class="wallet-limit-warn">
                <div class="limit-bar">
                  <div class="limit-fill" style="width:{Math.min(parseFloat(w.balance)/2*100,100)}%; background:{w.color}"></div>
                </div>
                <div class="wallet-limit-row">
                  <span class="text-muted" style="font-size:10px">{w.balance} / 2 {$currentNetwork.symbol} max</span>
                  {#if w.monthlyOut}
                    <span class="refill-chip" style="color:{w.color}">
                      {$t.walletRefillIn} {refillEstimate(w.balance, w.monthlyOut, $t)}
                    </span>
                  {/if}
                </div>
              </div>
              </div>
            </div>
          {/each}
        </div>
      {/if}

    <!-- ====== FIND ====== -->
    {:else if page === "find"}
      <div class="page-header">
        <h1>{$t.findTitle}</h1>
      </div>
      <div class="find-page">
        <p class="text-muted">{$t.findDesc}</p>
        <div class="find-notice card">
          <strong>{$t.findDirectoryTitle}</strong>
          <p>{$t.findDirectoryDesc}</p>
        </div>
        <div class="find-list">
          {#each ($t.findCategories || []) as cat}
            <div class="find-item card">
              <div class="find-item-header">
                <span class="find-item-name">{cat.icon} {cat.name}</span>
                {#if cat.url}
                  <a class="find-link text-accent" href={cat.url} target="_blank" rel="noopener">{cat.url}</a>
                {/if}
              </div>
              <div class="find-item-desc text-muted">{cat.desc}</div>

              {#if cat.demos && cat.demos.length}
                <div class="demos">
                  {#each cat.demos as demo}
                    <div class="demo-card">
                      <span class="badge badge-grey">{$t.findDemo}</span>
                      <strong>{demo.name}</strong>
                      <span class="text-muted">{demo.period}</span>
                      <span class="text-accent">{demo.amount}</span>
                      <span class="demo-hint text-muted">{$t.findDemoDesc}</span>
                    </div>
                  {/each}
                </div>
              {/if}
            </div>
          {/each}
        </div>
      </div>

    <!-- ====== ARCHIVED ====== -->
    {:else if page === "archived"}
      <div class="page-header">
        <h1>{$t.navArchived}</h1>
      </div>
      <SubscriptionList archived={true} {wallets} {subscriptions} />

    <!-- ====== HISTORY ====== -->
    {:else if page === "history"}
      <div class="page-header">
        <h1>{$t.navPaymentHistory}</h1>
      </div>
      <p class="text-muted placeholder-msg">— {$t.navPaymentHistory} (à venir) —</p>

    <!-- ====== SETTINGS ====== -->
    {:else if page === "settings"}
      <div class="page-header">
        <h1>{$t.settings}</h1>
      </div>
      <div class="settings-section card">
        <div class="setting-row">
          <span class="setting-label">{$t.language}</span>
          <div class="lang-wrapper-inline">
            <svg class="globe-icon-sm" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"
                 stroke-linecap="round" stroke-linejoin="round">
              <circle cx="12" cy="12" r="10"/>
              <line x1="2" y1="12" x2="22" y2="12"/>
              <path d="M12 2a15.3 15.3 0 0 1 4 10 15.3 15.3 0 0 1-4 10 15.3 15.3 0 0 1-4-10 15.3 15.3 0 0 1 4-10z"/>
            </svg>
            <select bind:value={$currentLang} class="setting-select">
              {#each LANGUAGES as lang}
                <option value={lang.code}>{lang.label}</option>
              {/each}
            </select>
          </div>
        </div>

        <div class="setting-row">
          <span class="setting-label">{$t.theme}</span>
          <div class="theme-switcher">
            <button class="btn-ghost btn-sm" class:active-theme={$theme === "light"} on:click={() => theme.set("light")}>
              ☀ {$t.themeLight}
            </button>
            <button class="btn-ghost btn-sm" class:active-theme={$theme === "dark"} on:click={() => theme.set("dark")}>
              ☾ {$t.themeDark}
            </button>
          </div>
        </div>

        <div class="setting-row">
          <span class="setting-label">{$t.timezone}</span>
          <span class="text-muted tz-display">{getTimezoneDisplay()}</span>
        </div>

        <div class="setting-row">
          <span class="setting-label">{$t.changeNetwork}</span>
          <div class="theme-switcher">
            {#each NETWORKS as net}
              <button class="btn-ghost btn-sm" class:active-theme={$selectedNetwork === net.id}
                on:click={() => selectedNetwork.set(net.id)}>
                <span class="net-dot-sm" style="background:{net.color}"></span>
                {net.label}
              </button>
            {/each}
          </div>
        </div>
      </div>

      <!-- Payment mode section -->
      <h2 class="section-title">{$t.paymentModeTitle}</h2>
      <div class="settings-section card">
        <div class="setting-row mode-row">
          <span class="mode-side-label">{$t.paymentModeSemiAuto}</span>
          <label class="toggle toggle-xl">
            <input type="checkbox"
              checked={$paymentMode === "auto"}
              on:change={e => paymentMode.set(e.target.checked ? "auto" : "semi-auto")}
            />
            <span class="slider"></span>
          </label>
          <span class="mode-side-label">{$t.paymentModeAuto}</span>
        </div>
        <p class="mode-desc">
          {$paymentMode === "semi-auto" ? $t.paymentModeSemiAutoDesc : $t.paymentModeAutoDesc}
        </p>
      </div>

      <!-- Notifications section -->
      <h2 class="section-title">{$t.notifTitle}</h2>
      <div class="settings-section card">
        <div class="setting-row">
          <span class="setting-label">{$t.notifCanary}</span>
          <label class="toggle"><input type="checkbox" bind:checked={notifSettings.canary}/><span class="slider"></span></label>
        </div>
        <div class="setting-row">
          <span class="setting-label">{$t.notifLowBalance}</span>
          <label class="toggle"><input type="checkbox" bind:checked={notifSettings.lowBalance}/><span class="slider"></span></label>
        </div>
        <div class="setting-row">
          <span class="setting-label">{$t.notifPaymentSent}</span>
          <label class="toggle"><input type="checkbox" bind:checked={notifSettings.paymentSent}/><span class="slider"></span></label>
        </div>
        <div class="setting-row">
          <span class="setting-label">{$t.notifPaymentFailed}</span>
          <label class="toggle"><input type="checkbox" bind:checked={notifSettings.paymentFailed}/><span class="slider"></span></label>
        </div>
        <div class="setting-row">
          <span class="setting-label">{$t.notifBeforePayment}</span>
          <div style="display:flex;align-items:center;gap:8px">
            <label class="toggle"><input type="checkbox" bind:checked={notifSettings.beforePayment}/><span class="slider"></span></label>
            {#if notifSettings.beforePayment}
              <input type="number" bind:value={notifSettings.beforePaymentDays} min="1" max="14"
                     style="width:48px;padding:4px 6px;font-size:12px" />
              <span class="text-muted" style="font-size:12px">{$t.notifBeforePaymentDays}</span>
            {/if}
          </div>
        </div>
      </div>

    <!-- ====== HOW IT WORKS ====== -->
    {:else if page === "howto"}
      <div class="page-header">
        <h1>{$t.howItWorks}</h1>
      </div>
      <div class="howto-body">
        <div class="howto-anim card">
          <PaymentAnimation fromLabel="Mon wallet" toLabel="Subscription" amount="0.05 XMR / mois"/>
        </div>
        <div class="howto card">
          <div class="howto-step">
            <span class="howto-num">1</span>
            <div><h3>{$t.welcomeStep1Title}</h3><p>{$t.welcomeStep1Desc}</p></div>
          </div>
          <div class="howto-step">
            <span class="howto-num">2</span>
            <div><h3>{$t.welcomeStep2Title}</h3><p>{$t.welcomeStep2Desc}</p></div>
          </div>
          <div class="howto-step">
            <span class="howto-num">3</span>
            <div><h3>{$t.welcomeStep3Title}</h3><p>{$t.welcomeStep3Desc}</p></div>
          </div>
        </div>
      </div>
    {/if}
  </main>
</div>

<!-- New subscription modal -->
{#if showNewSubModal}
  <div class="modal-backdrop" on:click={() => showNewSubModal = false} role="dialog" aria-modal="true">
    <div class="modal-panel" on:click|stopPropagation>
      <div class="modal-header">
        <h2 class="modal-title">{$t.newSubscription}</h2>
        <button class="modal-close" on:click={() => showNewSubModal = false} aria-label="Fermer">✕</button>
      </div>
      <div class="modal-body">
        <SubscriptionCodeEditor
          initialCode={modalInitialCode}
          compact={true}
          lang={$currentLang}
          on:generated={onSubGenerated}
          on:imported={onSubImported}
          on:close={() => showNewSubModal = false}
        />
      </div>
    </div>
  </div>
{/if}

<style>
  .layout {
    display: flex;
    height: 100vh;
    overflow: hidden;
  }

  /* ----- Sidebar ----- */
  .sidebar {
    width: 210px;
    min-width: 210px;
    background: var(--bg-sidebar);
    border-right: 1px solid var(--border);
    display: flex;
    flex-direction: column;
    padding: 14px 8px;
    gap: 6px;
    overflow-y: auto;
  }

  .sidebar-header { padding: 4px 4px 10px; border-bottom: 1px solid var(--border); }

  .sidebar-logo { display: flex; align-items: center; gap: 9px; }
  .sidebar-appname { font-size: 12px; font-weight: 700; color: var(--text); }

  .tray-row { display: flex; align-items: center; gap: 4px; margin-top: 1px; }
  .tray-label { font-size: 10px; color: var(--text-muted); }

  .sidebar-nav {
    flex: 1;
    display: flex;
    flex-direction: column;
    gap: 1px;
    padding-top: 6px;
  }

  .nav-item {
    text-align: left;
    background: transparent;
    border: none;
    border-radius: 6px;
    padding: 8px 10px;
    font-size: 12.5px;
    color: var(--text-muted);
    cursor: pointer;
    transition: background .1s, color .1s;
    font-weight: 500;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .nav-item:hover { background: var(--bg-card-hover); color: var(--text); }
  .nav-item.active { background: var(--accent-light); color: var(--accent); font-weight: 600; }
  .nav-divider { height: 1px; background: var(--border); margin: 5px 0; }

  /* Balance card at bottom */
  .sidebar-balance { padding: 10px; margin-top: auto; }
  .balance-label { font-size: 9px; text-transform: uppercase; letter-spacing: .07em; color: var(--text-muted); margin-bottom: 3px; }
  .balance-amount { font-size: 16px; font-weight: 800; margin-bottom: 8px; }
  .balance-wallets { display: flex; flex-direction: column; gap: 3px; }
  .balance-sub { display: flex; align-items: center; gap: 5px; font-size: 10.5px; }
  .w-dot { width: 7px; height: 7px; border-radius: 50%; flex-shrink: 0; }
  .w-name { flex: 1; color: var(--text-muted); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .w-val { color: var(--text); font-weight: 600; font-variant-numeric: tabular-nums; }

  /* ----- Content ----- */
  .content {
    flex: 1;
    display: flex;
    flex-direction: column;
    overflow-y: auto;
    padding: 22px 24px;
    gap: 14px;
    min-width: 0;
  }
  /* All direct children keep their natural height → parent scrolls */
  .content > * { flex-shrink: 0; }

  .page-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    flex-shrink: 0;
    flex-wrap: wrap;
    gap: 10px;
  }
  .page-header h1 { font-size: 19px; }

  .page-actions {
    display: flex;
    gap: 8px;
    flex-wrap: wrap;
    align-items: center;
  }

  .next-banner, .refill-banner {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 9px 14px;
    font-size: 13px;
    flex-shrink: 0;
    flex-wrap: wrap;
  }
  .refill-banner { font-size: 12px; opacity: 0.85; }
  .refill-hint { font-size: 11px; }

  .section-title { font-size: 13px; font-weight: 600; color: var(--text-muted); margin-top: 6px; text-transform: uppercase; letter-spacing: .05em; }

  .placeholder-msg { padding: 20px; }

  /* ----- Wallets page ----- */
  .wallet-list { display: flex; flex-direction: column; gap: 10px; }

  .wallet-card {
    display: flex;
    overflow: hidden;
  }
  .wallet-color-bar { width: 5px; flex-shrink: 0; }
  .wallet-body { flex: 1; padding: 14px 16px; display: flex; flex-direction: column; gap: 10px; }
  .wallet-top { display: flex; justify-content: space-between; align-items: flex-start; }
  .wallet-name { font-weight: 700; font-size: 14px; }
  .wallet-desc { font-size: 12px; margin-top: 2px; }
  .wallet-balance { font-size: 18px; font-weight: 800; }
  .balance-num {}

  .wallet-limit-warn { display: flex; flex-direction: column; gap: 4px; }
  .limit-bar { height: 4px; background: var(--border); border-radius: 2px; overflow: hidden; }
  .limit-fill { height: 100%; border-radius: 2px; transition: width .3s; }
  .wallet-limit-row { display: flex; justify-content: space-between; align-items: center; gap: 8px; }
  .refill-chip { font-size: 10px; font-weight: 600; }

  .wallet-edit-panel { padding: 20px; display: flex; flex-direction: column; gap: 14px; max-width: 400px; }
  .edit-actions { display: flex; gap: 8px; }

  .btn-sm { padding: 6px 12px; font-size: 12px; }

  /* ----- Find page ----- */
  .find-page { display: flex; flex-direction: column; gap: 14px; }
  .find-notice { padding: 12px 16px; display: flex; flex-direction: column; gap: 4px; }
  .find-list { display: flex; flex-direction: column; gap: 10px; }
  .find-item { padding: 14px 16px; display: flex; flex-direction: column; gap: 6px; }
  .find-item-header { display: flex; justify-content: space-between; align-items: center; flex-wrap: wrap; gap: 6px; }
  .find-item-name { font-weight: 700; font-size: 14px; }
  .find-item-desc { font-size: 12px; }
  .find-link { font-size: 11px; text-decoration: none; }
  .find-link:hover { text-decoration: underline; }

  .demos { display: flex; flex-direction: column; gap: 6px; margin-top: 4px; }
  .demo-card {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 8px 10px;
    background: var(--bg-sidebar);
    border-radius: var(--radius-sm);
    opacity: 0.65;
    font-size: 12px;
    flex-wrap: wrap;
  }
  .demo-card strong { font-size: 12px; }
  .demo-hint { font-size: 10px; font-style: italic; }

  /* ----- Settings ----- */
  .settings-section { padding: 0; overflow: hidden; }
  .setting-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 13px 18px;
    border-bottom: 1px solid var(--border);
    gap: 14px;
    flex-wrap: wrap;
  }
  .setting-row:last-child { border-bottom: none; }
  .setting-label { font-size: 13px; font-weight: 500; color: var(--text); white-space: nowrap; }

  .lang-wrapper-inline {
    display: flex; align-items: center; gap: 6px;
    background: var(--bg); border: 1px solid var(--border);
    border-radius: 6px; padding: 5px 10px;
  }
  .globe-icon-sm { width: 13px; height: 13px; color: var(--text-muted); flex-shrink: 0; }
  .setting-select {
    border: none; background: transparent; color: var(--text);
    font-size: 13px; cursor: pointer; outline: none; width: auto; padding: 0;
    -webkit-appearance: none; appearance: none;
  }
  .setting-select option { background: var(--bg-card); color: var(--text); }

  .theme-switcher { display: flex; gap: 6px; flex-wrap: wrap; }
  .active-theme { border-color: var(--accent) !important; color: var(--accent) !important; background: var(--accent-light) !important; }

  /* Payment mode — big toggle row */
  .mode-row { justify-content: center; gap: 1rem; padding: 1.1rem 1.5rem; }
  .mode-side-label {
    font-size: 0.88rem;
    font-weight: 600;
    color: var(--text-muted);
    min-width: 80px;
  }
  .mode-side-label:first-child { text-align: right; }
  .mode-side-label:last-child  { text-align: left; }

  /* 2× toggle */
  .toggle-xl { width: 72px !important; height: 38px !important; }
  .toggle-xl .slider { border-radius: 38px; }
  .toggle-xl .slider::before {
    width: 28px !important; height: 28px !important;
    left: 5px !important; top: 5px !important;
  }
  .toggle-xl input:checked + .slider::before { transform: translateX(34px) !important; }

  .mode-desc {
    margin: 0;
    padding: 0 1.5rem 1rem;
    font-size: 0.82rem;
    color: var(--text-muted);
    line-height: 1.4;
  }

  /* Sidebar mode indicator */
  .mode-tray { margin-top: 2px; }
  .mode-tray-dot {
    width: 7px; height: 7px; border-radius: 50%;
    background: var(--accent); /* auto = green/accent */
    flex-shrink: 0;
  }
  .mode-tray-dot.semi { background: #f59e0b; /* amber for semi-auto */ }

  .tz-display { font-size: 12px; }

  .net-dot-sm { width: 8px; height: 8px; border-radius: 50%; display: inline-block; margin-right: 4px; }

  /* ----- Toggle switch ----- */
  .toggle {
    position: relative;
    display: inline-block;
    width: 36px;
    height: 20px;
    flex-direction: row;
    gap: 0;
    cursor: pointer;
  }
  .toggle input { opacity: 0; width: 0; height: 0; }
  .slider {
    position: absolute;
    inset: 0;
    background: var(--border);
    border-radius: 20px;
    transition: background .2s;
    cursor: pointer;
  }
  .slider::before {
    content: "";
    position: absolute;
    width: 14px; height: 14px;
    left: 3px; top: 3px;
    background: white;
    border-radius: 50%;
    transition: transform .2s;
  }
  .toggle input:checked + .slider { background: var(--accent); }
  .toggle input:checked + .slider::before { transform: translateX(16px); }

  /* ----- How it works ----- */
  .howto-body { display: flex; flex-direction: column; gap: 14px; }
  .howto-anim { padding: 16px; max-width: 360px; }
  .howto { padding: 0; overflow: hidden; }
  .howto-step {
    display: flex; align-items: flex-start; gap: 14px;
    padding: 16px 20px; border-bottom: 1px solid var(--border);
  }
  .howto-step:last-child { border-bottom: none; }
  .howto-num {
    width: 26px; height: 26px; background: var(--accent); color: #fff;
    border-radius: 50%; display: flex; align-items: center; justify-content: center;
    font-weight: 700; font-size: 12px; flex-shrink: 0;
  }
  .howto-step h3 { margin-bottom: 4px; font-size: 13px; }
  .howto-step p { font-size: 12px; color: var(--text-muted); }

  /* Modal */
  .modal-backdrop {
    position: fixed; inset: 0;
    background: rgba(0,0,0,0.45);
    z-index: 1000;
    display: flex; align-items: center; justify-content: center;
    padding: 20px;
  }
  .modal-panel {
    background: var(--bg-card);
    border-radius: 14px;
    width: 100%; max-width: 640px;
    max-height: 90vh; overflow-y: auto;
    box-shadow: 0 24px 60px rgba(0,0,0,0.25);
    display: flex; flex-direction: column;
  }
  .modal-header {
    display: flex; align-items: center; justify-content: space-between;
    padding: 16px 20px 0;
    position: sticky; top: 0;
    background: var(--bg-card); z-index: 1;
    border-bottom: 1px solid var(--border); padding-bottom: 12px;
  }
  .modal-title { font-size: 15px; font-weight: 700; margin: 0; }
  .modal-close {
    background: none; border: none; font-size: 16px; cursor: pointer;
    color: var(--text-muted); padding: 4px 8px; border-radius: 6px;
  }
  .modal-close:hover { background: var(--border); color: var(--text); }
  .modal-body { padding: 16px 20px 20px; }
</style>
