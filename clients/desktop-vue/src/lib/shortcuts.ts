// The app's keyboard shortcuts as data, and the one place that knows how the
// platform writes its modifier — a label that says Ctrl on a Mac teaches the
// wrong key, and hardcoding it per call site is how the labels drift.

/** WKWebView still reports a usable platform string; user agent is the
 * fallback for engines that dropped `navigator.platform`. */
export const isMac = /mac/i.test(
  globalThis.navigator?.platform ?? globalThis.navigator?.userAgent ?? "",
);

/** One key of a combo: `mod` becomes the platform's modifier, everything
 * else names itself. */
export function keyLabel(part: string, mac: boolean = isMac): string {
  return part === "mod" ? (mac ? "⌘" : "Ctrl") : part;
}

/** A whole combo the way the platform writes it: `⌘K` on a Mac, `Ctrl K`
 * elsewhere — matching how each convention spaces its chords. */
export function comboLabel(parts: string[], mac: boolean = isMac): string {
  return parts.map((part) => keyLabel(part, mac)).join(mac && parts[0] === "mod" ? "" : " ");
}

/** Everything the keyboard can do, in the order worth learning it. */
export const SHORTCUTS: { combo: string[]; does: string }[] = [
  { combo: ["mod", "K"], does: "Open the command palette — every action, from anywhere" },
  { combo: ["mod", "1–9"], does: "Jump to the nth project in the sidebar" },
  { combo: ["↑", "↓", "Enter"], does: "Move and choose inside the palette" },
  { combo: ["Esc"], does: "Clear the sidebar filter; close menus and dialogs" },
  { combo: ["?"], does: "Show this list" },
];
