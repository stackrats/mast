# ADR-0011: Every process tree in the app container, and who started it

- **Status:** accepted
- **Date:** 2026-10-14
- **Applies to:** `mast-engine` (`inventory`, `custom_command`, the reconcile pass, `Action::StopContainerRun`), `mast-contract` (`ContainerRun`, `Advisory`), `clients/desktop-vue` (`ContainerRuns.vue`, `Advisories.vue`)

## Question

Mast observed containers. Everything that matters for a Laravel app runs as
processes _inside_ one — Horizon, the scheduler, queue workers, Reverb,
Vite — and every `docker exec` leaves its own process tree there, which
outlives the client that started it. Quitting Mast, a Stop that silently
failed (v0.7.2–v0.7.4), an agent's `sail artisan horizon`: each left trees
nobody could see. One real container held two `artisan dev` stacks, 3 GB and
0.5 GB: two Horizon masters, two schedulers running every task twice, and a
Vite whose exit had deleted the other's `public/hot`, so pages served an old
build while every chip was green. What should Mast show, and on what
evidence?

## Decision

**One `/proc` walk per running app container, grouped into trees.** Each
reconcile pass runs one POSIX-sh script (`inventory::SCAN_SCRIPT`) as the
container's default user. It prints PID, parent, start time, resident set
and command line per process, plus Mast's run records. A tree is a process
whose parent is outside the container (`ppid 0`, a `docker exec`) or is
init (`ppid 1`), with everything beneath it. The card lists every tree
beyond the container's own startup, with owner, age, memory and the daemon
kinds inside it, and offers Stop for anything Mast is not running.

**Ownership comes from evidence Mast leaves, never from `environ`.** Sail's
PHP binary carries `cap_net_bind_service`. Running a binary with file
capabilities makes the process non-dumpable, so its `/proc/<pid>/environ`
is readable by no one without `CAP_SYS_PTRACE`, which Docker withholds.
That rules out the environment marker for any PHP process, including for
its own user. So:

- **A Sail command** starts through a wrapper that writes `pid start name`
  to `/tmp/mast-runs/<run>` and then `exec`s the command, so the PID is the
  command's own. A record whose run this engine holds is **Mast**; one it
  does not is an **earlier Mast session**; a record whose start time does
  not match is ignored (the PID was reused).
- **A built-in daemon** (the Processes chips) starts as `sail artisan …`
  for terminal parity and has no record. The newest tree that is exactly
  that daemon is claimed for **Mast** while Mast supervises one.
- **Anything else under `ppid 0`** was started **outside Mast**.
- **Under init:** the container's own when init is not a supervisor, or the
  child came up within ten seconds of init, or it is Sail's supervised
  `artisan serve` (`-d variables_order=EGPCS`). Otherwise it is
  **orphaned**: its starter exited and init adopted it.

**A daemon "runs" when a process is that daemon.** The kind comes from the
program position (`php [-d …] artisan <verb>`, the Vite CLI itself and not
its wrappers or a `build`), never from the name anywhere in a command line.
A dev-stack runner carries `php artisan horizon` among its arguments long
after the pane running it has died. The old substring check showed such a
Horizon as running.

**Stopping is by root PID and start time, as root.** Signalling needs only
`CAP_KILL`, which Docker grants, and the tree is read from
`/proc/<pid>/stat`, which anyone can read. A stale listing whose root's
start time no longer matches stops nothing.

**Running a Sail command replaces what an earlier session left.** Quitting
Mast never ends what its `docker exec`s started. Without this, every
relaunch's auto-start would double the dev stack. Before a Sail command
runs, trees whose record names that exact command and belongs to no live
run are stopped, and the operation's output says so. Nothing started
outside Mast is ever stopped without a click.

**Advisories put the fix beside the sentence.** Kinds that must run once
(Horizon, the scheduler, Reverb, Vite, Pail) running more than once become
one advisory per set of copies, offering to stop each copy Mast is not
running. `public/hot` is compared with `VITE_PORT` and probed. Three cases
get an advisory: Vite running with no hot file (pages get the old build), a
hot file pointing at another dev server, and a hot file nothing answers.
Restarting the dev command and removing the hot file are the offered fixes.
`public/` is watched non-recursively, filtered to `hot`, so the card
changes when the file does.

## Not decided here

Naming the host process behind a tree started outside Mast (a terminal, or
which agent session) means mapping a `docker exec` client to the exec it
created. Docker exposes no such link, and command-line matching is a guess.
Mast says "outside Mast" until there is better evidence than that.
