import { defineConfig } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";
import preprocess from "svelte-preprocess";

export default defineConfig(({ command }) => ({
  plugins: [
    // Dev: component CSS travels inside the JS module. As separate virtual CSS modules,
    // vite-plugin-svelte served the raw .svelte file whenever its CSS cache missed while
    // Vite still had the JS transform cached (unstyled components after a restart).
    svelte({ preprocess: preprocess(), emitCss: command === "build" }),
  ],
  server: {
    port: 5173,
    strictPort: true,
    watch: {
      ignored: ["**/src-tauri/**"],
    },
  },
  build: {
    outDir: "dist",
    target: "esnext",
  },
  clearScreen: false,
  envPrefix: ["VITE_", "TAURI_"],
}));
