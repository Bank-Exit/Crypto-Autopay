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
  import { createEventDispatcher, onMount } from "svelte";
  import { encodeAutopay, decodeAutopay, SCHEDULE_PRESETS, NETWORKS_EXTENDED } from "./autopay-encode.js";

  const dispatch = createEventDispatcher();

  export let mode = "create";   // "create" | "new" | "import"
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

  // When a deep-link code is provided, switch to import mode, pre-fill, and auto-decode
  onMount(async () => {
    if (initialCode) {
      mode = "import";
      importInput = initialCode;
      await parseImport();
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

  // ---- Shared payload validation & building ----
  function buildCleanPayload() {
    const amountStr = String(payload.amount ?? "").trim();
    let err = null;
    if (!payload.custom_label.trim()) err = lang === "fr" ? "Nom requis." : "Label required.";
    else if (!payload.destination_address.trim()) err = lang === "fr" ? "Adresse requise." : "Address required.";
    else if (addressValid === false) err = lang === "fr" ? "Adresse invalide pour ce réseau." : "Invalid address for this network.";
    else if (!amountStr || isNaN(parseFloat(amountStr)) || parseFloat(amountStr) <= 0) err = lang === "fr" ? "Montant invalide (> 0 requis)." : "Invalid amount (must be > 0).";
    if (err) return { error: err };
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
    return { payload: clean };
  }

  // ---- mode="create" → generate shareable code ----
  async function generate() {
    generatedError = "";
    generatedCode = "";
    const result = buildCleanPayload();
    if (result.error) { generatedError = result.error; return; }
    console.log("[SCE] generate() — encoding payload");
    try {
      generatedCode = await encodeAutopay(result.payload);
      generatedPayloadForShare = result.payload;
      dispatch("generated", { code: generatedCode, payload: result.payload });
      console.log("[SCE] Code generated, sharing view shown");
    } catch (e) {
      generatedError = String(e);
      console.error("[SCE] encode error:", e);
    }
  }

  // ---- mode="new" → add subscription directly (no code generation) ----
  function addSubscriptionDirectly() {
    generatedError = "";
    const result = buildCleanPayload();
    if (result.error) { generatedError = result.error; return; }
    console.log("[SCE] addSubscriptionDirectly() — dispatching new-sub");
    dispatch("new-sub", { payload: result.payload });
  }

  let generatedPayloadForShare = null;

  // ---- Fiat conversion (privacy-opt-in) ----
  let fiatEnabled = false;
  let fiatValue = null;
  let fiatLoading = false;
  let showFiatWarning = false;
  let fiatError = null;

  function requestFiatEnable() { showFiatWarning = true; }
  async function confirmFiatEnable() {
    showFiatWarning = false;
    fiatEnabled = true;
    await fetchFiatRate();
  }
  async function fetchFiatRate() {
    if (!payload.amount || !fiatEnabled) return;
    fiatLoading = true; fiatError = null; fiatValue = null;
    try {
      const res = await fetch(`https://libremonero.com/api/price?from=${payload.currency || "XMR"}&to=EUR`);
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      const data = await res.json();
      const rate = data.price || data.rate || data.EUR;
      if (rate) fiatValue = parseFloat(payload.amount) * parseFloat(rate);
      else throw new Error("No rate in response");
    } catch (e) {
      fiatError = "Erreur API"; fiatValue = null;
      console.warn("[SCE] Fiat rate fetch failed:", e);
    } finally { fiatLoading = false; }
  }
  $: if (fiatEnabled && payload.amount) { fetchFiatRate(); }

  // ---- Paste from clipboard ----
  async function pasteFromClipboard() {
    try {
      const text = await navigator.clipboard.readText();
      importInput = text;
      console.log("[SCE] Pasted from clipboard, length:", text.length);
      if (text.trim()) parseImport();
    } catch (e) {
      console.warn("[SCE] Clipboard read failed:", e);
    }
  }

  // Computed share assets
  $: shareWebUrl = generatedCode
    ? `https://autopay.libre-market.com/add?code=${encodeURIComponent(generatedCode)}`
    : "";
  $: shareHtmlButton = generatedCode && generatedPayloadForShare
    ? `<a href="${generatedCode}">\n  <button>S'abonner – ${generatedPayloadForShare.amount} ${generatedPayloadForShare.currency || 'XMR'}</button>\n</a>`
    : "";

  let copiedField = null;
  async function copyField(text, field) {
    try { await navigator.clipboard.writeText(text); } catch { prompt("Copier :", text); }
    copiedField = field;
    setTimeout(() => copiedField = null, 2000);
  }

  async function copyCode() {
    if (!generatedCode) return;
    await copyField(generatedCode, "uri");
  }

  // ---- Import / decode ----
  async function parseImport() {
    importError = "";
    importResult = null;
    importLoading = true;
    console.log("[SCE] parseImport() — decoding, length:", importInput.trim().length);
    try {
      const { prefix, version, payload: parsed } = await decodeAutopay(importInput.trim());
      importResult = { prefix, version, payload: parsed };
      console.log("[SCE] parseImport() success, prefix:", prefix, "label:", parsed.custom_label);
      // Note: do NOT auto-dispatch "imported" here — user must click "Ajouter cet abonnement"
    } catch (e) {
      importError = String(e);
      console.warn("[SCE] parseImport() error:", e);
    } finally {
      importLoading = false;
    }
  }

  // ---- i18n helpers (minimal, works standalone too) ----
  function l(fr, en) { return lang === "fr" ? fr : en; }
</script>

<div class="sce" class:compact>
  <!--
    Tab switcher only shown when mode is NOT pre-set to a specific single-purpose mode.
    mode="create": merchant code generator (full page) — no tabs
    mode="new": subscriber manual add — no tabs
    mode="import": subscriber code import — no tabs
  -->
  <!--  (tabs removed — each mode is now opened in its own dedicated context)  -->

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
          <input type="number" bind:value={payload.amount} min="0" step="0.01" placeholder="0.05" />
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

      <!-- Fiat equivalent (opt-in for privacy) -->
      <div class="fiat-row">
        <span class="fiat-label">≈ {l("Équivalent fiat", "Fiat equivalent")} (€)</span>
        {#if !fiatEnabled}
          <button class="btn-ghost btn-xs fiat-enable-btn" type="button" on:click={requestFiatEnable}>
            {l("Activer (⚠ privacy)", "Enable (⚠ privacy)")}
          </button>
        {:else if fiatLoading}
          <span class="fiat-value fiat-loading">…</span>
        {:else if fiatError}
          <span class="fiat-error">{fiatError}</span>
        {:else if fiatValue !== null}
          <span class="fiat-value">≈ {fiatValue.toFixed(2)} €</span>
        {/if}
      </div>
      {#if showFiatWarning}
        <!-- svelte-ignore a11y-click-events-have-key-events a11y-no-static-element-interactions -->
        <div class="fiat-popup-overlay" on:click={() => showFiatWarning = false}>
          <!-- svelte-ignore a11y-click-events-have-key-events a11y-no-static-element-interactions -->
          <div class="fiat-popup card" on:click|stopPropagation>
            <p class="fiat-popup-title">⚠ {l("Activation de la conversion fiat", "Enable fiat conversion")}</p>
            <p class="fiat-popup-body">
              {l(
                "L'API LibreMonero.com sera utilisée pour récupérer le cours actuel. C'est un service bénévole qui garantit l'anonymat et ne conserve aucun log.",
                "The LibreMonero.com API will be used to fetch the current rate. It is a volunteer service that guarantees anonymity and keeps no logs."
              )}
            </p>
            <p class="fiat-popup-body" style="font-style:italic;color:var(--text-muted)">
              {l(
                "Pour un anonymat maximal, il est recommandé de laisser ce champ désactivé.",
                "For maximum anonymity, it is recommended to leave this field disabled."
              )}
            </p>
            <div class="fiat-popup-actions">
              <button class="btn-primary" type="button" on:click={confirmFiatEnable}>{l("Activer", "Enable")}</button>
              <button class="btn-ghost" type="button" on:click={() => showFiatWarning = false}>{l("Annuler", "Cancel")}</button>
            </div>
          </div>
        </div>
      {/if}

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

      {#if mode === "new"}
        <button class="btn-primary btn-generate" type="button" on:click={addSubscriptionDirectly}>
          ＋ {l("Ajouter à mes abonnements", "Add to my subscriptions")}
        </button>
      {:else}
        <button class="btn-primary btn-generate" type="button" on:click={generate}>
          {l("Générer le code AutoPay →", "Generate AutoPay code →")}
        </button>
      {/if}

      <!-- ===== Share view after generation ===== -->
      {#if generatedCode}
        <div class="share-view">
          <div class="share-success">✅ {l("Code généré — à partager avec vos abonnés", "Code generated — share with your subscribers")}</div>

          <div class="share-block">
            <span class="share-label">🔗 URI brut</span>
            <div class="share-code">{generatedCode}</div>
            <button class="btn-copy-sm" type="button" on:click={() => copyField(generatedCode, 'uri')}>
              {copiedField === 'uri' ? '✓ Copié' : 'Copier'}
            </button>
          </div>

          <div class="share-block">
            <span class="share-label">🌐 Lien web (email / SMS)</span>
            <div class="share-code">{shareWebUrl}</div>
            <button class="btn-copy-sm" type="button" on:click={() => copyField(shareWebUrl, 'web')}>
              {copiedField === 'web' ? '✓ Copié' : 'Copier'}
            </button>
          </div>

          <div class="share-block">
            <span class="share-label">🖥 Bouton HTML pour votre site</span>
            <pre class="share-code share-pre">{shareHtmlButton}</pre>
            <button class="btn-copy-sm" type="button" on:click={() => copyField(shareHtmlButton, 'html')}>
              {copiedField === 'html' ? '✓ Copié' : 'Copier HTML'}
            </button>
          </div>

          <!-- Live preview of the button (only shown in full-page mode="create") -->
          {#if !compact}
            <div class="share-block">
              <span class="share-label">👁 Prévisualisation du bouton</span>
              <p class="share-preview-hint" style="font-size:11px;color:var(--text-muted)">
                {l("Aperçu rendu tel qu'il apparaîtra sur votre site. Cliquable pour tester.", "Rendered preview as it will appear on your site. Clickable to test.")}
              </p>
              <div class="html-preview">
                <a class="preview-link" href={generatedCode}
                   on:click|preventDefault={() => {
                     if (typeof window.__TAURI_INTERNALS__ !== "undefined") {
                       import("@tauri-apps/plugin-shell").then(m => m.open(generatedCode)).catch(() => {});
                     } else {
                       window.open(generatedCode, "_blank");
                     }
                   }}>
                  <button class="preview-btn" type="button">
                    S'abonner — {generatedPayloadForShare?.amount} {generatedPayloadForShare?.currency || "XMR"} / {l("période", "period")}
                  </button>
                </a>
              </div>
            </div>
          {/if}

          <button class="btn-ghost btn-sm share-back" type="button" on:click={() => { generatedCode = ""; generatedPayloadForShare = null; }}>
            ← {l("Modifier le code", "Edit code")}
          </button>
        </div>
      {/if}
    </div>

  <!-- ======================== IMPORT ======================== -->
  {:else}
    <div class="sce-import">
      <label class="field">
        <span class="import-label-row">
          {l("Collez un code autopay ou monero-request", "Paste an autopay or monero-request code")}
          <button class="btn-paste-icon" type="button" title={l("Coller depuis le presse-papier", "Paste from clipboard")}
            on:click={pasteFromClipboard}>
            📋
          </button>
        </span>
        <textarea
          bind:value={importInput}
          rows="3"
          placeholder="crypto-autopay:1:H4sI...  ou  monero-request:1:H4sI..."
          class="monospace"
        />
      </label>

      <button class="btn-primary" type="button" on:click={() => {
          console.log("[SCE] Décoder clicked, input length:", importInput.trim().length);
          parseImport();
        }} disabled={!importInput.trim() || importLoading}>
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
          <button class="btn-primary" type="button" on:click={() => {
              console.log("[SCE] 'Ajouter cet abonnement' clicked — dispatching imported event");
              dispatch("imported", { code: importInput.trim(), payload: importResult.payload });
            }}>
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

  /* Import label row with paste icon */
  .import-label-row { display: flex; align-items: center; justify-content: space-between; }
  .btn-paste-icon {
    background: none; border: 1px solid var(--border); border-radius: 4px;
    cursor: pointer; padding: 1px 5px; font-size: 14px; line-height: 1;
    color: var(--text-muted);
  }
  .btn-paste-icon:hover { background: var(--border); }

  /* Fiat conversion row */
  .fiat-row { display: flex; align-items: center; gap: 8px; font-size: 12px; color: var(--text-muted); }
  .fiat-label { font-weight: 500; }
  .fiat-enable-btn { font-size: 11px !important; }
  .fiat-value { color: #22c55e; font-weight: 600; }
  .fiat-loading { font-style: italic; }
  .fiat-error { color: #ef4444; font-size: 11px; }
  .btn-xs { padding: 2px 7px !important; font-size: 11px !important; }

  /* Fiat warning popup */
  .fiat-popup-overlay {
    position: fixed; inset: 0; z-index: 300;
    display: flex; align-items: center; justify-content: center;
    background: #0005;
  }
  .fiat-popup {
    max-width: 320px; width: 90%; padding: 1.25rem;
    display: flex; flex-direction: column; gap: 10px;
  }
  .fiat-popup-title { font-weight: 700; font-size: 0.95rem; }
  .fiat-popup-body { font-size: 0.84rem; line-height: 1.5; }
  .fiat-popup-actions { display: flex; gap: 8px; }

  /* HTML button live preview */
  .html-preview {
    padding: 16px; background: #ffffff; border: 1px dashed var(--border); border-radius: 6px;
    display: flex; align-items: center; justify-content: center;
  }
  .preview-link { text-decoration: none; }
  .preview-btn {
    background: #f26822; color: #fff; border: none; border-radius: 6px;
    padding: 10px 22px; font-size: 15px; font-weight: 600; cursor: pointer;
    box-shadow: 0 2px 6px #f2682240; transition: opacity 0.15s;
  }
  .preview-btn:hover { opacity: 0.9; }


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
  /* Share view (replaces old generated-output) */
  .share-view { display: flex; flex-direction: column; gap: 12px; }
  .share-success {
    font-size: 0.88rem; font-weight: 600; color: #22c55e;
    padding: 8px 12px; background: #22c55e12; border-radius: 6px;
  }
  .share-block {
    display: flex; flex-direction: column; gap: 4px;
    border: 1px solid var(--border); border-radius: 8px; padding: 10px 12px;
  }
  .share-label { font-size: 11px; font-weight: 700; color: var(--text-muted); text-transform: uppercase; letter-spacing: .04em; }
  .share-code {
    font-family: monospace; font-size: 11px; word-break: break-all;
    line-height: 1.5; color: var(--accent); background: var(--bg); padding: 6px; border-radius: 4px;
  }
  .share-pre { white-space: pre-wrap; }
  .btn-copy-sm {
    align-self: flex-start; padding: 3px 10px; font-size: 11px;
    background: var(--accent); color: #fff; border: none; border-radius: 5px; cursor: pointer;
  }
  .share-back { margin-top: 4px; align-self: flex-start; }

  /* Legacy (keep for any references) */
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
