// The UI scale, as arithmetic — kept apart from the webview call that applies
// it so the rules can be tested without a webview.
//
// Scale is a webview zoom factor rather than a root font size. Half the app's
// sizes are in rem and would follow a font-size change, but the other half are
// the deliberate pixel values that make the chrome line up — the 16.85px
// wordmark, the 3px banner rows, the 1.5px resize handle. Zoom takes all of
// them together, which is the only way the two halves stay in register.

import { SCALE_MAX, SCALE_MIN } from "./prefs";

/** The steps the zoom shortcuts walk through. Not a free-running multiplier:
 * arbitrary factors land on fractional pixels, and a 1px border that rounds to
 * nothing is how a panel loses its edge at one zoom level and gets it back at
 * the next. */
export const SCALE_STEPS = [0.7, 0.8, 0.9, 1, 1.1, 1.25, 1.5] as const;

/** What a session gets before the user has expressed a preference. A tiling
 * compositor sizes the window itself, and usually smaller than the config asks
 * for, so it starts one step down. */
export function defaultScale(tiling: boolean): number {
  return tiling ? 0.9 : 1;
}

export function clampScale(scale: number): number {
  return Math.min(Math.max(scale, SCALE_MIN), SCALE_MAX);
}

/** The next step up or down from wherever the scale currently sits, including
 * from a value that is not itself a step — a stored preference from an older
 * build, or a step list that changed under it. */
export function stepScale(current: number, direction: 1 | -1): number {
  const steps = SCALE_STEPS;
  if (direction === 1) {
    return steps.find((step) => step > current + 1e-9) ?? steps[steps.length - 1];
  }
  return [...steps].reverse().find((step) => step < current - 1e-9) ?? steps[0];
}

/** For the readout: "100%", "90%". */
export function scaleLabel(scale: number): string {
  return `${Math.round(scale * 100)}%`;
}
