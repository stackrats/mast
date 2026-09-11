import { describe, expect, it } from "vite-plus/test";

import { SIDEBAR_OVERLAY_BELOW, logsHeight, sidebarMode, sidebarWidth } from "./layout";
import { LOGS_MAX_HEIGHT, LOGS_MIN_HEIGHT, SIDEBAR_MAX_WIDTH, SIDEBAR_MIN_WIDTH } from "./prefs";

// The window sizes a tiling manager actually hands out: a 1920px screen split
// two ways, three ways, and a quarter tile.
const HALF = 960;
const THIRD = 640;
const QUARTER = 480;

describe("sidebarWidth", () => {
  it("leaves a comfortable window alone", () => {
    expect(sidebarWidth(380, 1100)).toBe(380);
    expect(sidebarWidth(SIDEBAR_MIN_WIDTH, 1100)).toBe(SIDEBAR_MIN_WIDTH);
  });

  it("caps the sidebar at two fifths of a tiled window", () => {
    expect(sidebarWidth(SIDEBAR_MAX_WIDTH, HALF)).toBe(384);
    expect(sidebarWidth(SIDEBAR_MAX_WIDTH, THIRD)).toBe(256);
  });

  // The bug in the screenshot: a width dragged at 1100px, rendered untouched
  // in a window Hyprland tiled to 640.
  it("does not let a width dragged on a big window own a small one", () => {
    const dragged = sidebarWidth(420, 1400);
    expect(dragged).toBe(420);
    expect(sidebarWidth(dragged, THIRD)).toBeLessThan(dragged);
    expect(sidebarWidth(dragged, THIRD)).toBeLessThanOrEqual(THIRD * 0.4);
  });

  // The whole point of clamping the rendered value instead of the stored one.
  it("restores the dragged width when the window grows back", () => {
    const stored = 420;
    expect(sidebarWidth(stored, THIRD)).toBe(256);
    expect(sidebarWidth(stored, 1400)).toBe(stored);
  });

  it("never renders narrower than the minimum, however small the window", () => {
    for (const w of [QUARTER, 400, 300, 200, 120, 1, 0]) {
      expect(sidebarWidth(240, w)).toBeGreaterThanOrEqual(SIDEBAR_MIN_WIDTH);
    }
  });

  it("clamps a stored value that is out of range in either direction", () => {
    expect(sidebarWidth(9000, 4000)).toBe(SIDEBAR_MAX_WIDTH);
    expect(sidebarWidth(-50, 1100)).toBe(SIDEBAR_MIN_WIDTH);
    expect(sidebarWidth(0, 1100)).toBe(SIDEBAR_MIN_WIDTH);
  });

  it("leaves some content uncovered once it floats", () => {
    expect(sidebarWidth(SIDEBAR_MAX_WIDTH, 500)).toBeLessThan(500);
    expect(sidebarWidth(SIDEBAR_MAX_WIDTH, 460)).toBeLessThan(460);
  });
});

describe("sidebarMode", () => {
  it("docks the sidebar in any window wide enough for both panes", () => {
    expect(sidebarMode(1100)).toBe("inline");
    expect(sidebarMode(HALF)).toBe("inline");
    expect(sidebarMode(THIRD)).toBe("inline");
    expect(sidebarMode(SIDEBAR_OVERLAY_BELOW)).toBe("inline");
  });

  it("floats it once the content pane would stop being readable", () => {
    expect(sidebarMode(SIDEBAR_OVERLAY_BELOW - 1)).toBe("overlay");
    expect(sidebarMode(QUARTER)).toBe("overlay");
    expect(sidebarMode(320)).toBe("overlay");
  });
});

describe("logsHeight", () => {
  it("leaves a tall window alone", () => {
    expect(logsHeight(224, 720)).toBe(224);
    expect(logsHeight(400, 1080)).toBe(400);
  });

  it("gives the content pane back its room in a short window", () => {
    expect(logsHeight(400, 500)).toBe(340);
    expect(logsHeight(400, 400)).toBe(240);
  });

  // The regression the old drag-time clamp could produce: composed the other
  // way round, a short window drove the ceiling under the floor and won.
  it("never goes below the minimum in a very short window", () => {
    for (const h of [300, 256, 200, 100, 40, 0]) {
      expect(logsHeight(224, h)).toBeGreaterThanOrEqual(LOGS_MIN_HEIGHT);
    }
  });

  it("restores the dragged height when the window grows back", () => {
    const stored = 500;
    expect(logsHeight(stored, 480)).toBe(320);
    expect(logsHeight(stored, 1080)).toBe(stored);
  });

  it("clamps a stored value that is out of range in either direction", () => {
    expect(logsHeight(99_999, 4000)).toBe(LOGS_MAX_HEIGHT);
    expect(logsHeight(-1, 720)).toBe(LOGS_MIN_HEIGHT);
  });
});
