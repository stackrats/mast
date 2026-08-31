// The command palette's contents and its matching.
//
// Commands are DATA, not closures: every entry carries an `effect` the
// palette component interprets. That keeps the whole list — which commands
// exist for a project in a given state, what they are called, how they rank
// for a query — testable without mounting anything or faking a store.

import type { Action, ProjectId, ProjectSummary, WorkspaceSummary } from "../bindings";

/** What choosing an entry does. */
export type PaletteEffect =
  /** Move the main pane. */
  | { kind: "select"; id: ProjectId }
  | { kind: "home" }
  | { kind: "workspace"; id: string }
  /** Dispatch an engine action, tracked as an operation on `project`. */
  | { kind: "action"; action: Action; project: ProjectId; label: string }
  /** Fire-and-forget engine action with no operation of its own. */
  | { kind: "run"; action: Action }
  /** Open one of the app's own dialogs. */
  | { kind: "dialog"; what: "settings" | "newProject" | "newWorkspace" };

export interface PaletteItem {
  id: string;
  title: string;
  /** Right-hand context — usually which project the command acts on. */
  hint?: string;
  group: string;
  /** Extra words that should match, never displayed. */
  keywords: string;
  effect: PaletteEffect;
}

/**
 * Every command available right now.
 *
 * Ordered by how often it is wanted, not alphabetically: jumping to a
 * project is the overwhelming majority of palette use, so those come first
 * and a bare query lands on them. Lifecycle verbs are offered only where
 * they make sense — a stopped project has nothing to stop — because an
 * entry that cannot work is worse than one that is absent.
 */
export function buildPalette(
  projects: ProjectSummary[],
  workspaces: WorkspaceSummary[],
  attention: Record<string, string[]> = {},
): PaletteItem[] {
  const items: PaletteItem[] = [];

  // What happened while you were elsewhere, first: the palette's whole job
  // is "take me to the thing", and a project with an unread marker is the
  // likeliest thing. Selecting it clears the marker, so the group drains
  // itself.
  for (const p of projects) {
    const titles = attention[p.id];
    if (!titles?.length) continue;
    items.push({
      id: `attention:${p.id}`,
      title: p.name,
      hint: titles[titles.length - 1],
      group: "Needs attention",
      keywords: `attention while away ${titles.join(" ")}`,
      effect: { kind: "select", id: p.id },
    });
  }

  for (const p of projects) {
    items.push({
      id: `go:${p.id}`,
      title: p.name,
      hint: p.status,
      group: "Projects",
      keywords: `${p.path} open go to jump ${p.status}`,
      effect: { kind: "select", id: p.id },
    });
  }

  for (const p of projects) {
    const running = p.status === "running";
    const verb = (title: string, action: Action, label: string, keywords: string): PaletteItem => ({
      id: `${label}:${p.id}`,
      title: `${title} ${p.name}`,
      hint: p.name,
      group: "Lifecycle",
      keywords,
      effect: { kind: "action", action, project: p.id, label },
    });
    if (!running) {
      items.push(verb("Start", { type: "startProject", id: p.id }, "start", "up boot launch"));
    }
    if (p.status !== "stopped") {
      items.push(verb("Stop", { type: "stopProject", id: p.id }, "stop", "down halt"));
      items.push(verb("Restart", { type: "restartProject", id: p.id }, "restart", "bounce reload"));
    }
  }

  // The things people leave the app to do. These are the reason a palette
  // beats the button row: they are one keystroke from anywhere, and they do
  // not each need a permanent place on the card.
  for (const p of projects) {
    const open = (title: string, action: Action, keywords: string): PaletteItem => ({
      id: `${title}:${p.id}`,
      title: `${title} — ${p.name}`,
      hint: p.name,
      group: "Open",
      keywords,
      effect: { kind: "run", action },
    });
    items.push(open("Terminal", { type: "openTerminal", id: p.id }, "shell console cli"));
    items.push(open("Editor", { type: "openInEditor", id: p.id }, "code ide vscode"));
    items.push(open("Files", { type: "revealInFileManager", id: p.id }, "finder explorer reveal"));
    if (p.appUrl) {
      items.push(open("Browser", { type: "openInBrowser", id: p.id }, `web url ${p.appUrl}`));
    }
    // The REPL execs into the app container, so only a project that has one.
    if (p.isSail && (p.status === "running" || p.status === "degraded")) {
      items.push(open("Tinker", { type: "openTinker", id: p.id }, "artisan repl php console"));
    }
  }

  for (const p of projects) {
    for (const cmd of p.commands ?? []) {
      items.push({
        id: `cmd:${p.id}:${cmd.name}`,
        title: `${cmd.name} — ${p.name}`,
        hint: p.name,
        group: "Commands",
        keywords: `${cmd.command} run ${p.name}`,
        effect: {
          kind: "action",
          action: { type: "runProjectCommand", id: p.id, name: cmd.name },
          project: p.id,
          label: cmd.name,
        },
      });
    }
  }

  for (const w of workspaces) {
    items.push({
      id: `ws:${w.id}`,
      title: w.name,
      group: "Workspaces",
      keywords: "workspace group",
      effect: { kind: "workspace", id: w.id },
    });
    items.push({
      id: `ws-start:${w.id}`,
      title: `Start ${w.name}`,
      hint: w.name,
      group: "Workspaces",
      keywords: "up boot workspace",
      effect: {
        kind: "action",
        action: { type: "startWorkspace", id: w.id },
        project: w.id,
        label: "start workspace",
      },
    });
    items.push({
      id: `ws-stop:${w.id}`,
      title: `Stop ${w.name}`,
      hint: w.name,
      group: "Workspaces",
      keywords: "down halt workspace",
      effect: {
        kind: "action",
        action: { type: "stopWorkspace", id: w.id },
        project: w.id,
        label: "stop workspace",
      },
    });
  }

  items.push(
    {
      id: "nav:home",
      title: "Go home",
      group: "App",
      keywords: "overview fleet dashboard",
      effect: { kind: "home" },
    },
    {
      id: "app:settings",
      title: "Settings",
      group: "App",
      keywords: "preferences config options",
      effect: { kind: "dialog", what: "settings" },
    },
    {
      id: "app:new-project",
      title: "New project",
      group: "App",
      keywords: "create scaffold laravel install",
      effect: { kind: "dialog", what: "newProject" },
    },
    {
      id: "app:new-workspace",
      title: "New workspace",
      group: "App",
      keywords: "create group",
      effect: { kind: "dialog", what: "newWorkspace" },
    },
  );

  return items;
}

/**
 * How well `query` matches `text`, or null for no match.
 *
 * A subsequence match, so "stst" finds "Start storefront", scored so the
 * tighter and earlier match wins: a prefix beats a word boundary beats a
 * letter in the middle of a word. Lower is better, which makes the sort a
 * plain ascending one.
 */
export function matchScore(query: string, text: string): number | null {
  if (!query) return 0;
  const q = query.toLowerCase();
  const t = text.toLowerCase();
  let score = 0;
  let from = 0;
  for (const ch of q) {
    const at = t.indexOf(ch, from);
    if (at === -1) return null;
    // Distance from where we were looking is the cost: consecutive letters
    // are free, a jump across a word is not.
    const gap = at - from;
    const boundary = at === 0 || /[\s\-_/.:]/.test(t[at - 1] ?? "");
    score += gap === 0 ? 0 : boundary ? 1 : 1 + gap;
    from = at + 1;
  }
  // Shorter targets win ties: "Stop app" over "Stop application-server".
  return score + t.length / 1000;
}

/**
 * Entries that match, best first.
 *
 * The title is what the eye reads, so it scores; keywords only qualify an
 * entry, and carry a penalty so a keyword hit never outranks a title hit.
 */
export function filterPalette(items: PaletteItem[], query: string, limit = 40): PaletteItem[] {
  const q = query.trim();
  if (!q) return items.slice(0, limit);
  const scored: { item: PaletteItem; score: number }[] = [];
  for (const item of items) {
    const title = matchScore(q, item.title);
    const keyword = title == null ? matchScore(q, `${item.title} ${item.keywords}`) : null;
    const score = title ?? (keyword == null ? null : keyword + 50);
    if (score != null) scored.push({ item, score });
  }
  scored.sort((a, b) => a.score - b.score);
  return scored.slice(0, limit).map((s) => s.item);
}

/** Entries grouped for display, preserving the ranked order within a group. */
export function groupPalette(items: PaletteItem[]): { group: string; items: PaletteItem[] }[] {
  const groups: { group: string; items: PaletteItem[] }[] = [];
  for (const item of items) {
    const existing = groups.find((g) => g.group === item.group);
    if (existing) existing.items.push(item);
    else groups.push({ group: item.group, items: [item] });
  }
  return groups;
}
