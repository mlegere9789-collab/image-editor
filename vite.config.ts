import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Vite is only ever served to the Tauri webview, so the dev server is pinned to
// a fixed port that `tauri.conf.json` points at via `build.devUrl`.
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    watch: {
      // Rust sources are watched by the Tauri CLI, not by Vite.
      ignored: ["**/src-tauri/**"],
    },
  },
});
