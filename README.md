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

### Keep Sail. Lose the juggling.

**A Linux-first desktop control center for Laravel Sail.**

Manage all your Laravel Sail projects, containers, logs, workers, and development services from one place — without replacing Sail or Docker.

**Linux-first · macOS compatible · Open source**

[**Download Mast**](../../releases/latest) · [Features](#features) · [Build from source](#development)

<img src="docs/media/workspace-start.gif" alt="Starting the Acme workspace: billing-api starts first, Mast waits for it to become healthy, then storefront follows" width="900">

<sub>Starting a workspace in dependency order. Mast waits for each layer to become healthy before starting the next.</sub>

</div>

---

## Why Mast?

Laravel Sail works great from the terminal.

The friction starts when you're working across **several Laravel projects at once** — each with its own containers, logs, workers, Horizon, Reverb, databases, ports, and commands.

Mast gives your existing Sail projects a single control center.

- See every project and its current state at a glance.
- Start complete development workspaces in dependency order.
- Start, stop, and restart individual projects or services.
- View live logs without hunting through terminal tabs.
- Run Horizon, Reverb, queue workers, and project commands automatically.
- Diagnose and repair common local environment problems.
- Manage services and environment variables without leaving Mast.

### Mast doesn't replace Sail

Your existing Sail and Docker Compose workflow remains intact.

Docker remains the source of truth, and Mast stays in sync with changes made from the terminal. Your existing `compose.yaml`, `.env`, and Sail commands continue to work normally.

Close Mast and go straight back to the terminal whenever you want.

---

# Installation

## Platform support

| Platform       | Status                                      |
| -------------- | ------------------------------------------- |
| 🐧 **Linux**   | **Primary platform — tested and supported** |
| 🍎 **macOS**   | Compatible                                  |
| 🪟 **Windows** | Not yet tested                              |

Mast is cross-platform, but **Linux is the primary development and testing platform**.

## Requirements

- Docker Engine, reachable by your user
- Docker Compose v2 (`docker compose`)

## Linux

Download the latest Mast release from:

**[Download Mast](../../releases/latest)**

### Debian / Ubuntu

Download the `.deb` package and install it with:

```bash
sudo apt install ./Mast_<version>_<architecture>.deb
```

### AppImage

Download the AppImage from the latest release. No installation is required.

Make it executable:

```bash
chmod +x Mast_<version>_<architecture>.AppImage
```

Then run it:

```bash
./Mast_<version>_<architecture>.AppImage
```

After starting Mast, open **Settings**, add the directories containing your projects, and import the projects Mast discovers.

## macOS

Mast also runs on macOS, although prebuilt macOS releases are not currently provided.

Build Mast from source using the instructions under [Development](#development).

## Windows

Windows has not yet been tested.

Mast is built with cross-platform technologies, but Windows should be considered unsupported until it has been properly tested.

---

# Features

## One place for every Sail project

Mast automatically discovers Laravel Sail and Docker Compose projects from the directories you configure.

Every project shows its services, application processes, saved commands, and live container state.

<img src="docs/media/project.png" alt="Project pane showing service, process, and command chips for a running Sail project" width="900">

### Project & service controls

Start, stop, or restart an entire project or an individual service.

Mast reads the real Docker state rather than maintaining a separate idea of what should be running.

### Workspaces

Group related projects into workspaces and start them together.

Dependencies can be defined between workspace members. Mast starts each layer in order and waits for its dependencies to become healthy before continuing.

That means a workspace containing an API, frontend, and supporting applications can be brought up with one action instead of manually starting each repository.

<img src="docs/media/workspace-start.gif" alt="Starting the Acme workspace in dependency order" width="900">

---

## Development processes

Run the processes that belong to your Laravel application alongside its containers.

Mast can automatically start:

- Reverb
- Horizon
- Queue workers
- Saved project commands

These processes remain attached to the project they belong to rather than disappearing into another terminal window.

---

## System tray

You don't need to keep the Mast window open.

The system tray exposes the same project and workspace controls, so starting or stopping an environment is only a couple of clicks away.

<img src="docs/media/tray.png" alt="Tray menu showing workspaces and individual project controls" width="456">

---

## Diagnostics & repairs

Local Docker environments break.

Mast checks for common problems and presents each finding with a guided repair.

Every repair has a risk tier and a preview of what Mast intends to change before anything is written.

<img src="docs/media/diagnostics.png" alt="Diagnostics dialog listing errors with repair actions" width="900">

---

## Service catalog

Add common development services without manually rebuilding Compose configuration from scratch.

Mast matches services against your existing Compose file and can expose available registry versions where applicable.

<img src="docs/media/services.png" alt="Services catalog showing installed and available services with version selectors" width="900">

---

## Environment editor

Edit `.env` values directly from the project view.

Sensitive values remain masked while Mast edits the existing file in place.

<img src="docs/media/env.png" alt="Environment editor with APP_KEY and DB_PASSWORD masked" width="900">

---

## Create Laravel projects without local PHP

Mast can bootstrap new Laravel applications using containers.

You don't need a local PHP installation just to create the project that will ultimately run inside Docker anyway.

---

# CLI

Mast isn't limited to the desktop interface.

The `mast` CLI talks to the same engine as the desktop application and system tray.

```bash
mast status
mast start <project>
mast stop <project>
mast restart <project>
mast diagnose
mast history
```

Control an individual service with:

```bash
mast start <project> --service <name>
```

Projects can be matched by name or path fragment.

When the desktop application or daemon is running, the CLI connects to it over IPC.

Otherwise, the CLI starts its own embedded engine. If another Mast process already holds the ownership lock, that engine operates read-only.

---

# How Mast works

Mast sits **on top of your existing Laravel Sail and Docker Compose workflow**.

It doesn't create a proprietary environment that your projects need to be migrated into.

Three principles guide how Mast interacts with your development environment:

### Terminal commands remain the source of behaviour

Operations performed by Mast correspond to the commands you would otherwise run yourself.

### Docker inspection remains the source of truth

Mast asks Docker what's actually running rather than relying on cached application state.

Changes made outside Mast remain visible inside Mast.

### File writes are validated transactions

When Mast changes project configuration, writes are validated rather than treated as arbitrary text manipulation.

The goal is simple:

**Mast should make Sail easier to operate without making your projects dependent on Mast.**

---

# Architecture

Mast is built with **Rust**, **Tauri 2**, and **Vue 3**.

A single Rust engine owns application state, and every client communicates with it through the same `MastClient` trait.

```text
Desktop App ─┐
System Tray ─┼─ MastClient ── mast-engine ── Docker / Compose / Laravel
mast CLI ────┘   in-process, or over an IPC socket to the owning process
```

Whichever process holds the engine serves a Unix socket.

This allows the CLI to retain full control while the desktop application is running while still allowing either client to operate independently.

A headless `mast-daemon` binary serves the same socket without requiring the GUI.

Architecture decisions and their reasoning are documented in:

```text
docs/adr/
```

---

# Development

## Build dependencies

### Debian / Ubuntu

Install the required system packages:

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

Clone Mast:

```bash
git clone <repository>
cd mast
```

Install dependencies:

```bash
vp install
```

Build the desktop application:

```bash
cargo tauri build
```

Configured Linux bundle targets are:

- `.deb`
- `.rpm`
- AppImage

Bundles are written to:

```text
target/release/bundle/
```

Build the optional CLI separately:

```bash
cargo build --release -p mast-cli
```

The resulting binary is:

```text
target/release/mast
```

---

## Build a Debian package

Build only the Debian bundle:

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

The desktop bundle installs the GUI only.

The `mast` CLI can be distributed separately from:

```text
target/release/mast
```

or bundled using `externalBin` in `tauri.conf.json`.

### AppIndicator build errors

If the build fails with:

```text
Can't detect any appindicator library
```

install the development package:

```bash
sudo apt install libayatana-appindicator3-dev
```

The runtime library alone is not sufficient because Tauri locates AppIndicator using `pkg-config`.

### Cross-architecture builds

Cross-architecture builds are not currently configured.

Build `arm64` packages on an `arm64` host or inside a matching environment with the same build dependencies installed.

---

# Running Mast for development

Install dependencies after cloning or pulling changes:

```bash
vp install
```

Start the desktop application from the repository root:

```bash
cargo tauri dev
```

Tauri automatically starts the frontend development server (`vp dev`) on port `1420` and launches the desktop application.

## Running the frontend manually

If you need to run the frontend separately:

```bash
cd clients/desktop-vue
vp dev
```

Do **not** run `vp dev` from the repository root.

The root `vite.config.ts` configures the Vite+ workspace, while the application configuration lives in:

```text
clients/desktop-vue/
```

If Tauri remains on:

```text
Waiting for your frontend dev server
```

clear any stale Vite+ process holding port `1420`:

```bash
pkill -f vite-plus-core
```

Then run:

```bash
cargo tauri dev
```

---

# Running tests

Run the Rust workspace tests:

```bash
cargo test --workspace
```

Run the frontend tests:

```bash
cd clients/desktop-vue
vp test run
```

Tests that require Docker skip cleanly when Docker is unavailable.

Fixtures under `fixtures/` contain deliberate formatting traps used to prove byte-faithful editing.

**Do not reformat them.**

---

# Contributing

Issues, bug reports, and contributions are welcome.

Mast is particularly interested in feedback from developers using **Laravel Sail on Linux** and developers working across multiple local Laravel projects.

If Mast doesn't understand something about your existing Sail or Compose setup, open an issue with the relevant configuration and expected behaviour.
