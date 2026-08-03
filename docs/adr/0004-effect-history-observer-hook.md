# ADR-0004: effect history via a process-wide command observer

- **Status:** accepted
- **Date:** 2026-08-03
- **Milestone:** M9
- **Applies to:** `mast-docker::observer`, `mast-engine::history`, `MastClient::history_recent` / `subscribe_history`

## Question

ADR-0001 makes terminal commands the mechanism: Mast runs `vendor/bin/sail up -d` or the exact resolved `docker compose` invocation, not an API call dressed up as one. That is an architectural promise the user cannot currently see. When a start fails, the failure surfaces as a message; the argv behind it does not, so the developer cannot reproduce it in their own terminal.

How should Mast record what it actually did — every subprocess and every config write — so that it is visible, copyable, and honest, without the recording itself becoming a liability?

Three sub-questions drove the design: **where** commands get captured, **how** history reaches clients, and **what** must never appear in it.

## Decision

### 1. Capture at the spawn primitives, not at call sites

`mast-docker::command` has exactly three ways to start a process (`run_command`, `run_streaming`, `spawn_detached`). All three notify an observer registered through `mast_docker::register_command_observer`; `mast-engine` implements it.

The alternative — passing a recorder into each call site — was rejected because coverage would then depend on every future author remembering to record. There are ~20 spawn sites today across `mast-engine`, `mast-compose`, `mast-diagnostics`, and `mast-docker` itself, and the ones most worth showing (invocation resolution, endpoint probing) live in crates that never see the engine. With the hook, a shell-out added anywhere in the workspace appears in history for free, and the transparency claim does not decay.

Cost: a process-global registry, which is a real smell. It is mitigated by holding observers **weakly** and **additively**. A second engine in the same process — which the test suite creates routinely — registers alongside the first instead of displacing it, so no engine is ever silently switched off, and a dropped engine stops being notified without anyone deregistering it.

### 2. Attribution by task-local context, defaulting to "background"

A command is worth showing only if the user can tell what caused it. `Engine::dispatch` derives a label from the action's own wire tag (`startService` → "Start service") plus the project name, stores it against the operation id, and `spawn_operation` scopes it as a task-local around the work. Commands spawned underneath inherit it.

Deriving the label from the serde tag rather than a match over `Action` is deliberate: a new action gets a sensible label with no arm to forget. Where a hand-written label is clearly better — lifecycle verbs, which read "Start acme (redis)" — the call site supplies one.

Anything running outside a scoped context is Mast's own upkeep: reconciliation, readiness probes, container inspection, `compose config` resolution. It is recorded as `Background` and **hidden by default in clients**. This is not cosmetic. A ten-project workspace produces a command roughly every second at rest; shown by default it would bury the handful of entries the user caused.

Task-locals do not cross `tokio::spawn`, which is a feature here: a detached upkeep task cannot accidentally inherit a user action's label.

### 3. History gets its own channel — it must not ride the patch stream

Patches carry contiguous `seq` numbers and a bounded replay window (`replay_capacity`, 256 by default); a subscriber that falls outside the window is told to resynchronize. Emitting two patch events per command would push tens of events per second through that window at rest, shrinking the replay horizon to seconds and turning ordinary reconnects into full snapshot resyncs. History would degrade the very state stream it sits beside.

So history follows the precedent container logs already set (plan §3): `Engine::history_recent` for the backlog, `Engine::subscribe_history` for the live feed, both on the `MastClient` trait and both exposed by the daemon. Entries are delivered whole, once on creation and again on completion; consumers upsert by `id`. A lagging subscriber **skips** entries rather than resyncing — history is a transparency aid, not an audit log, and the diagnostics history in `mast-diagnostics` remains the durable record.

The ring is bounded (300 entries, 25 output lines each clipped to 240 chars) and lives in memory only. Nothing here is persisted.

### 4. Config writes are effects too

`.env` edits and compose transactions are recorded alongside subprocesses, as a `FileWrite` variant carrying the path and the same human summary the edit preview shows — including the writes the transaction **refused**, which are exactly the ones a user needs explained. A history listing only subprocesses would understate what Mast changed on the machine, and would have made "Commands" the honest name for the tab. It is called History because it is not only commands.

### 5. Redaction is a precondition, not a nicety

History is rendered with a copy button. A secret that reaches a record reaches a clipboard, and from there anywhere. Three consequences:

- Every recorded field — argv, env overlay, output tail, derived label, and error text — passes through a redactor before it is stored, not before it is displayed.
- The redactor is the **union** of every project's `.env` redactor, maintained on each reconcile. Per-project redaction is insufficient because a background command belongs to no project yet can still echo one's password.
- Env values whose keys match the secret patterns are masked by key regardless of the redactor, and the copy affordance emits the mask, not the value. A copied line that silently re-inflated a secret would be worse than no copy button.

Two refinements followed from the union, because widening the value set widened the false positives with it:

**Tokens are redacted differently from prose.** `Redactor::redact` replaces a secret anywhere it appears, which is right for free-form output. Argv elements and env values are _tokens_, and a token either is a secret, carries one after `=`, or — when the secret is long enough to be unambiguous — contains one. Blind substring replacement turned `/app/vendor/bin/sail` into a mangled path, and a history entry whose copy button yields a command that will not run defeats the feature. `Redactor::redact_token` is the token rule; free-form output still uses `redact`.

**Documented defaults are not secrets.** Sail ships `AWS_ACCESS_KEY_ID=sail`, `DB_PASSWORD=password`, `REDIS_PASSWORD=null`. Those keys match the secret markers, so the union redactor learned `sail` from one project and redacted it out of every other project's runner path. These values are printed in Laravel's public documentation and committed to `compose.yaml`; redacting them protects nothing measurable and corrupts output everywhere. `PLACEHOLDER_VALUES` excludes them. The trade is explicit: a developer who genuinely sets their password to the string `password` is not protected by Mast's redactor — but neither are they protected by the compose file that already contains it.

This extends `redact.rs`'s existing rule rather than reinterpreting it. Container log streams remain unredacted (they render the developer's own application output transiently and are never persisted); history is persisted in memory and copyable, so it is redacted.

## Consequences

- Coverage is automatic and stays automatic; there is no per-call-site discipline to maintain.
- Clients get history without touching the patch protocol, and `mast history` in the CLI came at no additional engine cost — the daemon serves it over the same socket.
- A failed operation can link directly to the command behind it (`HistoryEntry::operation`), which is the difference between a transparency feature and a diagnosis path.
- The process-global registry is the accepted cost. It is safe for multiple engines and for dropped engines, but it does mean history is a property of the process, not of an `Engine` handle passed around.
- `mast-docker` now knows the concept of an observer. It does not know what an observer does with the data, and gains no dependency on `mast-engine` or `mast-contract`.

## Not decided here

- **Persistence.** The ring dies with the process. If history should survive a restart, it needs a store, a retention policy, and a fresh look at redaction — an in-memory secret that leaks is bounded by uptime; a written one is not.
- **Re-running an entry from the UI.** `up -d` is idempotent; `down -v` destroys volumes. Any re-run affordance needs a safe-subset rule or a confirmation, and neither is designed yet.
- **Sampling under load.** The ring bounds memory but every command still allocates an entry and a broadcast send. If a future workload makes that measurable, background entries are the ones to sample.
