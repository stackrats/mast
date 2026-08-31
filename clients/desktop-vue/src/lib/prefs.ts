// Purely-visual preferences: persisted in localStorage (not the engine's
// settings store — they never affect behavior outside this window).

export type Theme = "system" | "light" | "dark";

const THEME_KEY = "mast.theme";
const SIDEBAR_KEY = "mast.sidebarWidth";
const LOGS_HEIGHT_KEY = "mast.logsHeight";
const LOGS_WRAP_KEY = "mast.logsWrap";
const LOGS_OPEN_KEY = "mast.logsOpen";
const CAPTURES_SEEN_KEY = "mast.capturesSeen";

// Guarded like `media` below. The engine store reads a preference while its
// state is being built, and the store's unit tests run without a DOM — with no
// storage, every preference falls back to its default rather than throwing.
const local = typeof localStorage === "undefined" ? null : localStorage;

export function loadTheme(): Theme {
  const raw = local?.getItem(THEME_KEY);
  return raw === "light" || raw === "dark" ? raw : "system";
}

export function saveTheme(theme: Theme): void {
  local?.setItem(THEME_KEY, theme);
}

// Guarded so the module loads in non-browser test environments.
const media =
  typeof window === "undefined" ? null : window.matchMedia("(prefers-color-scheme: dark)");
let current: Theme = "system";

function sync(): void {
  const dark = current === "dark" || (current === "system" && (media?.matches ?? false));
  document.documentElement.classList.toggle("dark", dark);
}

/** Apply and remember a theme; "system" follows the OS live. */
export function applyTheme(theme: Theme): void {
  current = theme;
  sync();
}

media?.addEventListener("change", sync);

// Native-notification categories (M8). Defaults: all on — each is gated on
// the window being hidden anyway, so they only fire when Mast is in the tray.
export interface NotificationPrefs {
  /** A project turned unhealthy/failed, or recovered. */
  health: boolean;
  /** Docker connection lost or restored. */
  docker: boolean;
  /** A lifecycle operation failed. */
  operations: boolean;
}

const NOTIFY_KEY = "mast.notifications";

export function loadNotificationPrefs(): NotificationPrefs {
  try {
    const raw = JSON.parse(local?.getItem(NOTIFY_KEY) ?? "{}") as Partial<NotificationPrefs>;
    return {
      health: raw.health ?? true,
      docker: raw.docker ?? true,
      operations: raw.operations ?? true,
    };
  } catch {
    return { health: true, docker: true, operations: true };
  }
}

export function saveNotificationPrefs(prefs: NotificationPrefs): void {
  local?.setItem(NOTIFY_KEY, JSON.stringify(prefs));
}

// Recently started workspaces (dashboard quick-start), newest first.
const RECENT_WS_KEY = "mast.recentWorkspaces";
const RECENT_WS_CAP = 4;

export function loadRecentWorkspaces(): string[] {
  try {
    const raw = JSON.parse(local?.getItem(RECENT_WS_KEY) ?? "[]");
    return Array.isArray(raw) ? raw.filter((id) => typeof id === "string") : [];
  } catch {
    return [];
  }
}

export function recordWorkspaceStart(id: string): void {
  const next = [id, ...loadRecentWorkspaces().filter((known) => known !== id)];
  local?.setItem(RECENT_WS_KEY, JSON.stringify(next.slice(0, RECENT_WS_CAP)));
}

export function loadSidebarWidth(): number {
  const raw = Number(local?.getItem(SIDEBAR_KEY));
  return Number.isFinite(raw) && raw >= 180 && raw <= 480 ? raw : 240;
}

export function saveSidebarWidth(width: number): void {
  local?.setItem(SIDEBAR_KEY, String(width));
}

// Sidebar rows folded shut, keyed "group:workspaces" / "group:projects" /
// "ws:<id>". Only `true` entries are kept — everything starts open, and a
// removed workspace's stale key costs nothing.
const SIDEBAR_COLLAPSED_KEY = "mast.sidebarCollapsed";

export function loadSidebarCollapsed(): Record<string, boolean> {
  try {
    const raw = JSON.parse(local?.getItem(SIDEBAR_COLLAPSED_KEY) ?? "{}");
    if (raw == null || typeof raw !== "object" || Array.isArray(raw)) return {};
    return Object.fromEntries(
      Object.entries(raw).filter(([, folded]) => folded === true),
    ) as Record<string, boolean>;
  } catch {
    return {};
  }
}

export function saveSidebarCollapsed(collapsed: Record<string, boolean>): void {
  local?.setItem(
    SIDEBAR_COLLAPSED_KEY,
    JSON.stringify(Object.fromEntries(Object.entries(collapsed).filter(([, f]) => f))),
  );
}

export const LOGS_MIN_HEIGHT = 96;
export const LOGS_MAX_HEIGHT = 800;

export function loadLogsHeight(): number {
  const raw = Number(local?.getItem(LOGS_HEIGHT_KEY));
  return Number.isFinite(raw) && raw >= LOGS_MIN_HEIGHT && raw <= LOGS_MAX_HEIGHT ? raw : 224;
}

export function saveLogsHeight(height: number): void {
  local?.setItem(LOGS_HEIGHT_KEY, String(height));
}

/** Off by default: compose output is column-aligned, and wrapping it turns a
 * readable table into a wall. The panel scrolls sideways instead. */
export function loadLogsWrap(): boolean {
  return local?.getItem(LOGS_WRAP_KEY) === "true";
}

export function saveLogsWrap(wrap: boolean): void {
  local?.setItem(LOGS_WRAP_KEY, String(wrap));
}

/** Whether the logs panel is showing. Closed until the user opens it, and
 * nothing but the user opens or closes it after that. */
export function loadLogsOpen(): boolean {
  return local?.getItem(LOGS_OPEN_KEY) === "true";
}

export function saveLogsOpen(open: boolean): void {
  local?.setItem(LOGS_OPEN_KEY, String(open));
}

/** Highest log-capture id the user has looked at. Persisted because captures
 * outlive the process: without it, every restart would re-badge captures the
 * user already read. */
export function loadCapturesSeen(): number {
  const raw = Number(local?.getItem(CAPTURES_SEEN_KEY));
  return Number.isFinite(raw) && raw > 0 ? raw : 0;
}

export function saveCapturesSeen(id: number): void {
  local?.setItem(CAPTURES_SEEN_KEY, String(id));
}
