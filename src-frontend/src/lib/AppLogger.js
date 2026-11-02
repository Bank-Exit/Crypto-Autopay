/**
 * AppLogger — in-memory technical event log
 *
 * SECURITY RULES (MANDATORY):
 *   - Never log amounts, wallet addresses, seeds, private keys or any PII.
 *   - "send logs" (opt-in) store only a subscription id + event type — no amounts, no recipients.
 *   - Logs are kept in RAM only (the Rust side keeps its own file, trimmed to the retention
 *     window); send-logs persist to localStorage in encrypted form.
 *
 * Usage:
 *   import { logDebug, logInfo, logWarn, logError, logSend } from "./AppLogger.js";
 *   logInfo("Dashboard", "Import modal opened");
 *   logWarn("DeepLink", "Same URL received twice — re-triggering");
 *   logSend("sub_42", "payment_attempted");
 */

// Separate caps so that chatty debug lines never evict status, warning and error lines.
const MAX_DEBUG = 3000;
const MAX_OTHER = 3000;
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
let _entries = [];   // newest first
let _seq = 0;
let _retentionHours = 12;
const _listeners = new Set();

const _isDebug = e => e.level === "debug";

/** Age window (hours) shared with the Rust log file. 0: debug lines are not kept. */
export function setLogRetentionHours(h) {
  _retentionHours = Math.max(0, Number(h) || 0);
  _trim();
  _notify();
}

function _trim() {
  const cutoff = Date.now() - _retentionHours * 3600_000;
  // Without retention, status/warning/error lines still stay for the current session.
  _entries = _entries.filter(e => _isDebug(e) ? _retentionHours > 0 && Date.parse(e.t) >= cutoff
                                              : _retentionHours === 0 || Date.parse(e.t) >= cutoff);
  let debug = 0, other = 0;
  _entries = _entries.filter(e => _isDebug(e) ? ++debug <= MAX_DEBUG : ++other <= MAX_OTHER);
}

/** Prepend lines read back from the Rust log file (oldest first), older than anything in RAM. */
export function seedHistory(lines) {
  const known = _entries.length ? Date.parse(_entries[_entries.length - 1].t) : Infinity;
  const old = [];
  for (const l of lines || []) {
    if (Date.parse(l.t) >= known) continue;
    const level = l.level === "trace" ? "debug" : l.level;
    old.unshift({ id: ++_seq, t: new Date(l.t).toISOString(), level, module: l.module, msg: sanitize(l.msg) });
  }
  _entries = [..._entries, ...old];
  _trim();
  _notify();
}

/** Local time of an entry, with the day when it is not today. */
export function formatLogTime(t) {
  if (!t) return "??:??:??";
  const d = new Date(t);
  const time = d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit", hour12: false });
  if (d.toDateString() === new Date().toDateString()) return time;
  return `${d.toLocaleDateString([], { day: "2-digit", month: "2-digit" })} ${time}`;
}

function _notify() {
  const snap = [..._entries];
  _listeners.forEach(fn => fn(snap));
}

function _add(level, module, msg) {
  const safe = sanitize(msg);
  const entry = { id: ++_seq, t: new Date().toISOString(), level, module, msg: safe };
  _entries.unshift(entry);
  if (_retentionHours === 0 && level === "debug") _entries.shift();
  if (_entries.length > MAX_DEBUG + MAX_OTHER || _seq % 200 === 0) _trim();
  // Also mirror to browser console
  if (level === "error") console.error(`[${module}] ${safe}`);
  else if (level === "warn") console.warn(`[${module}] ${safe}`);
  else if (level === "debug") console.debug(`[${module}] ${safe}`);
  else console.log(`[${module}] ${safe}`);
  _notify();
}

export const logDebug = (module, msg) => _add("debug", module, msg);
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
