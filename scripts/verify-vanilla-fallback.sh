#!/usr/bin/env bash
# Proves the vanilla-vite escape hatch (plan §9): if Vite Plus (beta) breaks,
# the frontend must still test and build with plain vite + vitest.
#
# The workspace routes every `vite` resolution to vite-plus-core via a pnpm
# override, so this assembles the frontend in a temp dir OUTSIDE the
# workspace, installs vanilla tooling with npm, and runs the suite + build
# against vite.config.fallback.ts. Run locally or in CI; needs node + npm.
set -euo pipefail

APP_DIR="$(cd "$(dirname "$0")/../clients/desktop-vue" && pwd)"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/mast-fallback.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT

cp -r "$APP_DIR/src" "$APP_DIR/index.html" "$APP_DIR/tsconfig.json" \
      "$APP_DIR/vite.config.fallback.ts" "$WORK/"

# Derived from the app's own package.json rather than hand-written, so a newly
# added dependency cannot leave the fallback silently unbuildable: drop the
# vite-plus-only tooling, and pin real vite + vitest in its place.
node -e '
const fs = require("node:fs");
const app = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
const deps = { ...app.dependencies };
const dev = { ...app.devDependencies };
for (const vpOnly of ["vite-plus", "vue-tsc"]) delete dev[vpOnly];
// The workspace catalog routes `vite` to vite-plus-core; vanilla means vanilla.
dev.vite = "^7";
dev.vitest = "^3";
const local = Object.entries({ ...deps, ...dev })
  .filter(([, v]) => /^(catalog|workspace|link|file):/.test(v))
  .map(([k, v]) => k + "@" + v);
if (local.length) {
  console.error("fallback: no plain-npm version for " + local.join(", "));
  process.exit(1);
}
fs.writeFileSync(process.argv[2], JSON.stringify(
  { name: "mast-desktop-ui-fallback", private: true, type: "module",
    dependencies: deps, devDependencies: dev }, null, 2) + "\n");
' "$APP_DIR/package.json" "$WORK/package.json"

cd "$WORK"
echo "== installing vanilla toolchain (npm) =="
npm install --no-audit --no-fund --loglevel=error

echo "== vitest (vanilla) =="
npx vitest run -c vite.config.fallback.ts

echo "== vite build (vanilla) =="
npx vite build -c vite.config.fallback.ts

echo "vanilla fallback: OK"
