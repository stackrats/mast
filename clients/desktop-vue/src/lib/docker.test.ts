import { describe, expect, it } from "vite-plus/test";

import type { DockerUnavailable } from "../bindings";
import { dockerAdvice } from "./docker";

const ALL: DockerUnavailable[] = ["notInstalled", "notRunning", "permissionDenied", "unreachable"];

describe("dockerAdvice", () => {
  it("says something different for each way it can fail", () => {
    const titles = new Set(ALL.map((reason) => dockerAdvice(reason).title));
    expect(titles.size).toBe(ALL.length);
  });

  it("always offers a repair, not just a diagnosis", () => {
    for (const reason of ALL) {
      const advice = dockerAdvice(reason);
      expect(advice.title.length).toBeGreaterThan(0);
      expect(advice.fix.length).toBeGreaterThan(0);
    }
  });

  // The regression this whole change exists to prevent: a card that names the
  // HTTP client library rather than the reader's problem.
  it("never blames a library the reader has never heard of", () => {
    for (const reason of ALL) {
      const text = `${dockerAdvice(reason).title} ${dockerAdvice(reason).fix}`.toLowerCase();
      for (const jargon of ["hyper", "bollard", "legacy client", "os error", "econnrefused"]) {
        expect(text).not.toContain(jargon);
      }
    }
  });

  // Each repair has to match its diagnosis. Telling someone whose daemon is
  // stopped to add themselves to a group sends them off for ten minutes.
  it("gives the repair that matches the diagnosis", () => {
    expect(dockerAdvice("notInstalled").fix).toMatch(/install/i);
    expect(dockerAdvice("notRunning").command).toMatch(/start/i);
    expect(dockerAdvice("permissionDenied").command).toMatch(/usermod/i);
    expect(dockerAdvice("unreachable").command).toMatch(/context/i);
  });

  // Backticks in a plain-text node render as backticks. The command is a
  // separate field precisely so the card can mark it up as code.
  it("never smuggles markdown into prose", () => {
    for (const reason of ALL) {
      const advice = dockerAdvice(reason);
      expect(advice.title).not.toContain("`");
      expect(advice.fix).not.toContain("`");
      expect(advice.command ?? "").not.toContain("`");
    }
  });

  // An install command differs per distro and per OS. Guessing one is worse
  // than offering none: it sends the reader to a package manager they do not
  // have.
  it("offers no command where no single command is right", () => {
    expect(dockerAdvice("notInstalled").command).toBeUndefined();
  });

  // An engine older than this client sends no reason at all, and a card with
  // no text is worse than a generic one.
  it("falls back rather than rendering nothing", () => {
    expect(dockerAdvice(null)).toEqual(dockerAdvice("unreachable"));
  });
});
