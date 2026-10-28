import { writable, derived } from "svelte/store";
import fr from "./locales/fr.js";
import en from "./locales/en.js";
import de from "./locales/de.js";
import es from "./locales/es.js";

// Supported languages (no flags — globe icon used in UI)
export const LANGUAGES = [
  { code: "fr", label: "Français" },
  { code: "en", label: "English" },
  { code: "de", label: "Deutsch" },
  { code: "es", label: "Español" },
];

function detectLanguage() {
  const saved = localStorage.getItem("autopay_lang");
  if (saved && LANGUAGES.find(l => l.code === saved)) return saved;
  const nav = (navigator.language || navigator.languages?.[0] || "en").slice(0, 2).toLowerCase();
  return LANGUAGES.find(l => l.code === nav) ? nav : "en";
}

export const currentLang = writable(detectLanguage());
currentLang.subscribe(v => localStorage.setItem("autopay_lang", v));

const T = { fr, en, de, es };

export const t = derived(currentLang, ($lang) => ({ ...en, ...(T[$lang] || {}) }));

/** Replace "{name}" placeholders: fmt("in {n} days", { n: 3 }). */
export function fmt(str, params = {}) {
  return String(str ?? "").replace(/\{(\w+)\}/g, (m, k) => (k in params ? params[k] : m));
}
