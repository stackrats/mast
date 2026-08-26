import { describe, expect, it } from "vite-plus/test";

import { parseAnsi, stripAnsi } from "./ansi";

/** The line from `vp dev` that started this: bold + dim `note:`, then a
 * bright-blue command name, all with no TTY in sight. */
const VP_NOTE =
  "\x1b[1m\x1b[2mnote:\x1b[0m\x1b[0m You are running \x1b[94m`vp dev`\x1b[39m as a Vite+ built-in command.";

describe("parseAnsi", () => {
  it("keeps the text and drops the sequences", () => {
    expect(stripAnsi(VP_NOTE)).toBe("note: You are running `vp dev` as a Vite+ built-in command.");
  });

  it("styles each run and returns to the inherited style on reset", () => {
    const segments = parseAnsi(VP_NOTE);
    const note = segments.find((s) => s.text === "note:");
    expect(note?.class).toContain("font-bold");
    expect(note?.class).toContain("opacity-70");

    const command = segments.find((s) => s.text === "`vp dev`");
    expect(command?.class).toContain("text-blue-500");

    // `\x1b[39m` is default-foreground, not a full reset — the run after it
    // carries no colour.
    const tail = segments.find((s) => s.text.startsWith(" as a Vite+"));
    expect(tail?.class).toBe("");
  });

  it("treats a bare \x1b[m as a reset", () => {
    const segments = parseAnsi("\x1b[31mred\x1b[mplain");
    expect(segments.map((s) => s.text)).toEqual(["red", "plain"]);
    expect(segments[0].class).toContain("text-red-600");
    expect(segments[1].class).toBe("");
  });

  it("swallows extended-colour selectors whole", () => {
    // Without consuming all five parameters the trailing `1` would be read
    // as bold and leak a style the writer never asked for.
    const segments = parseAnsi("\x1b[38;2;255;0;1mhi");
    expect(segments.map((s) => s.text)).toEqual(["hi"]);
    expect(segments[0].class).toBe("");
    expect(parseAnsi("\x1b[38;5;196mhi")[0].class).toBe("");
  });

  it("drops sequences that are not styling", () => {
    // Cursor moves, erase-line, and an OSC window title.
    expect(stripAnsi("\x1b[2K\r\x1b[1Adone")).toBe("done");
    expect(stripAnsi("\x1b]0;a title\x1b\\after")).toBe("after");
  });

  it("shows only what survived a carriage return, as a terminal would", () => {
    expect(stripAnsi("Resolving... 40%\rResolving... 100%")).toBe("Resolving... 100%");
  });

  it("passes ordinary output through untouched", () => {
    const plain = "  Container kusina-redis-1  Started";
    expect(stripAnsi(plain)).toBe(plain);
    expect(parseAnsi(plain)).toEqual([{ text: plain, class: "" }]);
  });

  it("always yields a segment, so an empty line still occupies a row", () => {
    expect(parseAnsi("")).toEqual([{ text: "", class: "" }]);
    expect(parseAnsi("\x1b[0m")).toEqual([{ text: "", class: "" }]);
  });
});
