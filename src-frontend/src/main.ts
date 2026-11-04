import "./app.css";
import { invoke } from "@tauri-apps/api/core";
import App from "./App.svelte";

// Apply saved theme before first render to avoid flash
const savedTheme = localStorage.getItem("autopay_theme") || "light";
document.documentElement.setAttribute("data-theme", savedTheme);

const app = new App({
  target: document.getElementById("app")!,
});

// Tells the backend the interface is up (otherwise it shows the startup journal).
invoke("boot_frontend_ready").catch(() => {});

export default app;
