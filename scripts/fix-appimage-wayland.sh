#!/usr/bin/env bash
#
# fix-appimage-wayland.sh — remove host-coupled Wayland libraries from a built
# AppImage and repack it.
#
# Why this exists
# ---------------
# linuxdeploy-plugin-gtk, which Tauri's AppImage bundler invokes, copies the
# build machine's libwayland-* into the AppDir. AppRun then puts
# <AppDir>/usr/lib ahead of the system libraries.
#
# On any host whose Mesa is not the build machine's, that breaks. Mesa provides
# EGL and links the *system* libwayland-client, while the app has loaded the
# bundled one. Two libwayland instances in one process means wl_proxy objects
# neither side recognises, and eglGetPlatformDisplay fails:
#
#     Could not create default EGL display: EGL_BAD_PARAMETER. Aborting...
#
# The app dies before drawing a window. Reproduced on Arch (Omarchy 4.0.3,
# Hyprland 0.56.2, Mesa 26.2.2) against Mast 0.7.0; removing exactly these four
# files makes it launch and render correctly. libepoxy, libgdk-3 and libgtk-3
# were each tested and are NOT implicated — they stay bundled.
#
# libwayland-client.so.0 is already on the upstream AppImage excludelist that
# linuxdeploy honours, so the GTK plugin is re-adding it after the excludelist
# has been applied. If that is fixed upstream this script turns into a no-op,
# says so, and can be deleted.
#
# Usage:
#   scripts/fix-appimage-wayland.sh <AppImage> [more...]   strip and repack
#   scripts/fix-appimage-wayland.sh --strip-only <AppDir>  strip in place
#
# --strip-only exists so the removal logic can be tested without appimagetool
# or a real AppImage; see scripts/test-fix-appimage-wayland.sh.
#
set -euo pipefail

# Anything that has to agree with the host's graphics stack. Everything above
# that line — WebKit, GTK, ICU, appindicator — is safe to bundle.
STRIP_LIBS=(
  libwayland-client.so.0
  libwayland-cursor.so.0
  libwayland-egl.so.1
  libwayland-server.so.0
)

info() { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m!!\033[0m  %s\n' "$*"; }
die()  { printf '\033[1;31mxx\033[0m  %s\n' "$*" >&2; exit 1; }

# Removes the listed libraries anywhere under an AppDir. Echoes how many files
# it deleted; the GTK plugin has used both usr/lib and usr/lib/<triplet> over
# time, so this does not assume a location.
strip_appdir() {
  local appdir="$1" lib found removed=0
  [[ -d "$appdir" ]] || { printf '0'; return 1; }
  for lib in "${STRIP_LIBS[@]}"; do
    while IFS= read -r found; do
      [[ -n "$found" ]] || continue
      rm -f "$found"
      printf '    removed %s\n' "${found#"$appdir"/}" >&2
      removed=$((removed + 1))
    done < <(find "$appdir" -type f -name "$lib" 2>/dev/null)
  done
  printf '%s' "$removed"
}

# --------------------------------------------------------------- modes -----
if [[ "${1:-}" == "--strip-only" ]]; then
  [[ -n "${2:-}" ]] || die "usage: $0 --strip-only <AppDir>"
  [[ -d "$2" ]] || die "Not a directory: $2"
  n=$(strip_appdir "$2") || die "strip failed"
  info "Removed $n file(s) from $2"
  exit 0
fi

(( $# )) || die "usage: $0 <AppImage> [more...]  |  $0 --strip-only <AppDir>"

# ---------------------------------------------------------- appimagetool ----
# CI runners have no libfuse2, so AppImages are invoked with
# --appimage-extract-and-run throughout.
find_appimagetool() {
  local candidate arch dest url
  for candidate in \
      "${APPIMAGETOOL:-}" \
      "$(command -v appimagetool 2>/dev/null || true)" \
      "$HOME/.cache/tauri/appimagetool-$(uname -m).AppImage" \
      "$HOME/.cache/tauri/appimagetool.AppImage"; do
    [[ -n "$candidate" && -x "$candidate" ]] && { printf '%s' "$candidate"; return 0; }
  done

  arch=$(uname -m)
  dest="${TMPDIR:-/tmp}/appimagetool-${arch}.AppImage"
  if [[ ! -x "$dest" ]]; then
    info "Downloading appimagetool for $arch" >&2
    for url in \
      "https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-${arch}.AppImage" \
      "https://github.com/AppImage/AppImageKit/releases/download/continuous/appimagetool-${arch}.AppImage"
    do
      curl -fsSL --retry 3 -o "$dest" "$url" && { chmod +x "$dest"; break; }
    done
    [[ -x "$dest" ]] || return 1
  fi
  printf '%s' "$dest"
}

APPIMAGETOOL=$(find_appimagetool) || die "Could not obtain appimagetool.
     Set APPIMAGETOOL=/path/to/appimagetool to use your own."
info "appimagetool: $APPIMAGETOOL"

for IMG in "$@"; do
  [[ -f "$IMG" ]] || die "Not a file: $IMG"
  IMG=$(readlink -f "$IMG")
  info "Processing $(basename "$IMG")"

  WORK=$(mktemp -d)
  trap 'rm -rf "$WORK"' EXIT

  ( cd "$WORK" && "$IMG" --appimage-extract >/dev/null ) || die "Could not extract $IMG"
  AD="$WORK/squashfs-root"
  [[ -d "$AD" ]] || die "No squashfs-root after extracting $IMG"

  removed=$(strip_appdir "$AD")
  if (( removed == 0 )); then
    warn "  none of the target libraries were present — nothing to do."
    warn "  Either upstream fixed this, or the bundle layout changed."
    warn "  Verify before assuming the workaround is still needed."
    rm -rf "$WORK"; trap - EXIT
    continue
  fi

  # Repack over the original path so downstream upload globs are unaffected.
  info "  repacking"
  ARCH="${ARCH:-$(uname -m)}" "$APPIMAGETOOL" --appimage-extract-and-run \
      "$AD" "$IMG" >/dev/null 2>&1 || die "appimagetool failed to repack $IMG"
  chmod +x "$IMG"

  # Prove it rather than trusting an exit code.
  VERIFY="$WORK/verify"; mkdir -p "$VERIFY"
  ( cd "$VERIFY" && "$IMG" --appimage-extract >/dev/null ) \
    || die "Repacked AppImage will not extract — it is broken."
  for lib in "${STRIP_LIBS[@]}"; do
    find "$VERIFY/squashfs-root" -type f -name "$lib" | grep -q . \
      && die "$lib survived the repack."
  done
  find "$VERIFY/squashfs-root" -name 'libwebkit2gtk-4.1.so.0' | grep -q . \
    || die "libwebkit2gtk missing from the repacked AppImage — bad repack."

  info "  verified: $removed removed, WebKit intact, $(stat -c %s "$IMG") bytes"
  rm -rf "$WORK"; trap - EXIT
done

info "Done."
