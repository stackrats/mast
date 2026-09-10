import { describe, expect, it } from "vite-plus/test";

import { SCALE_MAX, SCALE_MIN } from "./prefs";
import {
  SCALE_SLIDER_STEP,
  SCALE_STEPS,
  clampScale,
  defaultScale,
  scaleLabel,
  snapScale,
  stepScale,
} from "./scale";

describe("defaultScale", () => {
  // The whole point of detecting the compositor: a window the user did not
  // size is usually smaller than the one the config asks for.
  it("starts a tiling session one step down", () => {
    expect(defaultScale(true)).toBe(0.9);
    expect(defaultScale(false)).toBe(1);
  });

  it("only ever returns a real step", () => {
    for (const tiling of [true, false]) {
      expect(SCALE_STEPS).toContain(defaultScale(tiling));
    }
  });
});

describe("stepScale", () => {
  it("walks the steps in both directions", () => {
    expect(stepScale(1, 1)).toBe(1.1);
    expect(stepScale(1, -1)).toBe(0.9);
    expect(stepScale(0.9, 1)).toBe(1);
    expect(stepScale(1.25, 1)).toBe(1.5);
  });

  // The slider moves in 5% increments; the shortcuts must not, or Ctrl+- has
  // to be pressed fifteen times to make a visible difference.
  it("moves further than the slider's granularity", () => {
    let scale: number = SCALE_STEPS[0];
    while (scale < SCALE_STEPS[SCALE_STEPS.length - 1]) {
      const next = stepScale(scale, 1);
      expect(next - scale).toBeGreaterThanOrEqual(SCALE_SLIDER_STEP - 1e-9);
      scale = next;
    }
  });

  it("stops at the ends rather than running past them", () => {
    expect(stepScale(SCALE_STEPS[SCALE_STEPS.length - 1], 1)).toBe(SCALE_MAX);
    expect(stepScale(SCALE_STEPS[0], -1)).toBe(SCALE_MIN);
    expect(stepScale(99, 1)).toBe(SCALE_MAX);
    expect(stepScale(0.01, -1)).toBe(SCALE_MIN);
  });

  // A stored preference need not be a step: the list can change between
  // releases, and a value from an older build has to keep working.
  it("snaps onto the ladder from a value between steps", () => {
    expect(stepScale(0.95, 1)).toBe(1);
    expect(stepScale(0.95, -1)).toBe(0.9);
    expect(stepScale(1.37, -1)).toBe(1.25);
    expect(stepScale(1.37, 1)).toBe(1.5);
  });

  // Floating-point stepping is where an off-by-epsilon shows up as a control
  // that appears to do nothing: 1.1 stored, "zoom in" returns 1.1 again.
  it("always moves, never returns its own input", () => {
    let scale: number = SCALE_STEPS[0];
    for (let i = 0; i < SCALE_STEPS.length - 1; i += 1) {
      const next = stepScale(scale, 1);
      expect(next).toBeGreaterThan(scale);
      scale = next;
    }
    for (let i = 0; i < SCALE_STEPS.length - 1; i += 1) {
      const next = stepScale(scale, -1);
      expect(next).toBeLessThan(scale);
      scale = next;
    }
  });
});

describe("snapScale", () => {
  it("lands a slider drag on the granularity the readout can print", () => {
    expect(snapScale(0.7333)).toBe(0.75);
    expect(snapScale(1.02)).toBe(1);
    expect(snapScale(1.13)).toBe(1.15);
  });

  // 0.05 * 15 is 0.7500000000000001 in binary floating point. Without the
  // second rounding that escapes into the readout as "75.00000000000001%".
  it("never leaks a floating-point artefact into the readout", () => {
    for (let raw = SCALE_MIN; raw <= SCALE_MAX + 1e-9; raw += SCALE_SLIDER_STEP) {
      const snapped = snapScale(raw);
      expect(scaleLabel(snapped)).toMatch(/^\d+%$/);
      expect(Math.round(snapped * 100) / 100).toBe(snapped);
    }
  });

  it("stays inside the supported range", () => {
    expect(snapScale(0.01)).toBe(SCALE_MIN);
    expect(snapScale(99)).toBe(SCALE_MAX);
  });
});

describe("clampScale", () => {
  it("keeps a scale inside the supported range", () => {
    expect(clampScale(0.1)).toBe(SCALE_MIN);
    expect(clampScale(9)).toBe(SCALE_MAX);
    expect(clampScale(1)).toBe(1);
  });

  // Every offered value must actually do something. WebKit clamps page zoom
  // below 50%, so a range reaching further down would move a number that
  // changes nothing on screen — which reads as a broken setting rather than as
  // the end of the range.
  it("offers no scale the webview will refuse to apply", () => {
    expect(SCALE_MIN).toBeGreaterThanOrEqual(0.5);
    expect(SCALE_MAX).toBeGreaterThanOrEqual(2);
    for (const step of SCALE_STEPS) {
      expect(step).toBeGreaterThanOrEqual(SCALE_MIN);
      expect(step).toBeLessThanOrEqual(SCALE_MAX);
    }
  });

  it("accepts every step it offers", () => {
    for (const step of SCALE_STEPS) expect(clampScale(step)).toBe(step);
  });
});

describe("scaleLabel", () => {
  it("reads as a percentage", () => {
    expect(scaleLabel(1)).toBe("100%");
    expect(scaleLabel(0.9)).toBe("90%");
    expect(scaleLabel(1.25)).toBe("125%");
  });
});
