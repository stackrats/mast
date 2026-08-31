// Container paths in app output are project files one prefix away: Sail
// mounts the project at /var/www/html, so `/var/www/html/app/X.php:42` in a
// stack trace IS app/X.php line 42 on this machine — the whole reason a log
// line can carry an "open in editor" affordance at all.

export interface FileLinkPart {
  text: string;
  /** Project-relative path, present only on the linkable parts. */
  file?: string;
  line?: number;
}

/** Where Sail mounts the project inside every container it builds. */
const CONTAINER_ROOT = "/var/www/html/";

// A path needs an extension to count — bare directories in prose ("in
// /var/www/html/storage") open nothing useful. Trace frames write the line
// as `(25)`, exception headers as `:25`; both are the same fact.
const FILE_RE = /\/var\/www\/html\/([A-Za-z0-9_\-./]*?\.[A-Za-z0-9]+)(?:\((\d+)\)|:(\d+))?/g;

/** Split text into plain and linkable parts, in order, losslessly. */
export function splitFileLinks(text: string): FileLinkPart[] {
  const parts: FileLinkPart[] = [];
  let last = 0;
  for (const hit of text.matchAll(FILE_RE)) {
    const at = hit.index;
    if (at > last) parts.push({ text: text.slice(last, at) });
    const line = hit[2] ?? hit[3];
    parts.push({
      text: hit[0],
      file: hit[1],
      line: line ? Number(line) : undefined,
    });
    last = at + hit[0].length;
  }
  if (last < text.length || parts.length === 0) parts.push({ text: text.slice(last) });
  return parts;
}

export { CONTAINER_ROOT };
