import { defineConfig } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";
import path from "path";

export default defineConfig({
  plugins: [svelte()],
  resolve: {
    alias: {
      // Share the component library with the Tauri frontend
      "$lib": path.resolve(__dirname, "../src-frontend/src/lib"),
    },
  },
  build: {
    outDir: "dist",
    emptyOutDir: true,
  },
});
