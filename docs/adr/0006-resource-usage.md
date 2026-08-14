# ADR-0006: resource usage — measuring the machine without becoming the load

- **Status:** accepted
- **Date:** 2026-08-12
- **Milestone:** M11
- **Applies to:** `mast-docker::RuntimeAdapter::container_stats`, `mast-engine::usage`, `MastClient::subscribe_usage`

## Question

Mast could say what was running but never what it cost. The three questions a
developer with four Sail projects up actually has — _what is making my fan
scream, can I afford to start another project, is this container leaking_ —
were all unanswerable.

The obvious implementation is the one most container GUIs ship: a CPU
percentage and a memory figure per project, refreshed continuously. That is
wrong in four specific ways, and getting each one right is most of this
decision.

## Decision

### 1. Cores, not percent

Docker reports CPU as a share of **all** cores, so `800%` is reachable on an
eight-core box. Rendered into a 0–100 bar — as the conventional design does —
it is not merely imprecise, it is incapable of expressing the common case.

`ServiceUsage::cpu_cores` is cores: `1.0` is one saturated core, and the client
pairs it with `host_cores` so the reading is `2.3 of 8`. No ceiling ambiguity,
no unit to explain.

### 2. Working set, not raw usage

Raw cgroup `usage` counts reclaimable page cache. A mysql container that is
holding 240 MB reads as multiple gigabytes, which makes the number worse than
useless — it invites people to kill the wrong thing. `working_set()` subtracts
`inactive_file` (cgroups v2) or `cache` (v1), which is exactly what
`docker stats` shows.

### 3. Against the limit, and knowing whether there is one

An absolute megabyte count carries no risk information. A ratio does — but only
if you know what the denominator is.

A container with no limit of its own reports **host RAM** as its limit, which is
the normal Sail case. So `memory_limited` records whether the limit is real,
and the UI reads the same ratio two different ways: with a real cgroup limit,
90% means _about to be OOM-killed_; without one, it means _using most of this
machine_, which is a headroom question and never escalates to red.

This is where the feature meets M10: an OOM kill surfaces as exit 137 and a
container that vanishes for no visible reason. **Resources predicts it,
captures explains it.**

### 4. History, not instants

One CPU reading is noise. A momentary spike and a steady climb produce the same
number and are completely different problems; only the shape distinguishes
them. Clients keep a ring of samples and render a sparkline, scaled to the
machine rather than self-scaled — a self-scaling strip makes an idle container
look identical to a saturated one.

### 5. Sampling is subscriber-driven

The sampler makes **no docker calls at all** while `usage_tx.receiver_count()`
is zero. The desktop drops its subscription on `visibilitychange`, so a
minimised window costs nothing.

This is not an optimisation, it is the difference between shippable and not.
Mast runs on a developer's laptop, and spending CPU to measure CPU when nobody
is looking at the answer would make the tool part of the problem it reports on.
`nothing_is_sampled_while_nobody_is_subscribed` pins all three edges: silent
before, sampling after subscribe, silent again after the last drop.

The consequence is gaps in history when the window was hidden. Those are
rendered as gaps, not interpolated — inventing points would show activity that
was never measured.

### 6. One-shot reads, delta computed here

Docker will compute a CPU delta itself, but only by blocking about a second per
container to collect two cycles. In `one_shot` mode it answers immediately with
raw counters, and the engine subtracts against the reading it already holds.

Cheaper, and steadier: the delta spans our interval rather than the daemon's
fixed one-second window. `StatsSample` therefore carries raw counters and no
arithmetic — the subtraction belongs where the previous reading lives.

Two failure modes are handled explicitly. A container whose id changed has its
previous reading dropped, because diffing a fresh counter against a predecessor
is meaningless. A counter that went backwards (a restart under the same id)
yields zero rather than a negative, and a ratio that would exceed the machine
is clamped to it — one bad tick must not rescale every sparkline in the UI.

### 7. Its own channel, and no persistence

A sample every couple of seconds is precisely the volume ADR-0004 refused for
the patch stream: it would evict real state patches from the replay window.
This is the fourth side channel after logs, history and captures.

Unlike captures, samples are **not persisted and not redacted**. They are
numbers, they carry none of the developer's own content, and they are worthless
a minute later — which is also why there is no backlog method to pair with the
subscription.

## Consequences

- Sampling is a read, so read-only instances measure normally. Contrast
  captures, which had to be gated on mutation ownership to avoid two instances
  writing the same rows.
- `ProgressBar.vue` is deleted. It was never imported, its track had no dark
  variant, and its fill colour was hardcoded; `Meter.vue` replaces it with a
  caller-supplied tone.
- `Sparkline.vue` is the first data visualisation in the app. It is built from
  divs rather than SVG — there is no other vector markup in the codebase, and a
  row of bars needs no viewBox arithmetic to stay crisp.
- The frontend now has its first visibility-driven subscription. The tick
  itself still belongs to the engine; the client only attaches and detaches.

## Not decided here

- **Disk.** `docker system df` and reclaimable space are genuinely actionable —
  Docker quietly eating 40GB is a common complaint — but it is a slow-changing
  global figure rather than a live per-container one, and diagnostics already
  carries a disk check.
- **Network and block I/O.** The stats response includes both. Neither answers
  one of the three questions above, so they are not collected.
- **Alerting.** Nothing notifies on sustained pressure, even though the data to
  do it now exists and OOM prediction is the obvious candidate.
- **Per-process attribution.** Mast knows about Horizon, Reverb and queue
  workers as processes inside the app container, but `docker stats` is
  per-container — splitting a container's cost across its processes would need
  a different mechanism entirely.
