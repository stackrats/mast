import { describe, expect, it } from "vite-plus/test";

import { comboLabel, keyLabel, SHORTCUTS } from "./shortcuts";

describe("shortcut labels", () => {
  /// The whole point: the same combo reads natively on each platform.
  it("mod is the platform's own modifier", () => {
    expect(comboLabel(["mod", "K"], true)).toBe("⌘K");
    expect(comboLabel(["mod", "K"], false)).toBe("Ctrl K");
    expect(comboLabel(["mod", "1–9"], true)).toBe("⌘1–9");
    expect(comboLabel(["mod", "1–9"], false)).toBe("Ctrl 1–9");
  });

  it("plain keys pass through unspaced by the modifier rule", () => {
    expect(keyLabel("Esc", true)).toBe("Esc");
    expect(comboLabel(["↑", "↓", "Enter"], true)).toBe("↑ ↓ Enter");
    expect(comboLabel(["↑", "↓", "Enter"], false)).toBe("↑ ↓ Enter");
  });

  it("every listed shortcut renders on both platforms", () => {
    for (const shortcut of SHORTCUTS) {
      expect(comboLabel(shortcut.combo, true)).not.toBe("");
      expect(comboLabel(shortcut.combo, false)).not.toBe("");
      expect(shortcut.does.length).toBeGreaterThan(0);
    }
  });
});
