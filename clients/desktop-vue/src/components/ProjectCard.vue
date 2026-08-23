<script setup lang="ts">
import { computed, ref, watch } from "vue";
import {
  Camera,
  ChartColumn,
  Check,
  ChevronDown,
  Copy,
  Cpu,
  FileCog,
  Globe,
  Plus,
  FolderOpen,
  GitBranch,
  MemoryStick,
  Pencil,
  Play,
  RotateCw,
  ScrollText,
  Share2,
  Square,
  SquareTerminal,
  Trash2,
  TriangleAlert,
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

import type { ProjectCommand, ProjectSummary, ServiceState } from "../bindings";
import { iconButtonClass, menuContentClass, menuItemClass, menuSeparatorClass } from "../lib/menu";
import { formatBytes, formatCores, rollupByProject, series } from "../lib/usage";
import { statusBadgeVariant } from "../lib/status";
import { envReport } from "../lib/transport";
import { commandKey, shareKey, useEngineStore } from "../stores/engine";
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

const { project } = defineProps<{ project: ProjectSummary }>();
const store = useEngineStore();

const op = computed(() => store.operations[project.id]);
const processes = computed(() => project.processes ?? []);
const opRunning = computed(() => op.value != null && op.value.terminal === null);

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

function lifecycle(label: string, type: "startProject" | "stopProject" | "restartProject") {
  void store.runLifecycle(project.id, label, { type, id: project.id });
}

function serviceVerb(
  service: string,
  label: string,
  type: "startService" | "stopService" | "restartService",
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
const addCommandOpen = ref(false);
const catalogOpen = ref(false);
const newName = ref("");
const newCommand = ref("");
const newAuto = ref(false);

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
async function addCommand() {
  const name = newName.value.trim();
  const command = newCommand.value.trim();
  if (!name || !command) return;
  await saveCommands([...commands.value, { name, command, autoStart: newAuto.value }]);
  if (!store.error) {
    addCommandOpen.value = false;
    newName.value = "";
    newCommand.value = "";
    newAuto.value = false;
  }
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
          <Badge :variant="statusBadgeVariant[project.status]" class="shrink-0">
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
            <Badge variant="outline" class="shrink-0 normal-case">
              <GitBranch class="h-3 w-3" />
              {{ project.gitBranch }}
              <span v-if="project.gitDirty" class="h-1.5 w-1.5 rounded-full bg-amber-500" />
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

    <div class="mt-2 flex gap-1">
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
          text="The containers this project is made of, colored by live state (green running, amber starting, red unhealthy). Click a chip for its logs, an in-container shell, and start/stop/restart of just that service."
        />
      </div>
      <div class="mt-1.5 flex flex-wrap gap-2">
        <DropdownMenuRoot v-for="service in project.services" :key="service.name">
          <DropdownMenuTrigger as-child>
            <Chip :dot="serviceDot(service)">
              {{ service.name }}
              <ChevronDown class="h-3 w-3 text-slate-400" />
            </Chip>
          </DropdownMenuTrigger>
          <DropdownMenuPortal>
            <DropdownMenuContent :class="menuContentClass" :side-offset="4" align="start">
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
                :disabled="store.readOnly"
                @select="serviceVerb(service.name, 'start', 'startService')"
              >
                <Play class="h-3.5 w-3.5 text-slate-400" /> Start
              </DropdownMenuItem>
              <template v-else>
                <DropdownMenuItem
                  :class="menuItemClass"
                  :disabled="store.readOnly"
                  @select="serviceVerb(service.name, 'restart', 'restartService')"
                >
                  <RotateCw class="h-3.5 w-3.5 text-slate-400" /> Restart
                </DropdownMenuItem>
                <DropdownMenuItem
                  :class="menuItemClass"
                  :disabled="store.readOnly"
                  @select="serviceVerb(service.name, 'stop', 'stopService')"
                >
                  <Square class="h-3.5 w-3.5 text-slate-400" /> Stop
                </DropdownMenuItem>
              </template>
            </DropdownMenuContent>
          </DropdownMenuPortal>
        </DropdownMenuRoot>
        <Chip
          dashed
          tip="Add or remove a service (Redis, Mailpit, a database, …), or change an installed one's version — each as a previewed compose edit."
          :disabled="store.readOnly"
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
        <DropdownMenuRoot v-if="phpChoices.length > 0 && !store.readOnly && !opRunning">
          <DropdownMenuTrigger as-child>
            <Chip>
              PHP {{ project.php.current }}
              <ChevronDown class="h-3 w-3 text-slate-400" />
            </Chip>
          </DropdownMenuTrigger>
          <DropdownMenuPortal>
            <DropdownMenuContent :class="menuContentClass" :side-offset="4" align="start">
              <DropdownMenuItem
                v-for="s in phpChoices"
                :key="s"
                :class="menuItemClass"
                @select="pendingPhp = s"
              >
                Switch to PHP {{ s }}
              </DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenuPortal>
        </DropdownMenuRoot>
        <Chip
          v-else
          :interactive="false"
          :tip="
            runtimeLockReason(phpChoices.length, 'No other PHP runtimes are vendored to switch to.')
          "
        >
          PHP {{ project.php.current }}
        </Chip>
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
                :disabled="store.readOnly || project.status !== 'running'"
                @select="processVerb(proc.id, proc.title, 'startProcess')"
              >
                <Play class="h-3.5 w-3.5 text-slate-400" /> Start{{
                  project.status !== "running" ? " (start the project first)" : ""
                }}
              </DropdownMenuItem>
              <DropdownMenuItem
                v-else
                :class="menuItemClass"
                :disabled="store.readOnly"
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
              :tip="cmd.command + (cmd.autoStart ? ' · runs automatically on start' : '')"
            >
              {{ cmd.name }}
              <span v-if="cmd.autoStart" class="text-[10px] text-slate-400">auto</span>
              <ChevronDown class="h-3 w-3 text-slate-400" />
            </Chip>
          </DropdownMenuTrigger>
          <DropdownMenuPortal>
            <DropdownMenuContent :class="menuContentClass" :side-offset="4" align="start">
              <DropdownMenuItem
                v-if="!commandRunning(cmd.name)"
                :class="menuItemClass"
                :disabled="store.readOnly"
                @select="runCommand(cmd)"
              >
                <Play class="h-3.5 w-3.5 text-slate-400" /> Run
              </DropdownMenuItem>
              <DropdownMenuItem v-else :class="menuItemClass" @select="stopCommand(cmd)">
                <Square class="h-3.5 w-3.5 text-slate-400" /> Stop
              </DropdownMenuItem>
              <DropdownMenuSeparator :class="menuSeparatorClass" />
              <DropdownMenuItem
                :class="menuItemClass"
                :disabled="store.readOnly"
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
                :disabled="store.readOnly"
                @select="saveCommands(commands.filter((c) => c.name !== cmd.name))"
              >
                <Trash2 class="h-3.5 w-3.5 text-slate-400" /> Delete
              </DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenuPortal>
        </DropdownMenuRoot>
        <Chip dashed :disabled="store.readOnly" @click="addCommandOpen = true">
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
            <button
              class="mt-1 cursor-pointer text-[11px] text-slate-400 underline decoration-slate-300 underline-offset-2 hover:text-slate-600 dark:decoration-slate-600 dark:hover:text-slate-200"
              @click="
                shareOpen = false;
                showEnv = true;
              "
            >
              Change these in the Env panel
            </button>
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
              :disabled="
                store.readOnly || project.status === 'stopped' || project.status === 'failed'
              "
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
          v-if="shareOp && shareOp.terminal === 'failed' && shareOp.fix"
          :repair="shareOp.fix.repair"
          :project="shareOp.fix.project"
          @applied="store.dismissOperation(shareKey(project.id))"
        />
      </div>
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

    <Modal v-model:open="addCommandOpen" title="Add command">
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
        <Checkbox v-model="newAuto" label="Run automatically when the project starts" />
        <div class="flex justify-end gap-2">
          <Button variant="outline" @click="addCommandOpen = false">Cancel</Button>
          <Button :disabled="!newName.trim() || !newCommand.trim()" @click="addCommand">
            Add command
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
        <span v-if="opRunning" class="text-amber-600">running…</span>
        <span v-else-if="op.terminal === 'cancelled'" class="text-amber-700">cancelled</span>
        <span v-else class="text-red-700">failed: {{ op.error }}</span>
      </p>
      <!-- A failure is only actionable if you can see what was run — and,
           when the engine matched the failure to a repair, fixable in place. -->
      <div v-if="op.terminal === 'failed'" class="mt-1 flex flex-wrap items-center gap-1">
        <FixButton
          v-if="op.fix"
          :repair="op.fix.repair"
          :project="op.fix.project"
          @applied="store.dismissOperation(project.id)"
        />
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
