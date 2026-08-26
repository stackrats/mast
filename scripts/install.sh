#!/bin/sh
# Mast's one-line installer — what sits behind `curl -fsSL https://mast.sh/install | sh`.
#
# Puts the `mast` CLI and the headless `mast-daemon` from a GitHub release onto
# PATH, and nothing else. The desktop app ships as a .deb/.rpm/.AppImage/.dmg/
# .exe and keeps its own install story in the README; a curl pipe is the wrong
# shape for dragging a GUI into /Applications.
#
# Deliberately POSIX sh, because the shell on the right of that pipe is
# whatever the reader happens to have — dash, ash, bash and zsh all run this
# unchanged. No sudo is ever asked for: the default target is a per-user
# directory, and MAST_INSTALL_DIR moves it.
#
# By default this installs the version of Mast you ALREADY have — if the
# desktop app is installed, the CLI is matched to it rather than to whatever
# is newest. The two talk to each other over a per-user socket and refuse to
# run as a mismatched pair, so "latest" is the wrong default whenever an app
# is already sitting there at 0.5 waiting for a CLI to agree with it.
#
#   MAST_VERSION=v0.5.0    pin a release; overrides the desktop match
#   MAST_INSTALL_DIR=DIR   where the two binaries land (default ~/.local/bin)
#
# The same two knobs exist as flags. Through a pipe they need sh's `-s --`
# handoff, since the script itself is occupying stdin:
#
#   curl -fsSL https://mast.sh/install | sh -s -- --version v0.5.0
#
# Everything lives in functions with `main "$@"` on the last line, so a
# download truncated mid-flight defines a few functions and then runs none of
# them, rather than executing half an installer.

set -eu

REPO="stackrats/mast"
RELEASES="https://github.com/${REPO}/releases"

# ---------------------------------------------------------------- output ----

# Colour only when a human is watching stderr; piped into a log or a CI step
# it stays plain text. All chatter goes to stderr so that stdout is free for
# anything a caller wants to capture.
if [ -t 2 ]; then
    C_RESET=$(printf '\033[0m')  C_BOLD=$(printf '\033[1m')
    C_DIM=$(printf '\033[2m')    C_BLUE=$(printf '\033[34m')
    C_GREEN=$(printf '\033[32m') C_YELLOW=$(printf '\033[33m')
    C_RED=$(printf '\033[31m')
else
    C_RESET='' C_BOLD='' C_DIM='' C_BLUE='' C_GREEN='' C_YELLOW='' C_RED=''
fi

say()  { printf '%s\n' "$*" >&2; }
step() { printf '%s==>%s %s\n' "$C_BLUE$C_BOLD" "$C_RESET" "$*" >&2; }
warn() { printf '%s warn%s %s\n' "$C_YELLOW$C_BOLD" "$C_RESET" "$*" >&2; }
err()  { printf '%serror%s %s\n' "$C_RED$C_BOLD" "$C_RESET" "$*" >&2; exit 1; }

usage() {
    cat >&2 <<'USAGE'
Install the Mast CLI (mast + mast-daemon).

  curl -fsSL https://mast.sh/install | sh
  curl -fsSL https://mast.sh/install | sh -s -- --version v0.5.0

By default this matches the Mast desktop app already installed on this
machine, because the CLI and the app share a daemon socket and refuse to run
as a mismatched pair. With no app installed, it takes the latest release.

Options:
  --version <tag>   release to install; overrides the desktop match
  --dir <path>      install directory (default: ~/.local/bin)
  -h, --help        this text

Environment:
  MAST_VERSION, MAST_INSTALL_DIR — the same two settings.
USAGE
}

# ------------------------------------------------------------- utilities ----

need_cmd() {
    command -v "$1" >/dev/null 2>&1 || err "\`$1\` is required but was not found on PATH."
}

# curl or wget: one of them fetched this script, so one of them is here.
choose_downloader() {
    if command -v curl >/dev/null 2>&1; then
        DOWNLOADER=curl
    elif command -v wget >/dev/null 2>&1; then
        DOWNLOADER=wget
    else
        err "need either curl or wget to download the release."
    fi
}

# Fetch $1 to the file $2. Returns non-zero on any HTTP error, which callers
# rely on to tell "no such asset" from "the network is on fire".
#
# The downloader's own complaint goes to a file rather than the terminal. A
# missing asset is an EXPECTED failure here — it is how the desktop-match
# fallback discovers there is no archive for a tag — and printing `curl: (22)
# ... 404` before the explanation reads like something broke. `last_fetch_error`
# hands the text back for the cases that really are fatal.
fetch_to() {
    _err_file="${WORK:-${TMPDIR:-/tmp}}/fetch.err"
    case "$DOWNLOADER" in
        curl) curl --proto '=https' --tlsv1.2 -fsSL --retry 3 -o "$2" -- "$1" 2>"$_err_file" ;;
        wget) wget --https-only --tries=3 -q -O "$2" -- "$1" 2>"$_err_file" ;;
    esac
}

# The last downloader error, indented for inclusion in an err() message, or
# empty when it had nothing to say.
last_fetch_error() {
    _err_file="${WORK:-${TMPDIR:-/tmp}}/fetch.err"
    [ -s "$_err_file" ] || return 0
    printf '\n      %s' "$(head -n 2 "$_err_file")"
}

cleanup() {
    [ -n "${WORK:-}" ] && [ -d "${WORK:-}" ] && rm -rf "$WORK"
    # Staging files live beside their target (see install_binaries); a
    # cancelled run must not leave dotfiles behind in the user's bin dir.
    [ -n "${INSTALL_DIR:-}" ] && rm -f \
        "$INSTALL_DIR/.mast.install.$$" "$INSTALL_DIR/.mast-daemon.install.$$"
    return 0
}

# The socket's compatibility unit, mirroring mast_contract::wire_compat_key:
# major.minor, because patch releases never move a wire shape and a minor bump
# is where the DTOs are allowed to grow.
minor_of() { printf '%s' "$1" | cut -d. -f1,2; }
same_minor() { [ "$(minor_of "$1")" = "$(minor_of "$2")" ]; }

# ------------------------------------------------------------------ args ----

parse_args() {
    VERSION_ARG="${MAST_VERSION:-}"
    DIR_ARG="${MAST_INSTALL_DIR:-}"
    while [ $# -gt 0 ]; do
        case "$1" in
            --version) [ $# -ge 2 ] || err "--version needs a tag, e.g. --version v0.5.0"
                       VERSION_ARG="$2"; shift 2 ;;
            --version=*) VERSION_ARG="${1#--version=}"; shift ;;
            --dir)     [ $# -ge 2 ] || err "--dir needs a path."
                       DIR_ARG="$2"; shift 2 ;;
            --dir=*)   DIR_ARG="${1#--dir=}"; shift ;;
            -h|--help) usage; exit 0 ;;
            *) err "unknown option \`$1\` — run with --help for the list." ;;
        esac
    done
}

# -------------------------------------------------------------- platform ----

# Maps this machine onto one of the CLI tarballs the release workflow
# uploads. Anything without a prebuilt binary fails here with the specific
# reason and the specific way out, rather than 404-ing on a download later.
detect_platform() {
    kernel="$(uname -s)"
    machine="$(uname -m)"

    case "$machine" in
        x86_64|amd64)         arch=x86_64 ;;
        arm64|aarch64)        arch=aarch64 ;;
        *)                    arch="$machine" ;;
    esac

    case "$kernel" in
        Linux)  os=linux ;;
        Darwin) os=macos ;;
        MINGW*|MSYS*|CYGWIN*|Windows_NT)
            err "this installer is for Linux and macOS. On Windows, take
      Mast_<version>_x64-setup.exe (desktop) or the mast-<version>-windows-x86_64.zip
      CLI archive from $RELEASES/latest." ;;
        *)      err "unsupported operating system \`$kernel\`." ;;
    esac

    # Under Rosetta, `uname -m` reports x86_64 on an Apple Silicon machine.
    # sysctl knows better, and the native build is the one worth downloading.
    if [ "$os" = macos ] && [ "$arch" = x86_64 ] &&
       [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || echo 0)" = "1" ]; then
        arch=aarch64
    fi

    PLATFORM="${os}-${arch}"
    case "$PLATFORM" in
        linux-x86_64|linux-aarch64|macos-x86_64|macos-aarch64) ;;
        *)
            err "no prebuilt CLI for \`$PLATFORM\`. Releases cover linux-x86_64,
      linux-aarch64, macos-aarch64 and macos-x86_64. Build from source with
      \`cargo build --release -p mast-cli -p mast-daemon\`, or open an issue at
      https://github.com/${REPO}/issues if this platform should ship." ;;
    esac
}

# --------------------------------------------------------- desktop probe ----

# What version of the Mast desktop app, if any, is already installed here.
# Empty when there is none — every probe is a query against a package database
# or a plist, never an exec of the app itself: running a Tauri binary to ask
# its version would put a GUI window on the user's screen mid-install.
detect_desktop_version() {
    DESKTOP_VERSION=""
    DESKTOP_SOURCE=""

    if [ "$os" = macos ]; then
        for app in "/Applications/Mast.app" "$HOME/Applications/Mast.app"; do
            plist="$app/Contents/Info.plist"
            [ -f "$plist" ] || continue
            found="$(plutil -extract CFBundleShortVersionString raw -o - "$plist" 2>/dev/null \
                || defaults read "$app/Contents/Info" CFBundleShortVersionString 2>/dev/null \
                || true)"
            if [ -n "$found" ]; then
                DESKTOP_VERSION="$found"
                DESKTOP_SOURCE="$app"
                return 0
            fi
        done
        return 0
    fi

    # Debian/Ubuntu: the bundle installs as package `mast` (the binary inside
    # is `mast-desktop`, so it never collides with the CLI installed here).
    if command -v dpkg-query >/dev/null 2>&1; then
        found="$(dpkg-query -W -f='${Version}' mast 2>/dev/null || true)"
        if [ -n "$found" ]; then
            # Strip any Debian revision (`0.4.0-1`) — upstream version only.
            DESKTOP_VERSION="${found%%-*}"
            DESKTOP_SOURCE="the mast deb package"
            return 0
        fi
    fi

    # Fedora/RHEL: the rpm keeps the capitalised product name.
    if command -v rpm >/dev/null 2>&1; then
        found="$(rpm -q --qf '%{VERSION}' Mast 2>/dev/null || true)"
        case "$found" in
            ""|*"not installed"*) found="" ;;
        esac
        if [ -n "$found" ]; then
            DESKTOP_VERSION="$found"
            DESKTOP_SOURCE="the Mast rpm package"
            return 0
        fi
    fi

    # AppImages install themselves nowhere, so the only handle on one is its
    # filename in the handful of places people actually keep them.
    for dir in "$HOME/Applications" "$HOME/.local/bin" "$HOME/bin" "$HOME/Downloads"; do
        [ -d "$dir" ] || continue
        for candidate in "$dir"/Mast_*.AppImage; do
            [ -f "$candidate" ] || continue
            base="${candidate##*/}"          # Mast_0.4.0_amd64.AppImage
            found="${base#Mast_}"
            found="${found%%_*}"
            case "$found" in
                [0-9]*)
                    DESKTOP_VERSION="$found"
                    DESKTOP_SOURCE="$candidate"
                    return 0 ;;
            esac
        done
    done

    return 0
}

# --------------------------------------------------------------- version ----

# `/releases/latest` 302s to `/releases/tag/vX.Y.Z`. Reading that redirect
# costs one request and — unlike the JSON API, capped at 60 anonymous calls an
# hour per IP — will not strand a whole office behind one NAT gateway.
latest_tag() {
    case "$DOWNLOADER" in
        curl) resolved="$(curl --proto '=https' --tlsv1.2 -fsSLI \
                -o /dev/null -w '%{url_effective}' -- "$RELEASES/latest" || true)" ;;
        wget) resolved="$(wget --https-only -q -S --max-redirect=0 --spider \
                -- "$RELEASES/latest" 2>&1 \
                | sed -n 's#^[[:space:]]*Location:[[:space:]]*##p' | head -n 1 || true)" ;;
    esac
    printf '%s' "${resolved##*/}"
}

normalise_tag() {
    case "$TAG" in
        v[0-9]*) ;;
        [0-9]*)  TAG="v$TAG" ;;
        *)       err "could not work out which release to install (got \`${TAG:-nothing}\`).
      Check $RELEASES and retry with --version v<x.y.z>." ;;
    esac
    VERSION="${TAG#v}"
}

# Which release to install. An explicit pin wins; otherwise an installed
# desktop app decides, because the CLI and the app share a socket and a
# mismatched pair now refuses to run rather than failing obscurely later.
# Only with no app installed does "latest" mean latest.
resolve_version() {
    MATCHED_DESKTOP=0

    if [ -n "$VERSION_ARG" ]; then
        TAG="$VERSION_ARG"
        normalise_tag
        if [ -n "$DESKTOP_VERSION" ] && ! same_minor "$VERSION" "$DESKTOP_VERSION"; then
            warn "installing CLI $VERSION alongside desktop $DESKTOP_VERSION.
       Those two cannot share the daemon socket — \`mast\` will refuse to talk to
       the running app and say so. Drop --version to match the app instead."
        fi
        return
    fi

    if [ -n "$DESKTOP_VERSION" ]; then
        TAG="v${DESKTOP_VERSION}"
        MATCHED_DESKTOP=1
        normalise_tag
        step "Matching the Mast desktop app already installed ($DESKTOP_VERSION)"
        say "    ${C_DIM}from $DESKTOP_SOURCE — pass --version to override$C_RESET"
        return
    fi

    TAG="$(latest_tag)"
    normalise_tag
}

# -------------------------------------------------------------- download ----

download_and_verify() {
    ARCHIVE="mast-${VERSION}-${PLATFORM}.tar.gz"
    url="$RELEASES/download/$TAG/$ARCHIVE"

    step "Downloading Mast $VERSION for $PLATFORM"
    say "    $C_DIM$url$C_RESET"
    if ! fetch_to "$url" "$WORK/$ARCHIVE"; then
        # A desktop-matched tag can be one no release carries a CLI for — a
        # locally built app, or a release predating the CLI archives. Falling
        # back beats stopping, but it is a change of plan and gets said out
        # loud, mismatch warning and all.
        if [ "$MATCHED_DESKTOP" = 1 ]; then
            warn "no $PLATFORM CLI archive published for $TAG (matched from the desktop app)."
            TAG="$(latest_tag)"
            normalise_tag
            MATCHED_DESKTOP=0
            say "    ${C_DIM}falling back to the latest release, $VERSION$C_RESET"
            if ! same_minor "$VERSION" "$DESKTOP_VERSION"; then
                warn "this leaves CLI $VERSION next to desktop $DESKTOP_VERSION — they will
       not share a socket. Update the app from $RELEASES/latest."
            fi
            ARCHIVE="mast-${VERSION}-${PLATFORM}.tar.gz"
            url="$RELEASES/download/$TAG/$ARCHIVE"
            fetch_to "$url" "$WORK/$ARCHIVE" || err "download failed: $url$(last_fetch_error)"
        else
            hint=""
        if [ "$PLATFORM" = linux-aarch64 ]; then
            hint="
      ARM Linux builds start at v0.5.0; older releases have no $PLATFORM archive."
        fi
        err "download failed: $url$(last_fetch_error)
      If $TAG is real, it may not carry a $PLATFORM CLI archive — check
      $RELEASES/tag/$TAG.$hint"
        fi
    fi

    verify_checksum "$TAG" "$ARCHIVE"

    # A GitHub error page is a perfectly valid file; only tar can tell us the
    # bytes are the archive they claim to be.
    tar -tzf "$WORK/$ARCHIVE" >/dev/null 2>&1 \
        || err "could not read $ARCHIVE as a gzipped tarball.
      Either the download is corrupt (try again), or this \`tar\` cannot find a
      \`gzip\` to decompress with. If it keeps happening, report it at
      https://github.com/${REPO}/issues."
    tar -xzf "$WORK/$ARCHIVE" -C "$WORK"
    for bin in mast mast-daemon; do
        [ -f "$WORK/$bin" ] || err "\`$bin\` missing from $ARCHIVE."
    done
}

# Releases do not all carry a SHA256SUMS asset, so this verifies when one is
# published and says plainly when there is nothing to verify against. It never
# passes silently: a checksums file that exists and disagrees is fatal.
verify_checksum() {
    tag="$1"; archive="$2"
    sums_url="$RELEASES/download/$tag/SHA256SUMS"

    if ! fetch_to "$sums_url" "$WORK/SHA256SUMS" 2>/dev/null; then
        say "    ${C_DIM}no SHA256SUMS published for $tag — skipping checksum verification$C_RESET"
        return 0
    fi

    # Exact string match on the filename rather than a regex: the archive name
    # is full of dots, and a regex would let `mast-0.4.0-…` be satisfied by a
    # line for `mast-0X4Y0-…`. awk compares the second field literally, with
    # the `*` that sha256sum's binary mode prepends stripped off.
    expected="$(awk -v want="$archive" '
        { name = $2; sub(/^\*/, "", name); if (name == want) { print $1; exit } }
    ' "$WORK/SHA256SUMS" || true)"
    if [ -z "$expected" ]; then
        say "    ${C_DIM}SHA256SUMS has no entry for $archive — skipping verification$C_RESET"
        return 0
    fi

    if command -v sha256sum >/dev/null 2>&1; then
        actual="$(sha256sum "$WORK/$archive" | cut -d' ' -f1)"
    elif command -v shasum >/dev/null 2>&1; then
        actual="$(shasum -a 256 "$WORK/$archive" | cut -d' ' -f1)"
    else
        say "    ${C_DIM}no sha256sum/shasum available — skipping verification$C_RESET"
        return 0
    fi

    [ "$actual" = "$expected" ] || err "checksum mismatch for $archive.
      expected $expected
      got      $actual
      Refusing to install. Report this at https://github.com/${REPO}/issues."
    say "    ${C_DIM}sha256 verified$C_RESET"
}

# --------------------------------------------------------------- install ----

resolve_install_dir() {
    if [ -n "$DIR_ARG" ]; then
        INSTALL_DIR="$DIR_ARG"
    else
        INSTALL_DIR="${HOME:?HOME is not set — pass --dir to choose an install directory}/.local/bin"
    fi
    mkdir -p "$INSTALL_DIR" 2>/dev/null \
        || err "cannot create $INSTALL_DIR — pass --dir to install somewhere writable."
    [ -w "$INSTALL_DIR" ] \
        || err "$INSTALL_DIR is not writable. Pass --dir to pick a directory you own,
      or re-run with elevated privileges if you meant a system path."
}

install_binaries() {
    step "Installing to $INSTALL_DIR"
    for bin in mast mast-daemon; do
        # Stage inside the target directory so the final move is a same-
        # filesystem rename: atomic, and it swaps the directory entry rather
        # than writing into an inode that a running mast still has open.
        staged="$INSTALL_DIR/.$bin.install.$$"
        cp "$WORK/$bin" "$staged"
        chmod 0755 "$staged"
        # curl never sets com.apple.quarantine, but a proxy or a mirrored
        # tarball might; clearing it costs nothing and saves a Gatekeeper stop.
        [ "$os" = macos ] && xattr -d com.apple.quarantine "$staged" 2>/dev/null || true
        mv -f "$staged" "$INSTALL_DIR/$bin"
        say "    $C_GREEN✓$C_RESET $INSTALL_DIR/$bin"
    done
}

# ------------------------------------------------------------ next steps ----

report() {
    say ""
    say "${C_GREEN}${C_BOLD}Mast $VERSION installed.$C_RESET"
    if [ "$MATCHED_DESKTOP" = 1 ]; then
        say "${C_DIM}Matched to the desktop app, so both ends share one engine.$C_RESET"
    fi

    case ":${PATH}:" in
        *":$INSTALL_DIR:"*)
            say ""
            say "Run ${C_BOLD}mast status${C_RESET} to see your Sail projects, or ${C_BOLD}mast --help${C_RESET} for the rest."
            ;;
        *)
            # A binary nobody can invoke is not installed, so this is the one
            # piece of follow-up worth spelling out per shell.
            say ""
            warn "$INSTALL_DIR is not on your PATH. Add it:"
            shell_name="${SHELL:-sh}"
            case "${shell_name##*/}" in
                zsh)  say "    echo 'export PATH=\"$INSTALL_DIR:\$PATH\"' >> ~/.zshrc && exec zsh" ;;
                fish) say "    fish_add_path $INSTALL_DIR" ;;
                *)    say "    echo 'export PATH=\"$INSTALL_DIR:\$PATH\"' >> ~/.bashrc && exec bash" ;;
            esac
            say ""
            say "Then run ${C_BOLD}mast status${C_RESET}."
            ;;
    esac

    # Mast drives Docker Compose; without Docker the CLI installs fine and then
    # cannot do anything, which is worth saying now rather than at first use.
    if ! command -v docker >/dev/null 2>&1; then
        say ""
        warn "Docker was not found on PATH. Mast needs Docker Engine and Compose v2;
       install those before running \`mast status\`."
    fi

    if [ -z "$DESKTOP_VERSION" ]; then
        say ""
        say "${C_DIM}The desktop app is a separate download: $RELEASES/latest$C_RESET"
    fi
}

# ------------------------------------------------------------------ main ----

main() {
    parse_args "$@"
    need_cmd tar
    choose_downloader
    detect_platform
    detect_desktop_version
    resolve_version
    resolve_install_dir

    WORK="$(mktemp -d "${TMPDIR:-/tmp}/mast-install.XXXXXX")"
    trap cleanup EXIT INT TERM

    # Worth naming what is being replaced, so an upgrade that goes sideways has
    # a version number to roll back to.
    if [ -x "$INSTALL_DIR/mast" ]; then
        previous="$("$INSTALL_DIR/mast" --version 2>/dev/null | head -n 1 || true)"
        [ -n "$previous" ] && step "Replacing $previous in $INSTALL_DIR"
    fi

    download_and_verify
    install_binaries
    report
}

main "$@"
