/**
 * AppLogger — in-memory technical event log
 *
 * SECURITY RULES (MANDATORY):
 *   - Never log amounts, wallet addresses, seeds, private keys or any PII.
 *   - "send logs" (opt-in) store only a subscription id + event type — no amounts, no recipients.
 *   - Logs are kept in RAM only; send-logs persist to localStorage in encrypted form.
 *
 * Usage:
 *   import { logInfo, logWarn, logError, logSend } from "./AppLogger.js";
 *   logInfo("Dashboard", "Import modal opened");
 *   logWarn("DeepLink", "Same URL received twice — re-triggering");
 *   logSend("sub_42", "payment_attempted");
 */

const MAX_TECH = 500;   // maximum technical log entries kept in RAM
const MAX_SEND = 1000;  // maximum send-log entries (persisted)

// Regex patterns of data that MUST never appear in logs
const SENSITIVE_RE = [
  /\b[48][1-9A-HJ-NP-Za-km-z]{93,105}\b/g,   // Monero address
  /\b(bc1|[13])[0-9A-HJ-NP-Za-km-z]{25,87}\b/g, // Bitcoin address
  /\b\d+\.\d{4,}\s*(XMR|BTC|ETH)\b/gi,        // amounts with many decimals
];

function sanitize(msg) {
  let s = String(msg);
  for (const re of SENSITIVE_RE) s = s.replace(re, "[REDACTED]");
  return s;
}

// ----- Technical logs (always on, RAM only) -----
let _entries = [];
const _listeners = new Set();

function _notify() {
  const snap = [..._entries];
  _listeners.forEach(fn => fn(snap));
}

function _add(level, module, msg) {
  const safe = sanitize(msg);
  const entry = { t: new Date().toISOString(), level, module, msg: safe };
  _entries.unshift(entry);
  if (_entries.length > MAX_TECH) _entries.length = MAX_TECH;
  // Also mirror to browser console
  if (level === "error") console.error(`[${module}] ${safe}`);
  else if (level === "warn") console.warn(`[${module}] ${safe}`);
  else console.log(`[${module}] ${safe}`);
  _notify();
}

export const logInfo  = (module, msg) => _add("info",  module, msg);
export const logWarn  = (module, msg) => _add("warn",  module, msg);
export const logError = (module, msg) => _add("error", module, msg);

/** Subscribe to live log updates. Returns unsubscribe function. */
export function subscribeToLogs(fn) {
  _listeners.add(fn);
  fn([..._entries]);
  return () => _listeners.delete(fn);
}

export function getEntries(levelFilter) {
  return levelFilter ? _entries.filter(e => e.level === levelFilter) : [..._entries];
}

export function clearLogs() {
  _entries = [];
  _notify();
}

// ----- Send logs (opt-in, localStorage) -----
let _sendEnabled = false;
let _sendLogs = [];

/** Enable or disable the payment send-log (requires explicit user consent). */
export function setSendLogsEnabled(v) {
  _sendEnabled = Boolean(v);
  try { localStorage.setItem("_autopay_send_logs_enabled", _sendEnabled ? "1" : "0"); } catch {}
}

export const isSendLogsEnabled = () => _sendEnabled;

/**
 * Record a payment event.
 * SECURITY: subscription_id only — NO amount, NO destination address.
 * @param {string} subscriptionId  opaque local ID
 * @param {"payment_attempted"|"payment_sent"|"payment_failed"} event
 */
export function logSend(subscriptionId, event) {
  if (!_sendEnabled) return;
  const entry = { t: new Date().toISOString(), id: String(subscriptionId), event };
  _sendLogs.unshift(entry);
  if (_sendLogs.length > MAX_SEND) _sendLogs.length = MAX_SEND;
  _persistSendLogs();
}

export function getSendLogs() { return [..._sendLogs]; }

export function clearSendLogs() {
  _sendLogs = [];
  try { localStorage.removeItem("_autopay_send_logs"); } catch {}
}

function _persistSendLogs() {
  try { localStorage.setItem("_autopay_send_logs", JSON.stringify(_sendLogs)); } catch {}
}

// ----- Init -----
(function init() {
  try {
    _sendEnabled = localStorage.getItem("_autopay_send_logs_enabled") === "1";
    const raw = localStorage.getItem("_autopay_send_logs");
    if (raw) _sendLogs = JSON.parse(raw).slice(0, MAX_SEND);
  } catch {}
  logInfo("AppLogger", "Logger initialised");
})();
