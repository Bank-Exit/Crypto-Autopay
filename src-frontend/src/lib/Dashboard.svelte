<script>
  import { tick, onMount, createEventDispatcher } from "svelte";
  const dispatch = createEventDispatcher();
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

  async function tauriListen(event, cb) {
    try {
      const { listen } = await import("@tauri-apps/api/event");
      return await listen(event, cb);
    } catch (_) { return () => {}; }
  }

  // Payment loader + success animation
  let showPayAnim = false;
  let showPayLoader = false;
  let payLoaderCountdown = 15;
  let payLoaderTimer = null;
  let payAnimTimer = null;

  function startPaymentLoader() {
    showPayLoader = true;
    showPayAnim = false;
    payLoaderCountdown = 15;
    if (payLoaderTimer) clearInterval(payLoaderTimer);
    payLoaderTimer = setInterval(() => {
      payLoaderCountdown = Math.max(0, payLoaderCountdown - 1);
    }, 1000);
  }

  function showPaymentAnimation() {
    // Stop loader, show success
    if (payLoaderTimer) { clearInterval(payLoaderTimer); payLoaderTimer = null; }
    showPayLoader = false;
    showPayAnim = true;
    if (payAnimTimer) clearTimeout(payAnimTimer);
    payAnimTimer = setTimeout(() => { showPayAnim = false; }, 4000);
  }

  /** Resolve alert → subscription label (from live subscriptions list, or raw msg as fallback) */
  function subLabelForAlert(alert) {
    if (alert.sub_id) {
      const found = subscriptions.find(s => s.id === alert.sub_id || s._db_id === alert.sub_id);
      if (found) return found.label;
    }
    // Try to extract a non-UUID token from the raw message
    const clean = alert.message.replace(/Sub #[0-9a-f-]{36}/gi, '').trim();
    return clean.replace(/[—–-].*$/, '').trim() || null;
  }

  /** Translate Rust alert kind → human message in current language */
  function translateAlertMsg(alert, tObj) {
    // Extract numeric params from Rust message (e.g. "~3 jour(s)" → 3)
    const numMatch = alert.message.match(/(\d+(?:\.\d+)?)/);
    const num = numMatch ? Math.round(parseFloat(numMatch[1])) : null;
    const threshMatch = alert.message.match(/seuil\s*:\s*(\d+)/);
    const thresh = threshMatch ? parseInt(threshMatch[1]) : 7;
    switch (alert.kind) {
      case 'BALANCE_LOW':
        if (num !== null && num < 1) return tObj.alertBalanceLowSoon || `${tObj.refillLessThanOneDay || 'moins de 1 jour'} (${tObj.alertThreshold || 'seuil'}: ${thresh}j)`;
        return (tObj.alertBalanceLow || 'Portefeuille vide dans ~{n} jour(s) (seuil: {t}j)').replace('{n}', num ?? '?').replace('{t}', thresh);
      case 'BALANCE_EMPTY': return tObj.alertBalanceEmpty || 'Solde nul — les paiements récurrents ne peuvent pas être envoyés';
      case 'CANARY_EXPIRED': {
        const label = subLabelForAlert(alert);
        return (tObj.alertCanaryExpired || 'Canary expiré').replace('{label}', label);
      }
      case 'CANARY_UNREACHABLE': {
        const label = subLabelForAlert(alert);
        return (tObj.alertCanaryUnreachable || 'Canary inaccessible').replace('{label}', label);
      }
      case 'PAYMENT_OVERDUE_SOFT': {
        const label = subLabelForAlert(alert);
        return label
          ? (tObj.alertOverdueSoft || 'Paiement en retard').replace('{label}', label)
          : (tObj.alertOverdueSoft || 'Paiement en retard');
      }
      case 'PAYMENT_OVERDUE_HARD': {
        const label = subLabelForAlert(alert);
        return label
          ? (tObj.alertOverdueHard || 'Paiement en retard critique').replace('{label}', label)
          : (tObj.alertOverdueHard || 'Paiement en retard critique');
      }
      case 'OUTPUTS_LOCKED': {
        const label = subLabelForAlert(alert);
        return label
          ? `${tObj.alertOutputsLocked || 'Fonds verrouillés'} — ${label}`
          : (tObj.alertOutputsLocked || 'Fonds verrouillés (confirmations en cours), nouvel essai dans 25 min');
      }
      case 'PAYMENT_FAILED': {
        const label = subLabelForAlert(alert);
        return label
          ? `${tObj.alertPaymentFailed || 'Échec paiement'} — ${label}`
          : (tObj.alertPaymentFailed || 'Échec d\'un paiement');
      }
      case 'WALLET_NOT_SYNCED': return tObj.alertNotSynced || 'Wallet non synchronisé avec la blockchain';
      case 'OFFLINE_WITH_PENDING': {
        const hoursMatch = alert.message.match(/(\d+)h/);
        const h = hoursMatch ? hoursMatch[1] : '?';
        return (tObj.alertOfflineWithPending || 'Hors ligne depuis {h}h — paiements prevus dans 7 j').replace('{h}', h);
      }
      default: {
        // Strip any "Sub #<uuid>" prefix from raw Rust messages
        return alert.message.replace(/Sub #[0-9a-f-]{36}\s*[—–-]?\s*/i, '').trim();
      }
    }
  }

  let activeAlerts = [];
  let showAlertsOverlay = false;
  // Semi-auto mode: pending confirmation modal
  let pendingPaymentModal = null; // { sub_id, sub_label, amount_xmr, schedule, payments_sent, last_payment_at }

  function formatRelativeDate(isoStr) {
    if (!isoStr) return null;
    const d = new Date(isoStr);
    if (isNaN(d)) return null;
    const diffMs = Date.now() - d.getTime();
    const diffMin = Math.round(diffMs / 60000);
    const diffH = Math.round(diffMs / 3600000);
    const diffD = Math.round(diffMs / 86400000);
    if (diffMin < 2) return "a l'instant";
    if (diffMin < 60) return `il y a ${diffMin} min`;
    if (diffH < 24) return `il y a ${diffH}h`;
    if (diffD < 31) return `il y a ${diffD} j`;
    return `il y a ${Math.round(diffD / 30)} mois`;
  }

  function scheduleLabel(sched) {
    if (!sched) return sched;
    const m = sched.match(/(\d+)\s*min/i);
    if (m) return `Toutes les ${m[1]} min`;
    const h = sched.match(/(\d+)\s*h(?!eure)/i);
    if (h) return `Toutes les ${h[1]}h`;
    const d = sched.match(/(\d+)\s*(day|jour)/i);
    if (d) return `Tous les ${d[1]} j`;
    const w = sched.match(/(\d+)\s*(week|semaine)/i);
    if (w) return `Toutes les ${w[1]} semaines`;
    if (/month|mois/i.test(sched)) return 'Mensuel';
    return sched;
  }
  let sessionPayments = []; // in-memory payment log for current session (superset of historyEntries)
  let historyEntries = [];  // loaded from encrypted DB vault
  let historyEnabled = true;
  let historyDays = 365;
  async function refreshAlerts() {
    const alerts = await tauriInvoke("get_alerts");
    activeAlerts = alerts || [];
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
      next_payment_at: payload.start_immediately === true
        ? new Date().toISOString()
        : (payload.start_date || nextOccurrence(payload.schedule)),
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
  async function onSubNew({ detail }) {
    logInfo("Dashboard", `onSubNew: label=${detail.payload?.custom_label || "?"}`);
    const payload = detail.payload;
    const sub = payloadToSubscription({ ...payload, _sourceCode: null });
    addOrConfirmDuplicate(sub);
    showNewSubModal = false;
    editingSubData = null;
    // Persist to DB via save_subscription (no code string needed)
    const result = await tauriInvoke("save_subscription", { payload });
    if (result) {
      logInfo("Dashboard", `Sub saved to DB: ${result.id}`);
      // Reload all subs from DB — ensures next_payment_at is fresh for ALL subs
      // (avoids desync when Rust scheduler updated a sub while this await was pending)
      const fresh = await tauriInvoke("get_subscriptions");
      if (fresh) {
        subscriptions = fresh.map(dbSubToUi);
      } else {
        subscriptions = subscriptions.map(s => s.id === sub.id
          ? { ...s, _db_id: result.id, id: result.id,
              next_payment_at: result.next_payment_at || s.next_payment_at }
          : s);
      }
    } else {
      logWarn("Dashboard", "Sub NOT persisted to DB (save_subscription failed)");
    }
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

    // Try parsed code first (crypto-autopay: or monero-request: prefix)
    let result = null;
    if (detail.code && (detail.code.startsWith("crypto-autopay:") || detail.code.startsWith("monero-request:"))) {
      result = await tauriInvoke("add_subscription", { code: detail.code });
      if (result) logInfo("Dashboard", `Sub persisted via code: ${result.id}`);
    }
    // Fallback: save via payload (works for manually decoded or converted codes)
    if (!result && detail.payload) {
      const payload = { ...detail.payload, start_immediately: detail.payload.start_immediately ?? true };
      result = await tauriInvoke("save_subscription", { payload });
      if (result) logInfo("Dashboard", `Sub persisted via payload fallback: ${result.id}`);
    }
    if (result) {
      // Reload from DB to get fresh next_payment_at for ALL subs
      const fresh = await tauriInvoke("get_subscriptions");
      if (fresh) {
        subscriptions = fresh.map(dbSubToUi);
      } else {
        subscriptions = subscriptions.map(s =>
          s._code === detail.code || s.id === sub.id
            ? { ...s, _db_id: result.id, id: result.id,
                next_payment_at: result.next_payment_at || s.next_payment_at }
            : s
        );
      }
    } else {
      logWarn("Dashboard", "Sub NOT persisted to DB — both add_subscription and save_subscription failed");
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
  let rpcStatusLabel = "";
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
    rpcStatusLabel = null; // computed via $t in template
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
  // Sync progress (from get_sync_info after each refresh)
  let syncInfo = null; // { wallet_height, daemon_height, blocks_behind, synced }
  let syncPollTimer = null;
  let isOffline = false;          // true when network is unreachable
  let offlineConsecutive = 0;    // require N consecutive offline readings before showing banner
  const OFFLINE_CONFIRM_COUNT = 3; // 3 × 5s = 15s before declaring offline
  // Monero blockchain is ~3.8M blocks (2026). daemon_height=0 can also mean local RPC is restarting.
  const OFFLINE_BLOCKS_THRESHOLD = 500_000; // >500k behind = almost certainly offline

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

  async function processPayment(sub, retryOnBusy = true) {
    logInfo("Scheduler", `Processing payment for sub #${sub.id}`);
    if (retryOnBusy) startPaymentLoader(); // show loader immediately

    let result = null;
    let invokeError = null;
    try {
      const { invoke } = await import("@tauri-apps/api/core");
      result = await invoke("send_payment", {
        destinationAddress: sub.destination_address || sub.address || "",
        amountXmr: parseFloat(sub.amount),
        subscriptionId: sub.id,
      });
    } catch (e) {
      invokeError = String(e);
    }

    // RPC busy → wait 35s for balance refresh to finish, then retry once
    if (invokeError?.includes("RPC busy") && retryOnBusy) {
      logInfo("Scheduler", `RPC occupe — retry dans 35s pour sub #${sub.id}`);
      await new Promise(r => setTimeout(r, 35000));
      return processPayment(sub, false); // single retry
    }

    if (invokeError) {
      if (payLoaderTimer) { clearInterval(payLoaderTimer); payLoaderTimer = null; }
      showPayLoader = false;
      const isLockedOutputs = invokeError.includes("rejected by daemon") || invokeError.includes("not enough") || invokeError.includes("unlocked");
      if (isLockedOutputs) {
        logWarn("Scheduler", `[ATTENTE] Fonds verrouilles (confirmations en cours) pour sub #${sub.id} — retry dans ~25 min`);
        // Defer next_payment_at by 25 min so the scheduler retries at the right time
        const retryAt = new Date(Date.now() + 25 * 60 * 1000).toISOString();
        const idx = subscriptions.findIndex(s => s.id === sub.id);
        if (idx !== -1) {
          subscriptions[idx] = { ...subscriptions[idx], next_payment_at: retryAt };
          subscriptions = [...subscriptions];
        }
        await tauriInvoke("update_subscription_next_payment", { id: sub.id, nextPaymentAt: retryAt });
        await refreshAlerts();
      } else {
        logError("Scheduler", `[ECHEC] Paiement echoue pour sub #${sub.id} — ${invokeError}`);
      }
      return;
    }

    if (result?.tx_hash) {
      logInfo("Scheduler", `[OK] Paiement envoye pour sub #${sub.id} — tx: ${result.tx_hash.slice(0, 12)}...`);
      lastPaymentSentAt = Date.now();
      // Optimistic balance subtraction — avoids showing stale balance while locked outputs settle
      const sentAmt = parseFloat(sub.amount) || 0;
      if (balanceXmr !== null && sentAmt > 0) {
        balanceXmr = Math.max(0, balanceXmr - sentAmt);
        if (wallets.length > 0) wallets[0] = { ...wallets[0], balance: balanceXmr.toFixed(4) };
      }
      showPaymentAnimation(); // fullscreen animation
      // Log to in-session history
      sessionPayments = [...sessionPayments, {
        label: sub.label,
        amount: sub.amount,
        currency: sub.currency || 'XMR',
        sent_at: new Date().toISOString(),
        tx_hash: result.tx_hash || null,
      }];
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
      balanceLoading = true;
      setTimeout(async () => { await refreshBalance(); }, 5000);
    } else {
      logWarn("Scheduler", `[WARN] Pas de tx_hash dans la reponse pour sub #${sub.id} — paiement non confirme`);
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
    if (err.includes("connect") || err.includes("TCP") || err.includes("Connexion refusée")
        || err.includes("Network") || err.includes("network") || err.includes("ENETUNREACH")
        || err.includes("No route") || err.includes("unreachable") || err.includes("offline")) {
      return {
        icon: "📡", level: "warn",
        title: "Hors ligne — pas de connexion Internet",
        detail: "Le nœud Monero est inaccessible. Vérifiez votre connexion réseau. Le solde affiché est celui de la dernière synchronisation.",
        action: "retry",
        actionLabel: "Réessayer",
        offline: true,
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

  async function loadHistoryConfig() {
    try {
      const cfg = await tauriInvoke("get_history_config");
      historyEnabled = cfg.enabled ?? true;
      historyDays = cfg.days ?? 365;
    } catch(e) { logWarn("Dashboard", `History config load failed: ${e}`); }
  }

  async function loadHistoryEntries() {
    try {
      const res = await tauriInvoke("get_payment_history");
      historyEntries = res.entries ?? [];
    } catch(e) { logWarn("Dashboard", `History load failed: ${e}`); }
  }

  async function saveHistoryConfig() {
    await tauriInvoke("set_history_config", { enabled: historyEnabled, days: historyDays });
  }

  async function toggleHistoryEnabled() {
    historyEnabled = !historyEnabled;
    await saveHistoryConfig();
  }

  async function clearHistory() {
    if (!confirm("Effacer tout l'historique ? Cette action est irréversible.")) return;
    await tauriInvoke("clear_payment_history");
    historyEntries = [];
  }

  async function loadWalletInfo() {
    balanceLoading = true;
    try {
      const info = await tauriInvoke("get_wallet_info");
      if (info) {
        // Only use cached balance if RPC is connected; otherwise show -.---- (unknown)
        balanceXmr = (info.rpc_connected && (info.balance_xmr ?? 0) > 0)
          ? info.balance_xmr
          : (info.rpc_connected ? 0 : null);
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

  async function pollSyncInfo() {
    try {
      const info = await tauriInvoke("get_sync_info");
      if (info) {
        syncInfo = info;
        // Detect offline: daemon_height = 0 AND wallet is supposedly synced,
        // OR blocks_behind > 500k (impossible unless daemon can't reach peers).
        // Require OFFLINE_CONFIRM_COUNT consecutive readings to avoid false positives
        // from the local RPC restarting (which briefly returns daemonH=0).
        const behind = info.blocks_behind ?? 0;
        const daemonH = info.daemon_height ?? 0;
        const looksOffline = (daemonH === 0 && (info.wallet_height ?? 0) > 100_000) || behind > OFFLINE_BLOCKS_THRESHOLD;
        if (looksOffline) {
          offlineConsecutive++;
          if (offlineConsecutive >= OFFLINE_CONFIRM_COUNT) isOffline = true;
        } else {
          offlineConsecutive = 0;
          isOffline = false; // clear offline when sync info looks normal again
        }
      }
    } catch(_) {
      // get_sync_info failed = RPC restarting, not necessarily offline
    }
  }

  async function refreshBalance() {
    balanceLoading = true;
    logInfo("Dashboard", "Refreshing balance via RPC…");
    // Poll sync progress every 5s while balance is loading
    if (syncPollTimer) clearInterval(syncPollTimer);
    syncPollTimer = setInterval(pollSyncInfo, 5000);
    try {
      const b = await tauriInvoke("refresh_balance");
      if (b) {
        balanceXmr = b.xmr;
        // Update wallets[0].balance for the Wallets page (async ctx = no reactive loop)
        if (wallets.length > 0) {
          wallets[0] = { ...wallets[0], balance: b.xmr.toFixed(4) };
          wallets = [...wallets];
        }
        rpcOk = true;
        rpcError = null;
        lastBalanceRefresh = Date.now();
        updateRefreshAgo();
        logInfo("Dashboard", `Balance refreshed: [REDACTED]`);
        await pollSyncInfo(); // update after refresh completes
      }
    } catch (e) {
      const errStr = String(e);
      if (errStr.includes("RPC busy")) {
        logInfo("Dashboard", "Balance refresh skipped (paiement en cours)");
      } else if (errStr.includes("Locked")) {
        logInfo("Dashboard", "Balance refresh skipped (wallet verrouillé)");
        balanceXmr = null;
      } else {
        rpcOk = false;
        rpcError = errStr;
        // NOTE: Do NOT detect offline from "Connexion refusée" — that is the LOCAL
        // wallet-rpc (127.0.0.1:18083) restarting, not an internet connectivity issue.
        // isOffline is set exclusively by pollSyncInfo() via daemon_height / blocks_behind.
        logWarn("Dashboard", `Balance refresh failed: ${e}`);
      }
    }
    if (syncPollTimer) { clearInterval(syncPollTimer); syncPollTimer = null; }
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
    // Load history config and entries from encrypted vault
    await loadHistoryConfig();
    await loadHistoryEntries();

    // Sync payment mode with Rust backend (source of truth for semi-auto enforcement)
    try {
      const isSemiAuto = await tauriInvoke("get_payment_mode");
      paymentMode.set(isSemiAuto ? "semi-auto" : "auto");
    } catch(e) { logWarn("Dashboard", `get_payment_mode failed: ${e}`); }

    // Restore any pending approvals that survived across sessions (e.g. after unlock)
    try {
      const pending = await tauriInvoke("get_pending_approvals");
      if (pending?.length > 0) pendingPaymentModal = pending[0];
    } catch(e) { /* noop */ }
    updateRefreshAgo();

    // Immediately trigger a background sync without blocking the UI
    // (sync takes ~47s on first run — shows spinner in sidebar)
    refreshBalance(); // intentionally NOT awaited

    const balanceTimer = setInterval(refreshBalance, 2 * 60 * 1000);
    const agoTimer = setInterval(updateRefreshAgo, 30 * 1000);

    // NOTE: auto payment scheduling is handled by the Rust background scheduler.
    // The frontend only handles manual triggers ("Déclencher maintenant").
    // No frontend payment timer — prevents double-firing with Rust scheduler.

    // Fast ticker every 10s — forces reactive nextPayment banner to update
    const tickTimer = setInterval(() => { nowTick = Date.now(); }, 10 * 1000);

    // Listen for Rust background scheduler events
    await tauriListen("payment-locked", async (ev) => {
      const { sub_id, retry_in_min, next_payment_at } = ev.payload || {};
      logWarn("Scheduler", `[ATTENTE] Fonds verrouilles pour sub #${sub_id} — retry automatique dans ${retry_in_min} min`);
      // Update in-memory subscription so frontend shows correct next time (not "Maintenant")
      if (sub_id && next_payment_at) {
        subscriptions = subscriptions.map(s =>
          s.id === sub_id ? { ...s, next_payment_at } : s
        );
      } else if (sub_id) {
        // Reload from DB to get the deferred time
        const fresh = await tauriInvoke("get_subscriptions");
        if (fresh) subscriptions = fresh.map(dbSubToUi);
      }
      await refreshAlerts();
    });

    // Semi-auto: show modal when payment needs user approval
    // Queue the payload and show only when sync is done (balance known)
    let _pendingQueue = [];
    const unlistenPending = await tauriListen("payment-pending", (ev) => {
      const p = ev.payload || {};
      logInfo("Scheduler", `[Semi-auto] Payment queued for approval: ${p.sub_label} — ${p.amount_xmr} XMR`);
      _pendingQueue.push(p);
    });

    // Check every 3s if we should display a queued pending payment
    const pendingFlushTimer = setInterval(() => {
      if (_pendingQueue.length > 0 && !balanceLoading && balanceXmr !== null && !isOffline) {
        pendingPaymentModal = _pendingQueue.shift();
      }
    }, 3000);

    const unlistenPayment = await tauriListen("payment-sent", async (ev) => {
      logInfo("Scheduler", "[Rust] Paiement envoyé par le scheduler Rust — refresh dans 5s");
      const payload = ev.payload || {};
      if (payload.sub_id) {
        const sub = subscriptions.find(s => s.id === payload.sub_id);
        if (sub) {
          sessionPayments = [...sessionPayments, {
            label: sub.label, amount: sub.amount, currency: sub.currency || 'XMR',
            sent_at: new Date().toISOString(), tx_hash: payload.tx_hash || null,
          }];
          // Update in-memory next_payment_at so frontend shows correct countdown
          if (payload.next_payment_at) {
            subscriptions = subscriptions.map(s =>
              s.id === payload.sub_id ? { ...s, next_payment_at: payload.next_payment_at } : s
            );
          }
        }
      }
      balanceLoading = true;
      setTimeout(async () => {
        await loadRpcState();
        await refreshBalance();
        await refreshAlerts();
        await loadHistoryEntries(); // reload encrypted history after payment
      }, 5000);
    });

    // Refresh alerts immediately on balance update + periodically
    await tauriListen("alerts-updated", () => refreshAlerts());
    await refreshAlerts();
    const alertTimer = setInterval(refreshAlerts, 60 * 1000);

    return () => {
      clearInterval(balanceTimer); clearInterval(agoTimer);
      clearInterval(tickTimer); clearInterval(alertTimer);
      unlistenPayment();
    };
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
      next_payment_at: s.next_payment_at || null, // keep full ISO timestamp — do NOT slice to date only
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
    // Single real wallet — balance starts null (unknown), updated after first RPC refresh
    { id: 1, name: "Wallet principal", desc: "", balance: null, color: "#f26822", monthlyOut: 0 },
  ];

  // Derive totalBalance from live balanceXmr — pure read, no loop
  $: totalBalance = balanceXmr !== null ? balanceXmr.toFixed(3) : (wallets[0]?.balance ?? "0.000");

  // Keep wallets[0].monthlyOut in sync using a guard to break the reactive loop
  let _lastMonthlyOut = -1;
  $: if (totalMonthlyOut !== _lastMonthlyOut) {
    _lastMonthlyOut = totalMonthlyOut;
    if (wallets.length > 0) {
      wallets[0] = { ...wallets[0], monthlyOut: totalMonthlyOut };
      wallets = [...wallets];
    }
  }

  // Compute monthly spending from REAL active subscriptions (not mock wallet data)
  $: totalMonthlyOut = (() => {
    const MINS_PER_MONTH = 43200;
    return subscriptions
      .filter(s => ["Active", "Actif"].includes(s.status))
      .reduce((sum, s) => {
        const amt = parseFloat(s.amount) || 0;
        const raw = s._raw_schedule || s.schedule || "";
        const parts = raw.trim().split(/\s+/);
        const minField = parts.length >= 6 ? parts[1] : parts[0];
        let periodMin = MINS_PER_MONTH; // default monthly
        if (minField?.startsWith("*/")) periodMin = parseInt(minField.slice(2)) || MINS_PER_MONTH;
        else if (minField === "0" && parts.length >= 3) periodMin = 60; // hourly
        const sendsPerMonth = MINS_PER_MONTH / Math.max(1, periodMin);
        return sum + amt * sendsPerMonth;
      }, 0);
  })();

  // Ticker updated every 10s by setInterval — forces nextPayment label to recompute
  let nowTick = Date.now();

  // Computed from actual subscriptions (reactive, refreshed every 10s via nowTick)
  $: nextPayment = (() => {
    void nowTick; // declare dependency so Svelte re-runs when nowTick changes
    const t_snap = $t; // track language changes
    const now = Date.now();
    const active = subscriptions.filter(s =>
      ["Active", "Actif"].includes(s.status) && s.next_payment_at
    );
    if (!active.length) return null;
    active.sort((a, b) => new Date(a.next_payment_at) - new Date(b.next_payment_at));
    const soonest = active[0];
    const ms = new Date(soonest.next_payment_at) - now;
    const days = Math.ceil(ms / 86400000);
    const h = Math.floor(ms / 3600000);
    const m = Math.floor(ms / 60000);
    const s = Math.floor(ms / 1000);
    const inPfx = t_snap.inPrefix || "dans";
    const dSfx = t_snap.inDaysSuffix || "jours";
    let label_time;
    if (s <= 0) {
      const late = Math.abs(ms);
      const lm = Math.floor(late / 60000);
      const lh = Math.floor(lm / 60);
      const ld = Math.floor(lh / 24);
      label_time = ld >= 1 ? `J+${ld}` : lh >= 1 ? `+${lh}h` : lm >= 1 ? `+${lm}min` : (t_snap.now || 'maintenant');
    } else if (s < 60) label_time = `${inPfx} ${s}s`;
    else if (m < 60) label_time = `${inPfx} ${m} min`;
    else if (h < 24) label_time = `${inPfx} ${h}h`;
    else label_time = `${t_snap.dayMinus || 'J-'}${days}`;
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
    lowBalanceThresholdDays: 7, // days of runway before BALANCE_LOW alert fires
  };

  // Sync alert config to Rust backend when settings change
  async function applyAlertConfig() {
    const enabled = [];
    if (notifSettings.lowBalance)    enabled.push("BALANCE_LOW", "BALANCE_EMPTY");
    if (notifSettings.canary)        enabled.push("CANARY_EXPIRED", "CANARY_UNREACHABLE");
    if (notifSettings.paymentFailed) enabled.push("PAYMENT_FAILED", "PAYMENT_OVERDUE_SOFT", "PAYMENT_OVERDUE_HARD");
    enabled.push("OUTPUTS_LOCKED", "WALLET_NOT_SYNCED"); // always enabled
    await tauriInvoke("set_alert_config", {
      balanceLowThresholdDays: notifSettings.lowBalanceThresholdDays,
      enabledAlerts: enabled,
    }).catch(() => {});
  }

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
    return t.refillLessThanOneDay || 'moins de 1 jour !';
  }

  // Total refill estimate
  $: totalRefillText = (tStr) => refillEstimate(totalBalance, totalMonthlyOut, tStr);
  // Is refill urgent? (< 7 days remaining)
  $: refillUrgent = (() => {
    const bal = parseFloat(totalBalance);
    if (!totalMonthlyOut || totalMonthlyOut <= 0) return false;
    const daysLeft = (bal / totalMonthlyOut) * 30;
    return daysLeft < 7;
  })();

  // Animation subjects (network-aware via PaymentAnimation internals)
  const animSubjects = [
    { toLabel: "Le Média Libre", xmrAmount: "0.05", btcAmount: "420 sats" },
    { toLabel: "VPN",            xmrAmount: "0.012", btcAmount: "120 sats" },
  ];
</script>

<!-- Semi-auto: payment confirmation modal -->
{#if pendingPaymentModal}
  <!-- svelte-ignore a11y-click-events-have-key-events a11y-no-noninteractive-element-interactions -->
  <div class="modal-backdrop" role="dialog" aria-modal="true">
    <!-- svelte-ignore a11y-click-events-have-key-events a11y-no-static-element-interactions -->
    <div class="modal-panel" on:click|stopPropagation role="document" style="max-width:400px;text-align:center">
      <div style="font-size:36px;margin-bottom:12px">🔔</div>
      <h2 style="margin:0 0 6px;font-size:18px">Paiement en attente</h2>
      <p style="color:var(--text-muted);font-size:13px;margin-bottom:16px">
        Mode semi-auto — confirmez avant envoi
      </p>
      <div class="card" style="padding:14px 18px;margin-bottom:18px;text-align:left">
        <div style="font-size:14px;font-weight:700;margin-bottom:6px">{pendingPaymentModal.sub_label}</div>
        <div style="font-size:24px;font-weight:800;color:var(--accent);margin-bottom:8px">{pendingPaymentModal.amount_xmr} XMR</div>
        <div style="display:flex;flex-direction:column;gap:4px;font-size:12px;color:var(--text-muted)">
          {#if pendingPaymentModal.schedule}
            <div>Cadence : <strong style="color:var(--text)">{scheduleLabel(pendingPaymentModal.schedule)}</strong></div>
          {/if}
          {#if pendingPaymentModal.payments_sent > 0}
            <div>Envois precedents : <strong style="color:var(--text)">{pendingPaymentModal.payments_sent}</strong></div>
          {:else}
            <div style="color:#f97316">Premier envoi</div>
          {/if}
          {#if pendingPaymentModal.last_payment_at && pendingPaymentModal.payments_sent > 0}
            {@const rel = formatRelativeDate(pendingPaymentModal.last_payment_at)}
            {#if rel}
              <div>Dernier envoi : <strong style="color:var(--text)">{rel}</strong></div>
            {/if}
          {/if}
        </div>
      </div>
      <div style="display:flex;gap:10px;justify-content:center">
        <button class="btn-ghost" style="flex:1;padding:10px"
          on:click={() => { tauriInvoke("reject_payment", { subId: pendingPaymentModal.sub_id }); pendingPaymentModal = null; }}>
          Passer / Reporter
        </button>
        <button class="btn-primary" style="flex:1;padding:10px;font-size:15px"
          on:click={async () => {
            await tauriInvoke("approve_payment", { subId: pendingPaymentModal.sub_id });
            pendingPaymentModal = null;
            // Trigger immediate scheduler cycle
            setTimeout(() => refreshBalance(), 2000);
          }}>
          Confirmer l'envoi
        </button>
      </div>
    </div>
  </div>
{/if}

<!-- Alerts detail overlay -->
{#if showAlertsOverlay}
  <!-- svelte-ignore a11y-click-events-have-key-events a11y-no-noninteractive-element-interactions -->
  <div class="modal-backdrop" on:click={() => showAlertsOverlay = false} role="dialog" aria-modal="true">
    <!-- svelte-ignore a11y-click-events-have-key-events a11y-no-static-element-interactions -->
    <div class="modal-panel alerts-overlay-panel" on:click|stopPropagation role="document" style="max-width:480px">
      <div class="modal-header">
        <h2 class="modal-title">
          {activeAlerts.some(a => a.level === 'Red')
            ? `🔴 ${$t.alertOverlayCritical || 'Alertes critiques'}`
            : `🟠 ${$t.alertOverlayWarning || 'Avertissements'}`}
        </h2>
        <button class="modal-close" on:click={() => showAlertsOverlay = false} aria-label={$t.alertOverlayClose || 'Fermer'}>✕</button>
      </div>
      <div class="modal-body" style="display:flex;flex-direction:column;gap:10px;padding:16px">
        {#if activeAlerts.length === 0}
          <p style="color:var(--text-muted);text-align:center">{$t.alertOverlayNone || 'Aucune alerte active.'}</p>
        {:else}
          {#each activeAlerts as alert}
            <div class="alert-detail-card" class:alert-detail-red={alert.level === 'Red'} class:alert-detail-orange={alert.level === 'Orange'}>
              <div class="alert-detail-icon">{alert.level === 'Red' ? '🔴' : '🟠'}</div>
              <div class="alert-detail-body">
                <div class="alert-detail-kind">{alert.kind}</div>
                <div class="alert-detail-msg">{translateAlertMsg(alert, $t)}</div>
                <div class="alert-detail-time">{new Date(alert.created_at * 1000).toLocaleTimeString()}</div>
              </div>
            </div>
          {/each}
        {/if}
        <button class="btn-ghost" on:click={() => showAlertsOverlay = false} style="margin-top:8px">{$t.alertOverlayClose || 'Fermer'}</button>
      </div>
    </div>
  </div>
{/if}

<!-- Payment loading overlay (shows while payment RPC is running) -->
{#if showPayLoader}
  <div class="pay-anim-overlay pay-loader-overlay" role="presentation">
    <div class="pay-loader-ring"></div>
    <div class="pay-loader-countdown">{payLoaderCountdown}</div>
    <div class="pay-anim-label" style="font-size:15px;margin-top:8px">Envoi en cours…</div>
  </div>
{/if}

<!-- Payment success animation overlay -->
{#if showPayAnim}
  <div class="pay-anim-overlay" on:click={() => showPayAnim = false} role="presentation">
    <svg class="pay-anim-svg" viewBox="0 0 400 400" xmlns="http://www.w3.org/2000/svg">
      <!-- Concentric ripple rings -->
      <circle class="ripple r1" cx="200" cy="200" r="40" fill="none" stroke="var(--accent)" stroke-width="3"/>
      <circle class="ripple r2" cx="200" cy="200" r="40" fill="none" stroke="var(--accent)" stroke-width="2" opacity="0.7"/>
      <circle class="ripple r3" cx="200" cy="200" r="40" fill="none" stroke="var(--accent)" stroke-width="1.5" opacity="0.5"/>
      <!-- Monero "M" arrows simplified -->
      <circle cx="200" cy="200" r="36" fill="var(--accent)" opacity="0.15"/>
      <!-- Checkmark -->
      <polyline class="pay-check" points="184,202 196,214 218,190" fill="none" stroke="var(--accent)" stroke-width="4" stroke-linecap="round" stroke-linejoin="round"/>
      <!-- Orbiting dots -->
      <circle class="orbit-dot od1" cx="200" cy="164" r="5" fill="var(--accent)" opacity="0.9"/>
      <circle class="orbit-dot od2" cx="200" cy="164" r="4" fill="var(--accent)" opacity="0.6"/>
      <circle class="orbit-dot od3" cx="200" cy="164" r="3" fill="var(--accent)" opacity="0.4"/>
    </svg>
    <div class="pay-anim-label">{$currentLang === 'fr' ? 'Paiement envoyé !' : 'Payment sent!'}</div>
  </div>
{/if}

<div class="layout">
  <!-- Sidebar -->
  <aside class="sidebar">
    <div class="sidebar-header">
      <div class="sidebar-logo">
        <img src="/logo.png" alt="Crypto AutoPay" width="52" height="52" style="border-radius:8px;flex-shrink:0"/>
        <div>
          <div class="sidebar-appname">Monero AutoPay</div>
          <div class="tray-row">
            <span class="status-dot {activeAlerts.some(a => a.level === 'Red') ? 'red' : activeAlerts.some(a => a.level === 'Orange') ? 'orange' : trayStatus}"></span>
            {#if activeAlerts.length > 0}
              <!-- svelte-ignore a11y-click-events-have-key-events a11y-no-static-element-interactions -->
              <span class="alert-badge" class:badge-red={activeAlerts.some(a => a.level === 'Red')} class:badge-orange={!activeAlerts.some(a => a.level === 'Red')}
                style="cursor:pointer" on:click={() => showAlertsOverlay = true}>
                {activeAlerts.length}
              </span>
              <!-- svelte-ignore a11y-click-events-have-key-events a11y-no-static-element-interactions -->
              <span class="tray-label" style="color:{activeAlerts.some(a=>a.level==='Red') ? '#ef4444' : '#f97316'};cursor:pointer"
                on:click={() => showAlertsOverlay = true}>{activeAlerts.some(a => a.level === 'Red') ? 'Alerte critique' : 'Avertissement'}</span>
            {:else}
              <span class="tray-label">OK</span>
            {/if}
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
      <!-- receive moved into wallet card -->
      <button class="nav-item" class:active={page === "wallets"} on:click={() => page = "wallets"}>
        {$t.navWallets}
      </button>
      <button class="nav-item" class:active={page === "history"} on:click={() => page = "history"}>
        {$t.navPaymentHistory}
      </button>
      <!-- archived moved inline in subscription list -->
      <button class="nav-item find-nav" class:active={page === "find"} on:click={() => page = "find"}>
        {$t.findSubscriptions}
      </button>

      {#if activeAlerts.length > 0}
        <div class="alerts-panel">
          {#each activeAlerts as a}
            <!-- svelte-ignore a11y-click-events-have-key-events a11y-no-static-element-interactions -->
            <div class="alert-item" class:alert-red={a.level === 'Red'} class:alert-orange={a.level === 'Orange'}
              style="cursor:pointer" on:click={() => showAlertsOverlay = true}>
              {translateAlertMsg(a, $t)}
            </div>
          {/each}
        </div>
      {/if}

      <div class="nav-divider"></div>

      <button class="nav-item" class:active={page === "howto"} on:click={() => page = "howto"}>
        {$t.navHowItWorks}
      </button>
      <button class="nav-item" class:active={page === "create-code"} on:click={() => page = "create-code"}>
        🔗 {$t.navCreateCode || 'Créer un code AutoPay'}
      </button>
      <button class="nav-item" class:active={page === "settings"} on:click={() => page = "settings"}>
        {$t.navSettings}
      </button>
      <!-- Lock button — centered below settings -->
      <div style="display:flex;justify-content:center;padding:12px 0 4px">
        <button class="lock-nav-btn" title="Verrouiller la session"
          on:click={() => tauriInvoke('lock_wallet').then(() => dispatch('lock'))}>
          <!-- Padlock: body (rect) + shackle (path), filled for visibility in dark mode -->
          <svg width="16" height="16" viewBox="0 0 20 22" fill="none" xmlns="http://www.w3.org/2000/svg">
            <rect x="1" y="10" width="18" height="12" rx="2.5" fill="var(--accent)" opacity="0.9"/>
            <path d="M5 10V7a5 5 0 0 1 10 0v3" stroke="var(--accent)" stroke-width="2.5" stroke-linecap="round" fill="none"/>
            <circle cx="10" cy="16" r="1.6" fill="#fff"/>
          </svg>
        </button>
      </div>
    </nav>

    <!-- Balance card: arbre complet des états RPC -->
    <div class="sidebar-balance card">
      <div class="balance-label">
        <!-- RPC status dot -->
        {#if isOffline}
          <span class="rpc-dot dot-offline" title="Hors ligne">●</span>
        {:else if rpcOk === null}
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
      {#if isOffline}
        <div class="offline-banner">
          📡 Hors ligne — connectez-vous à Internet pour synchroniser
          <button class="btn-inline" on:click={() => { isOffline = false; refreshBalance(); }}>Réessayer</button>
        </div>
      {:else if balanceLoading}
        <div class="refresh-ago sync-pulse">Synchro réseau en cours…</div>
        {#if syncInfo && syncInfo.blocks_behind > 10}
          {@const pct = Math.max(2, Math.min(99, 100 - (syncInfo.blocks_behind / Math.max(syncInfo.daemon_height, 1)) * 100))}
          <div class="sync-progress-wrap">
            <div class="sync-progress-bar" style="width:{pct}%"></div>
          </div>
          <span class="sync-progress-label">{syncInfo.blocks_behind.toLocaleString()} blocs · ~{Math.ceil(syncInfo.blocks_behind * 2 / 60)} min</span>
        {:else if syncInfo && syncInfo.blocks_behind > 0}
          <div class="sync-progress-label-small">{syncInfo.blocks_behind} blocs restants</div>
        {:else if !syncInfo}
          <div class="sync-progress-label-small">Connexion à la blockchain…</div>
        {/if}
      {:else if refreshAgoLabel}
        <div class="refresh-ago">{refreshAgoLabel} · ↺ 2 min</div>
      {/if}

        {#if isOffline && balanceXmr !== null}
          <!-- Offline but we have a cached balance: show it greyed -->
          <div class="balance-amount" style="color:var(--text-muted)">{balanceXmr.toFixed(4)} {$currentNetwork.symbol}</div>
          <div class="balance-hint" style="color:var(--text-muted);font-size:11px">Solde de la dernière synchro — non actualisé</div>
        {:else if isOffline}
          <div class="balance-amount" style="font-size:18px;color:var(--text-muted);letter-spacing:0.08em">-.---- {$currentNetwork.symbol}</div>
        {:else if balanceXmr === null}
        <div class="balance-amount" style="font-size:18px;color:var(--text-muted);letter-spacing:0.08em">-.---- {$currentNetwork.symbol}</div>

      {:else if rpcOk}
        <!-- Connected: just show the balance -->
        <div class="balance-amount text-accent">{balanceXmr.toFixed(4)} {$currentNetwork.symbol}</div>
        {#if balanceXmr === 0}
          <div class="balance-hint">{$t.balanceEmpty || 'Wallet vide — rechargez-le en Monero pour commencer.'}</div>
          <button class="btn-receive" on:click={() => page = "receive"}>⬇ {$t.receiveTitle || 'Recharger le wallet'}</button>
        {/if}

      {:else if rpcOk === false}
        <!-- RPC failed: show balance if known, otherwise -.---- -->
        {#if balanceXmr !== null && balanceXmr > 0}
          <div class="balance-amount" style="color:var(--text-muted)">{balanceXmr.toFixed(4)} {$currentNetwork.symbol}</div>
        {:else}
          <div class="balance-amount" style="font-size:18px;color:var(--text-muted);letter-spacing:0.08em">-.---- {$currentNetwork.symbol}</div>
        {/if}
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
      <div class="refill-banner card" class:refill-urgent={refillUrgent}>
        <span class="text-muted">{$t.nextRefillLabel} :</span>
        <strong class:refill-urgent-text={refillUrgent}>{$t.refillIn} {totalRefillText($t)}</strong>
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
        on:archive-sub={async e => {
          const sub = e.detail.sub;
          subscriptions = subscriptions.map(s => s.id === sub.id ? { ...s, status: "Cancelled" } : s);
          if (sub._db_id) await tauriInvoke("set_subscription_status", { id: sub._db_id, paused: true });
        }}
        on:restore-sub={async e => {
          const sub = e.detail.sub;
          subscriptions = subscriptions.map(s => s.id === sub.id ? { ...s, status: "Paused" } : s);
          if (sub._db_id) await tauriInvoke("set_subscription_status", { id: sub._db_id, paused: false });
        }}
      />
      <!-- Footer marketing + bottom spacer -->
      <div class="sub-footer">
        <div class="sub-footer-grid">
          <div class="sub-footer-item">
            <svg class="sub-footer-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">
              <rect x="3" y="11" width="18" height="11" rx="2" ry="2"/><path d="M7 11V7a5 5 0 0 1 10 0v4"/>
            </svg>
            <div>
              <strong>{$t.featurePrivacyTitle || 'Vie privée maximale'}</strong>
              <p>{$t.featurePrivacyDesc || 'Aucune donnée envoyée à l\'extérieur. Zéro tracking, zéro analytics.'}</p>
            </div>
          </div>
          <div class="sub-footer-item">
            <svg class="sub-footer-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">
              <circle cx="12" cy="12" r="10"/><path d="M12 8v4l3 3"/>
            </svg>
            <div>
              <strong>{$t.featureMoneroTitle || 'Anonymat Monero'}</strong>
              <p>{$t.featureMoneroDesc || 'Transactions confidentielles par protocole. Montants et identités masqués.'}</p>
            </div>
          </div>
          <div class="sub-footer-item">
            <svg class="sub-footer-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">
              <path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z"/>
            </svg>
            <div>
              <strong>{$t.featureSeedTitle || 'Seed chiffrée post-quantique'}</strong>
              <p>{$t.featureSeedDesc || 'Argon2id + AES-256-GCM. Résistant aux attaques quantiques.'}</p>
            </div>
          </div>
          <div class="sub-footer-item">
            <svg class="sub-footer-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">
              <polyline points="16 18 22 12 16 6"/><polyline points="8 6 2 12 8 18"/>
            </svg>
            <div>
              <strong>{$t.featureOpenSourceTitle || 'Open source'}</strong>
              <p>{$t.featureOpenSourceDesc || 'Code auditable. Pas de tiers de confiance. Vous contrôlez vos clés.'}</p>
            </div>
          </div>
        </div>
        <p class="sub-footer-version">Crypto AutoPay v0.2 · 2026 · open source · <span class="sub-footer-tagline">{$t.footerTagline || 'Vos paiements, vos règles.'}</span></p>
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
                  <div style="display:flex;flex-direction:column;gap:6px;align-items:flex-start">
                    <button class="btn-recharge" on:click={() => { page = 'receive'; }}>
                      {$t.receiveTitle || 'Recharger le wallet'}
                    </button>
                    <button class="btn-ghost btn-sm" on:click={() => startEditWallet(w)}>{$t.walletEdit}</button>
                  </div>
                </div>
                <div class="wallet-balance">
                  {#if w.balance === null}
                    <span class="balance-num" style="color:var(--text-muted);letter-spacing:0.08em">-.---- {$currentNetwork.symbol}</span>
                  {:else}
                    <span class="balance-num text-accent">{w.balance} {$currentNetwork.symbol}</span>
                  {/if}
                </div>
              <div class="wallet-limit-warn">
                {#if w.balance !== null}
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
                {:else}
                  <p style="font-size:11px;color:var(--text-muted);margin:4px 0">Solde en attente de synchronisation…</p>
                {/if}
              </div><!-- /wallet-limit-warn -->
            </div><!-- /wallet-body -->
            </div><!-- /wallet-card -->
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
      <div class="page-header" style="display:flex;align-items:center;justify-content:space-between">
        <div>
          <h1>{$t.navPaymentHistory}</h1>
          <p class="text-muted" style="font-size:12px;margin-top:4px">
            {historyEnabled ? `Chiffré · ${historyDays} jours de rétention` : 'Historique désactivé (vie privée)'}
          </p>
        </div>
        <!-- iOS-style toggle for history -->
        <div style="display:flex;align-items:center;gap:10px">
          <span style="font-size:12px;color:var(--text-muted)">{historyEnabled ? 'Activé' : 'Désactivé'}</span>
          <button class="ios-toggle-btn" class:ios-on={historyEnabled}
            on:click={toggleHistoryEnabled} title="Activer/désactiver l'enregistrement de l'historique">
            <span class="ios-thumb-btn"></span>
          </button>
        </div>
      </div>

      {#if historyEnabled}
        <!-- Retention setting -->
        <div class="card" style="padding:12px 16px;display:flex;align-items:center;justify-content:space-between;margin-bottom:10px">
          <span style="font-size:13px">Durée de conservation</span>
          <div style="display:flex;align-items:center;gap:8px">
            <input type="number" min="7" max="3650" bind:value={historyDays}
              on:change={() => saveHistoryConfig()}
              style="width:60px;background:var(--bg-input);border:1px solid var(--border);border-radius:6px;padding:4px 8px;color:var(--text);font-size:13px;text-align:center"/>
            <span style="font-size:12px;color:var(--text-muted)">jours</span>
          </div>
        </div>

        {#if historyEntries.length === 0}
          <div class="card" style="padding:24px;text-align:center;color:var(--text-muted)">
            Aucun paiement enregistré.
          </div>
        {:else}
          <div style="display:flex;justify-content:flex-end;margin-bottom:6px">
            <button class="btn-ghost btn-sm" style="color:#ef4444;font-size:11px"
              on:click={clearHistory}>Effacer l'historique</button>
          </div>
          <div class="history-list">
            {#each [...historyEntries].reverse() as p}
              <div class="history-card card">
                <div class="history-row">
                  <span class="history-label">{p.sub_label}</span>
                  <span class="history-amount">{p.amount_xmr.toFixed(4)} XMR</span>
                </div>
                <div class="history-row history-meta">
                  <span class="text-muted">{new Date(p.sent_at).toLocaleString()}</span>
                  {#if p.tx_prefix}
                    <span class="history-tx">tx: {p.tx_prefix}…</span>
                  {/if}
                </div>
              </div>
            {/each}
          </div>
          <!-- Totals by sub -->
          <div class="card" style="margin-top:12px;padding:12px 16px">
            <div style="font-size:12px;font-weight:700;color:var(--text-muted);margin-bottom:8px">TOTAUX (HISTORIQUE CHIFFRÉ)</div>
            {#each Object.entries(historyEntries.reduce((acc, p) => {
              acc[p.sub_label] = (acc[p.sub_label] || 0) + p.amount_xmr; return acc;
            }, {})) as [label, total]}
              <div style="display:flex;justify-content:space-between;font-size:13px;padding:3px 0;border-bottom:1px solid var(--border)">
                <span>{label}</span>
                <span>{total.toFixed(4)} XMR</span>
              </div>
            {/each}
          </div>
        {/if}
      {:else}
        <div class="card" style="padding:24px;text-align:center">
          <p style="font-size:20px;margin-bottom:8px">🔒</p>
          <p style="color:var(--text-muted);font-size:13px">
            L'enregistrement de l'historique est désactivé.<br/>Réactivez-le pour enregistrer les paiements futurs.
          </p>
        </div>
      {/if}

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
              on:change={e => {
                const mode = e.target.checked ? "auto" : "semi-auto";
                paymentMode.set(mode);
                tauriInvoke("set_payment_mode", { semiAuto: mode === "semi-auto" }).catch(() => {});
              }}
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
          <label class="toggle"><input type="checkbox" bind:checked={notifSettings.canary} on:change={applyAlertConfig}/><span class="slider"></span></label>
        </div>
        <div class="setting-row">
          <span class="setting-label">{$t.notifLowBalance}</span>
          <label class="toggle"><input type="checkbox" bind:checked={notifSettings.lowBalance} on:change={applyAlertConfig}/><span class="slider"></span></label>
        </div>
        {#if notifSettings.lowBalance}
        <div class="setting-row" style="padding-left:16px;gap:8px">
          <span class="setting-label" style="font-size:12px;color:var(--text-muted)">Solde bas si moins de</span>
          <input type="number" bind:value={notifSettings.lowBalanceThresholdDays}
            on:change={applyAlertConfig}
            min="1" max="60" step="1"
            style="width:52px;padding:4px 6px;font-size:13px;text-align:center" />
          <span class="text-muted" style="font-size:12px">jours de réserve</span>
        </div>
        {/if}
        <div class="setting-row">
          <span class="setting-label">{$t.notifPaymentSent}</span>
          <label class="toggle"><input type="checkbox" bind:checked={notifSettings.paymentSent}/><span class="slider"></span></label>
        </div>
        <div class="setting-row">
          <span class="setting-label">{$t.notifPaymentFailed}</span>
          <label class="toggle"><input type="checkbox" bind:checked={notifSettings.paymentFailed} on:change={applyAlertConfig}/><span class="slider"></span></label>
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
          <span class="setting-label">{$t.rpcStatus || 'Statut wallet-rpc'}</span>
          <span class="rpc-badge rpc-{rpcStatus}">
            {rpcStatus === 'running' ? ($t.rpcRunning || 'En marche') :
             rpcStatus === 'starting' ? ($t.rpcStarting || 'Démarrage…') :
             rpcStatus === 'stopped' ? ($t.rpcStopped || 'Arrêté') : ($t.rpcError || 'Erreur')}
          </span>
        </div>
        {#if rpcBinaryPath}
          <p class="rpc-info">{$t.rpcBinaryFound || 'Binaire détecté'} : <code>{rpcBinaryPath}</code></p>
        {:else}
          <p class="rpc-warn">
            ⚠ <code>monero-wallet-rpc</code> {$t.rpcBinaryMissing || 'introuvable dans PATH'}.<br>
            <a class="rpc-link" href="https://www.getmonero.org/downloads/" on:click|preventDefault={() => openExternal("https://www.getmonero.org/downloads/")}>
              {$t.rpcDownload || 'Télécharger Monero CLI'} →
            </a>
            {$t.rpcOrSetPath || 'ou renseigner le chemin ci-dessous.'}
          </p>
          <label class="field" style="margin-top:8px">
            <span>{$t.rpcBinaryPath || 'Chemin du binaire (optionnel)'}</span>
            <input type="text" bind:value={rpcConfig.binary_path} placeholder="/usr/bin/monero-wallet-rpc" />
          </label>
        {/if}

        <label class="field" style="margin-top:10px">
          <span>{$t.rpcPublicNode || 'Nœud Monero public'}</span>
          <select bind:value={rpcConfig.daemon_address} class="setting-select" style="width:100%">
            <option value="">{$t.rpcNodeAuto || 'Automatique (premier nœud disponible)'}</option>
            {#each publicNodes as node}
              <option value={node}>{node}</option>
            {/each}
          </select>
        </label>

        <label class="field" style="margin-top:10px">
          <span>{$t.rpcTolerance || 'Tolérance paiement'} <strong>{rpcConfig.tolerance_pct}%</strong>
            <span class="text-muted" style="font-size:10px"> — {$t.rpcToleranceHint || 'ex: 20% = ±6j sur mensuel'}</span>
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
        <h1>{$t.createCodeTitle || 'Créer un code AutoPay de partage'}</h1>
        <p class="subs-subtitle">{$t.createCodeSubtitle || 'Pour marchands, créateurs et influenceurs — à diffuser à vos abonnés'}</p>
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
              <label class="sweep-label" for="sweep-dest">{$t.sweepDestLabel || 'Adresse de destination'}</label>
              <input id="sweep-dest"
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
        <h2 class="modal-title">{editingSubData ? "Modifier l'abonnement" : "Nouvel abonnement"} <span style="font-size:10px;opacity:0.5;font-weight:400">format v1</span></h2>
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
        <h2 class="modal-title">Importer un code AutoPay <span style="font-size:10px;opacity:0.5;font-weight:400">format v1</span></h2>
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
  /* ===== Payment animation ===== */
  .pay-anim-overlay {
    position: fixed; inset: 0; z-index: 9000;
    display: flex; flex-direction: column; align-items: center; justify-content: center;
    background: rgba(0,0,0,0.55); backdrop-filter: blur(4px);
    animation: pay-fade-in 0.3s ease;
    cursor: pointer;
  }
  .pay-anim-svg { width: 220px; height: 220px; overflow: visible; }
  .pay-anim-label {
    margin-top: 18px; font-size: 20px; font-weight: 700;
    color: var(--accent); letter-spacing: 0.04em;
    animation: pay-fade-in 0.5s ease 0.2s both;
  }
  /* Ripple expand */
  .ripple { transform-origin: 200px 200px; }
  .r1 { animation: ripple-out 1.8s ease-out 0s infinite; }
  .r2 { animation: ripple-out 1.8s ease-out 0.4s infinite; }
  .r3 { animation: ripple-out 1.8s ease-out 0.8s infinite; }
  @keyframes ripple-out {
    0%   { r: 36; opacity: 0.9; }
    100% { r: 140; opacity: 0; }
  }
  /* Checkmark draw */
  .pay-check {
    stroke-dasharray: 50; stroke-dashoffset: 50;
    animation: draw-check 0.5s ease 0.1s forwards;
  }
  @keyframes draw-check { to { stroke-dashoffset: 0; } }
  /* Orbiting dots */
  .orbit-dot { transform-origin: 200px 200px; }
  .od1 { animation: orbit 1.4s linear infinite; }
  .od2 { animation: orbit 1.4s linear 0.47s infinite; }
  .od3 { animation: orbit 1.4s linear 0.94s infinite; }
  @keyframes orbit {
    from { transform: rotate(0deg) translateY(-70px) rotate(0deg); }
    to   { transform: rotate(360deg) translateY(-70px) rotate(-360deg); }
  }
  @keyframes pay-fade-in { from { opacity: 0; transform: scale(0.9); } to { opacity: 1; transform: scale(1); } }

  /* Payment loader */
  .pay-loader-overlay { cursor: default; }
  .pay-loader-ring {
    width: 80px; height: 80px; border-radius: 50%;
    border: 6px solid rgba(var(--accent-rgb, 249,115,22), 0.2);
    border-top-color: var(--accent);
    animation: spin 1s linear infinite;
  }
  .pay-loader-countdown {
    position: absolute; font-size: 28px; font-weight: 700;
    color: var(--accent); line-height: 80px;
    width: 80px; text-align: center;
  }

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
  .alert-badge {
    font-size: 9px; font-weight: 700; min-width: 14px; height: 14px;
    border-radius: 7px; display: flex; align-items: center; justify-content: center;
    padding: 0 3px; color: white;
  }
  .badge-red { background: #ef4444; }
  .badge-orange { background: #f97316; }
  .alerts-panel {
    margin: 4px 8px; padding: 6px 8px;
    background: var(--bg-card); border-radius: 6px;
    display: flex; flex-direction: column; gap: 4px;
    border: 1px solid var(--border);
  }
  .alert-item {
    font-size: 10px; line-height: 1.3; color: var(--text);
    word-break: break-word;
  }
  .alert-red { color: #ef4444; }
  .alert-orange { color: #f97316; }

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
  .sync-progress-wrap { position: relative; height: 4px; background: var(--border); border-radius: 2px; margin: 3px 0 2px; overflow: hidden; }
  .sync-progress-bar { height: 100%; background: var(--accent); border-radius: 2px; transition: width 1s ease; }
  .sync-progress-label { font-size: 9px; color: var(--text-muted); }
  .sync-progress-label-small { font-size: 9px; color: var(--text-muted); margin-top: 2px; }
  .balance-amount { font-size: 16px; font-weight: 800; margin-bottom: 4px; }
  .balance-hint { font-size: 10px; color: var(--text-muted); margin-bottom: 4px; }
  .btn-inline { background: none; border: none; color: var(--accent); cursor: pointer; font-size: 10px; padding: 0; text-decoration: underline; }

  /* Receive page */
  .receive-page { max-width: 520px; margin: 0 auto; display: flex; flex-direction: column; align-items: center; gap: 20px; padding-top: 12px; }
  .qr-wrapper { background: #fff; padding: 16px; border-radius: 12px; box-shadow: 0 2px 12px rgba(0,0,0,.12); }
  .qr-wrapper :global(svg) { display: block; }
  .receive-address-box { display: flex; flex-direction: column; align-items: center; gap: 10px; width: 100%; }
  .receive-address { font-family: monospace; font-size: 12px; word-break: break-all; text-align: center; color: var(--text-muted); padding: 8px 12px; background: var(--surface); border-radius: 6px; border: 1px solid var(--border); width: 100%; box-sizing: border-box; }
  .btn-copy { padding: 8px 18px; background: var(--accent); color: #fff; border: none; border-radius: 7px; font-size: 13px; cursor: pointer; }
  .btn-copy:hover { opacity: 0.88; }
  .btn-receive { margin-top: 4px; padding: 6px 14px; font-size: 12px; background: var(--accent); color: #fff; border: none; border-radius: 6px; cursor: pointer; width: 100%; }
  .btn-receive:hover { opacity: 0.88; }
  /* Wallet card recharge button — larger, orange, slow glow */
  @keyframes recharge-glow {
    0%, 100% { box-shadow: 0 0 0 0 rgba(242,104,34,0); }
    50%       { box-shadow: 0 0 16px 6px rgba(242,104,34,0.55); }
  }
  .btn-recharge {
    background: var(--accent); color: #fff; border: none; border-radius: 8px;
    padding: 10px 20px; font-size: 14px; font-weight: 600; cursor: pointer;
    animation: recharge-glow 6s ease-in-out infinite;
    transition: opacity 0.15s, transform 0.1s;
  }
  .btn-recharge:hover { opacity: 0.9; transform: scale(1.03); }
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
  .dot-offline { color: #64748b; animation: pulse-dot 3s ease-in-out infinite; }
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

  /* Payment history */
  .ios-toggle-btn {
    width: 46px; height: 26px; border-radius: 13px; border: none; cursor: pointer;
    background: var(--border); position: relative; transition: background 0.2s;
    flex-shrink: 0;
  }
  .ios-toggle-btn.ios-on { background: var(--accent); }
  .ios-thumb-btn {
    position: absolute; top: 3px; left: 3px; width: 20px; height: 20px;
    border-radius: 50%; background: #fff; transition: left 0.2s; box-shadow: 0 1px 3px rgba(0,0,0,0.3);
  }
  .ios-toggle-btn.ios-on .ios-thumb-btn { left: 23px; }
  .history-list { display: flex; flex-direction: column; gap: 8px; }
  .history-card { padding: 12px 16px; }
  .history-row { display: flex; justify-content: space-between; align-items: center; }
  .history-label { font-weight: 600; font-size: 14px; }
  .history-amount { font-weight: 700; color: var(--accent); font-size: 14px; }
  .history-meta { margin-top: 4px; }
  .history-tx { font-size: 10px; color: var(--text-muted); font-family: monospace; }

  /* Alert overlay */
  .alerts-overlay-panel { width: 100%; }
  .alert-detail-card {
    display: flex; gap: 10px; padding: 10px 12px; border-radius: 8px;
    border: 1px solid var(--border);
  }
  .alert-detail-red { border-color: #ef444440; background: #ef444408; }
  .alert-detail-orange { border-color: #f9731640; background: #f9731608; }
  .alert-detail-icon { font-size: 18px; flex-shrink: 0; }
  .alert-detail-body { display: flex; flex-direction: column; gap: 3px; }
  .alert-detail-kind { font-size: 11px; font-weight: 700; color: var(--text-muted); letter-spacing: 0.05em; }
  .alert-detail-msg { font-size: 13px; color: var(--text); }
  .alert-detail-time { font-size: 10px; color: var(--text-muted); }

  .offline-banner {
    background: #1e3a5f; border: 1px solid #3b82f6; border-radius: 8px;
    padding: 8px 12px; font-size: 12px; color: #93c5fd;
    display: flex; align-items: center; gap: 8px; margin-bottom: 6px;
  }
  .lock-nav-btn {
    background: none; border: 2px solid var(--accent); border-radius: 50%;
    width: 38px; height: 38px; display: flex; align-items: center; justify-content: center;
    cursor: pointer; transition: all 0.15s; opacity: 0.7;
  }
  .lock-nav-btn:hover { background: rgba(242,104,34,0.12); opacity: 1; transform: scale(1.08); }
  .refill-urgent { border: 1px solid #f97316 !important; background: #f9731610 !important; }
  .refill-urgent-text { color: #ef4444 !important; font-size: 15px !important; animation: blink-urgent 1.2s ease-in-out infinite; }
  @keyframes blink-urgent {
    0%, 100% { opacity: 1; color: #ef4444; }
    50% { opacity: 0.55; color: #f97316; }
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
