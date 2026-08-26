<script setup lang="ts">
import { computed, ref, watch } from "vue";
import {
  Camera,
  ChartColumn,
  Check,
  ChevronDown,
  Copy,
  Cpu,
  Database,
  FileCog,
  Globe,
  Hammer,
  Loader2,
  Lock,
  Plus,
  FolderOpen,
  GitBranch,
  MemoryStick,
  Pencil,
  Play,
  Puzzle,
  RotateCw,
  ScrollText,
  Share2,
  Square,
  SquareTerminal,
  Stethoscope,
  Trash2,
  TriangleAlert,
  Wrench,
  X,
} from "lucide-vue-next";
import { useQRCode } from "@vueuse/integrations/useQRCode";
import {
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuPortal,
  DropdownMenuRoot,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "reka-ui";

import type {
  LaravelLogReport,
  PhpRuntimeReport,
  ProjectCommand,
  ProjectSummary,
  ProxyCa,
  ServiceState,
} from "../bindings";
import { iconButtonClass, menuContentClass, menuItemClass, menuSeparatorClass } from "../lib/menu";
import { formatBytes, formatCores, rollupByProject, series } from "../lib/usage";
import { statusBadgeVariant } from "../lib/status";
import { stripAnsi } from "../lib/ansi";
import { formatElapsed, useElapsed } from "../lib/elapsed";
import { envReport, laravelLog, phpRuntime, proxyCa } from "../lib/transport";
import { commandKey, shareKey, useEngineStore, domainKey } from "../stores/engine";
import CatalogDialog from "./CatalogDialog.vue";
import EnvPanel from "./EnvPanel.vue";
import Badge from "./ui/Badge.vue";
import Button from "./ui/Button.vue";
import Checkbox from "./ui/Checkbox.vue";
import Chip from "./ui/Chip.vue";
import FixButton from "./ui/FixButton.vue";
import Hint from "./ui/Hint.vue";
import Sparkline from "./ui/Sparkline.vue";
import Tooltip from "./ui/Tooltip.vue";
import Input from "./ui/Input.vue";
import Modal from "./ui/Modal.vue";
import Select from "./ui/Select.vue";

const { project } = defineProps<{ project: ProjectSummary }>();
const emit = defineEmits<{ diagnose: [] }>();
const store = useEngineStore();

const op = computed(() => store.operations[project.id]);
const processes = computed(() => project.processes ?? []);
const opRunning = computed(() => op.value != null && op.value.terminal === null);

/** Nothing that changes this project may be touched while an operation is in
 * flight. Two reasons, and the second is the sharp one: the store keys one
 * operation per project, so a second dispatch silently supersedes the first
 * in the UI while its docker command keeps running unwatched — and compose
 * itself will not thank you for a `stop` landing mid-`up`. Folded into the
 * same guard as read-only so every control asks the question once. */
const locked = computed(() => store.readOnly || opRunning.value);

const elapsed = useElapsed(
  computed(() => (opRunning.value ? (op.value?.startedAt ?? null) : null)),
);

/** The operation's most recent word, for the card's one-line progress. A
 * build spends minutes inside a single step; without this the card says
 * "running…" for twenty minutes and the only way to tell a working build
 * from a wedged one is to open the panel. */
const lastLine = computed(() => {
  const lines = op.value?.lines;
  if (!lines) return null;
  for (let i = lines.length - 1; i >= 0; i--) {
    const line = stripAnsi(lines[i].line).trim();
    if (line) return line;
  }
  return null;
});

/** This project's share of the machine, or null when nothing of it is running
 * — which the template shows as a dash rather than a confident zero. */
const usage = computed(() => {
  const sample = store.latestUsage;
  if (!sample || !sample.services.some((s) => s.project === project.id)) return null;
  return rollupByProject(sample, project.id);
});
const hostCores = computed(() => store.latestUsage?.hostCores ?? 0);
/** Scaling a per-project strip to the whole machine flattens it — a typical
 * project sits well under a core, and every bar lands on the minimum height.
 * A quarter-core floor keeps small numbers looking small while leaving the
 * shape readable. */
const SPARKLINE_FLOOR_CORES = 0.25;
const cpuHistory = computed(() =>
  series(store.usage, (sample) =>
    sample.services.filter((s) => s.project === project.id).reduce((sum, s) => sum + s.cpuCores, 0),
  ),
);
const showEnv = ref(false);

function serviceDot(service: ServiceState): string {
  if (service.health === "unhealthy") return "bg-red-500";
  if (service.state === "running")
    return service.health === "starting" ? "bg-amber-400" : "bg-emerald-500";
  if (service.state === "restarting" || service.state === "paused") return "bg-amber-400";
  if (service.state == null) return "bg-slate-300";
  return "bg-slate-400";
}

function lifecycle(
  label: string,
  type: "startProject" | "stopProject" | "restartProject" | "rebuildProject",
) {
  void store.runLifecycle(project.id, label, { type, id: project.id });
}

function serviceVerb(
  service: string,
  label: string,
  type: "startService" | "stopService" | "restartService" | "rebuildService",
) {
  void store.runLifecycle(project.id, `${label} ${service}`, {
    type,
    id: project.id,
    service,
  });
}

// Process output streams into the same op panel as lifecycle verbs — for a
// dev server that panel doubles as its live log.
function processVerb(process: string, title: string, type: "startProcess" | "stopProcess") {
  void store.runLifecycle(project.id, `${type === "startProcess" ? "start" : "stop"} ${title}`, {
    type,
    id: project.id,
    process,
  });
}

const rowLabelClass = "text-[11px] font-medium tracking-wide text-slate-400";

const processHints: Record<string, string> = {
  reverb: "Laravel's WebSocket server (php artisan reverb:start).",
  horizon: "Queue supervisor with a dashboard (php artisan horizon).",
  queue: "Works queued jobs (php artisan queue:work).",
  schedule: "Runs scheduled tasks every minute (php artisan schedule:work).",
};

// --- sail share: the tunnel as a streamed op, URL + QR once expose reports
// it, and the tunnel's own output in view — the debuggability sail lacks ---
const shareOpen = ref(false);
const shareOp = computed(() => store.operations[shareKey(project.id)]);
const sharing = computed(() => shareOp.value != null && shareOp.value.terminal === null);
const shareQr = useQRCode(computed(() => project.shareUrl ?? ""));
const shareCopied = ref(false);

/** The effective SAIL_SHARE_* settings (sail's own defaults filled in), so
 * the dialog says exactly what would be shared BEFORE anything starts. */
const shareSettings = ref<
  { label: string; value: string; key: string; isDefault: boolean }[] | null
>(null);
watch(shareOpen, async (open) => {
  if (!open) return;
  shareSettings.value = null;
  try {
    const report = await envReport(project.id);
    const env = (key: string) => report.entries.find((e) => e.key === key)?.value?.trim() || null;
    // Every knob the sail script reads, with its default — set ones stand
    // out, defaults stay dim, so "what exactly am I sharing" has one answer.
    const row = (label: string, key: string, set: string | null, fallback: string) => ({
      label,
      key,
      value: set ?? fallback,
      isDefault: set == null,
    });
    const serverHost = env("SAIL_SHARE_SERVER_HOST");
    const token = env("SAIL_SHARE_TOKEN");
    shareSettings.value = [
      row(
        "Forwards",
        "APP_PORT",
        env("APP_PORT") && `localhost:${env("APP_PORT")}`,
        "localhost:80",
      ),
      row("Subdomain", "SAIL_SHARE_SUBDOMAIN", env("SAIL_SHARE_SUBDOMAIN"), "(random)"),
      row("Server host", "SAIL_SHARE_SERVER_HOST", serverHost, "laravel-sail.site"),
      row("Server port", "SAIL_SHARE_SERVER_PORT", env("SAIL_SHARE_SERVER_PORT"), "8080"),
      row(
        "Domain",
        "SAIL_SHARE_DOMAIN",
        env("SAIL_SHARE_DOMAIN"),
        serverHost ?? "laravel-sail.site",
      ),
      row("Server", "SAIL_SHARE_SERVER", env("SAIL_SHARE_SERVER"), "(relay default)"),
      row("Auth token", "SAIL_SHARE_TOKEN", token && "•••", "(none)"),
      row(
        "Dashboard",
        "SAIL_SHARE_DASHBOARD",
        env("SAIL_SHARE_DASHBOARD") && `localhost:${env("SAIL_SHARE_DASHBOARD")}`,
        "localhost:4040",
      ),
    ];
  } catch {
    shareSettings.value = null; // no .env yet — the op will say so
  }
});
function startShare() {
  void store.runLifecycle(shareKey(project.id), "share", {
    type: "shareProject",
    id: project.id,
  });
}
function stopShare() {
  void store.cancelLifecycle(shareKey(project.id));
}
// --- local HTTPS domain: one shared Caddy proxy serves every claimed
// .test address with a locally-trusted certificate; the operation's own
// output and follow-up Fix buttons live in this dialog ---
const httpsOpen = ref(false);
const domainOp = computed(() => store.operations[domainKey(project.id)]);
const domainBusy = computed(() => domainOp.value != null && domainOp.value.terminal === null);
const domainDraft = ref("");
watch(httpsOpen, (open) => {
  if (!open) return;
  const slug =
    project.name
      .toLowerCase()
      .replace(/[^a-z0-9-]+/g, "-")
      .replace(/^-+|-+$/g, "") || "app";
  domainDraft.value = project.localDomain ?? `${slug}.test`;
});
const domainValid = computed(() =>
  /^[a-z0-9]([a-z0-9-]*[a-z0-9])?(\.[a-z0-9]([a-z0-9-]*[a-z0-9])?)*\.(test|localhost)$/.test(
    domainDraft.value.trim().toLowerCase(),
  ),
);
function setDomain(domain: string | null) {
  void store.runLifecycle(
    domainKey(project.id),
    domain ? `Enable https://${domain}` : "Disable local domain",
    { type: "setLocalDomain", id: project.id, domain },
  );
}

/** The proxy CA's root certificate, for the trust that Fix buttons cannot
 * reach: Firefox's import dialog wants the file, curl --cacert and
 * NODE_EXTRA_CA_CERTS want the path, other tools want the PEM itself. */
const caFile = ref<ProxyCa | null>(null);
const manualCopied = ref<"path" | "pem" | "hosts" | null>(null);
async function loadProxyCa() {
  try {
    caFile.value = await proxyCa();
  } catch {
    caFile.value = null;
  }
}
watch(
  () => [httpsOpen.value, domainOp.value?.terminal] as const,
  ([open]) => {
    if (open) void loadProxyCa();
  },
);
/** The exact line the add-hosts-entry repair would append — shown so the
 * manual route needs no guesswork. */
const hostsLine = computed(() =>
  project.localDomain ? `127.0.0.1\t${project.localDomain}` : null,
);
async function copyManual(kind: "path" | "pem" | "hosts") {
  const value =
    kind === "hosts" ? hostsLine.value : kind === "path" ? caFile.value?.path : caFile.value?.pem;
  if (!value) return;
  await navigator.clipboard.writeText(value);
  manualCopied.value = kind;
  setTimeout(() => (manualCopied.value = null), 1500);
}

// --- laravel.log viewer: the app's own log, parsed — an error and its
// stack trace read as one entry, not two hundred raw lines ---
const appLogOpen = ref(false);
const appLog = ref<LaravelLogReport | null>(null);
const appLogError = ref<string | null>(null);
const appLogFilter = ref<"all" | "warnings" | "errors">("all");
const appLogFilters = [
  { id: "all", label: "All" },
  { id: "warnings", label: "Warnings+" },
  { id: "errors", label: "Errors" },
] as const;
const appLogExpanded = ref<Set<number>>(new Set());
async function loadAppLog() {
  appLogError.value = null;
  try {
    appLog.value = await laravelLog(project.id);
  } catch (e) {
    appLog.value = null;
    appLogError.value = String(e);
  }
}
watch(appLogOpen, (open) => {
  if (!open) return;
  appLog.value = null;
  appLogExpanded.value = new Set();
  void loadAppLog();
});
const ERROR_LEVELS = ["ERROR", "CRITICAL", "ALERT", "EMERGENCY"];
const appLogEntries = computed(() => {
  const entries = appLog.value?.entries ?? [];
  if (appLogFilter.value === "errors") {
    return entries.filter((e) => ERROR_LEVELS.includes(e.level));
  }
  if (appLogFilter.value === "warnings") {
    return entries.filter((e) => ERROR_LEVELS.includes(e.level) || e.level === "WARNING");
  }
  return entries;
});
function levelBadge(level: string): string {
  if (ERROR_LEVELS.includes(level)) {
    return "bg-red-100 text-red-700 dark:bg-red-950 dark:text-red-300";
  }
  if (level === "WARNING" || level === "NOTICE") {
    return "bg-amber-100 text-amber-700 dark:bg-amber-950 dark:text-amber-300";
  }
  return "bg-slate-100 text-slate-600 dark:bg-slate-800 dark:text-slate-300";
}
function toggleAppLogRow(i: number) {
  const next = new Set(appLogExpanded.value);
  if (next.has(i)) next.delete(i);
  else next.add(i);
  appLogExpanded.value = next;
}

// --- DB connection card: host/port from the resolved model, credentials
// from .env — everything a GUI client asks for, without grepping compose
// files. Fetched on open so it is never stale. ---
const connService = ref<ServiceState | null>(null);
const connOpen = computed({
  get: () => connService.value != null,
  set: (open: boolean) => {
    if (!open) connService.value = null;
  },
});
const connRows = ref<{ label: string; value: string; mask?: boolean }[] | null>(null);
const connUri = ref<string | null>(null);
const connCopied = ref<string | null>(null);
watch(connService, async (service) => {
  if (!service || service.dbPort == null) return;
  connRows.value = null;
  connUri.value = null;
  try {
    const report = await envReport(project.id);
    const env = (key: string) => report.entries.find((e) => e.key === key)?.value?.trim() || null;
    const port = String(service.dbPort);
    const database = env("DB_DATABASE") ?? "laravel";
    const username = env("DB_USERNAME") ?? "sail";
    const password = env("DB_PASSWORD") ?? "password";
    connRows.value = [
      { label: "Host", value: "127.0.0.1" },
      { label: "Port", value: port },
      { label: "Database", value: database },
      { label: "Username", value: username },
      { label: "Password", value: password, mask: true },
    ];
    const scheme = env("DB_CONNECTION") === "pgsql" ? "postgresql" : "mysql";
    connUri.value = `${scheme}://${encodeURIComponent(username)}:${encodeURIComponent(password)}@127.0.0.1:${port}/${database}`;
  } catch {
    connRows.value = [];
  }
});
async function copyConn(label: string, value: string) {
  await navigator.clipboard.writeText(value);
  connCopied.value = label;
  setTimeout(() => (connCopied.value = null), 1500);
}

async function copyShareUrl() {
  if (!project.shareUrl) return;
  await navigator.clipboard.writeText(project.shareUrl);
  shareCopied.value = true;
  setTimeout(() => (shareCopied.value = false), 1500);
}

// --- PHP version switch: context + image tag + no-cache rebuild + recreate
// as ONE operation (the four steps laravel/sail#442 victims half-do) ---
const phpChoices = computed(() =>
  (project.php?.available ?? []).filter((s) => s !== project.php?.current),
);
const pendingPhp = ref<string | null>(null);
const phpConfirmOpen = computed({
  get: () => pendingPhp.value != null,
  set: (value: boolean) => {
    if (!value) pendingPhp.value = null;
  },
});
function switchPhp() {
  const php = project.php;
  const series = pendingPhp.value;
  if (!php || !series) return;
  pendingPhp.value = null;
  void store.runLifecycle(project.id, `switch to PHP ${series}`, {
    type: "setPhpVersion",
    id: project.id,
    service: php.service,
    series,
  });
}

/** Why a runtime picker is unavailable right now — the fallback chip's
 * tooltip must state the real reason, not a guess. */
function runtimeLockReason(choices: number, noneMessage: string): string {
  if (store.readOnly) return "Read-only — the controlling Mast window can switch versions.";
  if (opRunning.value) return "Unavailable while an operation is running on this project.";
  if (choices === 0) return noneMessage;
  return noneMessage;
}

// --- Node switch: same verified rebuild as PHP, pinning build.args ---
const nodeChoices = computed(() =>
  (project.php?.nodeAvailable ?? []).filter((m) => m !== project.php?.node),
);
const pendingNode = ref<string | null>(null);
const nodeConfirmOpen = computed({
  get: () => pendingNode.value != null,
  set: (value: boolean) => {
    if (!value) pendingNode.value = null;
  },
});
function switchNode() {
  const php = project.php;
  const major = pendingNode.value;
  if (!php || !major) return;
  pendingNode.value = null;
  void store.runLifecycle(project.id, `switch to Node ${major}`, {
    type: "setNodeVersion",
    id: project.id,
    service: php.service,
    major,
  });
}

// --- user-defined commands, shown as chips like services/processes ---
const commands = computed(() => project.commands ?? []);
const commandDialogOpen = ref(false);
/** The name of the command being edited, or null when adding a new one. Held
 * as the ORIGINAL name, since the form may be renaming it. */
const editingCommand = ref<string | null>(null);
const catalogOpen = ref(false);
const newName = ref("");
const newCommand = ref("");
const newAuto = ref(false);
const newCwd = ref("");
const newAfter = ref("");
const newReadyWhen = ref("");

function openCommandDialog(cmd?: ProjectCommand) {
  editingCommand.value = cmd?.name ?? null;
  newName.value = cmd?.name ?? "";
  newCommand.value = cmd?.command ?? "";
  newAuto.value = cmd?.autoStart ?? false;
  newCwd.value = cmd?.cwd ?? "";
  newAfter.value = cmd?.after ?? "";
  newReadyWhen.value = cmd?.readyWhen ?? "";
  commandDialogOpen.value = true;
}

/** Waiting on a command that never starts is the one way to configure this
 * that fails silently: the chip just stays grey. Say so in the dialog, where
 * the mistake is still cheap to undo. */
const afterNotAuto = computed(() => {
  const after = newAfter.value.trim();
  if (!after || !newAuto.value) return false;
  return commands.value.some((c) => c.name === after && !c.autoStart);
});

/** The other commands this one could wait for — itself excluded, since a
 * command that waits for itself never starts. */
const afterOptions = computed(() => [
  { value: "", label: "start with the project" },
  ...commands.value
    .filter((c) => c.name !== editingCommand.value)
    .map((c) => ({ value: c.name, label: `after ${c.name}` })),
]);

/** Why the form cannot be submitted yet, or null when it can. Names are the
 * identity Mast runs and streams output under, so a collision would silently
 * merge two commands into one. */
const commandFormError = computed<string | null>(() => {
  const name = newName.value.trim();
  if (!name || !newCommand.value.trim()) return null; // nothing typed yet
  const clash = commands.value.some((c) => c.name === name && c.name !== editingCommand.value);
  return clash ? `A command named "${name}" already exists.` : null;
});

function commandOp(name: string) {
  return store.operations[commandKey(project.id, name)];
}
function commandRunning(name: string): boolean {
  const view = commandOp(name);
  return view != null && view.terminal === null;
}
function runCommand(cmd: ProjectCommand) {
  void store.runLifecycle(commandKey(project.id, cmd.name), cmd.name, {
    type: "runProjectCommand",
    id: project.id,
    name: cmd.name,
  });
}
function stopCommand(cmd: ProjectCommand) {
  void store.cancelLifecycle(commandKey(project.id, cmd.name));
}
async function saveCommands(list: ProjectCommand[]) {
  await store.run({ type: "setProjectCommands", id: project.id, commands: list });
}
async function saveCommandForm() {
  const name = newName.value.trim();
  const command = newCommand.value.trim();
  if (!name || !command || commandFormError.value) return;
  const edited: ProjectCommand = {
    name,
    command,
    autoStart: newAuto.value,
    cwd: newCwd.value.trim() || null,
    after: (newAuto.value && newAfter.value.trim()) || null,
    readyWhen: newReadyWhen.value.trim() || null,
  };
  const original = editingCommand.value;
  // Edited in place rather than removed and appended: the chip row is in list
  // order, and a command should not jump to the end because its cwd changed.
  await saveCommands(
    original === null
      ? [...commands.value, edited]
      : commands.value.map((c) => (c.name === original ? edited : c)),
  );
  if (!store.error) commandDialogOpen.value = false;
}

// --- PHP runtime viewer: php -m and the classic ini limits from the LIVE
// container, because what the runtime loaded beats what any file promises ---
const extOpen = ref(false);
const phpRt = ref<PhpRuntimeReport | null>(null);
const extError = ref<string | null>(null);
watch(extOpen, async (open) => {
  if (!open) return;
  phpRt.value = null;
  extError.value = null;
  try {
    phpRt.value = await phpRuntime(project.id);
  } catch (e) {
    extError.value = String(e);
  }
});
function editRuntimeFile(file: string) {
  void store.run({ type: "openProjectFile", id: project.id, file });
}

// Two-step clear for the app log: first click arms, second truncates —
// destructive enough for a pause, not enough for a whole modal.
const appLogClearArmed = ref(false);
async function clearAppLog() {
  if (!appLogClearArmed.value) {
    appLogClearArmed.value = true;
    setTimeout(() => (appLogClearArmed.value = false), 3000);
    return;
  }
  appLogClearArmed.value = false;
  await store.run({ type: "clearLaravelLog", id: project.id });
  setTimeout(() => void loadAppLog(), 300);
}
</script>

<template>
  <div
    class="rounded-xl border border-slate-200 bg-white p-4 shadow-sm dark:border-slate-800 dark:bg-slate-900"
  >
    <div class="flex items-center justify-between gap-4">
      <div class="min-w-0">
        <!-- nowrap + truncating title: late-arriving badges (git info lands
             after the first reconcile) may shorten the name but never re-wrap
             the header, so nothing below jumps. -->
        <div class="flex items-center gap-2 overflow-hidden">
          <h2 class="truncate font-semibold text-slate-900 dark:text-slate-100">
            {{ project.name }}
          </h2>
          <!-- Mid-operation the stored status describes where the project is
               coming from ("stopped" all through a rebuild), which is the one
               reading guaranteed to be wrong. Say what is happening instead. -->
          <Badge v-if="opRunning" variant="warning" class="shrink-0">
            <Loader2 class="h-3 w-3 animate-spin" />
            {{ op.label }}
          </Badge>
          <Badge v-else :variant="statusBadgeVariant[project.status]" class="shrink-0">
            {{ project.status }}
          </Badge>
          <Tooltip
            v-if="project.isSail"
            text="Runs through Laravel Sail (vendor/bin/sail) — exactly what your terminal would do."
          >
            <Badge variant="outline" class="shrink-0">Sail</Badge>
          </Tooltip>
          <Tooltip
            v-if="project.gitBranch"
            :text="
              project.gitDirty
                ? 'Current git branch — the amber dot means uncommitted changes.'
                : 'Current git branch — working tree is clean.'
            "
          >
            <!-- The one chip allowed to shrink: long branch names ellipsize
                 instead of being clipped by the row's overflow-hidden. -->
            <Badge variant="outline" class="min-w-0 normal-case">
              <GitBranch class="h-3 w-3 shrink-0" />
              <span class="truncate">{{ project.gitBranch }}</span>
              <span
                v-if="project.gitDirty"
                class="h-1.5 w-1.5 shrink-0 rounded-full bg-amber-500"
              />
            </Badge>
          </Tooltip>
        </div>
        <p class="mt-0.5 font-mono text-xs break-all text-slate-400">{{ project.path }}</p>
      </div>
      <div class="flex shrink-0 gap-1.5">
        <template v-if="opRunning">
          <Button variant="destructive" size="sm" @click="store.cancelLifecycle(project.id)">
            <X class="h-3.5 w-3.5" /> Cancel
          </Button>
        </template>
        <template v-else-if="!store.readOnly">
          <Button
            v-if="project.status === 'stopped' || project.status === 'failed'"
            size="sm"
            :disabled="project.resolutionError != null"
            @click="lifecycle('start', 'startProject')"
          >
            <Play class="h-3.5 w-3.5" /> Start
          </Button>
          <template v-else>
            <Button variant="outline" size="sm" @click="lifecycle('restart', 'restartProject')">
              <RotateCw class="h-3.5 w-3.5" /> Restart
            </Button>
            <Button variant="destructive" size="sm" @click="lifecycle('stop', 'stopProject')">
              <Square class="h-3.5 w-3.5" /> Stop
            </Button>
          </template>
          <Tooltip
            text="Rebuild images, pull newer ones and recreate every container — for when the compose config changed underneath the project (e.g. after a git pull). A cold image build can run for half an hour; the card shows what it is doing."
          >
            <Button
              variant="outline"
              size="sm"
              :disabled="project.resolutionError != null"
              @click="lifecycle('rebuild', 'rebuildProject')"
            >
              <Hammer class="h-3.5 w-3.5" /> Rebuild
            </Button>
          </Tooltip>
          <Tooltip text="Remove this project from Mast (files stay untouched).">
            <Button
              variant="ghost"
              size="icon"
              @click="store.run({ type: 'removeProject', id: project.id })"
            >
              <Trash2 class="h-3.5 w-3.5" />
            </Button>
          </Tooltip>
        </template>
      </div>
    </div>

    <div class="mt-2 flex flex-wrap gap-1">
      <Tooltip text="Open a terminal at the project root.">
        <Button
          variant="ghost"
          size="sm"
          @click="store.run({ type: 'openTerminal', id: project.id })"
        >
          <SquareTerminal class="h-3.5 w-3.5" /> Terminal
        </Button>
      </Tooltip>
      <Tooltip text="Open the project in your editor (configure in Settings).">
        <Button
          variant="ghost"
          size="sm"
          @click="store.run({ type: 'openInEditor', id: project.id })"
        >
          <Pencil class="h-3.5 w-3.5" /> Editor
        </Button>
      </Tooltip>
      <Tooltip text="Reveal the project in your file manager.">
        <Button
          variant="ghost"
          size="sm"
          @click="store.run({ type: 'revealInFileManager', id: project.id })"
        >
          <FolderOpen class="h-3.5 w-3.5" /> Files
        </Button>
      </Tooltip>
      <Tooltip text="Edit .env with validation, secret masking and the .env.example diff.">
        <Button variant="ghost" size="sm" @click="showEnv = true">
          <FileCog class="h-3.5 w-3.5" /> Env
        </Button>
      </Tooltip>
      <!-- Only when .env gives an address; the tooltip shows exactly what
           will open, since APP_PORT can make it differ from APP_URL. -->
      <Tooltip v-if="project.appUrl" :text="`Open ${project.appUrl} in your browser.`">
        <Button
          variant="ghost"
          size="sm"
          @click="store.run({ type: 'openInBrowser', id: project.id })"
        >
          <Globe class="h-3.5 w-3.5" /> Browser
        </Button>
      </Tooltip>
      <Tooltip
        :text="
          sharing
            ? 'The share tunnel is live — URL, QR code and tunnel output.'
            : 'Publish the running app at a temporary public URL (sail share).'
        "
      >
        <Button variant="ghost" size="sm" @click="shareOpen = true">
          <Share2 class="h-3.5 w-3.5" /> Share
          <span v-if="sharing" class="h-1.5 w-1.5 rounded-full bg-emerald-500" />
        </Button>
      </Tooltip>
      <Tooltip
        :text="
          project.localDomain
            ? `Serving https://${project.localDomain} through the local proxy.`
            : 'Serve this app at a stable, trusted https://…test address through a local proxy.'
        "
      >
        <Button variant="ghost" size="sm" @click="httpsOpen = true">
          <Lock class="h-3.5 w-3.5" /> HTTPS
          <span v-if="project.localDomain" class="h-1.5 w-1.5 rounded-full bg-emerald-500" />
        </Button>
      </Tooltip>
      <Tooltip
        text="storage/logs/laravel.log, parsed — each error with its stack trace as one entry."
      >
        <Button variant="ghost" size="sm" @click="appLogOpen = true">
          <ScrollText class="h-3.5 w-3.5" /> App log
        </Button>
      </Tooltip>
      <Tooltip
        :text="
          op?.terminal === 'failed' && op.fixes.length > 0
            ? 'A one-click fix for the last failure is waiting in here.'
            : 'Run the diagnostic checks scoped to this project\'s findings and fixes.'
        "
      >
        <Button variant="ghost" size="sm" @click="emit('diagnose')">
          <Stethoscope class="h-3.5 w-3.5" /> Diagnose
          <TriangleAlert
            v-if="op?.terminal === 'failed' && op.fixes.length > 0"
            class="h-3 w-3 text-amber-500"
          />
        </Button>
      </Tooltip>
    </div>

    <!-- Same rhythm as the tool row above: h-7 rows, px-2 so the icons line up
       under Terminal/Editor/Files, gap-1 between items. The readings are facts
       rather than actions, so they carry no hover state — but they must still
       sit on the row's grid, or the card reads as two unrelated strips.

       Rendered even when stopped, showing dashes, so that live numbers
       arriving does not shift everything below it — same reason the header
       reserves room for late git badges. -->
    <div class="mt-1 flex items-center gap-1 text-xs text-slate-400">
      <Tooltip
        :text="
          usage
            ? `${formatCores(usage.cpuCores)} of this machine's ${hostCores} cores, across every container in this project.`
            : 'Nothing running to measure.'
        "
      >
        <span class="flex h-7 items-center gap-1.5 px-2">
          <Cpu class="h-3.5 w-3.5" />
          <template v-if="usage">
            <span class="tabular-nums text-slate-500 dark:text-slate-400">
              {{ formatCores(usage.cpuCores) }}
            </span>
            <span>cores</span>
            <Sparkline :values="cpuHistory" :floor="SPARKLINE_FLOOR_CORES" />
          </template>
          <span v-else>—</span>
        </span>
      </Tooltip>
      <Tooltip
        :text="
          usage
            ? 'Working set across this project — page cache excluded, as docker stats reports it.'
            : 'Nothing running to measure.'
        "
      >
        <span class="flex h-7 items-center gap-1.5 px-2">
          <MemoryStick class="h-3.5 w-3.5" />
          <span v-if="usage" class="tabular-nums text-slate-500 dark:text-slate-400">
            {{ formatBytes(usage.memoryBytes) }}
          </span>
          <span v-else>—</span>
        </span>
      </Tooltip>
      <Tooltip v-if="usage" text="Break this down by service, ranked across every project.">
        <Button variant="ghost" size="sm" @click="store.showResources()">
          <ChartColumn class="h-3.5 w-3.5" /> Breakdown
        </Button>
      </Tooltip>
    </div>

    <p
      v-if="project.resolutionError"
      class="mt-2 rounded-md border border-amber-200 bg-amber-50 p-2 text-xs text-amber-800 dark:border-amber-900 dark:bg-amber-950/40 dark:text-amber-200"
    >
      {{ project.resolutionError }}
    </p>

    <p
      v-for="warning in project.warnings"
      :key="warning"
      class="mt-2 flex items-start gap-1.5 rounded-md border border-amber-200 bg-amber-50 p-2 text-xs text-amber-800 dark:border-amber-900 dark:bg-amber-950/40 dark:text-amber-200"
    >
      <TriangleAlert class="mt-0.5 h-3.5 w-3.5 shrink-0" />
      {{ warning }}
    </p>

    <!-- One clickable chip per service: the whole chip opens a menu with
         logs/shell plus per-service lifecycle verbs. -->
    <div class="mt-3">
      <div class="flex items-center gap-1.5">
        <p :class="rowLabelClass">Services</p>
        <Hint
          text="The containers this project is made of, colored by live state (green running, amber starting, red unhealthy). Click a chip for its logs, an in-container shell, start/stop/restart — and Open UI or connection details when the service has them."
        />
      </div>
      <div class="mt-1.5 flex flex-wrap gap-2">
        <DropdownMenuRoot v-for="service in project.services" :key="service.name">
          <DropdownMenuTrigger as-child>
            <Chip :dot="serviceDot(service)">
              {{ service.name }}
              <TriangleAlert v-if="service.orphaned" class="h-3 w-3 text-amber-500" />
              <ChevronDown class="h-3 w-3 text-slate-400" />
            </Chip>
          </DropdownMenuTrigger>
          <DropdownMenuPortal>
            <DropdownMenuContent :class="menuContentClass" :side-offset="4" align="start">
              <div
                v-if="service.orphaned"
                class="max-w-56 px-2 py-1.5 text-[11px] leading-snug text-amber-600 dark:text-amber-400"
              >
                Leftover container from an earlier compose config — stop it here, or Rebuild the
                project to replace the whole set.
              </div>
              <DropdownMenuSeparator v-if="service.orphaned" :class="menuSeparatorClass" />
              <DropdownMenuItem
                v-if="service.uiUrl"
                :class="menuItemClass"
                :disabled="service.state !== 'running'"
                @select="store.run({ type: 'openUrl', url: service.uiUrl })"
              >
                <Globe class="h-3.5 w-3.5 text-slate-400" /> Open UI
              </DropdownMenuItem>
              <DropdownMenuItem
                v-if="service.dbPort != null"
                :class="menuItemClass"
                @select="connService = service"
              >
                <Database class="h-3.5 w-3.5 text-slate-400" /> Connection info
              </DropdownMenuItem>
              <DropdownMenuSeparator
                v-if="service.uiUrl || service.dbPort != null"
                :class="menuSeparatorClass"
              />
              <DropdownMenuItem
                :class="menuItemClass"
                :disabled="service.containerId == null"
                @select="store.openLogs(project.id, service.name)"
              >
                <ScrollText class="h-3.5 w-3.5 text-slate-400" /> Logs
              </DropdownMenuItem>
              <DropdownMenuItem
                :class="menuItemClass"
                :disabled="service.containerId == null || store.readOnly"
                @select="store.captureServiceLogs(project.id, service.name)"
              >
                <Camera class="h-3.5 w-3.5 text-slate-400" /> Capture logs
              </DropdownMenuItem>
              <DropdownMenuItem
                :class="menuItemClass"
                :disabled="service.state !== 'running' || store.readOnly"
                @select="
                  store.run({ type: 'shellIntoContainer', id: project.id, service: service.name })
                "
              >
                <SquareTerminal class="h-3.5 w-3.5 text-slate-400" /> Shell
              </DropdownMenuItem>
              <DropdownMenuSeparator :class="menuSeparatorClass" />
              <DropdownMenuItem
                v-if="service.state !== 'running'"
                :class="menuItemClass"
                :disabled="locked"
                @select="serviceVerb(service.name, 'start', 'startService')"
              >
                <Play class="h-3.5 w-3.5 text-slate-400" /> Start
              </DropdownMenuItem>
              <template v-else>
                <DropdownMenuItem
                  :class="menuItemClass"
                  :disabled="locked"
                  @select="serviceVerb(service.name, 'restart', 'restartService')"
                >
                  <RotateCw class="h-3.5 w-3.5 text-slate-400" /> Restart
                </DropdownMenuItem>
                <DropdownMenuItem
                  :class="menuItemClass"
                  :disabled="locked"
                  @select="serviceVerb(service.name, 'stop', 'stopService')"
                >
                  <Square class="h-3.5 w-3.5 text-slate-400" /> Stop
                </DropdownMenuItem>
              </template>
              <DropdownMenuItem
                v-if="service.orphaned"
                :class="menuItemClass"
                :disabled="locked"
                @select="
                  store.runLifecycle(project.id, `remove ${service.name}`, {
                    type: 'removeOrphanContainer',
                    id: project.id,
                    service: service.name,
                  })
                "
              >
                <Trash2 class="h-3.5 w-3.5 text-slate-400" /> Remove container
              </DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenuPortal>
        </DropdownMenuRoot>
        <Chip
          dashed
          tip="Add or remove a service (Redis, Mailpit, a database, …), or change an installed one's version — each as a previewed compose edit."
          :disabled="locked"
          @click="catalogOpen = true"
        >
          <Plus class="h-3 w-3" /> Add
        </Chip>
      </div>
    </div>

    <!-- The Sail PHP runtime, same chip pattern as services: the chip opens
         a menu of the other vendored series, and a switch runs context +
         image tag + no-cache rebuild + recreate as one confirmed operation. -->
    <div v-if="project.php" class="mt-3">
      <div class="flex items-center gap-1.5">
        <p :class="rowLabelClass">Runtimes</p>
        <Hint
          text="What the app container is built from: the Sail PHP runtime and the Node it installs. Picking another version rewrites the compose file, rebuilds without cache, recreates the container, and verifies the switch inside it — the steps that go wrong when done by hand."
        />
      </div>
      <div class="mt-1.5 flex flex-wrap gap-2">
        <DropdownMenuRoot>
          <DropdownMenuTrigger as-child>
            <Chip>
              PHP {{ project.php.current }}
              <ChevronDown class="h-3 w-3 text-slate-400" />
            </Chip>
          </DropdownMenuTrigger>
          <DropdownMenuPortal>
            <DropdownMenuContent :class="menuContentClass" :side-offset="4" align="start">
              <template v-if="phpChoices.length > 0 && !store.readOnly && !opRunning">
                <DropdownMenuItem
                  v-for="s in phpChoices"
                  :key="s"
                  :class="menuItemClass"
                  @select="pendingPhp = s"
                >
                  Switch to PHP {{ s }}
                </DropdownMenuItem>
                <DropdownMenuSeparator :class="menuSeparatorClass" />
              </template>
              <DropdownMenuItem :class="menuItemClass" @select="extOpen = true">
                <Puzzle class="h-3.5 w-3.5 text-slate-400" /> Extensions
              </DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenuPortal>
        </DropdownMenuRoot>
        <template v-if="project.php.node">
          <DropdownMenuRoot v-if="nodeChoices.length > 0 && !store.readOnly && !opRunning">
            <DropdownMenuTrigger as-child>
              <Chip>
                Node {{ project.php.node }}
                <ChevronDown class="h-3 w-3 text-slate-400" />
              </Chip>
            </DropdownMenuTrigger>
            <DropdownMenuPortal>
              <DropdownMenuContent :class="menuContentClass" :side-offset="4" align="start">
                <DropdownMenuItem
                  v-for="m in nodeChoices"
                  :key="m"
                  :class="menuItemClass"
                  @select="pendingNode = m"
                >
                  Switch to Node {{ m }}
                </DropdownMenuItem>
              </DropdownMenuContent>
            </DropdownMenuPortal>
          </DropdownMenuRoot>
          <Chip
            v-else
            :interactive="false"
            :tip="
              runtimeLockReason(
                nodeChoices.length,
                'Node inside the app container, pinned by the Sail runtime — this build shape cannot take an override.',
              )
            "
          >
            Node {{ project.php.node }}
          </Chip>
        </template>
      </div>
    </div>

    <!-- Laravel app processes (Reverb/Horizon/…): in-container artisan
         daemons, same chip+menu pattern as services. -->
    <div v-if="processes.length > 0" class="mt-3">
      <div class="flex items-center gap-1.5">
        <p :class="rowLabelClass">Processes</p>
        <Hint
          text="Laravel daemons that run INSIDE the app container (detected from composer.json and .env). A green dot means it is running right now — even if you started it from a terminal. Start/stop from the chip menu."
        />
      </div>
      <div class="mt-1.5 flex flex-wrap gap-2">
        <DropdownMenuRoot v-for="proc in processes" :key="proc.id">
          <DropdownMenuTrigger as-child>
            <Chip
              :dot="proc.running ? 'bg-emerald-500' : 'bg-slate-300 dark:bg-slate-600'"
              :tip="processHints[proc.id] ?? proc.title"
            >
              {{ proc.title }}
              <ChevronDown class="h-3 w-3 text-slate-400" />
            </Chip>
          </DropdownMenuTrigger>
          <DropdownMenuPortal>
            <DropdownMenuContent :class="menuContentClass" :side-offset="4" align="start">
              <DropdownMenuItem
                v-if="!proc.running"
                :class="menuItemClass"
                :disabled="locked || project.status !== 'running'"
                @select="processVerb(proc.id, proc.title, 'startProcess')"
              >
                <Play class="h-3.5 w-3.5 text-slate-400" /> Start{{
                  project.status !== "running" ? " (start the project first)" : ""
                }}
              </DropdownMenuItem>
              <DropdownMenuItem
                v-else
                :class="menuItemClass"
                :disabled="locked"
                @select="processVerb(proc.id, proc.title, 'stopProcess')"
              >
                <Square class="h-3.5 w-3.5 text-slate-400" /> Stop
              </DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenuPortal>
        </DropdownMenuRoot>
      </div>
    </div>

    <!-- User-defined commands (host, cwd = project dir), same chip pattern. -->
    <div class="mt-3">
      <div class="flex items-center gap-1.5">
        <p :class="rowLabelClass">Commands</p>
        <Hint
          text="Your own commands, run on your machine in the project directory — e.g. `sail npm run dev` or `sail artisan tinker`. `auto` runs a command whenever the project comes up. Output streams into the panel below while it runs."
        />
      </div>
      <div class="mt-1.5 flex flex-wrap gap-2">
        <DropdownMenuRoot v-for="cmd in commands" :key="cmd.name">
          <DropdownMenuTrigger as-child>
            <Chip
              :dot="commandRunning(cmd.name) ? 'bg-emerald-500' : 'bg-slate-300 dark:bg-slate-600'"
              :tip="
                cmd.command +
                (cmd.cwd ? ` · in ${cmd.cwd}` : '') +
                (cmd.autoStart ? ' · runs automatically on start' : '')
              "
            >
              {{ cmd.name }}
              <span v-if="cmd.autoStart" class="text-[10px] text-slate-400">auto</span>
              <ChevronDown class="h-3 w-3 text-slate-400" />
            </Chip>
          </DropdownMenuTrigger>
          <DropdownMenuPortal>
            <DropdownMenuContent :class="menuContentClass" :side-offset="4" align="start">
              <!-- The line that will actually be run. A chip shows a name the
                   user chose; when a command misbehaves, the question is
                   always what it expands to, and a tooltip you have to hunt
                   for is the wrong place to answer it. -->
              <div class="max-w-72 px-2 py-1.5">
                <p class="font-mono text-[11px] break-all text-slate-600 dark:text-slate-300">
                  {{ cmd.command }}
                </p>
                <p class="mt-0.5 text-[10px] text-slate-400">
                  in {{ cmd.cwd || "the project directory" }}
                  <template v-if="cmd.autoStart">
                    · auto<template v-if="cmd.after">, after {{ cmd.after }}</template>
                  </template>
                </p>
              </div>
              <DropdownMenuSeparator :class="menuSeparatorClass" />
              <DropdownMenuItem
                v-if="!commandRunning(cmd.name)"
                :class="menuItemClass"
                :disabled="locked"
                @select="runCommand(cmd)"
              >
                <Play class="h-3.5 w-3.5 text-slate-400" /> Run
              </DropdownMenuItem>
              <DropdownMenuItem v-else :class="menuItemClass" @select="stopCommand(cmd)">
                <Square class="h-3.5 w-3.5 text-slate-400" /> Stop
              </DropdownMenuItem>
              <DropdownMenuSeparator :class="menuSeparatorClass" />
              <!-- Not while it runs: the live process would keep the old line
                   either way, and a rename would strand its output under a
                   key nothing is listening to any more. -->
              <DropdownMenuItem
                :class="menuItemClass"
                :disabled="locked || commandRunning(cmd.name)"
                @select="openCommandDialog(cmd)"
              >
                <Pencil class="h-3.5 w-3.5 text-slate-400" /> Edit
              </DropdownMenuItem>
              <DropdownMenuItem
                :class="menuItemClass"
                :disabled="locked"
                @select="
                  saveCommands(
                    commands.map((c) =>
                      c.name === cmd.name ? { ...c, autoStart: !c.autoStart } : c,
                    ),
                  )
                "
              >
                {{ cmd.autoStart ? "Disable auto-start" : "Enable auto-start" }}
              </DropdownMenuItem>
              <DropdownMenuItem
                :class="menuItemClass"
                :disabled="locked"
                @select="saveCommands(commands.filter((c) => c.name !== cmd.name))"
              >
                <Trash2 class="h-3.5 w-3.5 text-slate-400" /> Delete
              </DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenuPortal>
        </DropdownMenuRoot>
        <Chip dashed :disabled="locked" @click="openCommandDialog()">
          <Plus class="h-3 w-3" /> Add
        </Chip>
      </div>
    </div>

    <CatalogDialog v-model:open="catalogOpen" :project="project.id" />

    <Modal v-model:open="shareOpen" title="Share publicly" wide>
      <div class="space-y-3">
        <template v-if="!sharing">
          <p class="text-xs text-slate-500 dark:text-slate-400">
            Runs Sail's expose tunnel (<span class="font-mono">sail share</span>): the running app
            is published at a temporary public URL over plain HTTP. Anyone with the link reaches
            your machine while the tunnel is up.
          </p>
          <div
            v-if="shareSettings"
            class="rounded-md border border-slate-200 bg-slate-50 p-2 dark:border-slate-800 dark:bg-neutral-900"
          >
            <div
              v-for="setting in shareSettings"
              :key="setting.key"
              class="flex items-baseline gap-2 text-xs"
            >
              <span class="w-20 shrink-0 text-slate-400">{{ setting.label }}</span>
              <span
                class="font-mono"
                :class="setting.isDefault ? 'text-slate-400' : 'text-slate-700 dark:text-slate-200'"
                >{{ setting.value
                }}<span v-if="setting.isDefault" class="ml-1 text-[10px]">default</span></span
              >
              <span class="ml-auto font-mono text-[10px] text-slate-400">{{ setting.key }}</span>
            </div>
          </div>
          <!-- Outside the card: the card holds facts, the action stands on
               its own. -ml-2 lines the label up with the content edge, the
               same trick as the op panel's "Show the command" button. -->
          <div>
            <Button
              variant="ghost"
              size="sm"
              class="-ml-2"
              @click="
                shareOpen = false;
                showEnv = true;
              "
            >
              <FileCog class="h-3.5 w-3.5" /> Change these in the Env panel
            </Button>
          </div>
          <p
            v-if="project.status === 'stopped' || project.status === 'failed'"
            class="text-xs text-amber-700 dark:text-amber-300"
          >
            The tunnel forwards the running app — start the project first.
          </p>
          <p v-if="store.readOnly" class="text-xs text-amber-700 dark:text-amber-300">
            Read-only — the controlling Mast window can start a share.
          </p>
          <div class="flex justify-end">
            <Button
              :disabled="locked || project.status === 'stopped' || project.status === 'failed'"
              @click="startShare"
            >
              <Share2 class="h-3.5 w-3.5" /> Start sharing
            </Button>
          </div>
        </template>

        <template v-else>
          <div v-if="project.shareUrl" class="flex items-start gap-4">
            <!-- White backing keeps the code scannable in dark mode. -->
            <img
              v-if="shareQr"
              :src="shareQr"
              alt="QR code for the share URL"
              class="h-36 w-36 shrink-0 rounded-md bg-white p-2"
            />
            <div class="min-w-0 space-y-2">
              <p class="text-xs text-slate-500 dark:text-slate-400">Public URL (HTTP only):</p>
              <p
                class="flex items-baseline gap-1.5 font-mono text-sm break-all select-all text-slate-900 dark:text-slate-100"
              >
                {{ project.shareUrl }}
                <Tooltip text="Copy the URL.">
                  <button :class="['shrink-0', iconButtonClass]" @click="copyShareUrl">
                    <Check v-if="shareCopied" class="h-3.5 w-3.5 text-emerald-600" />
                    <Copy v-else class="h-3.5 w-3.5" />
                  </button>
                </Tooltip>
              </p>
              <div class="flex flex-wrap gap-2">
                <Button size="sm" @click="store.run({ type: 'openUrl', url: project.shareUrl! })">
                  <Globe class="h-3.5 w-3.5" /> Open
                </Button>
                <Button
                  v-if="project.shareDashboardUrl"
                  variant="outline"
                  size="sm"
                  @click="store.run({ type: 'openUrl', url: project.shareDashboardUrl! })"
                >
                  <ChartColumn class="h-3.5 w-3.5" /> Dashboard
                </Button>
              </div>
              <p class="text-xs text-slate-500 dark:text-slate-400">
                Scan the code to open it on your phone. The dashboard shows every request through
                the tunnel<template v-if="project.shareDashboardUrl">
                  ({{ project.shareDashboardUrl }})</template
                >.
              </p>
              <p class="text-xs text-slate-400">
                HTTPS is not available on the public
                <span class="font-mono">laravel-sail.site</span> relay — a known Sail limitation,
                use the HTTP link. For HTTPS, run your own expose server with TLS and point
                <span class="font-mono">SAIL_SHARE_SERVER_HOST</span> at it.
              </p>
            </div>
          </div>
          <p v-else class="text-xs text-slate-500 dark:text-slate-400">
            Starting the tunnel — the URL appears here as soon as expose reports it…
          </p>
          <div class="flex justify-end">
            <Button variant="destructive" size="sm" @click="stopShare">
              <Square class="h-3.5 w-3.5" /> Stop sharing
            </Button>
          </div>
        </template>

        <!-- The tunnel's own output: where the classic failures (Vite dev
             server through the tunnel, auth, DNS) explain themselves. -->
        <div
          v-if="shareOp && shareOp.lines.length > 0"
          class="max-h-48 overflow-y-auto rounded-md border border-slate-200 bg-slate-50 p-2 font-mono text-[11px] leading-relaxed dark:border-slate-800 dark:bg-neutral-900"
        >
          <p
            v-for="(line, i) in shareOp.lines"
            :key="i"
            :class="
              line.stderr
                ? 'text-amber-700 dark:text-amber-300'
                : 'text-slate-600 dark:text-slate-300'
            "
          >
            {{ line.line }}
          </p>
        </div>
        <p
          v-if="shareOp && shareOp.terminal === 'failed'"
          class="text-xs text-red-700 dark:text-red-300"
        >
          Share failed: {{ shareOp.error }}
        </p>
        <FixButton
          v-for="f in shareOp && shareOp.terminal === 'failed' ? shareOp.fixes : []"
          :key="f.repair.id + (f.repair.arg ?? '')"
          :repair="f.repair"
          :project="f.project"
          @applied="store.dismissOperation(shareKey(project.id))"
        />
      </div>
    </Modal>

    <!-- The application's own log, container logs' missing half: parsed
         Monolog entries with levels and grouped stack traces, filterable,
         newest first — instead of scrolling raw laravel.log in an editor. -->
    <Modal v-model:open="appLogOpen" title="Application log" wide>
      <div class="space-y-3">
        <div class="flex flex-wrap items-center gap-2">
          <Button
            v-for="f in appLogFilters"
            :key="f.id"
            :variant="appLogFilter === f.id ? undefined : 'outline'"
            size="sm"
            @click="appLogFilter = f.id"
          >
            {{ f.label }}
          </Button>
          <div class="ml-auto flex gap-2">
            <Tooltip text="Truncate storage/logs/laravel.log — the file stays, its history goes.">
              <Button
                variant="outline"
                size="sm"
                :disabled="store.readOnly || !appLog?.exists"
                :class="appLogClearArmed ? 'text-red-700 dark:text-red-300' : ''"
                @click="clearAppLog"
              >
                <Trash2 class="h-3.5 w-3.5" />
                {{ appLogClearArmed ? "Really clear?" : "Clear" }}
              </Button>
            </Tooltip>
            <Button variant="outline" size="sm" @click="loadAppLog">
              <RotateCw class="h-3.5 w-3.5" /> Refresh
            </Button>
          </div>
        </div>
        <p v-if="appLogError" class="text-xs text-red-700 dark:text-red-300">
          Could not read the log: {{ appLogError }}
        </p>
        <p v-else-if="appLog == null" class="text-sm text-slate-500 dark:text-slate-400">
          Reading storage/logs/laravel.log…
        </p>
        <p v-else-if="!appLog.exists" class="text-sm text-slate-500 dark:text-slate-400">
          No <span class="font-mono">storage/logs/laravel.log</span> — the app has not logged
          anything yet, or <span class="font-mono">LOG_CHANNEL</span> sends logs elsewhere (stderr
          output shows under the service chip's Logs).
        </p>
        <p
          v-else-if="appLogEntries.length === 0"
          class="text-sm text-slate-500 dark:text-slate-400"
        >
          No entries match this filter.
        </p>
        <div v-else class="max-h-[55vh] space-y-1 overflow-y-auto">
          <div
            v-for="(entry, i) in appLogEntries"
            :key="i"
            class="rounded-md border border-slate-200 dark:border-slate-800"
          >
            <button
              class="flex w-full items-baseline gap-2 px-2 py-1.5 text-left"
              :class="
                entry.detail
                  ? 'cursor-pointer hover:bg-slate-50 dark:hover:bg-slate-800/50'
                  : 'cursor-default'
              "
              @click="entry.detail && toggleAppLogRow(i)"
            >
              <span
                class="shrink-0 rounded px-1.5 py-0.5 text-[10px] font-semibold"
                :class="levelBadge(entry.level)"
              >
                {{ entry.level }}
              </span>
              <span class="shrink-0 font-mono text-[11px] text-slate-400">
                {{ entry.timestamp }}
              </span>
              <span class="min-w-0 flex-1 truncate text-xs text-slate-700 dark:text-slate-200">
                {{ entry.message }}
              </span>
              <ChevronDown
                v-if="entry.detail"
                class="h-3 w-3 shrink-0 self-center text-slate-400 transition-transform"
                :class="appLogExpanded.has(i) ? 'rotate-180' : ''"
              />
            </button>
            <pre
              v-if="entry.detail && appLogExpanded.has(i)"
              class="overflow-x-auto border-t border-slate-200 bg-slate-50 p-2 font-mono text-[11px] leading-relaxed whitespace-pre-wrap text-slate-600 dark:border-slate-800 dark:bg-neutral-900 dark:text-slate-300"
              >{{ `${entry.message}\n\n${entry.detail}` }}</pre>
          </div>
        </div>
        <p v-if="appLog?.truncated" class="text-xs text-slate-400">
          Showing the newest entries — the file is longer than the read window.
        </p>
      </div>
    </Modal>

    <!-- What the LIVE app container says about itself — ini limits via
         ini_get and extensions via php -m — with the vendored runtime's own
         files one click from the editor, and a rebuild to apply them. -->
    <Modal v-model:open="extOpen" title="PHP runtime" wide>
      <p v-if="extError" class="text-sm text-slate-500 dark:text-slate-400">
        {{ extError }}
      </p>
      <p v-else-if="phpRt == null" class="text-sm text-slate-500 dark:text-slate-400">
        Asking the app container…
      </p>
      <template v-else>
        <p class="text-xs font-medium text-slate-600 dark:text-slate-300">Limits</p>
        <div class="mt-1.5 grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 text-xs">
          <template v-for="row in phpRt.ini" :key="row.key">
            <span class="font-mono text-slate-500 dark:text-slate-400">{{ row.key }}</span>
            <span class="font-mono text-slate-900 dark:text-slate-100">
              {{ row.value || "(not set)" }}
            </span>
          </template>
        </div>
        <p class="mt-4 text-xs font-medium text-slate-600 dark:text-slate-300">
          Extensions ({{ phpRt.extensions.length }})
        </p>
        <div class="mt-1.5 grid max-h-[32vh] grid-cols-4 gap-x-3 gap-y-1 overflow-y-auto">
          <span
            v-for="ext in phpRt.extensions"
            :key="ext"
            class="truncate font-mono text-xs text-slate-700 dark:text-slate-200"
          >
            {{ ext }}
          </span>
        </div>
        <div class="mt-4 flex flex-wrap items-center gap-2">
          <Button
            v-if="phpRt.iniFile"
            variant="outline"
            size="sm"
            @click="editRuntimeFile(phpRt.iniFile)"
          >
            <Pencil class="h-3.5 w-3.5" /> Edit php.ini
          </Button>
          <Button
            v-if="phpRt.dockerfile"
            variant="outline"
            size="sm"
            @click="editRuntimeFile(phpRt.dockerfile)"
          >
            <Pencil class="h-3.5 w-3.5" /> Edit Dockerfile
          </Button>
          <Button
            variant="outline"
            size="sm"
            :disabled="locked || !project.php"
            @click="
              extOpen = false;
              serviceVerb(project.php!.service, 'rebuild', 'rebuildService');
            "
          >
            <RotateCw class="h-3.5 w-3.5" /> Rebuild to apply
          </Button>
        </div>
        <p class="mt-2 text-xs text-slate-400">
          Limits live in the runtime's <span class="font-mono">php.ini</span>; extensions are
          installed by its <span class="font-mono">Dockerfile</span>. Both are copied into the
          <span class="font-mono">sail-{{ project.php?.current }}/app</span> image when it builds,
          so edits take effect after a rebuild of
          <span class="font-mono">{{ project.php?.service ?? "the app service" }}</span
          >.
        </p>
      </template>
    </Modal>

    <!-- The HTTPS differentiator: claim a .test domain, watch the proxy
         converge, and pick up the two system-level steps as Fix buttons —
         the operation's output stays in the dialog so "why does the browser
         still warn" has an answer in view. -->
    <Modal v-model:open="httpsOpen" title="Local HTTPS domain">
      <div class="space-y-3">
        <p class="text-xs text-slate-500 dark:text-slate-400">
          One shared Caddy proxy (ports 80/443) serves every Mast project that claims a domain, with
          certificates from its own local-only authority — secure cookies, service workers and other
          HTTPS-only features work in dev, and the address stays stable however the ports move.
        </p>
        <label class="block text-xs text-slate-600 dark:text-slate-300">
          Domain
          <Input v-model="domainDraft" placeholder="myapp.test" class="mt-1 font-mono" />
        </label>
        <p v-if="!domainValid" class="text-xs text-amber-700 dark:text-amber-300">
          Lowercase letters, digits and hyphens, ending in
          <span class="font-mono">.test</span> or <span class="font-mono">.localhost</span>.
        </p>
        <div class="flex flex-wrap gap-2">
          <Button
            :disabled="locked || !domainValid || domainBusy"
            @click="setDomain(domainDraft.trim().toLowerCase())"
          >
            <Lock class="h-3.5 w-3.5" />
            {{ project.localDomain ? "Update" : "Enable" }}
          </Button>
          <Button
            v-if="project.localDomain"
            variant="outline"
            @click="store.run({ type: 'openUrl', url: `https://${project.localDomain}` })"
          >
            <Globe class="h-3.5 w-3.5" /> Open
          </Button>
          <Button
            v-if="project.localDomain"
            variant="outline"
            :disabled="locked || domainBusy"
            @click="setDomain(null)"
          >
            Disable
          </Button>
        </div>
        <div
          v-if="domainOp && domainOp.lines.length > 0"
          class="max-h-48 overflow-y-auto rounded-md border border-slate-200 bg-slate-50 p-2 font-mono text-[11px] leading-relaxed dark:border-slate-800 dark:bg-neutral-900"
        >
          <p
            v-for="(line, i) in domainOp.lines"
            :key="i"
            :class="
              line.stderr
                ? 'text-amber-700 dark:text-amber-300'
                : 'text-slate-600 dark:text-slate-300'
            "
          >
            {{ line.line }}
          </p>
        </div>
        <p
          v-if="domainOp && domainOp.terminal === 'failed'"
          class="text-xs text-red-700 dark:text-red-300"
        >
          Failed: {{ domainOp.error }}
        </p>
        <!-- Follow-up steps, not failure fallout: these render on success
             too, because /etc/hosts and CA trust are what stand between a
             green operation and a browser that stops warning. -->
        <div v-if="domainOp && domainOp.fixes.length > 0" class="flex flex-wrap gap-1">
          <FixButton
            v-for="f in domainOp.fixes"
            :key="f.repair.id + (f.repair.arg ?? '')"
            :repair="f.repair"
            :project="f.project"
          />
        </div>
        <p class="text-xs text-slate-400">
          Two one-time system steps — the hosts-file line and trusting the certificate authority —
          appear as Fix buttons after enabling, each with a preview and an elevation prompt. Nothing
          touches your system without them.
        </p>
        <!-- The manual route, always available: elevation prompts are not an
             option everywhere (no polkit agent, hardened machines, Firefox's
             own store), so everything a person needs to do it by hand is
             copyable right here. -->
        <div
          v-if="project.localDomain"
          class="space-y-2 border-t border-slate-200 pt-3 dark:border-slate-700"
        >
          <p class="text-xs font-medium text-slate-600 dark:text-slate-300">
            Prefer to set it up yourself?
          </p>
          <p class="text-xs text-slate-500 dark:text-slate-400">
            Add this line to your hosts file:
          </p>
          <p
            class="flex items-baseline gap-1.5 font-mono text-xs select-all text-slate-900 dark:text-slate-100"
          >
            {{ hostsLine }}
            <Tooltip text="Copy the hosts line.">
              <button :class="['shrink-0', iconButtonClass]" @click="copyManual('hosts')">
                <Check v-if="manualCopied === 'hosts'" class="h-3.5 w-3.5 text-emerald-600" />
                <Copy v-else class="h-3.5 w-3.5" />
              </button>
            </Tooltip>
          </p>
          <div>
            <Button variant="outline" size="sm" @click="store.run({ type: 'openHostsFile' })">
              <FileCog class="h-3.5 w-3.5" /> Open hosts file
            </Button>
          </div>
          <template v-if="caFile">
            <p class="text-xs text-slate-500 dark:text-slate-400">
              And trust the certificate authority where you need it — Firefox's import dialog,
              <span class="font-mono">curl --cacert</span>,
              <span class="font-mono">NODE_EXTRA_CA_CERTS</span>:
            </p>
            <p
              class="flex items-baseline gap-1.5 font-mono text-xs break-all select-all text-slate-900 dark:text-slate-100"
            >
              {{ caFile.path }}
              <Tooltip text="Copy the certificate file's path.">
                <button :class="['shrink-0', iconButtonClass]" @click="copyManual('path')">
                  <Check v-if="manualCopied === 'path'" class="h-3.5 w-3.5 text-emerald-600" />
                  <Copy v-else class="h-3.5 w-3.5" />
                </button>
              </Tooltip>
            </p>
            <div>
              <Button variant="outline" size="sm" @click="copyManual('pem')">
                <Check v-if="manualCopied === 'pem'" class="h-3.5 w-3.5 text-emerald-600" />
                <Copy v-else class="h-3.5 w-3.5" />
                {{ manualCopied === "pem" ? "Copied" : "Copy certificate (PEM)" }}
              </Button>
            </div>
          </template>
        </div>
      </div>
    </Modal>

    <!-- Everything a database GUI asks for, in the copy-icon rhythm of the
         share settings — plus the host-vs-container caveat, because pointing
         .env at 127.0.0.1 is the classic follow-up mistake. -->
    <Modal v-model:open="connOpen" :title="`Connect to ${connService?.name ?? 'database'}`">
      <p v-if="connRows == null" class="text-sm text-slate-500 dark:text-slate-400">
        Reading .env…
      </p>
      <template v-else>
        <div class="grid grid-cols-[auto_1fr] items-baseline gap-x-4 gap-y-1.5 text-sm">
          <template v-for="row in connRows" :key="row.label">
            <span class="text-slate-500 dark:text-slate-400">{{ row.label }}</span>
            <span
              class="flex min-w-0 items-baseline gap-1.5 font-mono text-slate-900 dark:text-slate-100"
            >
              <span class="truncate">{{ row.mask ? "••••••••" : row.value }}</span>
              <Tooltip :text="`Copy the ${row.label.toLowerCase()}.`">
                <button
                  :class="['shrink-0', iconButtonClass]"
                  @click="copyConn(row.label, row.value)"
                >
                  <Check v-if="connCopied === row.label" class="h-3.5 w-3.5 text-emerald-600" />
                  <Copy v-else class="h-3.5 w-3.5" />
                </button>
              </Tooltip>
            </span>
          </template>
        </div>
        <div v-if="connUri" class="mt-3 border-t border-slate-200 pt-3 dark:border-slate-700">
          <p class="text-xs text-slate-500 dark:text-slate-400">Connection URL:</p>
          <p
            class="mt-1 flex items-baseline gap-1.5 font-mono text-xs break-all select-all text-slate-900 dark:text-slate-100"
          >
            {{ connUri.replace(/:[^/@:]*@/, ":••••@") }}
            <Tooltip text="Copy the URL (with the real password).">
              <button :class="['shrink-0', iconButtonClass]" @click="copyConn('URL', connUri)">
                <Check v-if="connCopied === 'URL'" class="h-3.5 w-3.5 text-emerald-600" />
                <Copy v-else class="h-3.5 w-3.5" />
              </button>
            </Tooltip>
          </p>
        </div>
        <p class="mt-3 text-xs text-slate-400">
          These are for programs on this machine — TablePlus, DBeaver, artisan run outside Sail.
          Inside the containers the app keeps using the service hostname; don't point
          <span class="font-mono">DB_HOST</span> at 127.0.0.1.
        </p>
      </template>
    </Modal>

    <Modal v-model:open="phpConfirmOpen" title="Switch PHP version">
      <div class="space-y-3">
        <p class="text-xs text-slate-500 dark:text-slate-400">
          Rebuilds <span class="font-mono">{{ project.php?.service }}</span> from the PHP
          {{ pendingPhp }} runtime: the build context and
          <span class="font-mono">sail-{{ pendingPhp }}/app</span> image tag move together, the
          image rebuilds without cache (several minutes on a first build), and the container is
          recreated if the project is running — then <span class="font-mono">php -v</span> is
          checked inside it.
        </p>
        <div class="flex justify-end gap-2">
          <Button variant="outline" @click="pendingPhp = null">Cancel</Button>
          <Button @click="switchPhp">Switch to PHP {{ pendingPhp }}</Button>
        </div>
      </div>
    </Modal>

    <Modal v-model:open="nodeConfirmOpen" title="Switch Node version">
      <div class="space-y-3">
        <p class="text-xs text-slate-500 dark:text-slate-400">
          Pins <span class="font-mono">NODE_VERSION: '{{ pendingNode }}'</span> in the compose build
          args (Sail's documented override), rebuilds
          <span class="font-mono">{{ project.php?.service }}</span> without cache — several minutes
          on a first build — and recreates the container if the project is running, then checks
          <span class="font-mono">node -v</span> inside it.
        </p>
        <div class="flex justify-end gap-2">
          <Button variant="outline" @click="pendingNode = null">Cancel</Button>
          <Button @click="switchNode">Switch to Node {{ pendingNode }}</Button>
        </div>
      </div>
    </Modal>

    <Modal
      v-model:open="commandDialogOpen"
      :title="editingCommand === null ? 'Add command' : `Edit ${editingCommand}`"
    >
      <div class="space-y-3">
        <p class="text-xs text-slate-500 dark:text-slate-400">
          Runs on your machine with the project directory as working directory. A leading
          <span class="font-mono">sail</span> resolves to
          <span class="font-mono">vendor/bin/sail</span>; no shell features (pipes, &&).
        </p>
        <label class="block text-xs text-slate-600 dark:text-slate-300">
          Name
          <Input v-model="newName" placeholder="dev" class="mt-1" />
        </label>
        <label class="block text-xs text-slate-600 dark:text-slate-300">
          Command
          <Input v-model="newCommand" placeholder="sail npm run dev" mono class="mt-1" />
        </label>
        <label class="block text-xs text-slate-600 dark:text-slate-300">
          Working directory <span class="text-slate-400">(optional)</span>
          <Input v-model="newCwd" placeholder="../frontend or /an/absolute/path" class="mt-1" />
        </label>
        <p v-if="newCwd.trim()" class="text-xs text-slate-400">
          Relative paths start at this project — so a sibling repo's dev server is
          <span class="font-mono">../frontend</span> + <span class="font-mono">npm run dev</span>.
          <span class="font-mono">sail</span> commands only work from the project root.
        </p>
        <Checkbox v-model="newAuto" label="Run automatically when the project starts" />
        <template v-if="newAuto">
          <label class="block text-xs text-slate-600 dark:text-slate-300">
            Start it
            <Select v-model="newAfter" :options="afterOptions" class="mt-1" />
          </label>
          <p v-if="afterNotAuto" class="text-xs text-amber-700 dark:text-amber-300">
            <span class="font-mono">{{ newAfter }}</span> does not start with the project, so
            nothing will be waiting on — turn its auto-start on, or this command will never run by
            itself.
          </p>
          <p v-if="newAfter" class="text-xs text-slate-400">
            Waits for <span class="font-mono">{{ newAfter }}</span> to finish starting — not to
            exit, which a dev server never does. Mast takes it as up when its
            <span class="font-mono">ready when</span> text appears, or, if it has none, when its
            output goes quiet.
          </p>
          <label class="block text-xs text-slate-600 dark:text-slate-300">
            Ready when it prints <span class="text-slate-400">(optional)</span>
            <Input v-model="newReadyWhen" placeholder="Server running on" mono class="mt-1" />
          </label>
          <p class="text-xs text-slate-400">
            How anything waiting on <span class="font-mono">{{ newName.trim() || "this" }}</span>
            knows it is up. Leave it blank and Mast waits for the output to settle instead, which is
            a guess — a good one for a server that prints a banner and quietens down.
          </p>
        </template>
        <p v-if="commandFormError" class="text-xs text-amber-700 dark:text-amber-300">
          {{ commandFormError }}
        </p>
        <div class="flex justify-end gap-2">
          <Button variant="outline" @click="commandDialogOpen = false">Cancel</Button>
          <Button
            :disabled="!newName.trim() || !newCommand.trim() || commandFormError !== null"
            @click="saveCommandForm"
          >
            {{ editingCommand === null ? "Add command" : "Save changes" }}
          </Button>
        </div>
      </div>
    </Modal>

    <div
      v-if="op && (opRunning || op.terminal === 'failed' || op.terminal === 'cancelled')"
      class="mt-3 rounded-md border border-slate-200 bg-slate-50 p-2 dark:border-slate-800 dark:bg-neutral-800/50"
    >
      <p class="text-xs font-medium text-slate-600 dark:text-slate-300">
        {{ op.label }}
        <span v-if="opRunning" class="text-amber-600"> running… {{ formatElapsed(elapsed) }} </span>
        <span v-else-if="op.terminal === 'cancelled'" class="text-amber-700">cancelled</span>
        <span v-else class="text-red-700">failed: {{ op.error }}</span>
      </p>
      <!-- One line, truncated: enough to tell a build that is working from
           one that has stopped, without the panel and without ever growing
           the card. -->
      <p
        v-if="opRunning && lastLine"
        class="mt-1 truncate font-mono text-[11px] text-slate-400"
        :title="lastLine"
      >
        {{ lastLine }}
      </p>
      <!-- A failure is only actionable if you can see what was run. Matched
           fixes live in Diagnostics (one home for every repair) — the
           failure box and the Diagnose button both point there. -->
      <div v-if="op.terminal === 'failed'" class="mt-1 flex flex-wrap items-center gap-1">
        <Button v-if="op.fixes.length > 0" variant="outline" size="sm" @click="emit('diagnose')">
          <Wrench class="h-3.5 w-3.5 text-amber-500" />
          Fix available — open Diagnose
        </Button>
        <Button
          v-if="op.id >= 0"
          variant="ghost"
          size="sm"
          @click="store.showOperationCommand(op.id)"
        >
          <ScrollText class="h-3.5 w-3.5" />
          Show the command that failed
        </Button>
      </div>
    </div>

    <EnvPanel v-model:open="showEnv" :project="project.id" />
  </div>
</template>
