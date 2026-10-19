# ADR-0013: The engine knows which saved commands are running

- **Status:** accepted
- **Date:** 2026-10-15
- **Applies to:** `mast-engine` (`command_ops`, `RunningCommand`, `Action::StopProjectCommand`), `mast-contract` (`ProjectSummary::running_commands`), `clients/desktop-vue` (`commandRunningElsewhere`, the card's command chips, auto-start), `mast-cli` (`--command`, `mast workspace`, `mast agents --stop/--clean-up`)

## Question

A saved command — `sail artisan dev`, `vp dev` — runs as an operation in
the engine, but only the client that dispatched it knew. The desktop
tracked its own operations; the CLI and MCP could start a command and
then had no way to see or stop it, and the desktop showed a command they
started as stopped, offered Run, and auto-started a second copy when the
project came up. A bar widget that starts `dev` from the Omarchy panel
makes that the common case rather than an edge. Who should know a
command is running?

## Decision

**The engine keeps the registry.** Every `RunProjectCommand` is entered in
`command_ops` under (project, command name) for as long as its work runs,
and the project's summary carries `running_commands`, the names, sorted.
Every client reads the same list: the card's chips show a command another
client started as running, and auto-start leaves it alone.

**One copy at a time, whoever asks.** A second `RunProjectCommand` for a
name already running is refused with a conflict at dispatch, before an
operation exists. The slot is reserved under the registry's lock, so two
clients racing cannot both start one, and it is released when the work
ends — before the terminal event — so a client that reruns on
`Cancelled`, as Restart does, finds it free.

**Stopping by name.** `StopProjectCommand { id, name }` cancels the
registered operation exactly as its starter would, then waits up to 45
seconds for it to end: a command gets ten after TERM before it is killed.
Stopping a command that is not running succeeds and says so. The desktop
stops its own runs as before and uses this for anyone else's; Restart of
someone else's run stops it through the engine and reruns it from the
window, so the window follows the new run's output.

**The CLI drives the day-to-day verbs.** `mast start|stop|restart <project>
--command <name>` run and stop saved commands; `mast workspace start|stop
<name>` runs a workspace in dependency order; `mast agents <project>
--stop <pid>` / `--stop-container <pid>` and `mast agents --clean-up`
act on what ADR-0012 lists — the clean-up taking exactly what the app's
Clean up ticks by itself. A command needs an engine that outlives the CLI,
so starting one through the CLI's own embedded engine is refused with
what to start instead. MCP stays read-only for these, as ADR-0012 keeps it.

`mast://agents` opens the Agents page. Like every link (ADR-0010), it
only navigates.

## Consequences

`running_commands` and the new action are wire growth, so this is a minor
release (0.9.0): an 0.8 client neither shows nor sends them.

A command's output streams to the client that started it. Another client
sees it running and can stop or restart it, but not read what it has
printed so far.
