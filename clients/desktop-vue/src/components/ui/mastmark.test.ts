import { describe, expect, it } from "vite-plus/test";

import markSource from "./MastMark.vue?raw";
import wordmarkSource from "./MastWordmark.vue?raw";

// The mark and the wordmark are two halves of one banner drawn on one grid.
// The menubar aligns them with `items-start`, which only works while both
// viewBoxes describe the same number of grid rows — the art's last row is
// rules rather than blocks, so both fill 365 of 6x65 units rather than 390.
// Regenerate one half without the other and they drift by 25 units, which at
// 17px reads as the wordmark sitting slightly low.
/** The markup alone. These assertions are about what gets rendered, and the
 * comments above it discuss the very attributes being forbidden — checking the
 * whole file failed on its own explanation of why the attribute is absent. */
function markup(source: string): string {
  const match = /<template>([\s\S]*)<\/template>/.exec(source);
  if (!match) throw new Error("no template found");
  return match[1];
}

function viewBox(source: string): { width: number; height: number } {
  const match = /viewBox="0 0 ([\d.]+) ([\d.]+)"/.exec(source);
  if (!match) throw new Error("no viewBox found");
  return { width: Number(match[1]), height: Number(match[2]) };
}

describe("the two halves of the banner", () => {
  it("describe the same grid height", () => {
    expect(viewBox(wordmarkSource).height).toBe(viewBox(markSource).height);
  });

  it("fill 365 of the grid's six 65-unit rows", () => {
    expect(viewBox(markSource).height).toBe(365);
  });

  // Vectors, not glyphs. At `text-[3px]` a 5-unit rule inside a 65-unit cell
  // has to survive being scaled to three pixels, and whether it does depends
  // on how that size rounds — so the wordmark appeared, broke and vanished at
  // different zoom levels, and was mush at 100%.
  // Crisp-snapping rounds a sub-pixel rect to zero device pixels or one
  // depending on where it lands, which is precisely the failure vectors were
  // meant to end: the shadow rules are 0.23px at the size the menubar uses.
  it("leave sub-pixel rules to antialiasing rather than snapping them away", () => {
    for (const source of [markSource, wordmarkSource]) {
      expect(markup(source)).not.toContain("crispEdges");
    }
  });

  it("are drawn as shapes rather than text", () => {
    for (const source of [markSource, wordmarkSource]) {
      expect(markup(source)).not.toContain("<text");
      expect(markup(source)).not.toContain("font-size");
      expect(markup(source)).toMatch(/<(path|rect)\b/);
    }
  });
});
