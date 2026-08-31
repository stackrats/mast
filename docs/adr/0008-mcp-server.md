# ADR-0008: the MCP server — Mast as the environment authority for coding agents

- **Status:** accepted
- **Date:** 2026-08-31
- **Applies to:** `mast-cli::mcp` (`mast mcp`)

## Question

Coding agents working in a Sail project need the environment constantly: is
the stack up, what died, what's in the queue worker's output, which port did
the app land on. Today an agent shells `docker compose` blind, next to — and
sometimes fighting — the Mast that already knows all of it. Mast holds
per-container log captures, a parsed Laravel log, diagnostics with repairs,
merged command lists, and real observed state; none of it was reachable by
anything but the two human-facing clients.

The engine's surfaces already existed: a frozen contract, an NDJSON daemon
socket, an SDK trait both clients share. What was missing was a dialect
agents speak.

## Decision

`mast mcp` runs an MCP server over stdio — newline-delimited JSON-RPC 2.0,
hand-rolled on serde_json in the same spirit as the daemon's NDJSON framing,
with no protocol dependency to chase. It is a third client of the same
`MastClient` trait: it prefers the daemon socket (shared engine, full
mutation rights while the desktop runs) and falls back to an embedded
engine, with the same version-mismatch refusal as the CLI proper.

Twelve tools, deliberately read-heavy: `mast_status`, `mast_logs`,
`mast_laravel_log`, `mast_captures`, `mast_diagnose`, `mast_history`,
`mast_wait`, plus the lifecycle verbs (`mast_start` / `mast_stop` /
`mast_restart` / `mast_rebuild`) and `mast_run_command`. Registration is one line in
any MCP client's settings: `mast mcp` as an stdio server.

Two boundaries are load-bearing:

- **`mast_run_command` runs saved commands only.** The user-curated command
  list (app data + committed `mast.yml`, ADR-0007) is the consent boundary;
  an agent can run what the team wrote down, never an arbitrary string
  smuggled through Mast's process machinery. An unknown name answers with
  the list of what is available.
- **Long-running commands return at readiness, not exit.** A dev server
  never exits, and an MCP call that never returns wedges the agent.
  `mast_run_command` reports "still running" with output-so-far after its
  wait budget and leaves the command running — the same asymmetry the
  desktop's `after`/`ready_when` machinery already encodes.

Tool failures are `isError` results with plain sentences, not protocol
errors — an agent can read "no project named X (known: …)" and correct
itself; a JSON-RPC error code teaches it nothing.

## Consequences

- Repairs stay in the app: `mast_diagnose` names findings and says a repair
  exists, but applying one still goes through the desktop's previewed,
  consent-gated flow. Widening that to agents is a separate decision.
- Output is capped (last 100 lines per lifecycle call, bounded log tails) so
  a chatty `up` cannot flood an agent's context.
- The server holds no state of its own; every call re-reads the engine, so
  an agent and the desktop can operate the same fleet concurrently under the
  engine's existing per-project locks.
