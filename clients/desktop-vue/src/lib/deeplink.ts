// mast:// links: what the OS hands the app when someone clicks "Open in
// Mast" somewhere else. One rule outranks every feature here: a link may
// only NAVIGATE or PREFILL, never act — any webpage can fire a registered
// scheme, so a link that started or cloned things by itself would be
// drive-by control of the machine. The person still clicks the button.

export type DeepLink =
  /** `mast://clone?url=<encoded git url>` — open the add-project dialog in
   * From Git mode with the URL filled in. */
  | { kind: "clone"; url: string }
  /** `mast://project/<name or path suffix>` — select that project. */
  | { kind: "project"; ref: string };

function splitOnce(text: string, separator: string): [string, string] {
  const at = text.indexOf(separator);
  return at < 0 ? [text, ""] : [text.slice(0, at), text.slice(at + separator.length)];
}

/** Parse one raw link, or null for anything unrecognized — unknown actions
 * from a future Mast (or a hostile page) are ignored, never guessed at. */
export function parseDeepLink(raw: string): DeepLink | null {
  if (!raw.startsWith("mast://")) return null;
  const [head, query] = splitOnce(raw.slice("mast://".length), "?");
  const segments = head.split("/").filter(Boolean);
  const action = segments[0];
  if (action === "clone") {
    const url = new URLSearchParams(query).get("url")?.trim();
    return url ? { kind: "clone", url } : null;
  }
  if (action === "project" && segments.length > 1) {
    try {
      return { kind: "project", ref: decodeURIComponent(segments.slice(1).join("/")) };
    } catch {
      return null; // malformed percent-encoding
    }
  }
  return null;
}
