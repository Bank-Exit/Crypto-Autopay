/**
 * crypto-autopay:1: protocol encoder/decoder
 * Browser-native: uses CompressionStream API (no deps, works in Tauri WebView + modern browsers)
 * Also handles monero-request:1: import (backward compat)
 */

const PREFIX = "crypto-autopay";
const LEGACY_PREFIX = "monero-request";
const VERSION = 1;

/**
 * Encode a subscription payload to a crypto-autopay:1: code.
 * Sorts keys canonically, gzips, base64url-encodes.
 * @param {Object} payload - The subscription fields (null/undefined/empty-string fields are stripped)
 * @returns {Promise<string>} The full code string
 */
export async function encodeAutopay(payload) {
  // Strip null/undefined/empty-string optional fields, keep required ones
  const clean = Object.fromEntries(
    Object.entries(payload)
      .filter(([_, v]) => v !== null && v !== undefined && v !== "" && v !== 0 || isRequired(_, v))
      .sort(([a], [b]) => a.localeCompare(b)) // canonical sort
  );

  const json = JSON.stringify(clean, null, 0); // compact
  const bytes = new TextEncoder().encode(json);

  const stream = new Blob([bytes]).stream().pipeThrough(new CompressionStream("gzip"));
  const buf = await new Response(stream).arrayBuffer();

  const b64 = btoa(String.fromCharCode(...new Uint8Array(buf)))
    .replace(/\+/g, "-")
    .replace(/\//g, "_")
    .replace(/=/g, "");

  return `${PREFIX}:${VERSION}:${b64}`;
}

/**
 * Decode a crypto-autopay:1: or monero-request:1: code.
 * @param {string} code
 * @returns {Promise<{prefix: string, version: number, payload: Object}>}
 */
export async function decodeAutopay(code) {
  const trimmed = code.trim();
  const parts = trimmed.split(":");
  if (parts.length < 3) throw new Error("Invalid format: expected prefix:version:payload");

  const prefix = parts[0];
  if (prefix !== PREFIX && prefix !== LEGACY_PREFIX) {
    throw new Error(`Unknown protocol prefix: ${prefix}`);
  }

  const version = parseInt(parts[1], 10);
  const b64raw = parts.slice(2).join(":"); // rejoin in case payload had colons (shouldn't, but safe)
  const pad = b64raw + "=".repeat((-b64raw.length) & 3);
  const binary = atob(pad.replace(/-/g, "+").replace(/_/g, "/"));
  const bytes = Uint8Array.from(binary, (c) => c.charCodeAt(0));

  const stream = new Blob([bytes]).stream().pipeThrough(new DecompressionStream("gzip"));
  const text = await new Response(stream).text();
  const payload = JSON.parse(text);

  // Normalize legacy monero-request fields
  if (prefix === LEGACY_PREFIX) {
    return { prefix, version, payload: normalizeLegacy(payload) };
  }

  return { prefix, version, payload };
}

/**
 * Human-readable summary of a schedule + repeat_every combo.
 * @param {string} schedule - Cron expression
 * @param {number} repeatEvery - Multiplier (default 1)
 * @param {string} lang - "fr" | "en" | "de" | "es"
 * @returns {string}
 */
export function scheduleToHuman(schedule, repeatEvery = 1, lang = "en") {
  const labels = SCHEDULE_LABELS[lang] || SCHEDULE_LABELS.en;
  const n = parseInt(repeatEvery, 10) || 1;
  if (!schedule) return "";
  const s = schedule.trim();

  // Normalise cron to structural fields only (dom, month, dow), ignoring sec/min/hour.
  // Supports both 6-field Quartz "sec min hour dom month dow"
  //             and 5-field standard "min hour dom month dow".
  const parts = s.split(/\s+/);
  let dom, month, dow;
  if (parts.length === 6) { [,,,dom, month, dow] = parts; }
  else if (parts.length === 5) { [,,dom, month, dow] = parts; }
  else { return schedule + (n > 1 ? ` (×${n})` : ""); }

  // Match on structural pattern
  if (dom === "1" && month === "*" && dow === "*")  return n === 1 ? labels.monthly   : `${labels.every} ${n} ${labels.months}`;
  if (dom === "*" && month === "*" && dow === "1")  return n === 1 ? labels.weekly    : `${labels.every} ${n} ${labels.weeks}`;
  if (dom === "1" && month === "*/3" && dow === "*")return n === 1 ? labels.quarterly : `${labels.every} ${n * 3} ${labels.months}`;
  if (dom === "1" && month === "1"  && dow === "*") return n === 1 ? labels.yearly    : `${labels.every} ${n} ${labels.years}`;
  if (dom === "*" && month === "*"  && dow === "*") return labels.daily;
  // Biweekly / every-N-days
  if (dom === "*/14" || dom === "1,15") return n === 1 ? labels.biweekly || "Bimensuel" : `${labels.every} 2 ${labels.weeks}`;
  return schedule + (n > 1 ? ` (×${n})` : "");
}

const SCHEDULE_LABELS = {
  fr: { daily: "Quotidien", weekly: "Hebdomadaire", biweekly: "Bimensuel", monthly: "Mensuel", quarterly: "Trimestriel",
        yearly: "Annuel", every: "Tous les", months: "mois", weeks: "semaines", years: "ans" },
  en: { daily: "Daily", weekly: "Weekly", biweekly: "Biweekly", monthly: "Monthly", quarterly: "Quarterly",
        yearly: "Yearly", every: "Every", months: "months", weeks: "weeks", years: "years" },
  de: { daily: "Täglich", weekly: "Wöchentlich", biweekly: "Zweiwöchentlich", monthly: "Monatlich", quarterly: "Vierteljährlich",
        yearly: "Jährlich", every: "Alle", months: "Monate", weeks: "Wochen", years: "Jahre" },
  es: { daily: "Diario", weekly: "Semanal", biweekly: "Quincenal", monthly: "Mensual", quarterly: "Trimestral",
        yearly: "Anual", every: "Cada", months: "meses", weeks: "semanas", years: "años" },
};

/** Known preset schedules — 6-field Quartz cron (sec min hour dom month dow) */
export const SCHEDULE_PRESETS = [
  { id: "daily",     cron: "0 0 9 * * *",   label_fr: "Quotidien",      label_en: "Daily" },
  { id: "weekly",    cron: "0 0 9 * * 1",   label_fr: "Hebdomadaire",   label_en: "Weekly" },
  { id: "monthly",   cron: "0 0 9 1 * *",   label_fr: "Mensuel",        label_en: "Monthly" },
  { id: "quarterly", cron: "0 0 9 1 */3 *", label_fr: "Trimestriel",    label_en: "Quarterly" },
  { id: "yearly",    cron: "0 0 9 1 1 *",   label_fr: "Annuel",         label_en: "Yearly" },
  { id: "biennial",  cron: "0 0 9 1 1 *",   label_fr: "Tous les 2 ans", label_en: "Every 2 years", repeatEvery: 2 },
  { id: "triennial", cron: "0 0 9 1 1 *",   label_fr: "Tous les 3 ans", label_en: "Every 3 years", repeatEvery: 3 },
  { id: "custom",    cron: "",              label_fr: "Personnalisé",   label_en: "Custom" },
];

/** Known networks and their common tokens */
export const NETWORKS_EXTENDED = [
  { id: "monero",   label: "Monero",   symbol: "XMR", color: "#f26822", tokens: [] },
  { id: "bitcoin",  label: "Bitcoin",  symbol: "BTC", color: "#f7931a", tokens: [] },
  { id: "ethereum", label: "Ethereum", symbol: "ETH", color: "#627eea", tokens: ["USDT", "USDC", "DAI", "WBTC"] },
  { id: "tron",     label: "Tron",     symbol: "TRX", color: "#ff0013", tokens: ["USDT", "USDC"] },
  { id: "solana",   label: "Solana",   symbol: "SOL", color: "#9945ff", tokens: ["USDT", "USDC", "BONK"] },
  { id: "polygon",  label: "Polygon",  symbol: "MATIC", color: "#8247e5", tokens: ["USDT", "USDC", "DAI"] },
];

// --- Internal helpers ---

function isRequired(key, _v) {
  return ["version", "network", "custom_label", "destination_address", "amount", "currency", "schedule"].includes(key);
}

/**
 * Map monero-request:1: fields to crypto-autopay:1: equivalents.
 */
function normalizeLegacy(p) {
  const cycleDays = p.billing_cycle_days || 30;
  let schedule = "0 0 9 1 * *";
  if (cycleDays === 1) schedule = "0 0 9 * * *";
  else if (cycleDays === 7) schedule = "0 0 9 * * 1";
  else if (cycleDays === 365) schedule = "0 0 9 1 1 *";
  else if (cycleDays >= 2) schedule = `0 0 9 */${cycleDays} * *`;

  return {
    version: 1,
    network: "monero",
    custom_label: p.custom_label || "",
    description: p.description || "",
    destination_address: p.sellers_wallet || "",
    amount: String(p.amount || "0"),
    currency: p.currency || "XMR",
    schedule,
    number_of_payments: p.number_of_payments || 0,
    start_date: p.start_date || null,
    _imported_from: "monero-request:1:",
  };
}
