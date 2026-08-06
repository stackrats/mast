<div align="center">

<div style="width: 100%; overflow-x: auto; background-color: #0d1117; padding: 15px; border-radius: 6px;">
<pre style="white-space: pre !important; word-wrap: normal !important; font-family: monospace; font-size: 14px; line-height: 1.2; margin: 0; color: #c9d1d9;">
 ██╗██╗        ███╗   ███╗  █████╗  ███████╗ ████████╗
 ██║█████╗     ████╗ ████║ ██╔══██╗ ██╔════╝ ╚══██╔══╝
 ██║████████╗  ██╔████╔██║ ███████║ ███████╗    ██║   
 ██║█████╔═╝   ██║╚██╔╝██║ ██╔══██║ ╚════██║    ██║   
 ██║██╔═╝      ██║ ╚═╝ ██║ ██║  ██║ ███████║    ██║   
 ╚═╝╚═╝        ╚═╝     ╚═╝ ╚═╝  ╚═╝ ╚══════╝    ╚═╝   
</pre>
</div>

Control tower for local Laravel Sail development.

Discover, run, inspect, repair, and organise your Laravel Sail and Docker Compose projects from one place.

**Linux first.** macOS and Windows adapters coming soon.

<img src="docs/media/workspace-start.gif" alt="Starting the Acme workspace: billing-api starts first, Mast waits for it to become healthy, then storefront follows" width="900">

<sub>Starting a workspace. Members start in dependency order, and each layer waits for the one before it to report healthy.</sub>

</div>

## Features

- 🔍 Automatic Sail and Compose project discovery
- ▶️ Start, stop, and restart projects or individual services with live logs
- 📦 Workspace groups with dependency-aware startup
- 🎛️ Run Reverb, Horizon, queue workers, and saved commands on project start
- 🔑 Safe `.env` editing with masked secrets
- 🧩 Add common services from a built-in catalog
- 🏷️ Switch service versions from available registry tags
- 🔧 Diagnostics with guided repairs
- 🚀 Create Laravel projects without local PHP
- 💻 Desktop app, system tray, and `mast` CLI

---

## Why Mast?

Laravel Herd delivers a polished local development experience, but it's only available on macOS and Windows and doesn't use Laravel Sail's containerised workflow. Docker Desktop manages containers, but isn't designed around Sail.

Mast brings that experience to Laravel Sail. See what's running at a glance, launch complete workspaces in seconds, and get into your development workflow faster without giving up the containerised environment you already use.

Docker remains the source of truth, and Mast stays in sync with changes made from the terminal.

---

## A look around

Every project displays its services, the app processes running inside them, and your own saved commands.

<img src="docs/media/project.png" alt="Project pane showing service, process, and command chips for a running Sail project" width="900">

The tray carries the same controls, so a workspace or a single project is two clicks away without opening the window.

<img src="docs/media/tray.png" alt="Tray menu: workspaces, a Projects submenu listing every project with its status, and per-project Start, Stop, and Restart" width="456">

Diagnostics checks the things that break a local environment, and each finding carries a repair with a risk tier and a preview of exactly what it will change.

<img src="docs/media/diagnostics.png" alt="Diagnostics dialog listing two errors, each with a Repair button" width="900">

Common services are one click away, matched to your compose file.

<img src="docs/media/services.png" alt="Services catalog showing installed and available services with version selectors" width="900">

`.env` is editable in place.

<img src="docs/media/env.png" alt="Env panel with APP_KEY and DB_PASSWORD masked" width="900">

---

## Architecture

Mast is built with **Rust**, **Tauri 2**, and **Vue 3**.

A single Rust engine owns the application state, and every client talks to it through the same `MastClient` trait.

```text
Desktop App ─┐
System Tray ─┼─ MastClient ── mast-engine ── Docker / Compose / Laravel
mast CLI ────┘   in-process, or over an IPC socket to the owning process
```

Whichever process holds the engine serves a Unix socket, so the CLI keeps full control while the desktop app is running and falls back to its own embedded engine when it is not. A headless `mast-daemon` binary serves the same socket without a GUI.

### Design principles

- Terminal commands remain the source of behaviour.
- Docker inspection remains the source of truth.
- File writes are validated transactions.

For the reasoning behind these decisions, see the Architecture Decision Records in `docs/adr/`.

---

# Installation

## Requirements

- Linux
- Docker Engine (reachable by your user)
- Docker Compose v2 (`docker compose`)

## Build dependencies

Install the required packages on Debian or Ubuntu:

```bash
sudo apt update

sudo apt install \
  build-essential \
  curl \
  libwebkit2gtk-4.1-dev \
  libssl-dev \
  libgtk-3-dev \
  libayatana-appindicator3-dev \
  librsvg2-dev
```

Install Rust and Vite+:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
curl -fsSL https://vite.plus | bash
```

Open a new terminal, then install the Tauri CLI:

```bash
cargo install tauri-cli --version '^2.0.0' --locked
```

---

## Build from source

```bash
git clone <repository>
cd mast

vp install

cargo tauri build
```

The configured bundle targets are `deb`, `rpm`, and `appimage`. They are written to:

```text
target/release/bundle/
```

Build the optional CLI (`target/release/mast`):

```bash
cargo build --release -p mast-cli
```

---

## Build a Debian package

Build only the Debian package:

```bash
cargo tauri build --bundles deb
```

The package is written to:

```text
target/release/bundle/deb/Mast_<version>_<architecture>.deb
```

Install it with:

```bash
sudo apt install ./target/release/bundle/deb/Mast_<version>_<architecture>.deb
```

The desktop bundle installs the GUI only. Distribute the `mast` CLI separately from `target/release/mast`, or bundle it using `externalBin` in `tauri.conf.json`.

> **Note**
>
> If the build fails with:
>
> ```
> Can't detect any appindicator library
> ```
>
> install the development package:
>
> ```bash
> sudo apt install libayatana-appindicator3-dev
> ```
>
> The runtime library alone is not sufficient because Tauri locates AppIndicator using `pkg-config`.

Cross-architecture builds are not configured. Build `arm64` packages on an `arm64` host, or inside a matching container with the same build dependencies installed.

After installation, open **Settings**, add your project directories, and import the projects Mast discovers.

---

## CLI

```bash
mast status                     # projects, workspaces, and live container state
mast start <project>            # also stop and restart, each accepting --service <name>
mast diagnose                   # run the full diagnostic check set
mast history                    # what Mast has run and written recently
```

Projects are matched by name or path fragment. The CLI connects to the running desktop app or daemon when one is available; otherwise it starts its own engine, which is read-only if another instance already holds the ownership lock.

---

# Development

Install dependencies after cloning or pulling changes:

```bash
vp install
```

Start the desktop application from the repository root:

```bash
cargo tauri dev
```

Tauri automatically starts the frontend dev server (`vp dev`) on port `1420` and launches the desktop application.

If you need to run the frontend manually, start it from the desktop client directory:

```bash
cd clients/desktop-vue
vp dev
```

Do **not** run `vp dev` from the repository root. The root `vite.config.ts` configures the Vite+ workspace, while the application configuration lives in `clients/desktop-vue/`.

If Tauri remains on **"Waiting for your frontend dev server"**, clear any stale Vite+ process holding port `1420`:

```bash
pkill -f vite-plus-core
```

Then run:

```bash
cargo tauri dev
```

## Running tests

```bash
cargo test --workspace                    # engine, SDK, daemon, CLI
cd clients/desktop-vue && vp test run     # frontend
```

Tests that need Docker skip cleanly when it is unavailable. Fixtures under `fixtures/` contain deliberate formatting traps that prove byte-faithful editing — never reformat them.
