// Local HTTPS domains, in the terms the HTTPS dialog needs: what a name may
// look like, what to suggest, and when two names cannot share a session.
// The engine validates again and is the authority; this keeps the dialog
// from suggesting what it would refuse.

import type { ProjectCommand, ProjectSummary } from "../bindings";

const LABEL = "[a-z0-9]([a-z0-9-]*[a-z0-9])?";
const DOMAIN = new RegExp(`^${LABEL}(\\.${LABEL})*\\.(test|localhost)$`);

/** The shapes the engine accepts: lowercase labels ending in .test or
 * .localhost, the two names that can never collide with public DNS. */
export function isLocalDomain(domain: string): boolean {
  return DOMAIN.test(domain.trim().toLowerCase());
}

/** The registrable part — the last two labels. Neither .test nor .localhost
 * is a listed public suffix, so browsers treat the TLD itself as one, and
 * `api.shop.test` and `web.shop.test` are the same site. */
export function siteOf(domain: string): string {
  const labels = domain.split(".");
  return labels.length <= 2 ? domain : labels.slice(-2).join(".");
}

/** Top-level names people give dev hosts, dropped when a name becomes a
 * .test domain: `api.shop.local` → `api.shop.test`. */
const DEV_TLDS = new Set(["local", "localhost", "test", "dev", "lan", "internal", "home"]);

function slug(text: string): string {
  return (
    text
      .toLowerCase()
      .replace(/[^a-z0-9-]+/g, "-")
      .replace(/^-+|-+$/g, "") || "app"
  );
}

/** A .test domain from a project or folder name. A name that is already a
 * dev host keeps its labels, so siblings stay siblings: `api.shop.local` and
 * `web.shop.local` become `api.shop.test` and `web.shop.test`. */
export function domainFromName(name: string): string {
  const labels = name.toLowerCase().split(".").filter(Boolean);
  if (labels.length >= 2 && DEV_TLDS.has(labels[labels.length - 1])) {
    return `${labels.slice(0, -1).map(slug).join(".")}.test`;
  }
  return `${slug(name)}.test`;
}

/** Runs on this machine — the only kind of command a domain can serve. A
 * `sail …` command runs in the app container, behind the app's domain. */
export function isHostCommand(command: ProjectCommand): boolean {
  return command.command.trim().split(/\s+/)[0] !== "sail";
}

/** The folder a command runs in, by its last path segment, when that is
 * not the project itself: `../suite.shop.local` → `suite.shop.local`. */
function folderOf(command: ProjectCommand): string | null {
  const segment = command.cwd
    ?.split(/[\\/]/)
    .filter((s) => s && s !== "." && s !== "..")
    .pop();
  return segment ?? null;
}

/** A domain for a command, made to share a site with the app's so cookies
 * cross: a sibling when the app has a parent (`api.shop.test` →
 * `suite.shop.test`), the command's own dev host when its folder is one, and
 * otherwise a subdomain of the app's (`shop.test` → `web.shop.test`). */
export function suggestCommandDomain(project: ProjectSummary, command: ProjectCommand): string {
  const app = project.localDomain ?? domainFromName(project.name);
  const folder = folderOf(command);
  const fromFolder = folder ? domainFromName(folder) : null;
  const label = (fromFolder ?? domainFromName(command.name)).split(".")[0];
  if (app.split(".").length >= 3) return `${label}.${siteOf(app)}`;
  if (fromFolder && fromFolder.split(".").length >= 3) return fromFolder;
  return `${label}.${app}`;
}

/** What to do when a command's domain is on another site than the app's —
 * cookies the API sets would not reach the frontend. Two ways to line them
 * up: move the command under the app's site, or rename the app to a sibling
 * of the command (`api-shop.test` → `api.shop.test`) when the app's name
 * makes that obvious. */
export interface SiteFix {
  commandDomain: string;
  appDomain: string | null;
}

export function siteFix(app: string | null, commandDomain: string): SiteFix | null {
  if (!app || !isLocalDomain(commandDomain) || siteOf(app) === siteOf(commandDomain)) {
    return null;
  }
  const label = commandDomain.split(".")[0];
  const site = siteOf(commandDomain);
  const base = site.split(".")[0];
  const appLabel = app.split(".")[0];
  // `api-shop` beside a `shop.test` frontend is the API of `shop`.
  const stripped = appLabel
    .replace(new RegExp(`(^|-)${base}(-|$)`), (_, before: string, after: string) =>
      before && after ? "-" : "",
    )
    .replace(/^-+|-+$/g, "");
  return {
    commandDomain: `${label}.${siteOf(app)}`,
    appDomain: stripped && stripped !== appLabel ? `${stripped}.${site}` : null,
  };
}
