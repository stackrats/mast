import tailwindcss from "@tailwindcss/vite";
import vue from "@vitejs/plugin-vue";
import { defineConfig, lazyPlugins } from "vite-plus";

// Vite Plus is the primary toolchain (vp dev/build/test/check). The vanilla
// escape hatch lives in vite.config.fallback.ts and is proven continuously by
// scripts/verify-vanilla-fallback.sh in CI (plan §9).
export default defineConfig({
  plugins: lazyPlugins(() => [vue(), tailwindcss()]),
  resolve: {
    alias: { "@": new URL("./src", import.meta.url).pathname },
  },
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    // `target/` is the cargo build output at the workspace root — ~130k files
    // the dev server would otherwise hold inotify watches on, and which churn
    // on every `cargo build`, killing the watcher mid-session.
    watch: { ignored: ["**/src-tauri/**", "**/target/**"] },
  },
  test: {
    environment: "node",
    include: ["src/**/*.test.ts"],
  },
});
