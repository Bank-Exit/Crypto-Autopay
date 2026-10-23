import { writable } from "svelte/store";

function detectTheme() {
  const saved = localStorage.getItem("autopay_theme");
  if (saved === "dark" || saved === "light") return saved;
  // Default: light (warm, grand public)
  return "light";
}

export const theme = writable(detectTheme());

theme.subscribe(v => {
  localStorage.setItem("autopay_theme", v);
  document.documentElement.setAttribute("data-theme", v);
});

export function toggleTheme() {
  theme.update(v => v === "light" ? "dark" : "light");
}
