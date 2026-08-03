import { describe, expect, it } from "vite-plus/test";

import { findCycle } from "./graph";

describe("findCycle", () => {
  it("accepts acyclic graphs", () => {
    expect(
      findCycle([
        { id: "a", dependsOn: [] },
        { id: "b", dependsOn: ["a"] },
        { id: "c", dependsOn: ["b", "a"] },
      ]),
    ).toBeNull();
  });

  it("reports the nodes stuck in a cycle", () => {
    const cycle = findCycle([
      { id: "a", dependsOn: ["b"] },
      { id: "b", dependsOn: ["a"] },
      { id: "c", dependsOn: [] },
    ]);
    expect(cycle?.sort()).toEqual(["a", "b"]);
  });

  it("ignores self and external deps (mirrors the engine)", () => {
    expect(
      findCycle([
        { id: "a", dependsOn: ["a", "outside"] },
        { id: "b", dependsOn: ["a"] },
      ]),
    ).toBeNull();
  });
});
