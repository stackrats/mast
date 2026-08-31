import { describe, expect, it } from "vite-plus/test";

import type { ProjectSummary, WorkspaceSummary } from "../bindings";
import { buildPalette, filterPalette, groupPalette, matchScore } from "./palette";

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

const titles = (items: { title: string }[]) => items.map((i) => i.title);

describe("matchScore", () => {
  it("matches subsequences and prefers the tighter match", () => {
    expect(matchScore("st", "Start storefront")).not.toBeNull();
    expect(matchScore("zz", "Start storefront")).toBeNull();
    // A contiguous prefix beats letters scattered through the string.
    const tight = matchScore("stor", "storefront")!;
    const loose = matchScore("stor", "set the colour orange")!;
    expect(tight).toBeLessThan(loose);
  });

  it("prefers word boundaries over mid-word letters", () => {
    // Matching is greedy-leftmost, so the pair has to be chosen so the first
    // candidate `f` IS the boundary one — in "storefront finder" the greedy
    // match takes the f of storefront and never reaches the word break.
    const boundary = matchScore("sf", "start finder")!;
    const midword = matchScore("sf", "stuffing")!;
    expect(boundary).toBeLessThan(midword);
  });

  it("breaks ties toward the shorter target", () => {
    expect(matchScore("stop", "Stop app")!).toBeLessThan(matchScore("stop", "Stop application")!);
  });

  it("an empty query matches everything equally", () => {
    expect(matchScore("", "anything")).toBe(0);
  });
});

describe("buildPalette", () => {
  it("only offers lifecycle verbs that can work", () => {
    const stopped = buildPalette([project({ status: "stopped" })], []);
    expect(titles(stopped)).toContain("Start storefront");
    expect(titles(stopped)).not.toContain("Stop storefront");
    expect(titles(stopped)).not.toContain("Restart storefront");

    const running = buildPalette([project({ status: "running" })], []);
    expect(titles(running)).toContain("Stop storefront");
    expect(titles(running)).toContain("Restart storefront");
    expect(titles(running)).not.toContain("Start storefront");
  });

  it("offers Browser only when the project has an address", () => {
    expect(titles(buildPalette([project()], []))).not.toContain("Browser — storefront");
    const withUrl = buildPalette([project({ appUrl: "http://localhost:8080" })], []);
    expect(titles(withUrl)).toContain("Browser — storefront");
  });

  it("includes the project's own commands", () => {
    const items = buildPalette(
      [
        project({
          commands: [{ name: "migrate", command: "artisan migrate" }] as ProjectSummary["commands"],
        }),
      ],
      [],
    );
    const migrate = items.find((i) => i.title === "migrate — storefront");
    expect(migrate?.effect).toEqual({
      kind: "action",
      action: { type: "runProjectCommand", id: "p1", name: "migrate" },
      project: "p1",
      label: "migrate",
    });
  });

  it("puts projects before everything else, so a bare query lands on them", () => {
    const items = buildPalette([project()], []);
    expect(items[0].group).toBe("Projects");
  });

  it("covers workspaces and the app's own dialogs", () => {
    const ws = { id: "w1", name: "checkout", members: [] } as unknown as WorkspaceSummary;
    const items = buildPalette([], [ws]);
    expect(titles(items)).toEqual(
      expect.arrayContaining(["checkout", "Start checkout", "Stop checkout", "Settings"]),
    );
  });
});

describe("filterPalette", () => {
  const items = buildPalette(
    [project({ id: "p1", name: "storefront" }), project({ id: "p2", name: "admin" })],
    [],
  );

  it("ranks a title hit above a keyword-only hit", () => {
    // "shell" is only a keyword of the Terminal entry; a title match must win.
    const results = filterPalette(items, "term");
    expect(results[0].title.startsWith("Terminal")).toBe(true);
  });

  it("finds entries by keyword when the title does not contain the query", () => {
    const results = filterPalette(items, "vscode");
    expect(results.some((r) => r.title.startsWith("Editor"))).toBe(true);
  });

  it("returns nothing for a query that matches nothing", () => {
    expect(filterPalette(items, "qqqqzz")).toEqual([]);
  });

  it("an empty query returns the natural order, capped", () => {
    expect(filterPalette(items, "", 3)).toHaveLength(3);
    expect(filterPalette(items, "  ")[0].group).toBe("Projects");
  });
});

describe("groupPalette", () => {
  it("keeps ranked order inside each group and first-seen order across them", () => {
    const grouped = groupPalette([
      { id: "a", title: "A", group: "Projects", keywords: "", effect: { kind: "home" } },
      { id: "b", title: "B", group: "App", keywords: "", effect: { kind: "home" } },
      { id: "c", title: "C", group: "Projects", keywords: "", effect: { kind: "home" } },
    ]);
    expect(grouped.map((g) => g.group)).toEqual(["Projects", "App"]);
    expect(titles(grouped[0].items)).toEqual(["A", "C"]);
  });
});

describe("attention and tinker entries", () => {
  it("projects with unread markers lead, and selecting is the effect", () => {
    const items = buildPalette([project()], [], { p1: ["went degraded", "recovered"] });
    const attention = items.find((i) => i.group === "Needs attention");
    expect(attention).toBeDefined();
    expect(attention!.hint).toBe("recovered"); // the newest word wins the hint
    expect(attention!.effect).toEqual({ kind: "select", id: "p1" });
    // Built before the plain jump entry, so an empty query surfaces it first.
    expect(items.findIndex((i) => i.group === "Needs attention")).toBeLessThan(
      items.findIndex((i) => i.id === "go:p1"),
    );
  });

  it("no markers, no group", () => {
    const items = buildPalette([project()], []);
    expect(items.some((i) => i.group === "Needs attention")).toBe(false);
  });

  it("tinker is offered only where an app container exists to exec into", () => {
    const up = buildPalette([project()], []);
    expect(titles(up)).toContain("Tinker — storefront");
    const stopped = buildPalette([project({ status: "stopped" })], []);
    expect(titles(stopped)).not.toContain("Tinker — storefront");
    const plainCompose = buildPalette([project({ isSail: false })], []);
    expect(titles(plainCompose)).not.toContain("Tinker — storefront");
  });
});
