// Vanilla-vite escape hatch (plan §9). Used ONLY by
// scripts/verify-vanilla-fallback.sh, which assembles a temp project with
// plain vite + vitest (the workspace itself routes `vite` to vite-plus-core
// via a pnpm override, so vanilla proof must happen outside it).
//
// Imports from "vitest/config" — resolvable in the temp project, not here.

import tailwindcss from "@tailwindcss/vite";
import vue from "@vitejs/plugin-vue";
import type { Plugin } from "vite";
import { defineConfig } from "vitest/config";

// Source imports vite-plus's test API (its lint rule enforces this), and a
// `test.alias` is enough to resolve those symbols to vitest's — but not for
// `vi.mock`, whose hoisting pass looks for a literal import of "vitest" and
// bails with "problems in resolving the mocks API" when it finds none. So the
// specifier is rewritten before that pass ever sees the module.
const vitePlusTestAsVitest: Plugin = {
  name: "fallback:vite-plus-test-as-vitest",
  enforce: "pre",
  transform(code, id) {
    if (!id.includes(".test.") || !code.includes("vite-plus/test")) return null;
    return { code: code.replaceAll(/(["'])vite-plus\/test\1/g, "$1vitest$1"), map: null };
  },
};

export default defineConfig({
  plugins: [vitePlusTestAsVitest, vue(), tailwindcss()],
  resolve: {
    alias: { "@": new URL("./src", import.meta.url).pathname },
  },
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    watch: { ignored: ["**/src-tauri/**", "**/target/**"] },
  },
  test: {
    environment: "node",
    include: ["src/**/*.test.ts"],
    // Belt and braces for any non-test module that reaches for the API: the
    // transform above only rewrites the test files themselves.
    alias: { "vite-plus/test": "vitest" },
  },
});
