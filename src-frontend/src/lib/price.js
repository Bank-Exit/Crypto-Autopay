// XMR -> fiat rate, from LibreMonero only. Never fetched unless the user opted in.
// Shared by the Tauri app and the web generator.
import { writable } from "svelte/store";

// GET -> { "xmr": { "eur": 538.98, "usd": 604.07, ... }, "updated_at": "<RFC 3339>", "stale": false, ... }
export const LIBREMONERO_PRICE_URL = "https://libremonero.com/api/v1/price";

// The onion mirror is tried first by the desktop app, through a local Tor client
// (Rust command `fetch_price_onion`, URL kept in the binary). Clearnet is the fallback.
const ONION_COMMAND = "fetch_price_onion";

// Older quotes are not shown: a wrong fiat value is worse than none.
const MAX_QUOTE_AGE_MS = 60 * 60 * 1000;
const FETCH_TIMEOUT_MS = 10 * 1000;

const OPT_IN_KEY = "autopay.fiatOptIn";
const TTL_MS = 5 * 60 * 1000;
let cache = null;                // { quotes: { eur: 538.98, ... }, at }
let inflight = null;

function readOptIn() {
  try { return localStorage.getItem(OPT_IN_KEY) === "1"; } catch { return false; }
}

export const fiatOptIn = writable(readOptIn());
fiatOptIn.subscribe(v => {
  try { localStorage.setItem(OPT_IN_KEY, v ? "1" : "0"); } catch { /* private mode */ }
});

/** XMR quotes from a LibreMonero payload, or null when it is malformed or too old. */
export function parseQuotes(data, now = Date.now()) {
  const xmr = data?.xmr;
  if (!xmr || typeof xmr !== "object") return null;
  const updated = Date.parse(data.updated_at);
  if (!Number.isFinite(updated) || now - updated > MAX_QUOTE_AGE_MS) return null;
  const quotes = {};
  for (const [k, v] of Object.entries(xmr)) {
    if (typeof v === "number" && Number.isFinite(v) && v > 0) quotes[k.toLowerCase()] = v;
  }
  return Object.keys(quotes).length ? quotes : null;
}

async function fetchOnion() {
  if (!globalThis.__TAURI_INTERNALS__) return null;
  try {
    const { invoke } = await import("@tauri-apps/api/core");
    return parseQuotes(await invoke(ONION_COMMAND));
  } catch {
    return null; // no onion URL yet, or no local Tor client
  }
}

async function fetchClearnet() {
  const ctrl = new AbortController();
  const timer = setTimeout(() => ctrl.abort(), FETCH_TIMEOUT_MS);
  try {
    const res = await fetch(LIBREMONERO_PRICE_URL, {
      signal: ctrl.signal, credentials: "omit", referrerPolicy: "no-referrer", cache: "no-store",
    });
    return res.ok ? parseQuotes(await res.json()) : null;
  } catch {
    return null;
  } finally {
    clearTimeout(timer);
  }
}

async function loadQuotes() {
  if (cache && Date.now() - cache.at < TTL_MS) return cache.quotes;
  inflight ??= (async () => {
    const quotes = (await fetchOnion()) ?? (await fetchClearnet());
    if (quotes) cache = { quotes, at: Date.now() };
    return quotes;
  })().finally(() => { inflight = null; });
  return inflight;
}

/** Price of 1 XMR in `fiat`. Throws "PRICE_UNAVAILABLE" when no source is usable. */
export async function fetchXmrRate(fiat = "EUR") {
  const quotes = await loadQuotes();
  const rate = quotes?.[String(fiat).toLowerCase()];
  if (!(rate > 0)) throw new Error("PRICE_UNAVAILABLE");
  return rate;
}

export const FIAT_SYMBOLS = { EUR: "€", USD: "$", GBP: "£", CHF: "CHF" };
