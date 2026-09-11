#!/usr/bin/env bash
#
# test-fix-appimage-wayland.sh — regression test for the AppImage Wayland fix.
#
# Guards the packaging fix for the EGL_BAD_PARAMETER abort on non-Ubuntu hosts
# (see scripts/fix-appimage-wayland.sh). Runs against synthetic AppDirs, so it
# needs no network, no appimagetool and no real bundle, and finishes instantly.
#
# What it pins down:
#   * the four host-coupled libwayland files are removed
#   * libepoxy / libgdk-3 / libgtk-3 / libwebkit are NOT removed — each was
#     tested on Arch and is innocent; removing them would be an unnecessary
#     and riskier change
#   * both AppDir layouts are handled (usr/lib and usr/lib/<triplet>)
#   * the GTK plugin's hook stops forcing GDK_BACKEND=x11, prefers Wayland,
#     and still lets a user's own exported GDK_BACKEND win
#   * a second run is a clean no-op rather than an error
#   * a missing AppDir fails loudly instead of silently succeeding
#
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPT="$HERE/fix-appimage-wayland.sh"
fails=0

pass() { printf '  \033[1;32mok\033[0m    %s\n' "$*"; }
fail() { printf '  \033[1;31mFAIL\033[0m  %s\n' "$*"; fails=$((fails + 1)); }

[[ -x "$SCRIPT" || -f "$SCRIPT" ]] || { echo "missing $SCRIPT" >&2; exit 1; }

# A stand-in for what linuxdeploy-plugin-gtk actually produces: the offending
# Wayland libraries alongside the ones that must survive, spread over both
# directory layouts the plugin has used.
make_appdir() {
  local root="$1"
  mkdir -p "$root/usr/lib/x86_64-linux-gnu/webkit2gtk-4.1" "$root/usr/bin"
  local f
  for f in libwayland-client.so.0 libwayland-cursor.so.0 \
           libwayland-egl.so.1 libwayland-server.so.0 \
           libepoxy.so.0 libgdk-3.so.0 libgtk-3.so.0 \
           libwebkit2gtk-4.1.so.0 libicudata.so.74; do
    printf 'stub' > "$root/usr/lib/$f"
  done
  # The triplet directory is the other place the plugin has put them.
  printf 'stub' > "$root/usr/lib/x86_64-linux-gnu/libwayland-client.so.0"
  printf 'stub' > "$root/usr/bin/mast-desktop"
  # The hook as linuxdeploy-plugin-gtk writes it: the forced-X11 line between
  # unrelated exports that must come through untouched.
  mkdir -p "$root/apprun-hooks"
  cat > "$root/apprun-hooks/linuxdeploy-plugin-gtk.sh" <<'HOOK'
#! /usr/bin/env bash
export GTK_THEME="$APPIMAGE_GTK_THEME" # Custom themes are broken
export GDK_BACKEND=x11 # Crash with Wayland backend on Wayland - We tested it without it and ended up with this: https://github.com/tauri-apps/tauri/issues/8541
export XDG_DATA_DIRS="$APPDIR/usr/share:/usr/share:$XDG_DATA_DIRS" # g_get_system_data_dirs() from GLib
HOOK
}

# Runs a hook the way AppRun does and prints the GDK_BACKEND it leaves behind,
# given whatever the caller had exported beforehand.
backend_after() {
  local hook="$1"; shift
  env -i "$@" bash -c "source '$hook'; printf '%s' \"\$GDK_BACKEND\""
}

present() { [[ -e "$1" ]]; }

echo "fix-appimage-wayland.sh"

# ---------------------------------------------------------------- case 1 ----
T=$(mktemp -d); trap 'rm -rf "$T"' EXIT
make_appdir "$T/AppDir"
out=$(bash "$SCRIPT" --strip-only "$T/AppDir" 2>&1) || fail "strip-only exited non-zero"

for f in libwayland-client.so.0 libwayland-cursor.so.0 \
         libwayland-egl.so.1 libwayland-server.so.0; do
  if present "$T/AppDir/usr/lib/$f"; then
    fail "$f should have been removed"
  else
    pass "$f removed"
  fi
done

if present "$T/AppDir/usr/lib/x86_64-linux-gnu/libwayland-client.so.0"; then
  fail "libwayland-client.so.0 under the triplet dir should have been removed"
else
  pass "triplet-dir copy removed too"
fi

# ---------------------------------------------------------------- case 2 ----
# The innocents. Each of these was bisected on Arch and left bundled; if a
# future edit widens the list, this is what should stop it.
for f in libepoxy.so.0 libgdk-3.so.0 libgtk-3.so.0 \
         libwebkit2gtk-4.1.so.0 libicudata.so.74; do
  if present "$T/AppDir/usr/lib/$f"; then
    pass "$f left in place"
  else
    fail "$f must NOT be removed — it is not implicated in the EGL failure"
  fi
done

present "$T/AppDir/usr/bin/mast-desktop" \
  && pass "application binary untouched" \
  || fail "application binary went missing"

# ---------------------------------------------------------------- case 2b ---
# The hook. What matters is what a shell ends up with after sourcing it, not
# what the line looks like — so the rewritten hook is actually run.
HOOK="$T/AppDir/apprun-hooks/linuxdeploy-plugin-gtk.sh"
if grep -qF 'export GDK_BACKEND=x11' "$HOOK"; then
  fail "the hook still forces GDK_BACKEND=x11"
else
  pass "the forced-X11 line is gone"
fi

got=$(backend_after "$HOOK")
if [[ "$got" == "wayland,x11" ]]; then
  pass "with nothing exported, the hook prefers Wayland and falls back to X11"
else
  fail "expected GDK_BACKEND=wayland,x11 from a clean environment, got '$got'"
fi

# The escape hatch: someone whose Wayland session genuinely misbehaves can
# export GDK_BACKEND=x11 themselves, and the hook must not overwrite it — which
# is exactly what the original unconditional export did.
got=$(backend_after "$HOOK" GDK_BACKEND=x11)
if [[ "$got" == "x11" ]]; then
  pass "an exported GDK_BACKEND=x11 survives the hook"
else
  fail "the hook overwrote the user's GDK_BACKEND (got '$got')"
fi

# The lines either side of the rewrite are untouched: the rewrite is a
# whole-line replacement of one export, not a sed over the file.
if grep -qF 'export GTK_THEME="$APPIMAGE_GTK_THEME"' "$HOOK" \
   && grep -qF 'export XDG_DATA_DIRS="$APPDIR/usr/share' "$HOOK"; then
  pass "neighbouring exports left alone"
else
  fail "the rewrite disturbed lines other than the GDK_BACKEND export"
fi

# ---------------------------------------------------------------- case 3 ----
# Idempotency: re-running over an already-clean AppDir must succeed quietly,
# because the release workflow may be re-run against the same artefacts.
if bash "$SCRIPT" --strip-only "$T/AppDir" >/dev/null 2>&1; then
  pass "second run is a clean no-op"
else
  fail "second run should exit 0 on an already-stripped AppDir"
fi
# …and a second pass must not rewrite the already-rewritten line into
# something else, or stack a second replacement onto it.
if (( $(grep -c 'GDK_BACKEND' "$HOOK") == 1 )); then
  pass "second run leaves exactly one GDK_BACKEND export"
else
  fail "second run changed the hook again"
fi

# An AppDir with no hook at all (a future plugin that stopped writing one) is
# not an error: there is simply nothing to rewrite.
rm -rf "$T/AppDir/apprun-hooks"
if bash "$SCRIPT" --strip-only "$T/AppDir" >/dev/null 2>&1; then
  pass "an AppDir without the hook is fine"
else
  fail "a missing hook should not be an error"
fi

# ---------------------------------------------------------------- case 4 ----
# A typo in a path must not look like success.
if bash "$SCRIPT" --strip-only "$T/does-not-exist" >/dev/null 2>&1; then
  fail "a missing AppDir should be an error"
else
  pass "missing AppDir rejected"
fi

# ---------------------------------------------------------------- case 5 ----
if bash "$SCRIPT" --strip-only >/dev/null 2>&1; then
  fail "--strip-only with no argument should be an error"
else
  pass "--strip-only requires an argument"
fi

echo
if (( fails )); then
  printf '\033[1;31m%d check(s) failed\033[0m\n' "$fails"
  exit 1
fi
printf '\033[1;32mall checks passed\033[0m\n'
