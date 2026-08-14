# ADR-0005: log captures — persisting a container's last words

- **Status:** accepted
- **Date:** 2026-08-12
- **Milestone:** M10
- **Applies to:** `mast-docker::RuntimeAdapter::container_log_tail`, `mast-engine::captures`, `MastClient::log_captures` / `subscribe_log_captures`

## Question

`Engine::service_logs` resolves a container id once and follows it. That is the
right shape for watching a running service and the wrong shape for
understanding a dead one.

The failure is structural, not a gap in coverage. A developer does not know
they wanted a container's output until after it has stopped producing any, and
by then one of three things has already happened: `up -d --force-recreate`
replaced the container and Docker started a fresh log; the follow stream ended
when the container did and its scrollback went with the panel; or the container
died while Mast was closed and nothing was watching at all. The third case is
the one users describe as "it just disappeared".

The engine also had no transition detection whatsoever — reconcile rebuilt
`summary.services` wholesale, no code compared the previous `container_id` to
the new one, and `ServiceHealth::Unhealthy` was read in exactly one place. So
Mast could not have noticed a death even if it had somewhere to put the answer.

How should Mast keep the output that explains a container's ending, without
holding a log stream open for every service on the machine?

## Decision

### 1. Read on demand at the two moments the evidence exists

Docker retains an _exited_ container's log until the container is **removed**.
That single fact removes the need for continuous buffering. Two triggers cover
everything:

- **Before Mast destroys it.** `dispatch_lifecycle` captures ahead of
  `stop`/`restart`/`rebuild`, and `run_workspace_layers` does the same per
  member on a workspace stop. `restart` reuses the container, but `rebuild`
  recreates it — and since the call site cannot know which compose will decide,
  the capture always precedes the command. `Up` is excluded: there is no
  post-mortem for a container about to start.
- **After Mast observes it.** The reconcile fold compares the previous and
  current `ServiceState` and records an unexpected exit or an
  unhealthy **transition**. Deciding happens under the state lock (where
  nothing can await); the reads happen after it, detached.

A service with **no previous observation of that container** is treated as
though it had been alive and well, so its current state reads as the transition
into it. This is not a corner case: it is exactly what a container that died
while Mast was closed looks like on the first reconcile after it opens, and
handling it any other way would mean the store outlived the process while the
detection did not. Note that a declared-but-unobserved service carries
`container_id: None`, so "first sighting" has to be distinguished from
"replaced" by the id being absent rather than merely different.

It cannot flood the tab, because the capture window bounds what can be read: a
container that stopped hours ago yields no lines, and a capture with no lines is
never recorded. `a_long_dead_container_leaves_no_empty_row` pins that.

Plus a readiness timeout — the case that motivated the feature most directly,
since a dependency-ordered workspace start that stalls reports only that it
stalled — and a manual capture from the service menu.

The alternative, a rolling in-memory buffer per running service, was rejected
on cost and on coverage. It would mean N persistent follow-streams for a
ten-project workspace, all decoding output nobody asked for, and it would still
lose everything on quit. One bounded `docker logs --since --tail` per capture
does strictly more for strictly less.

Two suppressions keep the tab honest: a 30s per-container window, so a teardown
capture and the reconcile that observes the same teardown do not both record;
and the transition rule above, so a persistently unhealthy container is
recorded once rather than every 250ms. A manual capture deliberately bypasses
the window — asking twice means wanting a second look.

The window is checked against the **store**, not only the in-memory map. A
suppression that died with the process would let "stop a project, reopen Mast"
record the same container's last words twice, which is the same mistake as
keeping the captures themselves in memory.

A container whose id changed while Mast was not operating on it was replaced
from outside. Its log is already gone, so nothing is read and nothing is
recorded — an empty row would imply the output was empty rather than absent.

### 2. On disk, not in a ring

History (ADR-0004) is a 300-entry in-memory ring that dies with the process.
Captures are not, and the reason is the requirement itself: a container that
died while Mast was closed — or that died _because_ the machine went down — is
precisely the one nobody can explain afterwards. An in-memory capture store
would work in every case except the ones it exists for.

So captures live in `captures.db` (rusqlite, bundled), beside `diagnostics.db`
and reached the same way, through `MetadataStore`. Retention is enforced on
every write rather than at startup: 200 captures, nothing older than 14 days,
200 lines per capture. Retention that only runs at boot does not hold for the
session that produced the rows.

Separate from `diagnostics.db` deliberately. Captures hold the developer's own
application output — a different kind of content from a check result, with its
own retention and its own reason to be deleted on its own.

### 3. Persistence makes redaction mandatory

ADR-0004 §5 drew an explicit line: history is redacted because it is persisted
and copyable, while "container log streams remain unredacted (they render the
developer's own application output transiently and are never persisted)".

Captures cross that line, and this ADR is what records it. A capture is written
to a file and rendered with a copy button, so both halves of the exemption are
gone. Every captured line therefore passes through `state.redactor_all` — the
**union** redactor, not the per-project one, for the same reason history uses
it: a container can echo another project's secret, and the store is one store.

Redaction happens at write time, not display time. A secret that reaches the
database has already escaped; masking it on the way out would protect the panel
and not the file. `redact.rs`'s header comment is amended accordingly — the
live-stream exemption still stands, and now says what it is an exemption _from_.

`secrets_never_reach_a_capture` asserts both the returned capture and the raw
bytes of `captures.db`.

### 4. Its own channel, like history

Captures follow ADR-0004 §3 rather than riding the patch stream. The argument
there was volume; here it is kind. A capture is an event with a body, not
state, and a client that resynchronizes from a snapshot would silently lose one
that arrived during the resync. `log_captures(limit)` serves the backlog,
`subscribe_log_captures()` the live feed, and the daemon and CLI get both over
the same socket at no extra engine cost.

Unlike history, captures are **append-only** — one is never revised after it is
written — so clients prepend rather than upsert. The full payload travels
rather than a summary plus a lazy body fetch: captures are rare (a handful per
session) and bounded at 200 lines, so the second round trip would buy nothing.

`ContainerObservation` gained `exit_code`, parsed from the same status text
`parse_health` already reads, so a crash can be labelled with the code it died
on. Lines carry Docker's RFC3339 stamp verbatim rather than a parsed instant:
clients parse it natively, so the engine needs no date crate.

## Consequences

- The engine now diffs services across a reconcile. `collect_deaths` is the
  only such comparison in the codebase and is the natural home for anything
  else that wants to know a service _changed_ rather than what it _is_.
- A teardown waits for its capture, bounded at 2s. A missing post-mortem is a
  worse UI; a blocked restart is a broken app. Every capture failure is logged
  and swallowed for the same reason.
- Mast now writes the developer's application output to disk. This is a new
  category of stored data, which is why retention is bounded on two axes and
  why "clear captures" is a real delete rather than a cleared view.
- The `captures` tab is the first place in the UI that shows something Mast
  observed rather than something Mast did.

## Not decided here

- **Per-project retention or opt-out.** A team with a chatty container may want
  a shorter window; nothing here is configurable from the UI yet.
- **Capturing on `Up`.** Compose edits (catalog add/remove, service edits) only
  write files — `catalog_ops` never runs a lifecycle command — so the recreate
  happens later, on the user's next Start, through `up -d`. `Up` is excluded
  from capture, so a **healthy** container recreated because its config-hash
  changed loses its log. That is the low-value half of the problem: nothing had
  gone wrong. The high-value half is covered, because a container that had
  crashed or gone unhealthy was already captured by reconcile before the user
  got to Start.
  Capturing on `Up` unconditionally was rejected: every routine start of a
  stopped project would file a "before start" row per service, and noise is
  what makes a tab of failures worth ignoring.
- **Searching captures.** The tab lists and expands; there is no filter, and at
  200 captures there will eventually want to be one.
