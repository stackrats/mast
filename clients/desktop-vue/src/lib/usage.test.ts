import { describe, expect, it } from "vite-plus/test";

import type { ProjectSummary, ServiceUsage, UsageSample, WorkspaceSummary } from "../bindings";
import {
  cpuTone,
  defaultDirection,
  fleetRows,
  formatBytes,
  formatCores,
  formatPercent,
  memoryTone,
  quietProjects,
  QUIET_SAMPLES,
  rankServices,
  rollupByProject,
  rollupByWorkspace,
  rollupTotal,
  series,
} from "./usage";

const MB = 1024 * 1024;
const GB = 1024 * MB;

function service(overrides: Partial<ServiceUsage> = {}): ServiceUsage {
  return {
    project: "p1",
    service: "app",
    cpuCores: 0.5,
    memoryBytes: 100 * MB,
    memoryLimitBytes: 16 * GB,
    memoryLimited: false,
    ...overrides,
  };
}

function sample(services: ServiceUsage[]): UsageSample {
  return {
    atUnixMs: 1_700_000_000_000,
    hostCores: 8,
    hostMemoryBytes: 16 * GB,
    services,
  };
}

describe("formatBytes", () => {
  it("scales to the unit a human would use", () => {
    expect(formatBytes(512)).toBe("512 B");
    expect(formatBytes(240 * MB)).toBe("240 MB");
    expect(formatBytes(4.1 * GB)).toBe("4.1 GB");
  });

  it("keeps a decimal only where it carries information", () => {
    // 4.1 GB is worth a decimal; 240 MB is not — the tenth is noise.
    expect(formatBytes(4.1 * GB)).toBe("4.1 GB");
    expect(formatBytes(240.7 * MB)).toBe("241 MB");
  });

  it("says nothing rather than something wrong for absent data", () => {
    expect(formatBytes(0)).toBe("0 B");
    expect(formatBytes(Number.NaN)).toBe("0 B");
  });
});

describe("formatCores", () => {
  it("stays honest at both ends of the range", () => {
    // An idle container is not zero, and saying so would be a lie.
    expect(formatCores(0.04)).toBe("0.04");
    expect(formatCores(2.31667)).toBe("2.3");
    // Constant width down a column: "0" beside "0.06" reads as ragged.
    expect(formatCores(0)).toBe("0.00");
  });
});

describe("formatPercent", () => {
  it("rounds to whole percent", () => {
    expect(formatPercent(0.824)).toBe("82%");
    expect(formatPercent(0)).toBe("0%");
  });
});

describe("rollups", () => {
  const s = sample([
    service({ project: "p1", service: "app", cpuCores: 0.5, memoryBytes: 100 * MB }),
    service({ project: "p1", service: "mysql", cpuCores: 1.5, memoryBytes: 300 * MB }),
    service({ project: "p2", service: "redis", cpuCores: 0.25, memoryBytes: 50 * MB }),
  ]);

  it("totals everything", () => {
    expect(rollupTotal(s)).toEqual({ cpuCores: 2.25, memoryBytes: 450 * MB });
  });

  it("totals one project", () => {
    expect(rollupByProject(s, "p1")).toEqual({ cpuCores: 2, memoryBytes: 400 * MB });
  });

  it("totals a workspace from its members", () => {
    const workspace = {
      id: "w1",
      name: "suite",
      members: [{ project: "p1", dependsOn: [] }],
      status: "running",
      graphError: null,
      warnings: [],
    } as unknown as WorkspaceSummary;
    expect(rollupByWorkspace(s, workspace)).toEqual({ cpuCores: 2, memoryBytes: 400 * MB });
  });

  it("is zero, not a crash, before the first sample", () => {
    expect(rollupTotal(null)).toEqual({ cpuCores: 0, memoryBytes: 0 });
    expect(rollupByProject(null, "p1")).toEqual({ cpuCores: 0, memoryBytes: 0 });
  });
});

describe("series", () => {
  it("pulls one number per sample, oldest first", () => {
    const samples = [sample([service({ cpuCores: 0.1 })]), sample([service({ cpuCores: 0.9 })])];
    expect(series(samples, (s) => s.services[0].cpuCores)).toEqual([0.1, 0.9]);
  });
});

describe("memoryTone", () => {
  it("escalates to danger only against a real limit", () => {
    // A real cgroup limit: passing it means an OOM kill.
    expect(memoryTone(950 * MB, 1024 * MB, true)).toBe("danger");
    expect(memoryTone(800 * MB, 1024 * MB, true)).toBe("warn");
    expect(memoryTone(100 * MB, 1024 * MB, true)).toBe("neutral");
  });

  it("never panics about merely using the machine", () => {
    // No limit set, so the denominator is host RAM. Using most of it is a
    // headroom question, not an emergency — it must not read as fatal.
    expect(memoryTone(15 * GB, 16 * GB, false)).toBe("warn");
    expect(memoryTone(4 * GB, 16 * GB, false)).toBe("neutral");
  });

  it("says nothing when there is no denominator", () => {
    expect(memoryTone(100 * MB, 0, false)).toBe("neutral");
  });
});

describe("cpuTone", () => {
  it("nudges only when one service is taking most of the machine", () => {
    expect(cpuTone(5, 8)).toBe("warn");
    expect(cpuTone(1, 8)).toBe("neutral");
    expect(cpuTone(1, 0)).toBe("neutral");
  });
});

describe("rankServices", () => {
  const mixed = sample([
    service({ service: "quiet", cpuCores: 0.1, memoryBytes: 90 * MB }),
    service({ service: "hog", cpuCores: 1.9, memoryBytes: 20 * MB }),
    service({ service: "middle", cpuCores: 0.5, memoryBytes: 30 * MB }),
  ]);

  it("defaults to the loudest first, because that is the one you stop", () => {
    expect(rankServices(mixed).map((x) => x.service)).toEqual(["hog", "middle", "quiet"]);
  });

  it("breaks CPU ties on memory", () => {
    const s = sample([
      service({ service: "small", cpuCores: 0, memoryBytes: 10 * MB }),
      service({ service: "large", cpuCores: 0, memoryBytes: 90 * MB }),
    ]);
    expect(rankServices(s).map((x) => x.service)).toEqual(["large", "small"]);
  });

  it("sorts by memory when asked, which is a different order entirely", () => {
    expect(rankServices(mixed, "memory").map((x) => x.service)).toEqual(["quiet", "middle", "hog"]);
  });

  it("sorts by name, grouping a project's services together", () => {
    const s = sample([
      service({ project: "p2", service: "beta" }),
      service({ project: "p1", service: "zulu" }),
      service({ project: "p1", service: "alpha" }),
    ]);
    expect(rankServices(s, "service", "asc").map((x) => `${x.project}/${x.service}`)).toEqual([
      "p1/alpha",
      "p1/zulu",
      "p2/beta",
    ]);
  });

  it("reverses cleanly", () => {
    expect(rankServices(mixed, "cpu", "asc").map((x) => x.service)).toEqual([
      "quiet",
      "middle",
      "hog",
    ]);
  });

  it("is empty before the first sample", () => {
    expect(rankServices(null)).toEqual([]);
  });
});

describe("defaultDirection", () => {
  it("starts names at A and costs at the biggest", () => {
    expect(defaultDirection("service")).toBe("asc");
    expect(defaultDirection("cpu")).toBe("desc");
    expect(defaultDirection("memory")).toBe("desc");
  });
});

describe("the fleet", () => {
  function project(overrides: Partial<ProjectSummary> = {}): ProjectSummary {
    return {
      id: "p1",
      name: "storefront",
      path: "/home/dev/storefront",
      status: "running",
      composeProjectName: "storefront",
      isSail: true,
      services: [],
      resolutionError: null,
      commands: [],
      processes: [],
      warnings: [],
      ...overrides,
    } as ProjectSummary;
  }
  const window = (cores: number, count = QUIET_SAMPLES) =>
    Array.from({ length: count }, () => sample([service({ project: "p1", cpuCores: cores })]));

  it("calls a project quiet only on a full window of evidence", () => {
    expect(quietProjects(window(0.01)).has("p1")).toBe(true);
    // One sample short: no verdict yet, so a freshly opened window never
    // offers to stop anything.
    expect(quietProjects(window(0.01, QUIET_SAMPLES - 1)).has("p1")).toBe(false);
    expect(quietProjects([]).size).toBe(0);
  });

  it("does not average away a spike", () => {
    const samples = window(0.01);
    samples[5] = sample([service({ project: "p1", cpuCores: 3 })]);
    // The mean would still be tiny; every sample must agree, so it is not quiet.
    expect(quietProjects(samples).has("p1")).toBe(false);
  });

  it("ranks running projects first, then by cost", () => {
    const projects = [
      project({ id: "idle", name: "parked", status: "stopped" }),
      project({ id: "small", name: "small", status: "running" }),
      project({ id: "big", name: "big", status: "running" }),
    ];
    const latest = sample([
      service({ project: "small", cpuCores: 0.2 }),
      service({ project: "big", cpuCores: 2 }),
    ]);
    const rows = fleetRows(projects, [latest]);
    expect(rows.map((r) => r.project.id)).toEqual(["big", "small", "idle"]);
    expect(rows[0].cpuCores).toBe(2);
  });

  it("never marks a stopped project quiet — it costs nothing to stop again", () => {
    const projects = [project({ id: "p1", status: "stopped" })];
    const rows = fleetRows(projects, window(0));
    expect(rows[0].quiet).toBe(false);
    expect(rows[0].quietFor).toBe(0);
  });

  it("counts only running containers", () => {
    const projects = [
      project({
        services: [
          { name: "app", state: "running" },
          { name: "mysql", state: "running" },
          { name: "redis", state: "exited" },
        ] as ProjectSummary["services"],
      }),
    ];
    expect(fleetRows(projects, []).at(0)?.containers).toBe(2);
  });
});
