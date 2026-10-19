# ADR-0014: Local HTTPS for the dev servers a project runs on this machine

- **Status:** accepted
- **Date:** 2026-10-19
- **Applies to:** `mast-engine` (`command_https`, `proxy`, the output hook in `ops`), `mast-contract` (`Action::SetCommandDomain`, `CommandDomain`, `CommandPort`, `ProjectSummary::command_domains` / `command_ports`), `mast-project` (`CommandDomainRecord`), `clients/desktop-vue` (`LocalHttpsDialog.vue`, `lib/localDomains.ts`, the command chips), `mast-cli` (MCP project details)

## Question

A Laravel API and its frontend often live in separate repositories. Mast
runs the frontend's dev server as one of the API project's commands
(`vp dev` with `cwd: ../suite`), and the API can have a trusted
`https://….test` address through the shared Caddy proxy — but the
frontend could not. Moving only the API to HTTPS broke the field case
outright: the SPA on `http://suite.thinksolar.local:5173` logs in with
Sanctum's cookie session, and once the API answered on
`api-thinksolar.test`, its session cookie (scoped `.thinksolar.local`)
was rejected, and the two names were different sites anyway. How does a
dev server that is not a container get a domain, and how does Mast keep
the two names usable together?

## Decision

**A domain can be claimed for a host command.** `SetCommandDomain`
claims a `.test`/`.localhost` name for one of the project's commands
that runs on this machine. `sail …` commands are refused: they run in
the app container, whose address is the app's. Claims are stored beside
the commands (`ProjectRecord::command_domains`), keyed by command name,
not inside the command definition: a claim needs this machine's hosts
file and trust, while the command may come from a committed `mast.yml`.
Domains are unique across apps and commands of every project.

A claim whose command no longer exists stays until it is released. The
proxy answers for it with an explanation, and the dialog lists it with a
Release button, so a deleted command never hands its domain to someone
else silently. Claiming that domain for another command of the same
project moves the claim. That is also how a rename keeps its address:
the desktop's edit form re-claims the domain under the new name.

**The port comes from what the command prints.** Dev servers announce
their address: Vite, Nuxt, Astro and Storybook print `Local:`,
webpack-dev-server `Loopback:`, artisan `Server running on`, Django
`development server at`. The engine reads each output line of a host
command's run (`announced_port`): the first `http(s)://host:port` in a
line that announces an address. ANSI escapes are stripped first, since
Vite prints the port in bold, and Vite's `Network:` line is skipped. The
port appears on the summary while the run lasts (`command_ports`), and an
unpinned claim follows it: persisted, with the proxy converged right
away. A Vite that moves to 5174 because 5173 is busy stays at the same
address. A user can pin a port instead.

Reading output, not the process table, is deliberate. The `/proc` walk
behind ADR-0012 exists only on Linux, and a dev server's listening
socket would need `lsof` on macOS and the TCP table API on Windows. What
the server prints is the same everywhere, needs no configuration, and is
exactly what a person reads to find the address.

Until a claim has a port, its site answers 503 with a sentence that
starts with `Mast: ` — what the command has not done yet and how to fix
it. A browser gets an explanation instead of a bare connection error,
and the domain keeps its certificate.

**On a native Linux engine the proxy joins the host network.** Vite and
most dev servers listen on localhost only. A bridged container reaches
the machine through Docker's gateway (`172.17.0.1`), where a
loopback-only server refuses the connection. On the host network the
proxy reaches it directly. `host.docker.internal` is mapped to
`127.0.0.1` and `::1` there, so the Caddyfile is the same in both modes.
Caddy tries each address, so a server on `::1` only is reached as surely
as one on `127.0.0.1` only. That was verified against a real Vite and an
IPv6-only listener.

The mode is read from `docker info`. Host networking is used only when
Mast runs on Linux and the daemon is native: not Docker Desktop,
OrbStack, colima or Rancher (`runtime_flavor`), and not rootless.
Rootless host networking is rootlesskit's namespace, not the machine's.
Everything else, and any unreadable answer, keeps the bridge. An
existing bridged proxy is recreated on the host network the next time a
domain is enabled, and only then. The reconcile-time convergence never
starts or removes the container (unchanged from before).

On the host network Caddy's admin API would listen on the machine's own
`localhost:2019`. Any local process could then reconfigure a proxy whose
certificate authority every browser on the machine trusts, and mint
certificates for any name. The admin API moves to a socket inside the
proxy's data volume (`admin unix//data/caddy-admin.sock`) in both modes,
and reloads go through it. A proxy started before the socket existed is
reloaded the old way, which moves it onto the socket.

**After enabling, one request goes through the proxy.** The operation
requests `https://domain/` from `127.0.0.1:443` with the domain as SNI
and Host. It skips the hosts file and trust; those are the Fixes. It
reports what a browser would hit:

- **Nothing behind the domain (502):** if the port is listening on
  localhost and the proxy is bridged, the server has to listen on every
  interface. Otherwise the command has to be started.
- **A host-name refusal:** Vite's "Blocked request", answered with
  `server.allowedHosts`.
- **The proxy's own explanation**, for a claim with no port yet.
- **An answer.** "Everything is in place" now requires one.

The app's own domain gets the same probe.

**The `.env` is read, not written.** When a Laravel project with Sanctum
has both an app domain and a command domain, the operation names what
keeps the session from crossing: two different sites, a `SESSION_DOMAIN`
that does not cover both names, or a `SANCTUM_STATEFUL_DOMAINS` that does
not list the frontend. Each finding says what to change. A site is the
last two labels. Neither `.test` nor `.localhost` is on the public suffix
list, so browsers treat the TLD itself as one.

**The dialog shows every address together.** The HTTPS button opens one
dialog. It has the app's row, a row per host command (each with its own
operation slot), container commands named as reached through the app,
and orphaned claims. A command chip with a domain shows a lock and offers
**Open https://…**, and every host command's menu offers **HTTPS…**,
which opens the dialog on that row. Suggested names keep siblings
together:

- A dev-host project name keeps its labels (`api.thinksolar.local` →
  `api.thinksolar.test`).
- A command's suggestion is a sibling of the app's
  (`suite.thinksolar.test`).
- While a draft is on another site than the app, the row offers both
  fixes: move the command under the app's site, or rename the app to a
  sibling of the command.

## Consequences

The new action, two summary lists and one record field are wire growth,
all `#[serde(default)]`. An older client neither shows nor sends them,
and an older engine refuses the unknown action rather than misreading it
as `SetLocalDomain`, so this is a minor release.

`mast-engine` now links `reqwest` for the probe, built the way
`mast-registry` already builds it, which adds no crates.

Every output line of a host command passes through `announced_port`, a
few substring checks per line. A command that keeps printing the same
address does not churn the summary.

Tests run the whole chain on every CI platform. A real host command's
announcement reaches its claim through the engine, using `echo`, which
Git provides on the Windows runners as it provides `touch`. Network
choice, run arguments, Caddyfile rendering, probe classification, claim
planning and the `.env` findings are pure and unit-tested. No test
creates or reloads a proxy: a successful claim converges the machine's
one `mast-proxy` container, so tests seed claims in the store and check
what dispatch refuses through the API.

## Not decided here

A dev server that itself serves HTTPS (`@vitejs/plugin-basic-ssl`). The
announced `https://` URL could switch the upstream to TLS with
verification off. Until then, such a server needs its own TLS turned off
behind the proxy.

Writing the `.env` fixes. Mast already edits `.env` through the Env panel
with history. Offering `SESSION_DOMAIN` and `SANCTUM_STATEFUL_DOMAINS` as
previewed repairs is the natural next step. The frontend's own `.env`
(`VITE_API_URL`) belongs to another repository, and the engine cannot
know which key holds the API's address.
