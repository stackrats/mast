import { describe, expect, it } from "vite-plus/test";

import type { ContainerRun, HostRun, ProjectSummary, Worktree } from "../bindings";
import {
  cleanupPlan,
  hasActivity,
  hasAgentWork,
  madeAgo,
  overview,
  placeLabel,
  removalCost,
  runsOf,
  stackSummary,
  unattachedRuns,
  worktreeFacts,
} from "./agents";

const NOW = 1_000_000_000;
const WT = "/home/dev/shop/.claude/worktrees/fix-login";

function worktree(patch: Partial<Worktree> = {}): Worktree {
  return {
    path: WT,
    name: "fix-login",
    branch: "worktree-fix-login",
    head: "8ba4734",
    tool: "Claude Code",
    createdAtUnix: NOW / 1000 - 3600,
    missing: false,
    locked: false,
    changedFiles: 0,
    unmergedCommits: 0,
    containers: [],
    ...patch,
  };
}

function hostRun(pid: number, owner: HostRun["owner"], patch: Partial<HostRun> = {}): HostRun {
  return {
    pid,
    startTicks: pid * 10,
    owner,
    title: "Vite :5299",
    commandLine: "node vite",
    cwd: "/home/dev/shop",
    worktree: null,
    cwdDeleted: false,
    startedAtUnix: NOW / 1000 - 600,
    memoryBytes: 1_000_000_000,
    processCount: 2,
    kinds: ["Vite"],
    ...patch,
  };
}

function containerRun(pid: number, owner: ContainerRun["owner"]): ContainerRun {
  return {
    pid,
    startTicks: pid,
    owner,
    title: "artisan horizon",
    commandLine: "php artisan horizon",
    startedAtUnix: NOW / 1000 - 60,
    memoryBytes: 80_000_000,
    processCount: 3,
    kinds: ["Horizon"],
  };
}

function project(patch: Partial<ProjectSummary> = {}): ProjectSummary {
  return {
    id: "p1",
    name: "shop",
    path: "/home/dev/shop",
    status: "running",
    composeProjectName: "shop",
    isSail: true,
    services: [],
    resolutionError: null,
    commands: [],
    processes: [],
    gitBranch: "main",
    gitDirty: false,
    appUrl: null,
    php: null,
    shareUrl: null,
    shareDashboardUrl: null,
    localDomain: null,
    warnings: [],
    containerRuns: [],
    advisories: [],
    worktrees: [],
    agentSessions: [],
    hostRuns: [],
    rank: 0,
    ...patch,
  };
}

const session = {
  agent: "Claude Code",
  pid: 42,
  startTicks: 1,
  cwd: "/home/dev/shop",
  worktree: null,
  startedAtUnix: NOW / 1000 - 7200,
};

describe("agents", () => {
  it("has something to say only about projects agents touched", () => {
    expect(hasActivity(project())).toBe(false);
    expect(
      hasActivity(project({ containerRuns: [containerRun(1, { type: "mast", command: "dev" })] })),
    ).toBe(false);
    expect(hasActivity(project({ worktrees: [worktree()] }))).toBe(true);
    expect(hasActivity(project({ containerRuns: [containerRun(1, { type: "outside" })] }))).toBe(
      true,
    );
  });

  it("gives a card an Agents section only for what its container row does not show", () => {
    expect(hasAgentWork(project({ containerRuns: [containerRun(1, { type: "outside" })] }))).toBe(
      false,
    );
    expect(hasAgentWork(project({ worktrees: [worktree()] }))).toBe(true);
    expect(hasAgentWork(project({ hostRuns: [hostRun(1, { type: "orphaned" })] }))).toBe(true);
  });

  it("groups host runs under the session that started them", () => {
    const mine = hostRun(1, { type: "agent", agent: "Claude Code", pid: 42 });
    const elsewhere = hostRun(2, { type: "agent", agent: "Codex", pid: 99 });
    const orphan = hostRun(3, { type: "orphaned" });
    const p = project({ agentSessions: [session], hostRuns: [mine, elsewhere, orphan] });
    expect(runsOf(p, session).map((r) => r.pid)).toEqual([1]);
    expect(unattachedRuns(p).map((r) => r.pid)).toEqual([2, 3]);
  });

  it("dates worktrees coarsely", () => {
    expect(madeAgo(NOW / 1000 - 40 * 60, NOW)).toBe("40m");
    expect(madeAgo(NOW / 1000 - 5 * 3600, NOW)).toBe("5h");
    expect(madeAgo(NOW / 1000 - 185 * 3600, NOW)).toBe("7d");
  });

  it("names where something works", () => {
    const p = project({ worktrees: [worktree()] });
    expect(placeLabel(p, null)).toBe("the checkout");
    expect(placeLabel(p, WT)).toBe("fix-login");
    expect(placeLabel(p, "/elsewhere/gone-one")).toBe("gone-one");
  });

  it("states a worktree's state worst first", () => {
    expect(
      worktreeFacts(worktree({ missing: true, changedFiles: null })).map((f) => f.text),
    ).toEqual(["directory gone"]);
    expect(
      worktreeFacts(worktree({ changedFiles: 3, unmergedCommits: 1 })).map((f) => f.text),
    ).toEqual(["3 uncommitted changes", "1 commit not in the checkout"]);
    expect(worktreeFacts(worktree()).map((f) => f.text)).toEqual(["nothing unmerged"]);
  });

  it("summarises a worktree's own containers", () => {
    const containers = [
      {
        id: "a",
        name: "fix-login-laravel.test-1",
        service: "laravel.test",
        composeProject: "fix-login",
        running: true,
        ports: [80, 5173],
      },
      {
        id: "b",
        name: "fix-login-mysql-1",
        service: "mysql",
        composeProject: "fix-login",
        running: false,
        ports: [3306],
      },
    ];
    expect(stackSummary(worktree({ containers }))).toBe(
      "fix-login: 1/2 running · ports 80, 3306, 5173",
    );
    expect(stackSummary(worktree())).toBeNull();
  });

  it("says what removing a worktree costs and when it needs force", () => {
    expect(removalCost(worktree())).toEqual({
      force: false,
      lines: ["The branch worktree-fix-login is kept."],
    });
    const dirty = removalCost(worktree({ changedFiles: 2, unmergedCommits: 3 }));
    expect(dirty.force).toBe(true);
    expect(dirty.lines[0]).toContain("2 uncommitted changes will be lost");
    expect(dirty.lines[1]).toBe("The branch worktree-fix-login is kept, with its 3 commits.");
    const detached = removalCost(worktree({ branch: null, unmergedCommits: 1 }));
    expect(detached.force).toBe(true);
    expect(detached.lines[0]).toContain("no branch keeps");
  });

  it("counts leftovers and their memory across projects", () => {
    const p = project({
      agentSessions: [session],
      hostRuns: [
        hostRun(1, { type: "orphaned" }),
        hostRun(2, { type: "agent", agent: "Claude Code", pid: 42 }),
      ],
      containerRuns: [
        containerRun(5, { type: "earlierMast", command: "dev" }),
        containerRun(6, { type: "outside" }),
      ],
      worktrees: [worktree({ missing: true })],
    });
    expect(overview([p, project({ id: "p2" })])).toEqual({
      sessions: 1,
      worktrees: 1,
      missingWorktrees: 1,
      worktreeStacks: 0,
      leftovers: 2,
      leftoverBytes: 1_080_000_000,
    });
  });

  it("ticks only what nothing live is using, and never removes a worktree", () => {
    const busy = worktree({
      containers: [
        {
          id: "a",
          name: "x",
          service: "laravel.test",
          composeProject: "fix-login",
          running: true,
          ports: [],
        },
      ],
    });
    const p = project({
      agentSessions: [{ ...session, worktree: WT, cwd: WT }],
      hostRuns: [
        hostRun(1, { type: "orphaned" }),
        hostRun(2, { type: "agent", agent: "Claude Code", pid: 42 }),
      ],
      containerRuns: [
        containerRun(5, { type: "earlierMast", command: "dev" }),
        containerRun(6, { type: "outside" }),
        containerRun(7, { type: "mast", command: "dev" }),
      ],
      worktrees: [busy, worktree({ path: "/gone", name: "gone", missing: true })],
    });
    const plan = cleanupPlan([p], NOW);
    expect(plan.map((item) => [item.action.type, item.suggested])).toEqual([
      ["stopHostRun", true],
      ["stopContainerRun", true],
      ["pruneWorktrees", true],
      ["stopHostRun", false],
      ["stopContainerRun", false],
      ["removeWorktreeContainers", false],
    ]);
    expect(plan.some((item) => item.action.type === "removeWorktree")).toBe(false);
    expect(plan[0].detail).toBe("orphaned · in the checkout · 2 processes · 954 MB · up 10m 00s");
  });
});
