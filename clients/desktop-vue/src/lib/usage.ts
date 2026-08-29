// Presentation and arithmetic for resource usage (the status bar readout, the
// Resources tab, and the per-project/per-workspace rollups).
//
// The engine reports per-service numbers only. Every aggregate in the UI is
// derived here from one sample, so there is no second source of truth to keep
// in step — and the rollups stay unit-testable without a running engine.

import type {
  ProjectId,
  ProjectSummary,
  ServiceUsage,
  UsageSample,
  WorkspaceSummary,
} from "../bindings";

/** A rolled-up total for a project, a workspace, or everything. */
export interface UsageTotal {
  cpuCores: number;
  memoryBytes: number;
}

const EMPTY: UsageTotal = { cpuCores: 0, memoryBytes: 0 };

function sum(services: ServiceUsage[]): UsageTotal {
  return services.reduce<UsageTotal>(
    (total, s) => ({
      cpuCores: total.cpuCores + s.cpuCores,
      memoryBytes: total.memoryBytes + s.memoryBytes,
    }),
    EMPTY,
  );
}

export function rollupTotal(sample: UsageSample | null): UsageTotal {
  return sample ? sum(sample.services) : EMPTY;
}

export function rollupByProject(sample: UsageSample | null, project: ProjectId): UsageTotal {
  return sample ? sum(sample.services.filter((s) => s.project === project)) : EMPTY;
}

export function rollupByWorkspace(
  sample: UsageSample | null,
  workspace: WorkspaceSummary,
): UsageTotal {
  if (!sample) return EMPTY;
  const members = new Set(workspace.members.map((m) => m.project));
  return sum(sample.services.filter((s) => members.has(s.project)));
}

/** Pull one number out of each sample, for a sparkline. Samples arrive newest
 * last, which is also the order a sparkline is drawn in. */
export function series(samples: UsageSample[], pick: (sample: UsageSample) => number): number[] {
  return samples.map(pick);
}

/**
 * Bytes at the precision a human reads: three significant figures at most, and
 * no trailing `.0` on a whole number. Binary units, because that is what the
 * kernel is reporting and what `docker stats` shows.
 */
export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes <= 0) return "0 B";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  // Sub-10 values keep a decimal ("4.1 GB"); above that it is noise ("240 MB").
  const rounded = value >= 10 || unit === 0 ? Math.round(value) : Math.round(value * 10) / 10;
  return `${rounded} ${units[unit]}`;
}

/**
 * Cores, at a precision that stays honest at both ends: an idle container is
 * `0.01`, not `0.0`, and a busy one is `2.3`, not `2.31667`.
 */
export function formatCores(cores: number): string {
  if (!Number.isFinite(cores) || cores <= 0) return "0.00";
  if (cores < 0.1) return cores.toFixed(2);
  return cores.toFixed(1);
}

/** `0.42` → `42%`. */
export function formatPercent(fraction: number): string {
  if (!Number.isFinite(fraction) || fraction <= 0) return "0%";
  return `${Math.round(fraction * 100)}%`;
}

export type Tone = "neutral" | "warn" | "danger";

/**
 * How alarming a memory figure is.
 *
 * Only a **real** cgroup limit can be approached dangerously — passing it gets
 * the container OOM-killed, which surfaces as exit 137 and a mysterious
 * disappearance. Without a limit the denominator is just host RAM, and using
 * most of your machine is a headroom question, not an emergency, so it never
 * escalates past `warn`.
 */
export function memoryTone(bytes: number, limit: number, limited: boolean): Tone {
  if (limit <= 0) return "neutral";
  const ratio = bytes / limit;
  if (limited) {
    if (ratio >= 0.9) return "danger";
    if (ratio >= 0.75) return "warn";
    return "neutral";
  }
  return ratio >= 0.85 ? "warn" : "neutral";
}

/** CPU has no hard ceiling to breach, so this is only ever a nudge: a service
 * pinning most of the machine on its own is worth the eye going to it. */
export function cpuTone(cores: number, hostCores: number): Tone {
  if (hostCores <= 0) return "neutral";
  return cores / hostCores >= 0.5 ? "warn" : "neutral";
}

/** Column the resources table is ordered by. */
export type SortKey = "service" | "cpu" | "memory";
export type SortDirection = "asc" | "desc";

/** Where a column starts when you first click it: names read naturally A–Z,
 * but the point of sorting by a cost is to see the biggest one. */
export function defaultDirection(key: SortKey): SortDirection {
  return key === "service" ? "asc" : "desc";
}

/**
 * Services in the order the table should show them.
 *
 * The default — CPU, descending — is the one that answers "what do I stop",
 * which is why it is what the tab opens on. Memory breaks CPU ties, because
 * two services both idling at zero are still not equally worth looking at.
 */
export function rankServices(
  sample: UsageSample | null,
  key: SortKey = "cpu",
  direction: SortDirection = "desc",
): ServiceUsage[] {
  if (!sample) return [];
  const sign = direction === "asc" ? -1 : 1;
  return [...sample.services].sort((a, b) => {
    const by =
      key === "service"
        ? // Grouped by project so a project's services stay together, then by
          // service name — the reading order, not an arbitrary one.
          b.project.localeCompare(a.project) || b.service.localeCompare(a.service)
        : key === "memory"
          ? b.memoryBytes - a.memoryBytes || b.cpuCores - a.cpuCores
          : b.cpuCores - a.cpuCores || b.memoryBytes - a.memoryBytes;
    return by * sign;
  });
}

// --- The fleet: every project at once, and which of them are costing you
// something for nothing. ---

/**
 * Below this many cores a project is doing nothing you asked for. An idle
 * Sail stack (php-fpm waiting, mysql parked, redis parked) sits near a
 * hundredth of a core; anything actually serving a request leaves it far
 * behind. Deliberately generous, because the cost of calling a busy project
 * quiet is stopping someone's work.
 */
export const QUIET_CORES = 0.05;

/**
 * How many consecutive samples must agree. At the engine's 2s cadence 30
 * samples is a minute — long enough that a request lull is not mistaken for
 * a project nobody is using.
 */
export const QUIET_SAMPLES = 30;

/** One project's line in the fleet table. */
export interface FleetRow {
  project: ProjectSummary;
  cpuCores: number;
  memoryBytes: number;
  /** Services currently up — the containers this project is costing you. */
  containers: number;
  /** Running, and under [`QUIET_CORES`] for every retained sample. */
  quiet: boolean;
  /** Seconds the quiet verdict is based on; 0 when it is not quiet. */
  quietFor: number;
}

/**
 * Projects that have stayed under the threshold across the whole retained
 * window.
 *
 * Judged on EVERY sample rather than an average: a project that spiked once
 * and settled is still being used, and averaging would hide that. Requires
 * a full window of evidence, so a project cannot be called quiet moments
 * after the app opens.
 */
export function quietProjects(
  samples: UsageSample[],
  cores = QUIET_CORES,
  minSamples = QUIET_SAMPLES,
): Set<ProjectId> {
  const recent = samples.slice(-minSamples);
  if (recent.length < minSamples) return new Set();
  const seen = new Map<ProjectId, boolean>();
  for (const sample of recent) {
    const perProject = new Map<ProjectId, number>();
    for (const s of sample.services) {
      perProject.set(s.project, (perProject.get(s.project) ?? 0) + s.cpuCores);
    }
    for (const [project, total] of perProject) {
      seen.set(project, (seen.get(project) ?? true) && total <= cores);
    }
  }
  return new Set([...seen].filter(([, quiet]) => quiet).map(([project]) => project));
}

/**
 * The fleet table's rows. Running projects first — they are the ones with a
 * cost to answer for — then by CPU, so the row you would stop is at the top
 * of the half you would stop from.
 */
export function fleetRows(
  projects: ProjectSummary[],
  samples: UsageSample[],
  intervalMs = 2000,
): FleetRow[] {
  const latest = samples.at(-1) ?? null;
  const quiet = quietProjects(samples);
  const window = Math.round((Math.min(samples.length, QUIET_SAMPLES) * intervalMs) / 1000);
  return projects
    .map((project) => {
      const total = rollupByProject(latest, project.id);
      const running = project.status === "running";
      const isQuiet = running && quiet.has(project.id);
      return {
        project,
        cpuCores: total.cpuCores,
        memoryBytes: total.memoryBytes,
        containers: project.services.filter((s) => s.state === "running").length,
        quiet: isQuiet,
        quietFor: isQuiet ? window : 0,
      };
    })
    .sort((a, b) => {
      const aUp = a.project.status === "running" ? 0 : 1;
      const bUp = b.project.status === "running" ? 0 : 1;
      return (
        aUp - bUp ||
        b.cpuCores - a.cpuCores ||
        b.memoryBytes - a.memoryBytes ||
        a.project.name.localeCompare(b.project.name)
      );
    });
}
