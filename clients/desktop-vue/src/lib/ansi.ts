// ANSI escape sequences in streamed output.
//
// Everything Mast shells out to — vite, pnpm, composer, compose, artisan —
// colours its output, and plenty of them keep doing it with no TTY attached.
// Those bytes reach the panel verbatim, so a line arrives looking like
// `\x1b[1m\x1b[2mnote:\x1b[0m` and renders as punctuation. Terminal parity is
// the point of the panel, so the colours are honoured rather than discarded,
// and every other sequence (cursor moves, erases, window titles) is dropped —
// it has no meaning in a scrollback pane and only shows up as noise.
//
// Nothing here produces markup: a segment carries a class name from the fixed
// tables below and its text is interpolated, never injected. Tool output is
// untrusted, and it never gets to name its own styles.

export interface AnsiSegment {
  text: string;
  /** Tailwind classes for this run, or "" for the inherited style. */
  class: string;
}

/* eslint-disable no-control-regex */

/** Any escape sequence: a CSI with its parameters and final byte, an OSC up
 * to BEL or ST, or a lone two-character escape. One expression so a single
 * pass can decide what to honour and what to drop. */
const ESCAPE = /\x1b(?:\[([0-9;:?]*)([@-~])|\][^]*?(?:\x07|\x1b\\)|[@-Z\\-_])/g;

const FOREGROUND: Record<number, string> = {
  30: "text-slate-500",
  31: "text-red-600 dark:text-red-400",
  32: "text-emerald-600 dark:text-emerald-400",
  33: "text-amber-600 dark:text-amber-400",
  34: "text-blue-600 dark:text-blue-400",
  35: "text-fuchsia-600 dark:text-fuchsia-400",
  36: "text-cyan-600 dark:text-cyan-400",
  37: "text-slate-700 dark:text-slate-200",
  90: "text-slate-400 dark:text-slate-500",
  91: "text-red-500 dark:text-red-300",
  92: "text-emerald-500 dark:text-emerald-300",
  93: "text-amber-500 dark:text-amber-300",
  94: "text-blue-500 dark:text-blue-300",
  95: "text-fuchsia-500 dark:text-fuchsia-300",
  96: "text-cyan-500 dark:text-cyan-300",
  97: "text-slate-800 dark:text-slate-100",
};

const BACKGROUND: Record<number, string> = {
  40: "bg-slate-200 dark:bg-slate-700",
  41: "bg-red-200 dark:bg-red-900",
  42: "bg-emerald-200 dark:bg-emerald-900",
  43: "bg-amber-200 dark:bg-amber-900",
  44: "bg-blue-200 dark:bg-blue-900",
  45: "bg-fuchsia-200 dark:bg-fuchsia-900",
  46: "bg-cyan-200 dark:bg-cyan-900",
  47: "bg-slate-100 dark:bg-slate-600",
};

interface Style {
  fg: string;
  bg: string;
  bold: boolean;
  dim: boolean;
  italic: boolean;
  underline: boolean;
}

const CLEAR: Style = { fg: "", bg: "", bold: false, dim: false, italic: false, underline: false };

function classOf(style: Style): string {
  const parts = [style.fg, style.bg];
  if (style.bold) parts.push("font-bold");
  if (style.dim) parts.push("opacity-70");
  if (style.italic) parts.push("italic");
  if (style.underline) parts.push("underline");
  return parts.filter(Boolean).join(" ");
}

/** Fold one SGR sequence's parameters into `style`. Extended-colour selectors
 * (`38;5;n`, `38;2;r;g;b`) are consumed but not rendered: mapping an arbitrary
 * triple onto the theme's palette gives colours that fail against one
 * background or the other, and a wrong colour is worse than the inherited one.
 * They must still be swallowed whole, or their trailing numbers would be read
 * as further attributes. */
function applySgr(style: Style, params: string): Style {
  // A bare `\x1b[m` means `\x1b[0m`.
  const codes = (params === "" ? "0" : params).split(";").map((p) => Number.parseInt(p, 10) || 0);
  let next = { ...style };
  for (let i = 0; i < codes.length; i++) {
    const code = codes[i];
    if (code === 38 || code === 48) {
      const target = code === 38 ? "fg" : "bg";
      if (codes[i + 1] === 5) {
        next[target] = "";
        i += 2;
      } else if (codes[i + 1] === 2) {
        next[target] = "";
        i += 4;
      }
      continue;
    }
    if (code === 0) next = { ...CLEAR };
    else if (code === 1) next.bold = true;
    else if (code === 2) next.dim = true;
    else if (code === 3) next.italic = true;
    else if (code === 4) next.underline = true;
    else if (code === 22) {
      next.bold = false;
      next.dim = false;
    } else if (code === 23) next.italic = false;
    else if (code === 24) next.underline = false;
    else if (code === 39) next.fg = "";
    else if (code === 49) next.bg = "";
    else if (code in FOREGROUND) next.fg = FOREGROUND[code];
    else if (code in BACKGROUND) next.bg = BACKGROUND[code];
  }
  return next;
}

/** A carriage return means the writer rewound and painted over what it had
 * already drawn — progress bars and spinners are built out of it. A terminal
 * shows only what survived the last pass, so the pane does too. */
function lastPaintedFrame(line: string): string {
  const at = line.lastIndexOf("\r");
  return at === -1 ? line : line.slice(at + 1);
}

/** Split a line into styled runs. Always returns at least one segment, so an
 * empty line still occupies a row. */
export function parseAnsi(line: string): AnsiSegment[] {
  const source = lastPaintedFrame(line);
  const segments: AnsiSegment[] = [];
  let style = CLEAR;
  let at = 0;
  ESCAPE.lastIndex = 0;
  for (let m = ESCAPE.exec(source); m !== null; m = ESCAPE.exec(source)) {
    if (m.index > at) segments.push({ text: source.slice(at, m.index), class: classOf(style) });
    // `m[2]` is a CSI final byte, and only `m` — SGR — carries style. Anything
    // else matched here is dropped, which is the whole point of matching it.
    if (m[2] === "m") style = applySgr(style, m[1] ?? "");
    at = m.index + m[0].length;
  }
  if (at < source.length) segments.push({ text: source.slice(at), class: classOf(style) });
  return segments.length > 0 ? segments : [{ text: "", class: "" }];
}

/** The same line with every sequence removed and no styling — for the places
 * that show output as one plain string (a one-line progress summary, a
 * tooltip) rather than as a row of spans. */
export function stripAnsi(line: string): string {
  return parseAnsi(line)
    .map((segment) => segment.text)
    .join("");
}
