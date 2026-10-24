/**
 * Payment mode store
 * "semi-auto" — manual confirmation required before each payment
 * "auto"      — payments sent automatically without confirmation
 */
import { writable } from "svelte/store";

const STORAGE_KEY = "autopay_payment_mode";

function detectMode() {
  try {
    const saved = localStorage.getItem(STORAGE_KEY);
    if (saved === "auto" || saved === "semi-auto") return saved;
  } catch {}
  return "semi-auto"; // safe default
}

export const paymentMode = writable(detectMode());

paymentMode.subscribe((v) => {
  try { localStorage.setItem(STORAGE_KEY, v); } catch {}
});
