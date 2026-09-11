// How the shell's two resizable panels behave when the window is not the size
// they were dragged at.
//
// Both panels store an absolute pixel size, which is the right model for a
// floating window manager: the user picks the window size once, so clamping
// while the handle is being dragged is the only clamp that ever has to run. A
// tiling window manager picks the size instead — and re-picks it every time
// another window opens — so the stored size has to be measured against the
// live window rather than trusted.
//
// Nothing here writes the stored preference back. A sidebar squeezed to 208px
// in a tiled window has to come back at 380px when the window is given room
// again, and it only can if the number on disk is left alone. That is the
// whole reason these are pure functions of (stored, viewport) instead of
// something that edits `width` in place.

import { LOGS_MAX_HEIGHT, LOGS_MIN_HEIGHT, SIDEBAR_MAX_WIDTH, SIDEBAR_MIN_WIDTH } from "./prefs";

/** The most of the window the sidebar may take while it shares the row with
 * the content pane. Two fifths leaves a 900px window 540px of content — still
 * wider than the fleet table's 34rem — and stops a sidebar dragged wide on a
 * big monitor from owning a small one. */
const SIDEBAR_MAX_SHARE = 0.4;

/** Under this width the window cannot hold a sidebar and a readable content
 * pane at once: 180px of sidebar at its minimum, plus roughly 320px before
 * cards start wrapping mid-word. Below it the sidebar stops taking space from
 * the content and floats over it instead. */
export const SIDEBAR_OVERLAY_BELOW = 520;

/** Content left uncovered beside a floating sidebar, so it reads as laid over
 * the pane rather than as having replaced it. */
const OVERLAY_PEEK = 56;

/** Chrome that stays on screen however short the window gets: the menubar,
 * the status strip, and enough of the content pane to prove it is still
 * there. */
const LOGS_CHROME = 160;

export type SidebarMode = "inline" | "overlay";

export function sidebarMode(viewportWidth: number): SidebarMode {
  return viewportWidth < SIDEBAR_OVERLAY_BELOW ? "overlay" : "inline";
}

/**
 * The width to render the sidebar at right now, given what the user last
 * dragged it to and how wide the window currently is.
 *
 * The floor wins over the ceiling on purpose. In a window too narrow to give
 * the sidebar even its minimum, rendering it at the minimum and letting the
 * pane beside it scroll is legible; honouring the ceiling would collapse it
 * toward zero and leave a stripe of unreadable half-glyphs.
 */
export function sidebarWidth(stored: number, viewportWidth: number): number {
  const wanted = Math.min(Math.max(stored, SIDEBAR_MIN_WIDTH), SIDEBAR_MAX_WIDTH);
  const ceiling =
    sidebarMode(viewportWidth) === "overlay"
      ? viewportWidth - OVERLAY_PEEK
      : Math.floor(viewportWidth * SIDEBAR_MAX_SHARE);
  return Math.max(SIDEBAR_MIN_WIDTH, Math.min(wanted, ceiling));
}

/**
 * The height to render the logs panel at, same bargain as the sidebar.
 *
 * The drag handler this replaces composed its clamps the other way round —
 * `min(ceiling, max(MIN, …))` — so in a window shorter than 256px the ceiling
 * went below the minimum and won, and the panel could be handed a height of
 * zero or less. A tiling manager will happily give you a window that short.
 */
export function logsHeight(stored: number, viewportHeight: number): number {
  const wanted = Math.min(Math.max(stored, LOGS_MIN_HEIGHT), LOGS_MAX_HEIGHT);
  const ceiling = Math.min(LOGS_MAX_HEIGHT, viewportHeight - LOGS_CHROME);
  return Math.max(LOGS_MIN_HEIGHT, Math.min(wanted, ceiling));
}
