<!--
  SubscriptionCodeEditor — shared component used in:
    1. The Tauri app "New subscription" modal (Dashboard)
    2. The web generator at autopay.libre-market.com/generator

  Props:
    mode: "create" | "import" (default "create")
    compact: boolean — reduce padding for modal usage (default false)
    lang: "fr" | "en" | "de" | "es" (default "fr", overridden by i18n store when inside the app)

  Events:
    on:generated  — { code, payload } when a code is created
    on:imported   — { code, payload } when a code is decoded/imported
    on:close      — user clicked cancel/close
-->
<script>
  import { createEventDispatcher } from "svelte";
  import { encodeAutopay, decodeAutopay, SCHEDULE_PRESETS, NETWORKS_EXTENDED } from "./autopay-encode.js";

  const dispatch = createEventDispatcher();

  export let mode = "create";   // "create" | "import"
  export let compact = false;
  export let lang = "fr";
  /** Optional: pre-fill the import field with a code (from deep link) */
  export let initialCode = null;

  // In the app only Monero & Bitcoin are supported (no Ethereum RPC planned)
  const APP_NETWORKS = NETWORKS_EXTENDED.filter(n => ["monero", "bitcoin"].includes(n.id));

  // ---- Address validation ----
  const ADDRESS_RULES = {
    monero: {
      // Mainnet primary (4..., 95 chars) | subaddress (8..., 95) | integrated (4..., 106)
      re: /^[48][1-9A-HJ-NP-Za-km-z]{93,94}$|^4[1-9A-HJ-NP-Za-km-z]{105}$/,
      hint: "Adresse Monero (95 caractères, commence par 4 ou 8)",
      hint_en: "Monero address (95 chars, starts with 4 or 8)",
    },
    bitcoin: {
      // Legacy P2PKH (1...) | P2SH (3...) | bech32 segwit (bc1q...) | bech32m taproot (bc1p...)
      re: /^[13][1-9A-HJ-NP-Za-km-z]{25,33}$|^bc1[qp][0-9a-z]{38,87}$/,
      hint: "Adresse Bitcoin (Legacy 1…, P2SH 3…, Segwit bc1q…, Taproot bc1p…)",
      hint_en: "Bitcoin address (Legacy 1…, P2SH 3…, Segwit bc1q…, Taproot bc1p…)",
    },
  };

  function validateAddress(addr, network) {
    if (!addr) return null; // empty → not validated yet
    const rules = ADDRESS_RULES[network];
    if (!rules) return true; // unknown network, skip validation
    return rules.re.test(addr.trim());
  }

  $: addressValid = validateAddress(payload.destination_address, payload.network);
  $: addressHint = ADDRESS_RULES[payload.network]?.[lang === "fr" ? "hint" : "hint_en"] || "";

  // ---- "create" mode state ----
  let payload = {
    network: "monero",
    token: null,
    token_contract: null,
    custom_label: "",
    description: "",
    destination_address: "",
    amount: "",
    currency: "XMR",
    schedule: "0 0 9 1 * *",
    repeat_every: 1,
    number_of_payments: 0,
    start_date: "",
    canary_url: "",
    trial_days: 0,
  };

  let schedulePresetId = "monthly";
  let multiYearN = 2;          // for biennial/triennial custom
  let showAdvanced = false;
  let generatedCode = "";
  let generatedError = "";
  let copied = false;

  // ---- "import" mode state ----
  let importInput = "";
  let importResult = null;
  let importError = "";
  let importLoading = false;

  // When a deep-link code is provided, switch to import mode and pre-fill
  import { onMount } from "svelte";
  onMount(() => {
    if (initialCode) {
      mode = "import";
      importInput = initialCode;
    }
  });

  // ---- Reactive: network → currency default + token list ----
  $: selectedNetworkObj = NETWORKS_EXTENDED.find(n => n.id === payload.network) || NETWORKS_EXTENDED[0];
  $: availableTokens = selectedNetworkObj?.tokens || [];

  function onNetworkChange() {
    // Reset token fields when switching network
    payload.token = null;
    payload.token_contract = null;
    // Set default currency to native coin
    payload.currency = selectedNetworkObj.symbol;
  }

  function onTokenChange() {
    if (payload.token) {
      payload.currency = payload.token;
    } else {
      payload.currency = selectedNetworkObj.symbol;
    }
  }

  // ---- Schedule preset handling ----
  function onPresetChange() {
    const preset = SCHEDULE_PRESETS.find(p => p.id === schedulePresetId);
    if (!preset || preset.id === "custom") return;
    payload.schedule = preset.cron;
    payload.repeat_every = preset.repeatEvery || 1;
    if (preset.id === "biennial")  multiYearN = 2;
    if (preset.id === "triennial") multiYearN = 3;
  }

  function onMultiYearChange() {
    payload.repeat_every = parseInt(multiYearN, 10) || 2;
  }

  $: scheduleLabel = (() => {
    const preset = SCHEDULE_PRESETS.find(p => p.id === schedulePresetId);
    if (!preset) return payload.schedule;
    if (schedulePresetId === "custom") return payload.schedule || "—";
    const base = lang === "fr" ? preset.label_fr : preset.label_en;
    if (schedulePresetId === "yearly" && payload.repeat_every > 1) {
      return lang === "fr" ? `Tous les ${payload.repeat_every} ans` : `Every ${payload.repeat_every} years`;
    }
    return base;
  })();

  // ---- Generate code ----
  async function generate() {
    generatedError = "";
    generatedCode = "";

    // Normalize amount — input type="number" returns a JS number, not a string
    const amountStr = String(payload.amount ?? "").trim();

    if (!payload.custom_label.trim()) { generatedError = lang === "fr" ? "Nom requis." : "Label required."; return; }
    if (!payload.destination_address.trim()) { generatedError = lang === "fr" ? "Adresse requise." : "Address required."; return; }
    if (addressValid === false) { generatedError = lang === "fr" ? "Adresse invalide pour ce réseau." : "Invalid address for this network."; return; }
    if (!amountStr || isNaN(parseFloat(amountStr)) || parseFloat(amountStr) <= 0) { generatedError = lang === "fr" ? "Montant invalide (> 0 requis)." : "Invalid amount (must be > 0)."; return; }

    const clean = {
      version: 1,
      network: payload.network,
      custom_label: payload.custom_label.trim(),
      destination_address: payload.destination_address.trim(),
      amount: amountStr,
      currency: payload.currency,
      schedule: payload.schedule,
    };

    if (payload.token) { clean.token = payload.token; clean.token_contract = payload.token_contract || undefined; }
    if (payload.repeat_every > 1) clean.repeat_every = payload.repeat_every;
    if (payload.description.trim()) clean.description = payload.description.trim();
    if (payload.number_of_payments > 0) clean.number_of_payments = payload.number_of_payments;
    if (payload.start_date) clean.start_date = payload.start_date;
    if (payload.canary_url.trim()) clean.canary_url = payload.canary_url.trim();

    try {
      generatedCode = await encodeAutopay(clean);
      dispatch("generated", { code: generatedCode, payload: clean });
    } catch (e) {
      generatedError = String(e);
    }
  }

  async function copyCode() {
    if (!generatedCode) return;
    await navigator.clipboard.writeText(generatedCode);
    copied = true;
    setTimeout(() => copied = false, 2000);
  }

  // ---- Import / decode ----
  async function parseImport() {
    importError = "";
    importResult = null;
    importLoading = true;
    try {
      const { prefix, version, payload: parsed } = await decodeAutopay(importInput.trim());
      importResult = { prefix, version, payload: parsed };
      dispatch("imported", { code: importInput.trim(), payload: parsed });
    } catch (e) {
      importError = String(e);
    } finally {
      importLoading = false;
    }
  }

  // ---- i18n helpers (minimal, works standalone too) ----
  function l(fr, en) { return lang === "fr" ? fr : en; }
</script>

<div class="sce" class:compact>
  <!-- Tab switcher -->
  <div class="sce-tabs" role="tablist">
    <button
      class="sce-tab" class:active={mode === "create"}
      on:click={() => { mode = "create"; generatedCode = ""; }}
      role="tab"
    >
      {l("Créer un code", "Create a code")}
    </button>
    <button
      class="sce-tab" class:active={mode === "import"}
      on:click={() => { mode = "import"; importResult = null; }}
      role="tab"
    >
      {l("Importer / décoder", "Import / decode")}
    </button>
  </div>

  <!-- ======================== CREATE ======================== -->
  {#if mode === "create"}
    <div class="sce-form">

      <!-- Network selector (Monero + Bitcoin only in the app) -->
      <div class="field-row">
        <label class="field field-sm">
          <span>{l("Réseau crypto", "Crypto network")}</span>
          <select bind:value={payload.network} on:change={onNetworkChange}>
            {#each APP_NETWORKS as net}
              <option value={net.id}>{net.label} ({net.symbol})</option>
            {/each}
          </select>
        </label>
      </div>

      {#if payload.token === "__custom__"}
        <div class="field-row">
          <label class="field">
            <span>{l("Symbole du token", "Token symbol")}</span>
            <input type="text" bind:value={payload.token} placeholder="USDT" />
          </label>
          <label class="field">
            <span>{l("Adresse du contrat", "Contract address")}</span>
            <input type="text" bind:value={payload.token_contract} placeholder="0x..." />
          </label>
        </div>
      {:else if payload.token && payload.token_contract !== null}
        <label class="field">
          <span>{l("Adresse du contrat", "Contract address")} <span class="hint">(optionnel mais recommandé)</span></span>
          <input type="text" bind:value={payload.token_contract} placeholder="0x..." />
        </label>
      {/if}

      <!-- Label -->
      <label class="field">
        <span>{l("Nom de l'abonnement", "Subscription name")} <span class="required">*</span></span>
        <input type="text" bind:value={payload.custom_label} placeholder={l("Le Média Libre — Mensuel", "My Service — Monthly")} maxlength="80" />
      </label>

      <!-- Address + inline validation -->
      <label class="field">
        <span>{l("Adresse de réception", "Receiving address")} <span class="required">*</span></span>
        <input type="text" bind:value={payload.destination_address}
               placeholder={payload.network === "monero" ? "4At3X5rvVypTofgmueN9s9..." : "bc1q... ou 1... ou 3..."}
               class="monospace"
               class:input-ok={addressValid === true}
               class:input-error={addressValid === false} />
        {#if addressValid === false}
          <span class="field-error">{l("Format invalide.", "Invalid format.")} {addressHint}</span>
        {:else if addressValid === true}
          <span class="field-ok">✓ {l("Adresse valide", "Valid address")}</span>
        {:else}
          <span class="field-hint">{addressHint}</span>
        {/if}
      </label>

      <!-- Amount + currency -->
      <div class="field-row">
        <label class="field field-sm">
          <span>{l("Montant", "Amount")} <span class="required">*</span></span>
          <input type="number" bind:value={payload.amount} min="0" step="any" placeholder="0.05" />
        </label>
        <label class="field field-sm">
          <span>{l("Devise", "Currency")}</span>
          <select bind:value={payload.currency}>
            <option>{selectedNetworkObj.symbol}</option>
            {#if payload.token}<option>{payload.token}</option>{/if}
            <option>USD</option>
            <option>EUR</option>
          </select>
        </label>
      </div>

      <!-- Schedule -->
      <div class="field">
        <span class="field-label">{l("Fréquence", "Frequency")}</span>
        <div class="schedule-presets">
          {#each SCHEDULE_PRESETS.filter(p => !["biennial","triennial"].includes(p.id)) as preset}
            <button
              class="preset-btn"
              class:active={schedulePresetId === preset.id && !(schedulePresetId === "yearly" && payload.repeat_every > 1)}
              on:click={() => { schedulePresetId = preset.id; onPresetChange(); }}
              type="button"
            >
              {lang === "fr" ? preset.label_fr : preset.label_en}
            </button>
          {/each}
          <!-- Multi-year button -->
          <button
            class="preset-btn"
            class:active={schedulePresetId === "yearly" && payload.repeat_every > 1}
            on:click={() => { schedulePresetId = "yearly"; payload.schedule = "0 9 1 1 *"; payload.repeat_every = multiYearN; }}
            type="button"
          >
            {l("Pluriannuel", "Multi-year")}
          </button>
        </div>

        {#if schedulePresetId === "yearly" && payload.repeat_every > 1}
          <div class="repeat-row">
            <span>{l("Tous les", "Every")}</span>
            <input type="number" bind:value={multiYearN} min="2" max="10" on:input={onMultiYearChange} style="width:60px" />
            <span>{l("ans", "years")}</span>
          </div>
        {/if}

        {#if schedulePresetId === "custom"}
          <div class="cron-row">
            <input type="text" bind:value={payload.schedule} placeholder="0 9 1 * *" class="monospace" />
            <a href="https://crontab.guru" target="_blank" rel="noreferrer" class="cron-help">crontab.guru ↗</a>
          </div>
        {/if}

        {#if payload.schedule && schedulePresetId !== "custom"}
          <div class="cron-display">{l("Cron :", "Cron:")} <code>{payload.schedule}</code>
            {#if payload.repeat_every > 1}× {payload.repeat_every}{/if}
          </div>
        {/if}
      </div>

      <!-- Advanced fields (collapsible) -->
      <button class="btn-toggle-advanced" type="button" on:click={() => showAdvanced = !showAdvanced}>
        {showAdvanced ? "▲" : "▼"} {l("Options avancées", "Advanced options")}
      </button>

      {#if showAdvanced}
        <div class="advanced-fields">
          <label class="field">
            <span>{l("Description", "Description")}</span>
            <textarea bind:value={payload.description} rows="2" placeholder={l("Détails de l'abonnement…", "Subscription details…")} />
          </label>

          <div class="field-row">
            <label class="field field-sm">
              <span>{l("Nombre de paiements max", "Max payments")} <span class="hint">0={l("illimité","unlimited")}</span></span>
              <input type="number" bind:value={payload.number_of_payments} min="0" step="1" />
            </label>
            <label class="field field-sm">
              <span>{l("Date de début", "Start date")}</span>
              <input type="date" bind:value={payload.start_date} />
            </label>
          </div>

          <label class="field field-sm">
            <span>{l("Période d'essai gratuit (jours)", "Free trial (days)")} <span class="hint">0={l("aucun","none")}</span></span>
            <input type="number" bind:value={payload.trial_days} min="0" step="1" />
          </label>

          <label class="field">
            <span>{l("URL Canary (recommandé)", "Canary URL (recommended)")}</span>
            <input type="url" bind:value={payload.canary_url} placeholder="https://yoursite.com/.well-known/autopay-canary.json" />
          </label>
        </div>
      {/if}

      {#if generatedError}
        <div class="error-msg">{generatedError}</div>
      {/if}

      <button class="btn-primary btn-generate" type="button" on:click={generate}>
        {l("Générer le code AutoPay →", "Generate AutoPay code →")}
      </button>

      <!-- Generated code output -->
      {#if generatedCode}
        <div class="generated-output">
          <div class="output-header">
            <span class="output-label">{l("Code crypto-autopay", "crypto-autopay code")}</span>
            <button class="btn-copy" on:click={copyCode} type="button">
              {copied ? (l("Copié ✓", "Copied ✓")) : l("Copier", "Copy")}
            </button>
          </div>
          <div class="output-code">{generatedCode}</div>
          <div class="output-hint">
            {l("Partagez ce code sous forme de QR code, lien ou texte brut.", "Share this code as a QR code, link, or plain text.")}
          </div>
        </div>
      {/if}
    </div>

  <!-- ======================== IMPORT ======================== -->
  {:else}
    <div class="sce-import">
      <label class="field">
        <span>{l("Collez un code autopay ou monero-request", "Paste an autopay or monero-request code")}</span>
        <textarea
          bind:value={importInput}
          rows="3"
          placeholder="crypto-autopay:1:H4sI...  ou  monero-request:1:H4sI..."
          class="monospace"
        />
      </label>

      <button class="btn-primary" type="button" on:click={parseImport} disabled={!importInput.trim() || importLoading}>
        {importLoading ? l("Décodage…", "Decoding…") : l("Décoder →", "Decode →")}
      </button>

      {#if importError}
        <div class="error-msg">{importError}</div>
      {/if}

      {#if importResult}
        <div class="import-result card">
          <div class="import-badge">
            {importResult.prefix === "monero-request" ? "monero-request:1:" : "crypto-autopay:1:"}
            {importResult.payload._imported_from ? "→ " + l("converti", "converted") : ""}
          </div>
          <dl class="import-fields">
            {#each Object.entries(importResult.payload).filter(([k]) => !k.startsWith("_")) as [key, val]}
              <div class="import-row">
                <dt class="import-key">{key}</dt>
                <dd class="import-val">{typeof val === "object" ? JSON.stringify(val) : val}</dd>
              </div>
            {/each}
          </dl>
          <button class="btn-primary" type="button" on:click={() => dispatch("imported", { code: importInput.trim(), payload: importResult.payload })}>
            {l("Ajouter cet abonnement →", "Add this subscription →")}
          </button>
        </div>
      {/if}
    </div>
  {/if}

  <div class="sce-footer">
    <button class="btn-link" type="button" on:click={() => dispatch("close")}>
      {l("Annuler", "Cancel")}
    </button>
  </div>
</div>

<style>
  .sce {
    display: flex; flex-direction: column; gap: 14px;
    width: 100%; box-sizing: border-box;
    padding: 20px;
    max-width: 620px;
  }
  .sce.compact { padding: 0; gap: 12px; }

  /* Tabs */
  .sce-tabs {
    display: flex; gap: 0; border-bottom: 2px solid var(--border, #e2ddd7);
    margin-bottom: 4px;
  }
  .sce-tab {
    background: none; border: none; padding: 8px 16px;
    font-size: 13px; color: var(--text-muted, #888); cursor: pointer;
    border-bottom: 2px solid transparent; margin-bottom: -2px;
    transition: color .15s, border-color .15s;
  }
  .sce-tab.active { color: var(--accent, #e8601a); border-bottom-color: var(--accent, #e8601a); font-weight: 600; }

  /* Form */
  .sce-form, .sce-import { display: flex; flex-direction: column; gap: 12px; }

  .field { display: flex; flex-direction: column; gap: 5px; }
  .field-label { font-size: 12.5px; color: var(--text-muted, #666); font-weight: 500; margin-bottom: 6px; display: block; }
  .field > span:first-child { font-size: 12.5px; color: var(--text-muted, #666); font-weight: 500; }
  .field-row { display: flex; gap: 10px; }
  .field-sm { flex: 1; min-width: 0; }

  .required { color: var(--accent, #e8601a); }
  .hint { font-size: 10.5px; color: var(--text-muted, #aaa); font-weight: 400; }

  input, select, textarea {
    padding: 8px 10px; border: 1px solid var(--border, #ddd);
    border-radius: 6px; font-size: 13px;
    background: var(--bg, white); color: var(--text, #333);
    width: 100%; box-sizing: border-box;
    font-family: inherit;
  }
  input:focus, select:focus, textarea:focus {
    outline: none; border-color: var(--accent, #e8601a);
  }
  .monospace { font-family: "Courier New", monospace; font-size: 11.5px; word-break: break-all; }
  .input-ok  { border-color: #2e7d32 !important; }
  .input-error { border-color: #c0392b !important; }
  .field-error { font-size: 11px; color: #c0392b; margin-top: 2px; }
  .field-ok    { font-size: 11px; color: #2e7d32; margin-top: 2px; }
  .field-hint  { font-size: 10.5px; color: var(--text-muted, #aaa); margin-top: 2px; }

  /* Schedule presets */
  .schedule-presets { display: flex; flex-wrap: wrap; gap: 6px; margin-bottom: 8px; }
  .preset-btn {
    padding: 5px 11px; border: 1.5px solid var(--border, #ddd);
    border-radius: 20px; font-size: 12px; cursor: pointer;
    background: var(--bg, white); color: var(--text, #333);
    transition: border-color .12s, background .12s;
  }
  .preset-btn:hover { border-color: var(--accent, #e8601a); }
  .preset-btn.active { border-color: var(--accent, #e8601a); background: var(--accent-light, #fdf0e8); color: var(--accent, #e8601a); font-weight: 600; }

  .repeat-row, .cron-row { display: flex; align-items: center; gap: 8px; font-size: 13px; }
  .cron-row input { flex: 1; }
  .cron-help { font-size: 11px; color: var(--accent, #e8601a); }
  .cron-display { font-size: 11px; color: var(--text-muted, #888); }
  .cron-display code { font-family: "Courier New", monospace; background: var(--bg, #f5f5f5); padding: 2px 5px; border-radius: 4px; }

  /* Advanced */
  .btn-toggle-advanced {
    background: none; border: none; color: var(--text-muted, #888);
    font-size: 12px; cursor: pointer; padding: 2px 0; text-align: left;
  }
  .advanced-fields { display: flex; flex-direction: column; gap: 10px;
    padding: 12px; background: var(--bg-sidebar, #f8f5f0);
    border-radius: 8px; border: 1px solid var(--border, #ddd); }

  /* Buttons */
  .btn-primary {
    background: var(--accent, #e8601a); color: white; border: none;
    border-radius: 8px; padding: 10px 18px; font-size: 13.5px; font-weight: 600;
    cursor: pointer; transition: opacity .15s;
  }
  .btn-primary:hover { opacity: 0.88; }
  .btn-primary:disabled { opacity: 0.4; cursor: not-allowed; }
  .btn-generate { padding: 11px; width: 100%; }

  .error-msg {
    background: #fef0f0; color: #c0392b;
    padding: 8px 12px; border-radius: 6px; font-size: 12.5px;
  }

  /* Generated output */
  .generated-output {
    border: 2px solid var(--accent, #e8601a);
    border-radius: 10px; padding: 14px;
    background: var(--accent-light, #fdf0e8);
    display: flex; flex-direction: column; gap: 8px;
  }
  .output-header { display: flex; justify-content: space-between; align-items: center; }
  .output-label { font-size: 12px; font-weight: 700; color: var(--accent, #e8601a); }
  .btn-copy {
    background: var(--accent, #e8601a); color: white; border: none;
    border-radius: 6px; padding: 4px 10px; font-size: 12px; cursor: pointer;
  }
  .output-code {
    font-family: "Courier New", monospace; font-size: 11px;
    word-break: break-all; line-height: 1.6;
    background: var(--bg, white); padding: 10px; border-radius: 6px;
    color: var(--text, #333);
  }
  .output-hint { font-size: 11px; color: var(--text-muted, #888); }

  /* Import result */
  .import-result { padding: 14px; display: flex; flex-direction: column; gap: 10px; }
  .import-badge {
    display: inline-block; font-size: 11px; font-weight: 700;
    background: var(--accent, #e8601a); color: white;
    padding: 3px 8px; border-radius: 4px; align-self: flex-start;
  }
  .import-fields { margin: 0; display: flex; flex-direction: column; gap: 4px; }
  .import-row { display: flex; gap: 10px; border-bottom: 1px solid var(--border, #eee); padding: 4px 0; }
  .import-key { font-size: 11px; font-weight: 700; color: var(--text-muted, #888); min-width: 140px; font-family: monospace; }
  .import-val { font-size: 12px; color: var(--text, #333); word-break: break-all; }

  .sce-footer { display: flex; justify-content: flex-end; padding-top: 4px; }
  .btn-link {
    background: none; border: none; color: var(--text-muted, #888);
    font-size: 12px; cursor: pointer; padding: 4px;
    text-decoration: underline; text-underline-offset: 2px;
  }

  @media (max-width: 480px) {
    .field-row { flex-direction: column; }
    .schedule-presets { gap: 5px; }
    .preset-btn { font-size: 11px; padding: 4px 9px; }
  }
</style>
