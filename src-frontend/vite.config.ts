import { defineConfig } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";
import preprocess from "svelte-preprocess";

export default defineConfig({
  plugins: [
    svelte({ preprocess: preprocess() }),
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
});
