import "./app.css";
import App from "./App.svelte";

// Apply saved theme before first render to avoid flash
const savedTheme = localStorage.getItem("autopay_theme") || "light";
document.documentElement.setAttribute("data-theme", savedTheme);

const app = new App({
  target: document.getElementById("app")!,
});

export default app;
