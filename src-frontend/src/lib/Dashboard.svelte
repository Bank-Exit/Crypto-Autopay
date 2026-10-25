<script>
  import { tick, onMount } from "svelte";
  import { t, currentLang, LANGUAGES } from "./i18n.js";
  import { theme } from "./theme.js";
  import { selectedNetwork, currentNetwork, NETWORKS } from "./network.js";
  import { paymentMode } from "./paymentMode.js";
  import SubscriptionList from "./SubscriptionList.svelte";
  import PaymentAnimation from "./PaymentAnimation.svelte";
  import SubscriptionCodeEditor from "./SubscriptionCodeEditor.svelte";
  import { scheduleToHuman } from "./autopay-encode.js";

  // Tauri invoke helper (safe in browser dev too)
  async function tauriInvoke(cmd, args = {}) {
    try {
      const { invoke } = await import("@tauri-apps/api/core");
      return await invoke(cmd, args);
    } catch (e) {
      logWarn("Tauri", `invoke(${cmd}) unavailable or failed: ${e}`);
      return null;
    }
  }
  import { logInfo, logWarn, logError, subscribeToLogs, getEntries, clearLogs,
           setSendLogsEnabled, isSendLogsEnabled, getSendLogs, clearSendLogs } from "./AppLogger.js";

  // Tauri shell.open for external links (safe fallback for browser dev)
  async function openExternal(url) {
    try {
      const { open } = await import("@tauri-apps/plugin-shell");
      await open(url);
      logInfo("External", `Opened URL: ${url}`);
    } catch {
      window.open(url, "_blank", "noopener");
    }
  }

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
    // payload.destination_address is the canonical field name in autopay spec
    const addr = payload.destination_address || payload.address || null;
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
      address: addr,                              // stored for duplicate detection
      _raw_schedule: payload.schedule || null,    // raw cron for comparison
      _code: payload._sourceCode || null,
    };
  }

  // ---- Duplicate detection ----
  // Two subscriptions are duplicates if they share address AND raw schedule.
  // Subscriptions without an address (e.g., mock data) are never flagged.
  function duplicateKey(sub) {
    if (!sub.address) return null; // can't determine duplicate without address
    return `${sub.address}|${sub._raw_schedule || sub.schedule}`;
  }

  let duplicateCount = 0; // must be declared BEFORE the reactive block
  $: {
    const keys = subscriptions.map(duplicateKey).filter(Boolean);
    const seen = new Map();
    for (const k of keys) seen.set(k, (seen.get(k) || 0) + 1);
    let dups = 0;
    for (const count of seen.values()) if (count > 1) dups += count - 1;
    duplicateCount = dups;
    if (dups > 0) logWarn("Duplicates", `${dups} duplicate subscription(s) detected`);
  }

  // Pending duplicate subscription waiting for user confirmation
  let pendingDuplicateSub = null;
  let showDuplicatePopup = false;
  let dupTimeLeft = 5;
  let dupTimer = null;
  const DUP_RADIUS = 20;
  const DUP_CIRC = 2 * Math.PI * DUP_RADIUS; // ≈ 125.6

  $: dupDashOffset = DUP_CIRC * (1 - dupTimeLeft / 5);

  function startDupTimer() {
    clearDupTimer();
    dupTimeLeft = 5;
    dupTimer = setInterval(() => {
      dupTimeLeft = Math.max(0, dupTimeLeft - 0.05); // smooth at 20fps
      if (dupTimeLeft <= 0) rejectDuplicate();
    }, 50);
  }
  function clearDupTimer() {
    if (dupTimer) { clearInterval(dupTimer); dupTimer = null; }
  }
  function rejectDuplicate() {
    clearDupTimer();
    showDuplicatePopup = false;
    pendingDuplicateSub = null;
    showNewSubModal = false;
  }
  function acceptDuplicate() {
    clearDupTimer();
    if (pendingDuplicateSub) subscriptions = [...subscriptions, pendingDuplicateSub];
    showDuplicatePopup = false;
    pendingDuplicateSub = null;
    showNewSubModal = false;
    page = "subscriptions";
  }

  function addOrConfirmDuplicate(sub) {
    const key = duplicateKey(sub);
    const isDup = subscriptions.some(s => duplicateKey(s) === key);
    if (isDup) {
      pendingDuplicateSub = sub;
      showDuplicatePopup = true;
      startDupTimer();
    } else {
      subscriptions = [...subscriptions, sub];
      showNewSubModal = false;
      page = "subscriptions";
    }
  }

  /** Best-effort: compute next occurrence date from a 6-field cron preset */
  function nextOccurrence(schedule) {
    const now = new Date();
    const s = (schedule || "").trim();
    const parts = s.split(/\s+/);

    // Every N minutes: "0 */N * * * *" or "*/N * * * *"
    const minField = parts.length >= 6 ? parts[1] : parts[0];
    if (minField && minField.startsWith("*/")) {
      const n = parseInt(minField.slice(2)) || 1;
      return new Date(now.getTime() + n * 60 * 1000).toISOString();
    }
    // Hourly: min=0, hour=* → next full hour
    const hourField = parts.length >= 6 ? parts[2] : parts[1];
    if (minField === "0" && hourField === "*") {
      const d = new Date(now);
      d.setMinutes(0, 0, 0); d.setHours(d.getHours() + 1);
      return d.toISOString();
    }
    // Monthly (dom=1)
    if (s === "0 0 9 1 * *" || s === "0 9 1 * *") {
      return new Date(now.getFullYear(), now.getMonth() + 1, 1).toISOString();
    }
    // Weekly (Monday)
    if (s === "0 0 9 * * 1" || s === "0 9 * * 1") {
      const d = new Date(now);
      d.setDate(now.getDate() + ((8 - now.getDay()) % 7 || 7));
      return d.toISOString();
    }
    // Daily
    if (s === "0 0 9 * * *" || s === "0 9 * * *") {
      const d = new Date(now);
      d.setDate(now.getDate() + 1);
      return d.toISOString();
    }
    // Quarterly
    if (s.includes("*/3")) {
      const nextMonth = Math.ceil((now.getMonth() + 1) / 3) * 3;
      return new Date(now.getFullYear(), nextMonth, 1).toISOString();
    }
    // Yearly
    if (s === "0 0 9 1 1 *" || s === "0 9 1 1 *") {
      return new Date(now.getFullYear() + 1, 0, 1).toISOString();
    }
    // Fallback: now + 1 month
    return new Date(now.getFullYear(), now.getMonth() + 1, now.getDate()).toISOString();
  }

  // ---- 3 separate entry points ----
  // 1. "Nouvel abonnement" (manual form) → modal mode="new"
  // 2. "Importer un code autopay" → modal mode="import"
  // 3. "Créer un code de partage" → full page page="create-code" (not a modal)

  let showNewSubModal  = false;   // for "Nouvel abonnement" (mode="new")
  let showImportModal  = false;   // for "Importer un code" (mode="import")
  let importInitialCode = null;   // pre-filled code from deep link
  let editingSubData   = null;    // pre-filled data for edit mode

  function openNewSubModal() {
    logInfo("Dashboard", "Opening manual-add modal (mode=new)");
    showImportModal = false;
    editingSubData = null;
    showNewSubModal = true;
  }

  async function openEditSubModal(sub) {
    logInfo("Dashboard", `Opening edit modal for sub #${sub.id}`);
    showImportModal = false;
    showNewSubModal = false;
    editingSubData = { ...sub };
    await tick();
    showNewSubModal = true;
  }

  async function openImportModal(code = null) {
    logInfo("Dashboard", `Opening import modal, hasCode=${Boolean(code)}`);
    // Force re-mount even if already open (handles same-code re-trigger)
    showImportModal = false;
    importInitialCode = null;
    await tick();
    importInitialCode = code;
    showImportModal = true;
  }

  // React to external code from deep link — use showImportModal
  $: if (externalCode) {
    logInfo("Dashboard", "External code received via deep link");
    openImportModal(externalCode);
  }

  /** "Nouvel abonnement" mode="new" dispatches this */
  function onSubNew({ detail }) {
    logInfo("Dashboard", `onSubNew: label=${detail.payload?.custom_label || "?"}`);
    const sub = payloadToSubscription({ ...detail.payload, _sourceCode: null });
    addOrConfirmDuplicate(sub);
    showNewSubModal = false;
    editingSubData = null;
  }

  async function onSubEdit({ detail }) {
    const editId = editingSubData?.id;
    if (!editId) { onSubNew({ detail }); return; }
    const updated = payloadToSubscription({ ...detail.payload, _sourceCode: null });
    updated.id = editId;
    updated._db_id = editingSubData._db_id;
    updated.payments_sent = editingSubData.payments_sent || 0;
    updated.next_payment_at = editingSubData.next_payment_at;
    updated.status = editingSubData.status;
    subscriptions = subscriptions.map(s => s.id === editId ? updated : s);
    logInfo("Dashboard", `Sub #${editId} updated`);
    showNewSubModal = false;
    editingSubData = null;
  }

  /**
   * Duplicate checker passed to SubscriptionCodeEditor.
   * Returns "exact" | "soft" | null.
   * Called BEFORE the import result is shown.
   */
  function checkImportDuplicate(payload) {
    const addr = payload.destination_address || payload.sellers_wallet || payload.address;
    const sched = payload.schedule;
    const label = (payload.custom_label || "").toLowerCase().trim();
    const amount = String(payload.amount || "").trim();
    const currency = (payload.currency || "XMR").toUpperCase();
    // Exact: same address + schedule
    if (addr) {
      const exactMatch = subscriptions.some(s =>
        s.address && s.address === addr && (s._raw_schedule || s.schedule) === sched
      );
      if (exactMatch) return "exact";
    }
    // Soft: same label + amount + currency (catches mock-data duplicates)
    const softMatch = subscriptions.some(s =>
      s.label.toLowerCase().trim() === label &&
      String(s.amount).trim() === amount &&
      (s.currency || "XMR").toUpperCase() === currency
    );
    if (softMatch) return "soft";
    return null;
  }

  /** Import tab dispatches this after "Ajouter cet abonnement" */
  async function onSubImported({ detail }) {
    logInfo("Dashboard", "onSubImported fired");
    const sub = payloadToSubscription({ ...detail.payload, _sourceCode: detail.code });
    addOrConfirmDuplicate(sub);
    showImportModal = false;
    // Persist to SQLite
    if (detail.code) {
      const result = await tauriInvoke("add_subscription", { code: detail.code });
      if (result) {
        logInfo("Dashboard", `Subscription persisted to DB: ${result.id}`);
        // Update local sub with real DB id
        subscriptions = subscriptions.map(s =>
          s._code === detail.code ? { ...s, _db_id: result.id, id: result.id } : s
        );
      }
    }
  }

  /** "Créer un code" sharing mode — does NOT add to subscriptions */
  function onSubGenerated() {
    logInfo("Dashboard", "onSubGenerated: code sharing mode, no subscription added");
    // The share view is displayed inside the create-code page — nothing to close
  }

  let trayStatus = "green";

  // ---- RPC settings state ----
  let rpcStatus = "stopped";
  let rpcStatusLabel = "Arrêté";
  let rpcBinaryPath = null;
  let publicNodes = [];
  let rpcConfig = { binary_path: null, daemon_address: "", rpc_port: 18083, tolerance_pct: 20, send_logs_enabled: false };

  async function loadRpcState() {
    logInfo("RPC", "Fetching RPC status from backend…");
    const [status, binary, nodes] = await Promise.all([
      tauriInvoke("get_rpc_status"),
      tauriInvoke("detect_rpc_binary"),
      tauriInvoke("get_public_nodes"),
    ]);
    rpcStatus = (typeof status === "string" ? status : status?.toString?.() ?? "stopped").toLowerCase();
    rpcStatusLabel = rpcStatus === "running" ? "En marche" : rpcStatus === "starting" ? "Démarrage…" : rpcStatus === "stopped" ? "Arrêté" : "Erreur";
    rpcBinaryPath = binary;
    publicNodes = nodes ?? [];
    logInfo("RPC", `Status: ${JSON.stringify(status)}, binary: ${binary ?? "not found"}, nodes: ${nodes?.length ?? 0}`);
    if (!binary) {
      logWarn("RPC", "monero-wallet-rpc binary not found in PATH. Install the Monero CLI suite or set the path manually in Settings → RPC.");
    }
  }

  async function saveRpcConfig() {
    logInfo("RPC", `Saving config: node=${rpcConfig.daemon_address ?? "auto"}, port=${rpcConfig.rpc_port}`);
    const result = await tauriInvoke("save_rpc_config", { config: rpcConfig });
    if (result === null || result === undefined) logInfo("RPC", "Config saved OK");
    else logWarn("RPC", `save_rpc_config returned: ${JSON.stringify(result)}`);
    await loadRpcState();
  }

  // ---- Load persisted subscriptions from SQLite on mount ----
  // ---- Wallet / RPC state ----
  let balanceXmr = null;        // null = not loaded yet
  let balanceLoading = false;
  let lastBalanceRefresh = null; // Date of last successful balance fetch
  let refreshAgoLabel = "";      // "il y a X" string, updated every 30s
  let rpcOk = null;             // null=unknown, true=connected, false=error
  let rpcError = null;          // last error string
  let walletAddress = null;

  // Receive / QR state
  let copiedAddr = false;
  let sweepDest = "";
  let sweepLoading = false;
  let sweepError = null;
  let sweepTxHash = null;

  async function copyAddress(addr) {
    try {
      await navigator.clipboard.writeText(addr);
      copiedAddr = true;
      setTimeout(() => copiedAddr = false, 2000);
    } catch (e) {
      logWarn("Receive", `Clipboard error: ${e}`);
    }
  }

  // ---- Payment scheduler (frontend-driven) ----
  let paymentInProgress = false;

  // Monero 25-min minimum between sends
  const MONERO_MIN_INTERVAL_MS = 25 * 60 * 1000;
  let lastPaymentSentAt = null;

  async function checkDuePayments() {
    if (paymentInProgress) return;
    if (!walletAddress) return; // not unlocked
    // Enforce 25-min cooldown
    if (lastPaymentSentAt && Date.now() - lastPaymentSentAt < MONERO_MIN_INTERVAL_MS) {
      const remaining = Math.ceil((MONERO_MIN_INTERVAL_MS - (Date.now() - lastPaymentSentAt)) / 60000);
      logInfo("Scheduler", `Monero 25-min cooldown: ${remaining}min remaining`);
      return;
    }
    const now = new Date();
    const due = subscriptions.filter(s => {
      if (!["Active", "Actif"].includes(s.status)) return false;
      if (!s.next_payment_at) return false;
      return new Date(s.next_payment_at) <= now;
    });
    if (due.length === 0) return;

    logInfo("Scheduler", `${due.length} payment(s) due`);
    paymentInProgress = true;
    // Only send one payment per cycle (Monero 25-min rule)
    await processPayment(due[0]);
    paymentInProgress = false;
  }

  async function triggerNow(sub) {
    if (paymentInProgress) { alert("Un paiement est deja en cours"); return; }
    if (lastPaymentSentAt && Date.now() - lastPaymentSentAt < MONERO_MIN_INTERVAL_MS) {
      const remaining = Math.ceil((MONERO_MIN_INTERVAL_MS - (Date.now() - lastPaymentSentAt)) / 60000);
      alert(`Monero: attendre encore ${remaining} min (confirmations blocs)`);
      return;
    }
    paymentInProgress = true;
    logInfo("Scheduler", `Manual trigger for sub #${sub.id}`);
    await processPayment(sub);
    paymentInProgress = false;
  }

  async function processPayment(sub) {
    logInfo("Scheduler", `Processing payment for sub #${sub.id}`);
    try {
      const result = await tauriInvoke("send_payment", {
        destinationAddress: sub.destination_address || sub.address || "",
        amountXmr: parseFloat(sub.amount),
        subscriptionId: sub.id,
      });
      if (result?.tx_hash) {
        logInfo("Scheduler", `[OK] Paiement envoye pour sub #${sub.id} — tx: ${result.tx_hash.slice(0, 12)}...`);
        lastPaymentSentAt = Date.now();
        // Compute next occurrence from raw schedule
        const rawSched = sub._raw_schedule || sub.schedule;
        const next = computeNextOccurrence(rawSched, sub.repeat_every || 1);
        const nextIso = next instanceof Date ? next.toISOString() : new Date(next).toISOString();
        const idx = subscriptions.findIndex(s => s.id === sub.id);
        if (idx !== -1) {
          subscriptions[idx] = { ...subscriptions[idx], next_payment_at: nextIso, status: "Active", payments_sent: (sub.payments_sent || 0) + 1 };
          subscriptions = [...subscriptions];
        }
        await tauriInvoke("update_subscription_next_payment", {
          id: sub.id,
          nextPaymentAt: nextIso
        });
        await refreshBalance();
      } else {
        // RPC returned but no tx_hash — unexpected, just log
        logWarn("Scheduler", `[WARN] Reponse inattendue pour sub #${sub.id} — pas de tx_hash dans la reponse`);
      }
    } catch (e) {
      // Tauri command returned an Err() — log clearly, do NOT suspend the subscription
      logError("Scheduler", `[ECHEC] Paiement echoue pour sub #${sub.id} — ${e}`);
    }
  }

  function computeNextOccurrence(cronExpr, repeatEvery = 1) {
    // Parse CRON: sec min hour dom month dow (6-field) or min hour dom month dow (5-field)
    const now = new Date();
    const parts = String(cronExpr).trim().split(/\s+/);
    // Try to use croner if available, else fallback to +1 month
    try {
      // Simple built-in parser for common patterns
      const next = new Date(now);
      if (parts.length === 6) {
        const [, minP, hourP, domP, monP] = parts;
        if (minP.startsWith('*/')) {
          const every = parseInt(minP.slice(2));
          next.setMinutes(Math.ceil((now.getMinutes() + 1) / every) * every, 0, 0);
        } else if (domP !== '*') {
          next.setMonth(next.getMonth() + 1 * repeatEvery, parseInt(domP));
          next.setHours(parseInt(hourP) || 9, parseInt(minP) || 0, 0, 0);
        } else {
          next.setDate(next.getDate() + 1);
          next.setHours(parseInt(hourP) || 9, 0, 0, 0);
        }
      } else {
        next.setMonth(next.getMonth() + 1);
      }
      return next;
    } catch {
      const n = new Date(now);
      n.setMonth(n.getMonth() + 1);
      return n;
    }
  }

  async function doSweepAll() {
    sweepError = null;
    sweepTxHash = null;
    sweepLoading = true;
    logInfo("Sweep", "[SweepAll] Sending sweep_all transaction…");
    try {
      const hash = await tauriInvoke("sweep_all", { destination: sweepDest.trim() });
      if (hash) {
        sweepTxHash = hash;
        logInfo("Sweep", "[SweepAll] Done");
        // Refresh balance
        await tauriInvoke("refresh_balance");
        await loadWalletInfo();
      } else {
        sweepError = "Aucun hash retourné";
      }
    } catch (e) {
      sweepError = String(e);
      logError("Sweep", `[SweepAll] Error: ${e}`);
    } finally {
      sweepLoading = false;
    }
  }

  // QR code generation (returns SVG string)
  async function generateQR(address) {
    try {
      const QRCode = (await import("qrcode")).default;
      return await QRCode.toString(address, { type: "svg", margin: 2, width: 220 });
    } catch (e) {
      logWarn("QR", `QR generation failed: ${e}`);
      return `<svg width="220" height="220" viewBox="0 0 220 220" xmlns="http://www.w3.org/2000/svg"><rect width="220" height="220" fill="#eee"/><text x="110" y="115" text-anchor="middle" fill="#666" font-size="12">QR unavailable</text></svg>`;
    }
  }

  // RPC state → user-friendly diagnosis
  function rpcDiagnosis(err) {
    if (!err) return null;
    if (err.includes("word list") || err.includes("Electrum") || err.includes("checksum")) {
      return {
        icon: "🔴", level: "error",
        title: "Seed de récupération invalide",
        detail: "Votre phrase secrète a été créée avec une ancienne version bugguée. L'app tente de la corriger automatiquement au prochain déverrouillage.",
        action: "reset",
        actionLabel: "Réinitialiser le wallet",
      };
    }
    if (err.includes("not found") || err.includes("PATH") || err.includes("binary")) {
      return {
        icon: "🔴", level: "error",
        title: "Outil Monero introuvable",
        detail: "monero-wallet-rpc n'est pas installé ou n'est pas dans le PATH. Installez les outils Monero CLI.",
        action: "settings",
        actionLabel: "Configurer dans Paramètres",
        link: "https://getmonero.org/downloads/",
        linkLabel: "Télécharger Monero CLI",
      };
    }
    if (err.includes("timeout") || err.includes("not ready") || err.includes("attempt")) {
      return {
        icon: "🟠", level: "warn",
        title: "monero-wallet-rpc n'a pas démarré",
        detail: "Le processus s'est lancé mais n'a pas répondu dans les 15 secondes. Vérifiez qu'aucun autre processus n'utilise le port 18083.",
        action: "retry",
        actionLabel: "Réessayer",
      };
    }
    if (err.includes("connect") || err.includes("TCP") || err.includes("Connexion refusée")) {
      return {
        icon: "🟠", level: "warn",
        title: "Connexion au nœud Monero impossible",
        detail: "Impossible de joindre le nœud distant. Vérifiez votre connexion internet ou changez de nœud dans les Paramètres.",
        action: "settings",
        actionLabel: "Changer de nœud",
      };
    }
    if (err.includes("Task spawn") || err.includes("Internal task")) {
      return {
        icon: "🔴", level: "error",
        title: "Erreur interne",
        detail: "Une erreur inattendue s'est produite. Copiez les logs et signalez-la.",
        action: "logs",
        actionLabel: "Copier les logs",
      };
    }
    return {
      icon: "🟠", level: "warn",
      title: "RPC indisponible",
      detail: err.length > 120 ? err.slice(0, 120) + "…" : err,
      action: "settings",
      actionLabel: "Voir Paramètres",
    };
  }

  async function loadWalletInfo() {
    balanceLoading = true;
    try {
      const info = await tauriInvoke("get_wallet_info");
      if (info) {
        balanceXmr = info.balance_xmr ?? 0;
        rpcOk = info.rpc_connected;
        rpcError = info.rpc_last_error ?? null;
        walletAddress = info.address ?? null;
        logInfo("Dashboard", `Wallet info: rpc_ok=${rpcOk}, balance=${balanceXmr} XMR${rpcError ? ", error=" + rpcError.slice(0, 80) : ""}`);
      }
    } catch (e) {
      logWarn("Dashboard", `get_wallet_info failed: ${e}`);
    }
    balanceLoading = false;
  }

  function updateRefreshAgo() {
    if (!lastBalanceRefresh) { refreshAgoLabel = ""; return; }
    const secs = Math.round((Date.now() - lastBalanceRefresh) / 1000);
    if (secs < 60)        refreshAgoLabel = `il y a ${secs}s`;
    else if (secs < 3600) refreshAgoLabel = `il y a ${Math.round(secs/60)} min`;
    else                  refreshAgoLabel = `il y a ${Math.round(secs/3600)} h`;
  }

  async function refreshBalance() {
    balanceLoading = true;
    logInfo("Dashboard", "Refreshing balance via RPC…");
    try {
      const b = await tauriInvoke("refresh_balance");
      if (b) {
        balanceXmr = b.xmr;
        rpcOk = true;
        rpcError = null;
        lastBalanceRefresh = Date.now();
        updateRefreshAgo();
        logInfo("Dashboard", `Balance refreshed: ${b.xmr} XMR`);
      }
    } catch (e) {
      rpcOk = false;
      rpcError = String(e);
      logWarn("Dashboard", `Balance refresh failed: ${e}`);
    }
    balanceLoading = false;
  }

  onMount(async () => {
    await loadRpcState();
    logInfo("Dashboard", "Loading subscriptions from DB");
    const dbSubs = await tauriInvoke("get_subscriptions");
    if (dbSubs && dbSubs.length > 0) {
      const mapped = dbSubs.map(s => dbSubToUi(s));
      const existingKeys = new Set(subscriptions.map(duplicateKey).filter(Boolean));
      const newOnes = mapped.filter(s => {
        const k = duplicateKey(s);
        return k && !existingKeys.has(k);
      });
      if (newOnes.length > 0) {
        subscriptions = [...subscriptions, ...newOnes];
        logInfo("Dashboard", `Loaded ${newOnes.length} subscription(s) from DB`);
      }
    }

    // Load basic wallet info (address, cached balance) — fast, no sync
    await loadWalletInfo();
    updateRefreshAgo();

    // Immediately trigger a background sync without blocking the UI
    // (sync takes ~47s on first run — shows spinner in sidebar)
    refreshBalance(); // intentionally NOT awaited

    const balanceTimer = setInterval(refreshBalance, 2 * 60 * 1000);
    const agoTimer = setInterval(updateRefreshAgo, 30 * 1000);

    // Payment scheduler: check every minute for due subscriptions
    const paymentTimer = setInterval(checkDuePayments, 60 * 1000);

    return () => { clearInterval(balanceTimer); clearInterval(agoTimer); clearInterval(paymentTimer); };
  });

  /** Map a DB Subscription JSON to the UI subscription shape */
  function dbSubToUi(s) {
    return {
      id: s.id,
      label: s.label || "—",
      description: s.description || "",
      amount: String(s.amount || "0"),
      currency: s.currency || "XMR",
      schedule: scheduleToHuman(s.schedule, s.repeat_every || 1, $currentLang),
      next_payment_at: s.next_payment_at ? s.next_payment_at.slice(0, 10) : null,
      status: s.status === "active" ? "Active" : s.status === "paused" ? "Paused" : (s.status || "Active"),
      canary_status: s.canary_url ? (s.last_canary_status || "Ok") : "NotConfigured",
      payments_sent: s.payments_sent || 0,
      wallet_id: wallets[0]?.id || 1,
      address: s.destination_address || null,
      _raw_schedule: s.schedule || null,
      _db_id: s.id,   // keep original UUID for DB ops
    };
  }

  // ---- Logs panel state ----
  import { writable } from "svelte/store";
  let showLogs = false;
  let showSendLogs = false;
  let logFilter = "";
  // Reactive store backed by AppLogger
  const liveLogs = writable([]);
  subscribeToLogs(entries => liveLogs.set(entries));
  const sendLogsStore = writable(isSendLogsEnabled());

  // Mock wallets with monthly spend estimates
  let wallets = [
    { id: 1, name: "Wallet principal",   desc: "Médias & créateurs",       balance: "0.82", color: "#f26822", monthlyOut: 0.07 },
    { id: 2, name: "Wallet services",    desc: "VPN et hébergement",       balance: "1.21", color: "#2e7d32", monthlyOut: 0.04 },
  ];

  // Keep wallet[0] balance in sync with the live RPC balance
  $: if (balanceXmr !== null && wallets.length > 0) {
    wallets[0] = { ...wallets[0], balance: balanceXmr.toFixed(4) };
    wallets = wallets; // trigger reactivity
  }
  $: totalBalance = wallets.reduce((s, w) => s + parseFloat(w.balance), 0).toFixed(3);
  $: totalMonthlyOut = wallets.reduce((s, w) => s + (w.monthlyOut || 0), 0);

  // Computed from actual subscriptions (reactive)
  $: nextPayment = (() => {
    const active = subscriptions.filter(s =>
      ["Active", "Actif"].includes(s.status) && s.next_payment_at
    );
    if (!active.length) return null;
    active.sort((a, b) => new Date(a.next_payment_at) - new Date(b.next_payment_at));
    const soonest = active[0];
    const ms = new Date(soonest.next_payment_at) - Date.now();
    const days = Math.ceil(ms / 86400000);
    const h = Math.floor(ms / 3600000);
    const m = Math.floor(ms / 60000);
    let label_time;
    if (m < 60) label_time = `dans ${m} min`;
    else if (h < 24) label_time = `dans ${h}h`;
    else label_time = days <= 0 ? "maintenant" : `J-${days}`;
    return { label: soonest.label, amount: soonest.amount, in_days: days, label_time, sub: soonest };
  })();

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
            <span class="tray-label mode-tray-label">{($t.paymentModeLabel || 'Mode : {mode}').replace('{mode}', $paymentMode === 'semi-auto' ? ($t.paymentModeSemiAuto || 'Semi-auto') : ($t.paymentModeAuto || 'Auto'))}</span>
          </div>
        </div>
      </div>
    </div>

    <nav class="sidebar-nav">
      <!-- No emojis except Trouver des souscriptions -->
      <button class="nav-item" class:active={page === "subscriptions"} on:click={() => page = "subscriptions"}>
        {$t.navSubscriptions}
      </button>
      <button class="nav-item" class:active={page === "receive"} on:click={() => page = "receive"}>
        {$t.receiveTitle || 'Recevoir XMR'}
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
      <button class="nav-item" class:active={page === "create-code"} on:click={() => page = "create-code"}>
        🔗 Créer un code
      </button>
      <button class="nav-item" class:active={page === "settings"} on:click={() => page = "settings"}>
        {$t.navSettings}
      </button>
    </nav>

    <!-- Balance card: arbre complet des états RPC -->
    <div class="sidebar-balance card">
      <div class="balance-label">
        <!-- RPC status dot -->
        {#if rpcOk === null}
          <span class="rpc-dot dot-unknown" title="Vérification en cours…">●</span>
        {:else if rpcOk}
          <span class="rpc-dot dot-ok" title="Connecté au réseau Monero">●</span>
        {:else}
          <span class="rpc-dot dot-err" title="RPC indisponible">●</span>
        {/if}
        {$t.balance || 'Solde'}
        <button class="refresh-btn" on:click={refreshBalance} disabled={balanceLoading} title={$t.balanceRefresh || 'Actualiser le solde'}>
          <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5"
               stroke-linecap="round" stroke-linejoin="round"
               style={balanceLoading ? "animation: spin 1s linear infinite" : ""}>
            <polyline points="23 4 23 10 17 10"/>
            <path d="M20.49 15a9 9 0 1 1-2.12-9.36L23 10"/>
          </svg>
        </button>
      </div>
      {#if balanceLoading}
        <div class="refresh-ago sync-pulse">Synchro réseau en cours…</div>
      {:else if refreshAgoLabel}
        <div class="refresh-ago">{refreshAgoLabel} · ↺ 2 min</div>
      {/if}

        {#if balanceXmr === null}
        <div class="balance-amount" style="font-size:13px;color:var(--text-muted)">{$t.balanceLoading || 'Chargement…'}</div>

      {:else if rpcOk}
        <!-- Connected: just show the balance -->
        <div class="balance-amount text-accent">{balanceXmr.toFixed(4)} {$currentNetwork.symbol}</div>
        {#if balanceXmr === 0}
          <div class="balance-hint">{$t.balanceEmpty || 'Wallet vide — rechargez-le en Monero pour commencer.'}</div>
          <button class="btn-receive" on:click={() => page = "receive"}>⬇ Recevoir XMR</button>
        {/if}

      {:else if rpcOk === false}
        <!-- RPC failed: show balance anyway + diagnosis -->
        <div class="balance-amount" style="color:var(--text-muted)">{balanceXmr.toFixed(4)} {$currentNetwork.symbol}</div>
        {#if rpcError}
          {@const diag = rpcDiagnosis(rpcError)}
          {#if diag}
            <div class="rpc-diag rpc-diag-{diag.level}">
              <div class="rpc-diag-title">{diag.icon} {diag.title}</div>
              <div class="rpc-diag-detail">{diag.detail}</div>
              <div class="rpc-diag-actions">
                {#if diag.action === "settings"}
                  <button class="rpc-diag-btn" on:click={() => page = "settings"}>⚙️ {diag.actionLabel}</button>
                {:else if diag.action === "retry"}
                  <button class="rpc-diag-btn" on:click={refreshBalance} disabled={balanceLoading}>↻ {diag.actionLabel}</button>
                {:else if diag.action === "reset"}
                  <button class="rpc-diag-btn rpc-diag-btn-danger">⚠️ {diag.actionLabel}</button>
                {/if}
                {#if diag.link}
                  <a href={diag.link} class="rpc-diag-link" target="_blank">{diag.linkLabel} ↗</a>
                {/if}
              </div>
            </div>
          {/if}
        {:else}
          <div class="balance-hint">Solde non actualisé — <button class="btn-inline" on:click={refreshBalance}>Réessayer</button></div>
        {/if}

      {:else}
        <!-- rpcOk === null: never tried (fresh launch before unlock) -->
        <div class="balance-amount text-muted" style="font-size:13px">—</div>
      {/if}
    </div>
  </aside>

  <!-- Main content -->
  <main class="content">

    <!-- ====== SUBSCRIPTIONS ====== -->
    {#if page === "subscriptions"}
      <div class="page-header">
        <div>
          <h1>{($t.subscriptionsTitle || 'Mes abonnements récurrents en {network}').replace('{network}', $currentNetwork.label)}</h1>
          <p class="subs-subtitle">
            {#if subscriptions.filter(s => !["Cancelled","Expired"].includes(s.status)).length === 1}
              {($t.subscriptionsCount || '{n} abonnement').replace('{n}', 1)}
            {:else}
              {($t.subscriptionsCountPlural || '{n} abonnements').replace('{n}', subscriptions.filter(s => !["Cancelled","Expired"].includes(s.status)).length)}
            {/if}
            {#if duplicateCount > 0}
              &nbsp;·&nbsp;<span class="dup-warning">Doublons : {duplicateCount}</span>
            {/if}
          </p>
        </div>
        <div class="page-actions">
          <button class="btn-ghost" on:click={() => page = "find"}>{$t.findSubscriptions}</button>
          <button class="btn-ghost" on:click={() => openImportModal()}>📥 {$t.importSubscription}</button>
          <button class="btn-primary" on:click={openNewSubModal}>＋ {$t.newSubscription}</button>
        </div>
      </div>

      {#if nextPayment}
      <div class="next-banner card">
        <span class="text-muted">{$t.nextPayment}</span>
        <strong>{nextPayment.label}</strong>
        <span>{nextPayment.amount} {$currentNetwork.symbol}</span>
        <span class="text-muted">{nextPayment.label_time}</span>
      </div>
      {/if}

      <!-- Refill estimate banner -->
      <div class="refill-banner card">
        <span class="text-muted">{$t.nextRefillLabel} :</span>
        <strong>{$t.refillIn} {totalRefillText($t)}</strong>
        <span class="text-muted refill-hint">({totalMonthlyOut.toFixed(3)} {$currentNetwork.symbol} {$t.walletMonthlyOut})</span>
        <!-- svelte-ignore a11y-click-events-have-key-events a11y-no-static-element-interactions -->
        <span class="refill-help-link" role="link" tabindex="0"
          on:click={() => openExternal("https://libre-market.com/m/epicerie/tutorial")}
          on:keydown={e => e.key === "Enter" && openExternal("https://libre-market.com/m/epicerie/tutorial")}>
          Comment se procurer du Monero ?
        </span>
      </div>

      <SubscriptionList
        {wallets}
        {subscriptions}
        on:delete-sub={async e => {
          const sub = subscriptions.find(s => s.id === e.detail.id);
          subscriptions = subscriptions.filter(s => s.id !== e.detail.id);
          if (sub?._db_id) await tauriInvoke("delete_subscription", { id: sub._db_id });
        }}
        on:toggle-pause={async e => {
          subscriptions = subscriptions.map(s => s.id === e.detail.id
            ? { ...s, status: s.status === "Paused" ? "Active" : "Paused" }
            : s);
          const sub = subscriptions.find(s => s.id === e.detail.id);
          if (sub?._db_id) {
            await tauriInvoke("set_subscription_status", { id: sub._db_id, paused: sub.status === "Paused" });
          }
        }}
        on:trigger-now={e => triggerNow(e.detail.sub)}
        on:edit-sub={e => openEditSubModal(e.detail.sub)}
      />
      <!-- Footer marketing + bottom spacer -->
      <div class="sub-footer">
        <div class="sub-footer-grid">
          <div class="sub-footer-item">
            <svg class="sub-footer-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">
              <rect x="3" y="11" width="18" height="11" rx="2" ry="2"/><path d="M7 11V7a5 5 0 0 1 10 0v4"/>
            </svg>
            <div>
              <strong>Vie privée maximale</strong>
              <p>Aucune donnée envoyée à l'extérieur par défaut. Zéro tracking, zéro analytics.</p>
            </div>
          </div>
          <div class="sub-footer-item">
            <svg class="sub-footer-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">
              <circle cx="12" cy="12" r="10"/><path d="M12 8v4l3 3"/>
            </svg>
            <div>
              <strong>Anonymat Monero</strong>
              <p>Transactions confidentielles par protocole. Montants et identités masqués.</p>
            </div>
          </div>
          <div class="sub-footer-item">
            <svg class="sub-footer-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">
              <path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z"/>
            </svg>
            <div>
              <strong>Seed chiffrée post-quantique</strong>
              <p>Argon2id + AES-256-GCM. Résistant aux attaques quantiques futures.</p>
            </div>
          </div>
          <div class="sub-footer-item">
            <svg class="sub-footer-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">
              <polyline points="16 18 22 12 16 6"/><polyline points="8 6 2 12 8 18"/>
            </svg>
            <div>
              <strong>Open source</strong>
              <p>Code auditable. Pas de tiers de confiance. Vous contrôlez vos clés.</p>
            </div>
          </div>
        </div>
        <p class="sub-footer-version">Crypto AutoPay v0.2 · 2026 · open source · <span class="sub-footer-tagline">Vos paiements, vos règles.</span></p>
      </div>

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

      <!-- RPC / Wallet section -->
      <h2 class="section-title">Wallet RPC <span class="badge-v2">v0.2</span></h2>
      <div class="settings-section card">
        <div class="rpc-status-row">
          <span class="setting-label">Statut wallet-rpc</span>
          <span class="rpc-badge rpc-{rpcStatus}">{rpcStatusLabel}</span>
        </div>
        {#if rpcBinaryPath}
          <p class="rpc-info">Binaire détecté : <code>{rpcBinaryPath}</code></p>
        {:else}
          <p class="rpc-warn">
            ⚠ <code>monero-wallet-rpc</code> introuvable dans PATH.<br>
            <a class="rpc-link" href="https://www.getmonero.org/downloads/" on:click|preventDefault={() => openExternal("https://www.getmonero.org/downloads/")}>
              Télécharger Monero CLI →
            </a>
            ou renseigner le chemin ci-dessous.
          </p>
          <label class="field" style="margin-top:8px">
            <span>Chemin du binaire (optionnel)</span>
            <input type="text" bind:value={rpcConfig.binary_path} placeholder="/usr/bin/monero-wallet-rpc" />
          </label>
        {/if}

        <label class="field" style="margin-top:10px">
          <span>Nœud Monero public</span>
          <select bind:value={rpcConfig.daemon_address} class="setting-select" style="width:100%">
            <option value="">Automatique (premier nœud disponible)</option>
            {#each publicNodes as node}
              <option value={node}>{node}</option>
            {/each}
          </select>
        </label>

        <label class="field" style="margin-top:10px">
          <span>Tolérance paiement <strong>{rpcConfig.tolerance_pct}%</strong>
            <span class="text-muted" style="font-size:10px"> — ex: 20% = ±6j sur mensuel</span>
          </span>
          <input type="range" min="5" max="50" step="5" bind:value={rpcConfig.tolerance_pct} />
        </label>

        <button class="btn-primary btn-sm" style="margin-top:10px;width:fit-content" on:click={saveRpcConfig}>
          Enregistrer la config RPC
        </button>
      </div>

      <!-- Logs section in Settings -->
      <h2 class="section-title">Journaux techniques</h2>
      <div class="settings-section card">
        <p class="log-security-note">
          ⚠ Ces journaux ne contiennent aucun montant, adresse ou donnée personnelle.
          Ils sont stockés uniquement en RAM et effacés à chaque redémarrage.
        </p>
        <div class="log-toolbar">
          <select bind:value={logFilter} class="setting-select log-filter-select">
            <option value="">Tout</option>
            <option value="info">Info</option>
            <option value="warn">Avertissements</option>
            <option value="error">Erreurs</option>
          </select>
          <button class="btn-ghost btn-sm" on:click={() => showLogs = !showLogs}>
            {showLogs ? "Masquer" : "Afficher les logs"}
          </button>
          <button class="btn-ghost btn-sm" on:click={clearLogs}>Effacer</button>
        </div>
        {#if showLogs}
          <div class="log-list">
            {#each $liveLogs.filter(e => !logFilter || e.level === logFilter) as entry (entry.t + entry.msg)}
              <div class="log-row log-{entry.level}">
                <span class="log-time">{entry.t.slice(11,19)}</span>
                <span class="log-module">[{entry.module}]</span>
                <span class="log-msg">{entry.msg}</span>
              </div>
            {:else}
              <p class="text-muted" style="font-size:12px;padding:8px">Aucun log.</p>
            {/each}
          </div>
        {/if}
      </div>

      <!-- Send logs (opt-in) -->
      <h2 class="section-title">Journaux d'envoi <span class="badge-opt">opt-in</span></h2>
      <div class="settings-section card">
        <p class="log-security-note">
          📋 Trace des envois de paiements — utile comme preuve en cas de réclamation.<br>
          <strong>Contient uniquement l'identifiant interne et la date</strong> — aucun montant, aucun destinataire.<br>
          Persisté chiffré en localStorage. Désactivé par défaut.
        </p>
        <div class="setting-row">
          <span class="setting-label">Activer les logs d'envoi</span>
          <label class="toggle">
            <input type="checkbox"
              checked={$sendLogsStore}
              on:change={e => { setSendLogsEnabled(e.target.checked); sendLogsStore.set(e.target.checked); }}
            />
            <span class="slider"></span>
          </label>
        </div>
        {#if $sendLogsStore}
          <div class="log-toolbar">
            <button class="btn-ghost btn-sm" on:click={() => showSendLogs = !showSendLogs}>
              {showSendLogs ? "Masquer" : "Afficher"}
            </button>
            <button class="btn-ghost btn-sm" on:click={clearSendLogs}>Effacer</button>
          </div>
          {#if showSendLogs}
            <div class="log-list">
              {#each getSendLogs() as entry (entry.t + entry.id)}
                <div class="log-row log-info">
                  <span class="log-time">{entry.t.slice(0,19).replace("T"," ")}</span>
                  <span class="log-module">[sub#{entry.id}]</span>
                  <span class="log-msg">{entry.event}</span>
                </div>
              {:else}
                <p class="text-muted" style="font-size:12px;padding:8px">Aucun log d'envoi.</p>
              {/each}
            </div>
          {/if}
        {/if}
      </div>

    <!-- ====== CREATE CODE (full page, merchant / influencer) ====== -->
    {:else if page === "create-code"}
      <div class="page-header">
        <h1>Créer un code AutoPay de partage</h1>
        <p class="subs-subtitle">Pour marchands, créateurs et influenceurs — à diffuser à vos abonnés</p>
      </div>
      <div class="create-code-page">
        <SubscriptionCodeEditor
          mode="create"
          compact={false}
          lang={$currentLang}
          on:generated={onSubGenerated}
        />
      </div>

    <!-- ====== RECEIVE XMR ====== -->
    {:else if page === "receive"}
      <div class="page-header">
          <h1>{$t.receivePageTitle || 'Recevoir des XMR'}</h1>
        <p class="subs-subtitle">{$t.receiveDesc || 'Partagez cette adresse ou ce QR code pour recevoir des paiements Monero.'}</p>
      </div>
      <div class="receive-page">
        {#if walletAddress}
          <div class="qr-wrapper">
            {#await generateQR(walletAddress) then svgStr}
              <!-- eslint-disable-next-line svelte/no-at-html-tags -->
              {@html svgStr}
            {/await}
          </div>
          <div class="receive-address-box">
            <span class="receive-address">{walletAddress}</span>
            <button class="btn-copy" on:click={() => copyAddress(walletAddress)}>
              {copiedAddr ? ($t.receiveCopied || 'Copié !') : ($t.receiveCopy || 'Copier l\'adresse')}
            </button>
          </div>

          <!-- Sweep section (only when balance > 0) -->
          {#if balanceXmr !== null && balanceXmr > 0}
            <div class="sweep-section">
              <h2>{$t.sweepTitle || 'Vider le wallet'}</h2>
              <p class="sweep-warn">{$t.sweepWarning || '⚠️ Cette action est irréversible.'}</p>
              <p class="subs-subtitle">{$t.sweepDesc || 'Envoyer tous les fonds disponibles vers une autre adresse Monero.'}</p>
              <label class="sweep-label">{$t.sweepDestLabel || 'Adresse de destination'}</label>
              <input
                class="sweep-input"
                type="text"
                bind:value={sweepDest}
                placeholder={$t.sweepDestPlaceholder || '4... (adresse Monero)'}
              />
              {#if sweepError}
                <div class="sweep-error">{$t.sweepError || 'Erreur'} : {sweepError}</div>
              {/if}
              {#if sweepTxHash}
                <div class="sweep-success">{$t.sweepSuccess || 'Transaction envoyée !'} <code>{sweepTxHash}</code></div>
              {/if}
              <button
                class="btn-danger"
                on:click={doSweepAll}
                disabled={sweepLoading || sweepDest.trim().length < 90}
              >
                {sweepLoading ? 'Envoi en cours…' : ($t.sweepConfirm || 'Vider le wallet')}
              </button>
            </div>
          {/if}
        {:else}
          <div class="balance-hint">Adresse non disponible — déverrouillez le wallet d'abord.</div>
        {/if}
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

<!-- ===== Duplicate confirmation popup ===== -->
{#if showDuplicatePopup && pendingDuplicateSub}
  <!-- svelte-ignore a11y-click-events-have-key-events a11y-no-static-element-interactions -->
  <div class="dup-overlay" on:click={rejectDuplicate}>
    <div class="dup-popup card" on:click|stopPropagation>
      <div class="dup-icon">⚠️</div>
      <p class="dup-title">Abonnement similaire déjà présent</p>
      <p class="dup-label"><strong>{pendingDuplicateSub.label}</strong></p>
      <p class="dup-question text-muted">Ajouter quand même ce doublon ?</p>

      <!-- Pie countdown timer -->
      <svg class="dup-pie" viewBox="0 0 48 48" width="48" height="48">
        <circle cx="24" cy="24" r="{DUP_RADIUS}" fill="none" stroke="var(--border)" stroke-width="4"/>
        <circle
          cx="24" cy="24" r="{DUP_RADIUS}" fill="none"
          stroke="var(--accent)" stroke-width="4"
          stroke-dasharray="{DUP_CIRC}"
          stroke-dashoffset="{dupDashOffset}"
          stroke-linecap="round"
          transform="rotate(-90 24 24)"
          style="transition: stroke-dashoffset 0.05s linear"
        />
        <text x="24" y="28" text-anchor="middle" font-size="11" fill="var(--text-muted)">{Math.ceil(dupTimeLeft)}</text>
      </svg>

      <div class="dup-actions">
        <button class="btn-primary dup-no" on:click={rejectDuplicate}>
          Non (défaut)
        </button>
        <button class="btn-ghost" on:click={acceptDuplicate}>
          Oui, ajouter
        </button>
      </div>
    </div>
  </div>
{/if}

<!-- ===== "Nouvel abonnement" modal (manual form, mode="new") ===== -->
{#if showNewSubModal}
  <!-- svelte-ignore a11y-click-events-have-key-events a11y-no-noninteractive-element-interactions -->
  <div class="modal-backdrop" on:click={() => { showNewSubModal = false; editingSubData = null; }} role="dialog" aria-modal="true">
    <!-- svelte-ignore a11y-click-events-have-key-events a11y-no-static-element-interactions -->
    <div class="modal-panel" on:click|stopPropagation role="document">
      <div class="modal-header">
        <h2 class="modal-title">{editingSubData ? "Modifier l'abonnement" : "Nouvel abonnement"}</h2>
        <button class="modal-close" on:click={() => { showNewSubModal = false; editingSubData = null; }} aria-label="Fermer">✕</button>
      </div>
      <div class="modal-body">
        <SubscriptionCodeEditor
          mode="new"
          compact={true}
          lang={$currentLang}
          editData={editingSubData}
          on:new-sub={editingSubData ? onSubEdit : onSubNew}
          on:close={() => { showNewSubModal = false; editingSubData = null; }}
        />
      </div>
    </div>
  </div>
{/if}

<!-- ===== "Importer un code autopay" modal (mode="import") ===== -->
{#if showImportModal}
  <!-- svelte-ignore a11y-click-events-have-key-events a11y-no-noninteractive-element-interactions -->
  <div class="modal-backdrop" on:click={() => showImportModal = false} role="dialog" aria-modal="true">
    <!-- svelte-ignore a11y-click-events-have-key-events a11y-no-static-element-interactions -->
    <div class="modal-panel" on:click|stopPropagation role="document">
      <div class="modal-header">
        <h2 class="modal-title">Importer un code AutoPay</h2>
        <button class="modal-close" on:click={() => showImportModal = false} aria-label="Fermer">✕</button>
      </div>
      <div class="modal-body">
        <SubscriptionCodeEditor
          mode="import"
          initialCode={importInitialCode}
          compact={true}
          lang={$currentLang}
          duplicateChecker={checkImportDuplicate}
          on:imported={onSubImported}
          on:close={() => showImportModal = false}
        />
      </div>
    </div>
  </div>
{/if}

<style>
  .layout {
    display: flex;
    flex-direction: row;   /* explicit — WebKitGTK sometimes needs this */
    align-items: stretch;
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
  .balance-label { font-size: 9px; text-transform: uppercase; letter-spacing: .07em; color: var(--text-muted); margin-bottom: 2px; display: flex; align-items: center; gap: 4px; }
  .refresh-btn { background: none; border: none; cursor: pointer; color: var(--text-muted); padding: 0; display: flex; align-items: center; }
  .refresh-btn:hover { color: var(--accent); }
  .refresh-btn:disabled { opacity: 0.4; cursor: default; }
  .refresh-ago { font-size: 9px; color: var(--text-muted); opacity: 0.7; margin-bottom: 4px; }
  .sync-pulse { color: var(--accent) !important; opacity: 1 !important; animation: pulse-dot 1.5s ease-in-out infinite; }
  .balance-amount { font-size: 16px; font-weight: 800; margin-bottom: 4px; }
  .balance-hint { font-size: 10px; color: var(--text-muted); margin-bottom: 4px; }
  .btn-inline { background: none; border: none; color: var(--accent); cursor: pointer; font-size: 10px; padding: 0; text-decoration: underline; }

  /* Receive page */
  .receive-page { max-width: 520px; margin: 0 auto; display: flex; flex-direction: column; align-items: center; gap: 20px; padding-top: 12px; }
  .qr-wrapper { background: #fff; padding: 16px; border-radius: 12px; box-shadow: 0 2px 12px rgba(0,0,0,.12); }
  .qr-wrapper svg { display: block; }
  .receive-address-box { display: flex; flex-direction: column; align-items: center; gap: 10px; width: 100%; }
  .receive-address { font-family: monospace; font-size: 12px; word-break: break-all; text-align: center; color: var(--text-muted); padding: 8px 12px; background: var(--surface); border-radius: 6px; border: 1px solid var(--border); width: 100%; box-sizing: border-box; }
  .btn-copy { padding: 8px 18px; background: var(--accent); color: #fff; border: none; border-radius: 7px; font-size: 13px; cursor: pointer; }
  .btn-copy:hover { opacity: 0.88; }
  .btn-receive { margin-top: 4px; padding: 6px 14px; font-size: 12px; background: var(--accent); color: #fff; border: none; border-radius: 6px; cursor: pointer; width: 100%; }
  .btn-receive:hover { opacity: 0.88; }
  .sweep-section { width: 100%; border-top: 1px solid var(--border); padding-top: 20px; display: flex; flex-direction: column; gap: 8px; }
  .sweep-section h2 { margin: 0; font-size: 15px; }
  .sweep-warn { font-size: 12px; color: #f59e0b; margin: 0; }
  .sweep-label { font-size: 12px; color: var(--text-muted); }
  .sweep-input { width: 100%; padding: 9px 12px; border: 1px solid var(--border); border-radius: 7px; font-family: monospace; font-size: 12px; box-sizing: border-box; background: var(--surface); color: var(--text); }
  .sweep-error { color: #ef4444; font-size: 12px; }
  .sweep-success { color: #22c55e; font-size: 12px; word-break: break-all; }
  .btn-danger { padding: 10px 18px; background: #ef4444; color: #fff; border: none; border-radius: 7px; font-size: 13px; cursor: pointer; font-weight: 600; }
  .btn-danger:disabled { opacity: 0.5; cursor: not-allowed; }
  .btn-danger:not(:disabled):hover { background: #dc2626; }

  /* RPC dot indicator */
  .rpc-dot { font-size: 10px; line-height: 1; }
  .dot-unknown { color: #9ca3af; }
  .dot-ok { color: #22c55e; }
  .dot-err { color: #f59e0b; animation: pulse-dot 2s ease-in-out infinite; }
  @keyframes pulse-dot { 0%,100% { opacity:1; } 50% { opacity:0.4; } }

  /* RPC diagnosis panel */
  .rpc-diag {
    margin-top: 4px; border-radius: 6px; padding: 7px 9px;
    font-size: 10px; line-height: 1.45;
    border-left: 3px solid;
  }
  .rpc-diag-warn { background: #fffbeb; border-color: #f59e0b; color: #92400e; }
  .rpc-diag-error { background: #fef2f2; border-color: #ef4444; color: #7f1d1d; }
  .rpc-diag-title { font-weight: 700; margin-bottom: 3px; }
  .rpc-diag-detail { opacity: 0.85; margin-bottom: 6px; }
  .rpc-diag-actions { display: flex; flex-wrap: wrap; gap: 5px; }
  .rpc-diag-btn {
    border: 1px solid currentColor; border-radius: 4px; background: none;
    color: inherit; font-size: 10px; padding: 2px 7px; cursor: pointer; opacity: 0.9;
  }
  .rpc-diag-btn:hover { opacity: 1; background: #0001; }
  .rpc-diag-btn-danger { border-color: #ef4444; color: #ef4444; }
  .rpc-diag-link { font-size: 10px; color: inherit; opacity: 0.75; }

  @keyframes spin { to { transform: rotate(360deg); } }
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
  .refill-banner { font-size: 12px; opacity: 0.85; flex-wrap: wrap; }
  .refill-hint { font-size: 11px; }
  .refill-help-link {
    margin-left: auto; font-size: 11px;
    color: var(--accent); text-decoration: none; white-space: nowrap;
  }
  .refill-help-link:hover { text-decoration: underline; }

  /* Subscription page header subtitle */
  .subs-subtitle { font-size: 12px; color: var(--text-muted); margin-top: 2px; }
  .dup-warning { color: #f59e0b; font-weight: 600; }

  /* Duplicate confirmation popup */
  .dup-overlay {
    position: fixed; inset: 0; z-index: 200;
    display: flex; align-items: center; justify-content: center;
    background: #0006; backdrop-filter: blur(2px);
  }
  .dup-popup {
    max-width: 280px; width: 90%;
    padding: 1.5rem 1.25rem;
    display: flex; flex-direction: column; align-items: center; gap: 10px;
    text-align: center;
    animation: pop-in 0.18s cubic-bezier(.34,1.56,.64,1);
  }
  @keyframes pop-in { from { transform: scale(0.85); opacity: 0; } to { transform: scale(1); opacity: 1; } }
  .dup-icon { font-size: 1.8rem; }
  .dup-title { font-weight: 700; font-size: 0.95rem; }
  .dup-label { font-size: 0.9rem; }
  .dup-question { font-size: 0.82rem; }
  .dup-pie { display: block; margin: 4px auto; }
  .dup-actions { display: flex; gap: 8px; margin-top: 4px; flex-wrap: wrap; justify-content: center; }
  .dup-no { background: var(--accent) !important; }

  /* Logs panel */
  .log-security-note {
    font-size: 11px; color: var(--text-muted); padding: 6px 0 10px;
    border-bottom: 1px solid var(--border); margin-bottom: 8px;
  }
  .log-toolbar { display: flex; gap: 6px; align-items: center; flex-wrap: wrap; margin-bottom: 8px; }
  .log-filter-select { font-size: 11px; padding: 3px 6px; border-radius: 4px; border: 1px solid var(--border); background: var(--bg); color: var(--text); }
  .log-list {
    max-height: 240px; overflow-y: auto;
    font-family: monospace; font-size: 11px;
    background: var(--bg-sidebar); border-radius: 6px; padding: 4px;
  }
  .log-row { display: flex; gap: 6px; padding: 2px 4px; border-radius: 3px; line-height: 1.5; }
  .log-row:hover { background: var(--border); }
  .log-time { color: var(--text-muted); white-space: nowrap; flex-shrink: 0; }
  .log-module { color: var(--accent); white-space: nowrap; flex-shrink: 0; }
  .log-msg { word-break: break-all; }
  .log-info .log-module { color: #3b82f6; }
  .log-warn .log-module { color: #f59e0b; }
  .log-warn { background: #f59e0b10; }
  .log-error .log-module { color: #ef4444; }
  .log-error { background: #ef444410; }
  .badge-opt { font-size: 10px; background: #f59e0b22; color: #b45309; padding: 1px 5px; border-radius: 4px; vertical-align: middle; }
  .badge-v2 { font-size: 10px; background: #3b82f622; color: #1d4ed8; padding: 1px 5px; border-radius: 4px; vertical-align: middle; }

  /* RPC section */
  .rpc-status-row { display: flex; align-items: center; gap: 10px; }
  .rpc-badge { font-size: 11px; font-weight: 700; padding: 2px 8px; border-radius: 10px; }
  .rpc-running { background: #22c55e20; color: #15803d; }
  .rpc-stopped { background: var(--border); color: var(--text-muted); }
  .rpc-starting { background: #f59e0b20; color: #b45309; }
  .rpc-error { background: #ef444420; color: #dc2626; }
  .rpc-info { font-size: 11px; color: var(--text-muted); margin: 6px 0 0; }
  .rpc-info code { background: var(--bg-sidebar); padding: 1px 4px; border-radius: 3px; }
  .rpc-warn { font-size: 11.5px; color: #b45309; background: #fef3c7; padding: 8px 10px; border-radius: 6px; margin: 6px 0 0; line-height: 1.5; }
  .rpc-link { color: var(--accent); }

  /* Create-code full page */
  .create-code-page { padding: 0 2px; max-width: 640px; }

  /* Sub-footer marketing */
  .sub-footer {
    margin-top: 28px; padding: 20px 16px 28px;
    border-top: 1px solid var(--border);
  }
  .sub-footer-grid {
    display: grid; grid-template-columns: repeat(2, 1fr); gap: 14px;
    margin-bottom: 14px;
  }
  @media (max-width: 520px) { .sub-footer-grid { grid-template-columns: 1fr; } }
  .sub-footer-item {
    display: flex; gap: 10px; align-items: flex-start;
    font-size: 11.5px; line-height: 1.5;
  }
  .sub-footer-icon { width: 20px; height: 20px; flex-shrink: 0; margin-top: 1px; color: var(--accent); }
  .sub-footer-item strong { font-size: 12px; display: block; margin-bottom: 2px; }
  .sub-footer-item p { margin: 0; color: var(--text-muted); }
  .sub-footer-version {
    font-size: 10.5px; color: var(--text-muted); text-align: center; margin: 0;
    border-top: 1px solid var(--border); padding-top: 10px;
  }
  .sub-footer-tagline { font-style: italic; color: var(--accent); }

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
  .mode-tray-label { font-size: 10px; color: var(--text-muted); }

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
