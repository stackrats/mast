# ADR-0007: mast.yml and command supervision — commands that travel and stay up

- **Status:** accepted
- **Date:** 2026-08-31
- **Applies to:** `mast-engine::manifest`, `mast-engine::supervise`, `ProjectCommand::{auto_restart, restart_when_changed, from_manifest}`, `Action::ExportProjectManifest`

## Question

Saved commands (M7.5) solved "run my dev stack from the app" and quietly
created two new problems.

First, the commands live in app data. A project's command setup — which dev
servers, in what order, with what readiness — is knowledge about the project,
but it dies with the machine: a teammate clones the repo, imports the project,
and starts from an empty command row, re-deriving what the first person
already wrote down. Every other piece of project knowledge Mast consumes
(compose files, `.env`, `composer.json`) rides the repo; the one Mast itself
introduced did not.

Second, the commands are fire-and-forget. A queue worker that dies stays
dead, and — worse, because nothing looks wrong — a queue worker that outlives
a code change keeps running the old code. Laravel's own deployment answer is
Supervisor with `stopWaitTimeout` and a restart on deploy; local dev under
Mast had neither, and the symptom ("my job changes don't apply") reads as a
Laravel bug, not a stale process.

## Decision

**A committed manifest.** `mast.yml` at the project root carries the shared
command list, read on every reconcile. The summary's command list becomes a
merge: manifest commands first, then app-data ones, with a saved command
shadowing a manifest command of the same name — a local override beats the
shared default, which keeps the file authoritative without making it a cage.
Manifest entries are marked `from_manifest` on the wire; the editor shows
them read-only (the file is where they are edited), and `SetProjectCommands`
drops them before persisting so they are never duplicated into app data.

The manifest is someone else's commit, so its failure mode must be a warning,
never a project that refuses to load: parse errors, unknown keys, wrong
types, and an unsatisfiable `after` graph each become a named project
warning and the parseable remainder still works.

`Action::ExportProjectManifest` is the migration path: it writes the current
saved commands to a new `mast.yml` and clears them from app data — after an
export a command lives in exactly one place. It refuses to overwrite an
existing file; Mast writes the first draft, people maintain it.

**Supervision under one operation.** `auto_restart` relaunches after any exit
the user did not ask for; `restart_when_changed` globs stop-and-relaunch the
command when matching files change. Both run inside the existing
`RunProjectCommand` operation: the chip stays green across restarts, Stop
cancels the whole arrangement (each child run gets a child token of the
operation's), and every child still lands in effect history individually.
Nothing about the client's operation model changed — a supervised command is
indistinguishable from a plain one until the first restart line appears.

Two guardrails keep the supervisor from fighting the user. Rapid exits
(five in a row, each under 30s of uptime) stop the loop and fail the
operation — five restarts of a broken command are five copies of the same
failure, and a clean-but-instant exit gets told it is a one-shot that wants
auto-restart off. And file watches land on the literal prefix of each glob
(`app/**` watches `app/`), never the project root, so `vendor/` and
`node_modules/` cost no inotify watches.

## Consequences

- The manifest schema is deliberately just `commands:` — the same fields the
  dialog edits, nothing more. Room to grow (a `name:`, service templates) is
  left as unknown-section warnings rather than speculative parsing.
- `restart_when_changed` restarts are debounced (400ms) and reset the
  crash-loop count: a change during backoff may be the fix and restarts
  immediately.
- Auto-start remains client-driven; the manifest only changes where the
  command list comes from. Engine-side triggers stay on the roadmap
  unchanged.
- `mast.yaml` is read as an alternate spelling; Mast always writes
  `mast.yml`, and both existing at once is a warning.
