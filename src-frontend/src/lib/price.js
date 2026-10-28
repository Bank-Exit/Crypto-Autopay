// XMR -> fiat rate, from LibreMonero only. Never fetched unless the user opted in.
// Shared by the Tauri app and the web generator.
import { writable } from "svelte/store";

// LibreMonero has no public price route yet: leave null until it exists.
// Expected shape: GET <url with {fiat}> -> { "price": 540.21 }
export const LIBREMONERO_PRICE_URL = null;

// Dev builds only, so the UI can be worked on before the route exists.
const DEV_MOCK_RATES = { EUR: 540, USD: 600, GBP: 450, CHF: 500 };

const OPT_IN_KEY = "autopay.fiatOptIn";
const TTL_MS = 5 * 60 * 1000;
const cache = new Map();

function readOptIn() {
  try { return localStorage.getItem(OPT_IN_KEY) === "1"; } catch { return false; }
}

export const fiatOptIn = writable(readOptIn());
fiatOptIn.subscribe(v => {
  try { localStorage.setItem(OPT_IN_KEY, v ? "1" : "0"); } catch { /* private mode */ }
});

/** Price of 1 XMR in `fiat`. Throws "PRICE_UNAVAILABLE" when no source is usable. */
export async function fetchXmrRate(fiat = "EUR") {
  fiat = String(fiat).toUpperCase();
  const hit = cache.get(fiat);
  if (hit && Date.now() - hit.at < TTL_MS) return hit.rate;

  let rate = null;
  if (LIBREMONERO_PRICE_URL) {
    const res = await fetch(LIBREMONERO_PRICE_URL.replace("{fiat}", encodeURIComponent(fiat)));
    if (res.ok) {
      const data = await res.json();
      rate = parseFloat(data.price ?? data.rate ?? data[fiat]);
    }
  } else if (import.meta.env.DEV) {
    rate = DEV_MOCK_RATES[fiat] ?? null;
  }
  if (!(rate > 0)) throw new Error("PRICE_UNAVAILABLE");
  cache.set(fiat, { rate, at: Date.now() });
  return rate;
}

export const FIAT_SYMBOLS = { EUR: "€", USD: "$", GBP: "£", CHF: "CHF" };
