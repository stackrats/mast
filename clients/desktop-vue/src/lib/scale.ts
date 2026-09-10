// The UI scale, as arithmetic — kept apart from the webview call that applies
// it so the rules can be tested without a webview.
//
// Scale is a webview zoom factor rather than a root font size. Half the app's
// sizes are in rem and would follow a font-size change, but the other half are
// the deliberate pixel values that make the chrome line up — the 16.85px
// wordmark, the 3px banner rows, the 1.5px resize handle. Zoom takes all of
// them together, which is the only way the two halves stay in register.

import { SCALE_MAX, SCALE_MIN } from "./prefs";

/** The rungs the keyboard shortcuts climb. The slider moves continuously; the
 * shortcuts do not, because a keystroke that changes the size by 5% has to be
 * pressed fifteen times to do anything you would notice. These are the
 * familiar browser-zoom stops. */
export const SCALE_STEPS = [0.5, 0.67, 0.75, 0.8, 0.9, 1, 1.1, 1.25, 1.5, 1.75, 2] as const;

/** The slider's granularity. Fine enough to feel continuous, coarse enough
 * that the readout is a round number and the same drag lands on the same value
 * twice. */
export const SCALE_SLIDER_STEP = 0.05;

/** Snap an arbitrary value (a slider drag) onto that granularity, so a stored
 * scale is always something the readout can print exactly. */
export function snapScale(scale: number): number {
  const snapped = Math.round(scale / SCALE_SLIDER_STEP) * SCALE_SLIDER_STEP;
  // Multiply-then-round rather than trusting the float: 0.05 * 15 is
  // 0.7500000000000001, and a scale that prints as "75.00000000000001%" is a
  // rounding artefact escaping into the interface.
  return clampScale(Math.round(snapped * 100) / 100);
}

/** What a session gets before the user has expressed a preference.
 *
 * A tiling compositor sizes the window itself, and usually a good deal smaller
 * than the config asks for — 75% rather than one step down, measured against a
 * real Hyprland workspace rather than guessed at. Still only a starting point:
 * a stored preference wins outright. */
export function defaultScale(tiling: boolean): number {
  return tiling ? 0.75 : 1;
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
