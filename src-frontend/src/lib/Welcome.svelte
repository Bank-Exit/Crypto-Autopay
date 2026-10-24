<script>
  import { t, currentLang, LANGUAGES } from "./i18n.js";
  import { theme, toggleTheme } from "./theme.js";
  import { selectedNetwork, currentNetwork, NETWORKS } from "./network.js";
  import PaymentAnimation from "./PaymentAnimation.svelte";
  import { createEventDispatcher } from "svelte";
  const dispatch = createEventDispatcher();

  let showNetworkPicker = false;

  // Animation subjects — reactive to language and network
  $: animSubjects = [
    { toLabel: "Le Média Libre", xmrAmount: "0.05", btcAmount: "420 sats" },
    { toLabel: $t.vpnLabel || "VPN",  xmrAmount: "0.012", btcAmount: "120 sats" },
  ];
</script>

<!-- svelte-ignore a11y-click-events-have-key-events a11y-no-static-element-interactions -->
<div class="welcome" on:click={() => showNetworkPicker = false}>
  <!-- Top bar: language globe + theme -->
  <div class="topbar">
    <div class="lang-wrapper">
      <!-- Globe icon (no flags) -->
      <svg class="globe-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"
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

    <button class="theme-toggle" on:click|stopPropagation={toggleTheme}
            title={$theme === "light" ? $t.themeDark : $t.themeLight}>
      {#if $theme === "light"}
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" width="16" height="16">
          <path d="M21 12.79A9 9 0 1 1 11.21 3 7 7 0 0 0 21 12.79z"/>
        </svg>
      {:else}
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" width="16" height="16">
          <circle cx="12" cy="12" r="5"/>
          <line x1="12" y1="1" x2="12" y2="3"/>
          <line x1="12" y1="21" x2="12" y2="23"/>
          <line x1="4.22" y1="4.22" x2="5.64" y2="5.64"/>
          <line x1="18.36" y1="18.36" x2="19.78" y2="19.78"/>
          <line x1="1" y1="12" x2="3" y2="12"/>
          <line x1="21" y1="12" x2="23" y2="12"/>
          <line x1="4.22" y1="19.78" x2="5.64" y2="18.36"/>
          <line x1="18.36" y1="5.64" x2="19.78" y2="4.22"/>
        </svg>
      {/if}
    </button>
  </div>

  <div class="hero">
    <!-- Logo: circular recurrence arrow + A→B payment flow -->
    <div class="logo-mark" aria-label="Monero AutoPay logo">
      <svg width="68" height="68" viewBox="0 0 72 72" fill="none" xmlns="http://www.w3.org/2000/svg">
        <path d="M36 8 A28 28 0 1 1 8 36" stroke="var(--accent)" stroke-width="4.5" stroke-linecap="round" fill="none"/>
        <polyline points="6,24 8,36 20,34" stroke="var(--accent)" stroke-width="4.5" stroke-linecap="round" stroke-linejoin="round" fill="none"/>
        <circle cx="23" cy="36" r="5.5" fill="var(--accent)" opacity="0.9"/>
        <circle cx="49" cy="36" r="5.5" fill="var(--accent)" opacity="0.45"/>
        <line x1="31" y1="36" x2="41" y2="36" stroke="var(--accent)" stroke-width="2.5" stroke-linecap="round"/>
        <polyline points="38,32 43,36 38,40" stroke="var(--accent)" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round" fill="none"/>
      </svg>
    </div>

    <!-- Title with clickable network name -->
    <h1 class="welcome-title">
      {#if $t.welcomeTitle.includes("{network}")}
        {$t.welcomeTitle.split("{network}")[0]}<span
          class="network-pill"
          role="button"
          tabindex="0"
          on:click|stopPropagation={() => showNetworkPicker = !showNetworkPicker}
          on:keydown={e => e.key === 'Enter' && (showNetworkPicker = !showNetworkPicker)}
        >{$currentNetwork.label} ▾</span>{$t.welcomeTitle.split("{network}")[1]}
      {:else}
        {$t.welcomeTitle}
      {/if}
    </h1>

    <!-- Network picker dropdown -->
    {#if showNetworkPicker}
      <!-- svelte-ignore a11y-click-events-have-key-events a11y-no-static-element-interactions -->
      <div class="network-picker card" on:click|stopPropagation>
        <div class="network-picker-title">{$t.changeNetwork}</div>
        {#each NETWORKS as net}
          <button
            class="network-option"
            class:selected={$selectedNetwork === net.id}
            on:click={() => { selectedNetwork.set(net.id); showNetworkPicker = false; }}
          >
            <span class="net-dot" style="background:{net.color}"></span>
            {net.label}
          </button>
        {/each}
      </div>
    {/if}

    <p class="welcome-subtitle">
      {$t.welcomeSubtitle.replace("{network}", $currentNetwork.label)}
    </p>
  </div>

  <!-- Payment animation (alternates between subjects, network-aware) -->
  <div class="anim-section">
    <PaymentAnimation subjects={animSubjects} />
  </div>

  <!-- 3-step explainer -->
  <div class="steps-row">
    <div class="step-card card">
      <!-- Step 1: QR/link scan -->
      <svg class="step-icon-svg" viewBox="0 0 40 40" fill="none" xmlns="http://www.w3.org/2000/svg">
        <rect x="5" y="5" width="12" height="12" rx="2" stroke="var(--accent)" stroke-width="2"/>
        <rect x="8" y="8" width="6" height="6" fill="var(--accent)" opacity="0.5"/>
        <rect x="5" y="23" width="12" height="12" rx="2" stroke="var(--accent)" stroke-width="2"/>
        <rect x="8" y="26" width="6" height="6" fill="var(--accent)" opacity="0.5"/>
        <rect x="23" y="5" width="12" height="12" rx="2" stroke="var(--accent)" stroke-width="2"/>
        <rect x="26" y="8" width="6" height="6" fill="var(--accent)" opacity="0.5"/>
        <rect x="23" y="23" width="6" height="2" fill="var(--accent)" opacity="0.6"/>
        <rect x="23" y="28" width="12" height="2" fill="var(--accent)" opacity="0.6"/>
        <rect x="31" y="23" width="4" height="7" fill="var(--accent)" opacity="0.6"/>
      </svg>
      <h3>{$t.welcomeStep1Title}</h3>
      <p>{$t.welcomeStep1Desc}</p>
    </div>

    <div class="step-arrow">
      <svg viewBox="0 0 20 20" fill="none" width="18" height="18">
        <line x1="2" y1="10" x2="16" y2="10" stroke="var(--text-muted)" stroke-width="1.5" stroke-linecap="round"/>
        <polyline points="12,6 16,10 12,14" stroke="var(--text-muted)" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" fill="none"/>
      </svg>
    </div>

    <div class="step-card card">
      <!-- Step 2: Auto payment clock -->
      <svg class="step-icon-svg" viewBox="0 0 40 40" fill="none" xmlns="http://www.w3.org/2000/svg">
        <circle cx="20" cy="20" r="13" stroke="var(--accent)" stroke-width="2"/>
        <line x1="20" y1="10" x2="20" y2="20" stroke="var(--accent)" stroke-width="2.5" stroke-linecap="round"/>
        <line x1="20" y1="20" x2="27" y2="25" stroke="var(--accent)" stroke-width="2" stroke-linecap="round"/>
        <circle cx="20" cy="20" r="2" fill="var(--accent)"/>
        <!-- small arrow (recurrence) -->
        <path d="M30 12 A12 12 0 0 1 33 20" stroke="var(--accent)" stroke-width="1.5" stroke-linecap="round" fill="none" opacity="0.5"/>
        <polyline points="28,9.5 30,12 32.5,10" stroke="var(--accent)" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" fill="none" opacity="0.5"/>
      </svg>
      <h3>{$t.welcomeStep2Title}</h3>
      <p>{$t.welcomeStep2Desc}</p>
    </div>

    <div class="step-arrow">
      <svg viewBox="0 0 20 20" fill="none" width="18" height="18">
        <line x1="2" y1="10" x2="16" y2="10" stroke="var(--text-muted)" stroke-width="1.5" stroke-linecap="round"/>
        <polyline points="12,6 16,10 12,14" stroke="var(--text-muted)" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" fill="none"/>
      </svg>
    </div>

    <div class="step-card card">
      <!-- Step 3: Lock/key (non-custodial) -->
      <svg class="step-icon-svg" viewBox="0 0 40 40" fill="none" xmlns="http://www.w3.org/2000/svg">
        <rect x="10" y="20" width="20" height="14" rx="3" stroke="var(--accent)" stroke-width="2"/>
        <path d="M14 20 V15 A6 6 0 0 1 26 15 V20" stroke="var(--accent)" stroke-width="2" fill="none" stroke-linecap="round"/>
        <circle cx="20" cy="27" r="2.5" fill="var(--accent)" opacity="0.8"/>
        <line x1="20" y1="29" x2="20" y2="31" stroke="var(--accent)" stroke-width="2" stroke-linecap="round" opacity="0.8"/>
      </svg>
      <h3>{$t.welcomeStep3Title}</h3>
      <p>{$t.welcomeStep3Desc}</p>
    </div>
  </div>

  <!-- CTA buttons: side by side -->
  <div class="welcome-actions">
    <button class="btn-primary btn-lg" on:click={() => dispatch("start")}>
      {$t.welcomeStart}
    </button>
    <button class="btn-ghost btn-lg-ghost" on:click={() => dispatch("restore")}
            title="Restaurer un wallet existant depuis une seed de 25 mots">
      {$t.welcomeAlreadySetup}
    </button>
  </div>

  <p class="legal">{$t.welcomeFooter}</p>
</div>

<style>
  .welcome {
    min-height: 100vh;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    padding: 40px 24px 28px;
    gap: 24px;
    background: var(--bg);
  }

  /* ----- Top bar ----- */
  .topbar {
    position: fixed;
    top: 14px; right: 18px;
    display: flex;
    align-items: center;
    gap: 8px;
    z-index: 20;
  }

  .lang-wrapper {
    display: flex;
    align-items: center;
    gap: 5px;
    background: var(--bg-card);
    border: 1px solid var(--border);
    border-radius: 8px;
    padding: 5px 9px;
  }

  .globe-icon {
    width: 14px;
    height: 14px;
    color: var(--text-muted);
    flex-shrink: 0;
    pointer-events: none;
  }

  .lang-select {
    border: none;
    background: transparent;
    color: var(--text);
    font-size: 12px;
    cursor: pointer;
    outline: none;
    width: auto;
    padding: 0;
    -webkit-appearance: none;
    appearance: none;
  }

  /* Ensure options also inherit dark background */
  .lang-select option {
    background: var(--bg-card);
    color: var(--text);
  }

  .theme-toggle {
    background: var(--bg-card);
    border: 1px solid var(--border);
    border-radius: 8px;
    padding: 7px 9px;
    display: flex;
    align-items: center;
    justify-content: center;
    color: var(--text-muted);
  }
  .theme-toggle:hover { color: var(--text); }

  /* ----- Hero ----- */
  .hero {
    text-align: center;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 12px;
    max-width: 540px;
    position: relative;
  }

  .logo-mark { margin-bottom: 2px; }

  .welcome-title {
    font-size: 24px;
    font-weight: 800;
    color: var(--text);
    line-height: 1.25;
  }

  /* Clickable network name in title */
  .network-pill {
    color: var(--accent);
    cursor: pointer;
    border-bottom: 1.5px dashed var(--accent);
    padding-bottom: 1px;
    transition: opacity .15s;
    white-space: nowrap;
  }
  .network-pill:hover { opacity: 0.75; }

  /* Network picker dropdown */
  .network-picker {
    position: absolute;
    top: calc(100% + 6px);
    left: 50%;
    transform: translateX(-50%);
    z-index: 30;
    padding: 8px;
    min-width: 180px;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }

  .network-picker-title {
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: .08em;
    color: var(--text-muted);
    padding: 0 6px 4px;
    border-bottom: 1px solid var(--border);
    margin-bottom: 2px;
  }

  .network-option {
    display: flex;
    align-items: center;
    gap: 10px;
    background: transparent;
    border: none;
    border-radius: 6px;
    padding: 8px 10px;
    font-size: 13px;
    color: var(--text);
    cursor: pointer;
    text-align: left;
  }
  .network-option:hover { background: var(--bg-card-hover); }
  .network-option.selected { color: var(--accent); font-weight: 600; }

  .net-dot {
    width: 9px; height: 9px;
    border-radius: 50%;
    flex-shrink: 0;
  }

  .welcome-subtitle {
    font-size: 14px;
    color: var(--text-muted);
    line-height: 1.7;
    max-width: 460px;
  }

  /* ----- Animation ----- */
  .anim-section {
    width: 100%;
    max-width: 340px;
  }

  /* ----- Steps ----- */
  .steps-row {
    display: flex;
    align-items: flex-start;
    gap: 10px;
    max-width: 700px;
    width: 100%;
  }

  .step-card {
    flex: 1;
    padding: 18px 14px;
    text-align: center;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 8px;
  }

  .step-icon-svg {
    width: 40px;
    height: 40px;
  }

  .step-card h3 {
    font-size: 12px;
    font-weight: 700;
    color: var(--text);
    line-height: 1.3;
  }

  .step-card p {
    font-size: 11px;
    color: var(--text-muted);
    line-height: 1.6;
  }

  .step-arrow {
    flex-shrink: 0;
    margin-top: 38px;
    color: var(--text-muted);
  }

  /* ----- CTAs: side by side ----- */
  .welcome-actions {
    display: flex;
    flex-direction: row;
    align-items: center;
    gap: 12px;
    flex-wrap: wrap;
    justify-content: center;
  }

  .btn-lg {
    padding: 13px 32px;
    font-size: 14px;
    border-radius: 10px;
  }

  .btn-lg-ghost {
    padding: 12px 20px;
    font-size: 13px;
    border-radius: 10px;
  }

  .legal {
    font-size: 10.5px;
    color: var(--text-muted);
    opacity: 0.55;
    text-align: center;
    max-width: 480px;
  }

  /* ----- Mobile / narrow screens ----- */
  @media (max-width: 650px) {
    .welcome { padding: 48px 16px 20px; gap: 18px; }
    .welcome-title { font-size: 19px; }
    .steps-row { flex-direction: column; align-items: center; gap: 8px; }
    .step-card { max-width: 320px; width: 100%; }
    .step-arrow { transform: rotate(90deg); margin: 0 auto; }
    .anim-section { max-width: 260px; }
    .welcome-actions { flex-direction: column; }
  }
</style>
