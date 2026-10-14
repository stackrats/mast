import { describe, expect, it } from "vite-plus/test";

import type { ContainerRun, ContainerRunOwner } from "../bindings";
import {
  needsAttention,
  orderedRuns,
  ownerLabel,
  ownerSentence,
  runAt,
  runFacts,
} from "./containerRuns";

function run(pid: number, owner: ContainerRunOwner, startedAtUnix: number): ContainerRun {
  return {
    pid,
    startTicks: pid * 10,
    owner,
    title: "dev",
    commandLine: "php artisan dev",
    startedAtUnix,
    memoryBytes: 3_200_000_000,
    processCount: 30,
    kinds: ["Horizon", "Vite"],
  };
}

const NOW = 1_000_000_000;

describe("container runs", () => {
  it("labels every owner", () => {
    expect(ownerLabel({ type: "mast", command: "dev" })).toBe("Mast");
    expect(ownerLabel({ type: "earlierMast", command: "dev" })).toBe("earlier Mast session");
    expect(ownerLabel({ type: "outside" })).toBe("outside Mast");
    expect(ownerLabel({ type: "orphaned" })).toBe("orphaned");
  });

  it("explains a leftover in terms of what to do about it", () => {
    const sentence = ownerSentence({ type: "earlierMast", command: "dev" });
    expect(sentence).toContain("no longer tracks it");
    expect(sentence).toContain("Running dev again replaces it");
  });

  it("flags everything Mast is not running", () => {
    expect(needsAttention(run(1, { type: "mast", command: "dev" }, 0))).toBe(false);
    expect(needsAttention(run(1, { type: "outside" }, 0))).toBe(true);
    expect(needsAttention(run(1, { type: "earlierMast", command: "dev" }, 0))).toBe(true);
  });

  it("states count, memory and age", () => {
    const facts = runFacts(run(1, { type: "outside" }, NOW / 1000 - 6 * 3600 - 720), NOW);
    expect(facts).toBe("30 processes · 3 GB · up 6h 12m");
    const lone = { ...run(1, { type: "outside" }, NOW / 1000 - 5), processCount: 1 };
    expect(runFacts(lone, NOW)).toMatch(/^1 process · /);
  });

  it("lists Mast's own first, then the oldest leftovers", () => {
    const runs = [
      run(3, { type: "outside" }, 300),
      run(1, { type: "mast", command: "dev" }, 900),
      run(2, { type: "earlierMast", command: "dev" }, 100),
    ];
    expect(orderedRuns(runs).map((r) => r.pid)).toEqual([1, 2, 3]);
  });

  it("finds an advisory's run by pid and start time, never by pid alone", () => {
    const runs = [run(42, { type: "outside" }, 0)];
    expect(runAt(runs, 42, 420)?.pid).toBe(42);
    expect(runAt(runs, 42, 421)).toBeUndefined();
  });
});
