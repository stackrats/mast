<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, ref, watch } from "vue";
import {
  ArrowDown,
  ArrowUp,
  Check,
  ChevronDown,
  ChevronRight,
  ChevronsUpDown,
  CircleStop,
  Copy,
  Eraser,
  TextWrap,
} from "lucide-vue-next";

import type { HistoryEntry, LogCapture, ServiceUsage } from "../bindings";
import {
  captureSummary,
  copyableCapture,
  isPostMortem,
  isUnprompted,
  lineTime,
  reasonLabel,
} from "../lib/captures";
import {
  cpuTone,
  defaultDirection,
  formatBytes,
  formatCores,
  formatPercent,
  memoryTone,
  rankServices,
  series,
  type SortDirection,
  type SortKey,
} from "../lib/usage";
import {
  commandLine,
  copyableCommand,
  formatDuration,
  formatTime,
  isFailure,
  isRunning,
  outcomeDetail,
  outcomeLabel,
} from "../lib/history";
import {
  LOGS_MAX_HEIGHT,
  LOGS_MIN_HEIGHT,
  loadLogsHeight,
  loadLogsWrap,
  saveLogsHeight,
  saveLogsWrap,
} from "../lib/prefs";
import { iconButtonClass } from "../lib/menu";
import { useEngineStore } from "../stores/engine";
import Badge from "./ui/Badge.vue";
import Button from "./ui/Button.vue";
import AnsiText from "./ui/AnsiText.vue";
import Checkbox from "./ui/Checkbox.vue";
import Meter from "./ui/Meter.vue";
import Sparkline from "./ui/Sparkline.vue";
import Tabs from "./ui/Tabs.vue";
import TabsContent from "./ui/TabsContent.vue";
import TabsList from "./ui/TabsList.vue";
import TabsTrigger from "./ui/TabsTrigger.vue";
import Tooltip from "./ui/Tooltip.vue";

const ICON_BUTTON_CLASS = iconButtonClass;

const store = useEngineStore();
const scroller = ref<HTMLElement | null>(null);
// Follow the tail unless the user scrolled up to read something.
const pinned = ref(true);

const height = ref(loadLogsHeight());
let dragging = false;

/** Soft-wrap long lines instead of scrolling sideways. Applies to both tabs —
 * the same compose output shows up in each. */
const wrap = ref(loadLogsWrap());
/** Tailwind's `whitespace-pre` keeps a line whole so the panel scrolls; the
 * wrapped form needs `break-all` because container ids and paths have no
 * spaces to break at. */
const lineClass = computed(() => (wrap.value ? "break-all whitespace-pre-wrap" : "whitespace-pre"));

function toggleWrap() {
  wrap.value = !wrap.value;
  saveLogsWrap(wrap.value);
  // Wrapping changes the content height, so a pinned view has to re-anchor.
  if (store.logsTab === "output" && pinned.value) void scrollToEnd();
}

/** Expanded history rows, by entry id. */
const expanded = ref(new Set<number>());
/** Expanded capture rows. A separate set from `expanded`: capture ids and
 * history ids are both small integers from different sequences, so one set
 * would have them expanding each other. */
const expandedCaptures = ref(new Set<number>());
/** Entry whose command was just copied — resets the icon after a moment. */
const copied = ref<number | null>(null);
/** Same, for captures — separate for the same reason the expanded sets are. */
const copiedCapture = ref<number | null>(null);
let copiedTimer: ReturnType<typeof setTimeout> | null = null;

const failures = computed(() => store.history.filter((e) => isFailure(e.outcome)).length);

// Opens on CPU descending: the ordering that answers "what do I stop". View
// state, so it lives in the component rather than the store.
const sortKey = ref<SortKey>("cpu");
const sortDirection = ref<SortDirection>("desc");

function sortBy(key: SortKey) {
  // Clicking the column you are already on flips it; a new column starts at
  // whichever end of it is worth looking at first.
  if (sortKey.value === key) {
    sortDirection.value = sortDirection.value === "asc" ? "desc" : "asc";
  } else {
    sortKey.value = key;
    sortDirection.value = defaultDirection(key);
  }
}

/** The inactive columns show a faint two-way arrow, so it is discoverable
 * that they can be sorted at all. */
function sortIcon(key: SortKey) {
  if (sortKey.value !== key) return ChevronsUpDown;
  return sortDirection.value === "asc" ? ArrowUp : ArrowDown;
}

const SORT_HEADER_CLASS =
  "rounded px-1 py-0.5 hover:bg-slate-100 hover:text-slate-600 dark:hover:bg-slate-800 dark:hover:text-slate-300";

const ranked = computed(() => rankServices(store.latestUsage, sortKey.value, sortDirection.value));
const hostCores = computed(() => store.latestUsage?.hostCores ?? 0);
/** See Sparkline's `floor`: a quarter core keeps a quiet service's trace
 * visible without amplifying jitter into a mountain range. */
const SPARKLINE_FLOOR_CORES = 0.25;

/** The name a project is known by, for a row that only carries its id. */
function projectName(id: string): string {
  return store.projects.find((p) => p.id === id)?.name ?? id;
}

/** One service's CPU over the retained samples. Keyed by project+service
 * because service names repeat across projects. */
function cpuHistory(usage: ServiceUsage): number[] {
  return series(
    store.usage,
    (sample) =>
      sample.services.find((s) => s.project === usage.project && s.service === usage.service)
        ?.cpuCores ?? 0,
  );
}

function memoryShare(usage: ServiceUsage): number {
  return usage.memoryLimitBytes > 0 ? usage.memoryBytes / usage.memoryLimitBytes : 0;
}

function toggle(entry: HistoryEntry) {
  const next = new Set(expanded.value);
  if (!next.delete(entry.id)) next.add(entry.id);
  expanded.value = next;
}

function toggleCapture(capture: LogCapture) {
  const next = new Set(expandedCaptures.value);
  if (!next.delete(capture.id)) next.add(capture.id);
  expandedCaptures.value = next;
}

/** A capture line named a project file; open it at that line. Captures
 * outlive projects (that is their point), so a capture of a since-removed
 * project answers with the engine's not-found rather than a dead click. */
function openCaptureFile(capture: LogCapture, file: string, line: number | null) {
  void store.run({ type: "openProjectFile", id: capture.project, file, line });
}

async function copyCapture(capture: LogCapture) {
  try {
    await navigator.clipboard.writeText(copyableCapture(capture));
    copiedCapture.value = capture.id;
    if (copiedTimer) clearTimeout(copiedTimer);
    copiedTimer = setTimeout(() => (copiedCapture.value = null), 1500);
  } catch {
    // Clipboard access can be refused; the lines are on screen anyway.
  }
}

async function copyCommand(entry: HistoryEntry) {
  try {
    await navigator.clipboard.writeText(copyableCommand(entry));
    copied.value = entry.id;
    if (copiedTimer) clearTimeout(copiedTimer);
    copiedTimer = setTimeout(() => (copied.value = null), 1500);
  } catch {
    // Clipboard access can be refused; the command text is on screen anyway.
  }
}

function startDrag(event: MouseEvent) {
  dragging = true;
  event.preventDefault();
  const move = (e: MouseEvent) => {
    if (!dragging) return;
    // Dragged from the top edge, so the panel grows as the pointer rises.
    // Leave room for the chrome above it however tall the window is.
    const ceiling = Math.min(LOGS_MAX_HEIGHT, window.innerHeight - 160);
    height.value = Math.min(ceiling, Math.max(LOGS_MIN_HEIGHT, window.innerHeight - e.clientY));
    if (pinned.value) void scrollToEnd();
  };
  const up = () => {
    dragging = false;
    saveLogsHeight(height.value);
    window.removeEventListener("mousemove", move);
    window.removeEventListener("mouseup", up);
  };
  window.addEventListener("mousemove", move);
  window.addEventListener("mouseup", up);
}

onBeforeUnmount(() => {
  dragging = false;
  if (copiedTimer) clearTimeout(copiedTimer);
});

function onScroll() {
  const el = scroller.value;
  if (!el) return;
  pinned.value = el.scrollTop + el.clientHeight >= el.scrollHeight - 8;
}

async function scrollToEnd() {
  await nextTick();
  const el = scroller.value;
  if (el) el.scrollTop = el.scrollHeight;
}

watch(
  () => store.activity[store.activity.length - 1]?.n,
  () => {
    if (store.logsTab === "output" && pinned.value) void scrollToEnd();
  },
);

watch(
  () => store.logsOpen,
  (open) => {
    if (!open) return;
    pinned.value = true;
    if (store.logsTab === "output") void scrollToEnd();
  },
);

// Jumping here from a failure expands the entry — its output is the point of
// the jump — and brings it into view.
watch(
  () => store.historyFocus,
  async (id) => {
    if (id === null) return;
    expanded.value = new Set(expanded.value).add(id);
    await nextTick();
    document.getElementById(`history-${id}`)?.scrollIntoView({ block: "center" });
  },
);

watch(
  () => store.logsTab,
  async (tab) => {
    if (tab === "output") {
      pinned.value = true;
      void scrollToEnd();
    } else if (tab === "history" && store.historyFocus !== null) {
      // The focus watcher above is bringing a specific entry into view.
    } else {
      // History and captures both read newest-first, so they want the top.
      await nextTick();
      if (scroller.value) scroller.value.scrollTop = 0;
    }
    // Arriving on the tab is what counts as having seen what is on it.
    if (tab === "captures") store.markCapturesSeen();
  },
);
</script>

<template>
  <div
    v-if="store.logsOpen"
    class="relative flex shrink-0 flex-col border-t border-slate-200 bg-white dark:border-slate-800 dark:bg-slate-950"
    :style="{ height: `${height}px` }"
  >
    <!-- Drag handle -->
    <div
      class="absolute -top-0.5 right-0 left-0 z-10 h-1.5 cursor-row-resize hover:bg-slate-300/60 dark:hover:bg-neutral-600/60"
      @mousedown="startDrag"
    />
    <Tabs v-model="store.logsTab" class="flex min-h-0 flex-1 flex-col">
      <div class="flex items-center justify-between px-2 py-1">
        <TabsList>
          <TabsTrigger value="output">output</TabsTrigger>
          <TabsTrigger value="history">
            history
            <Badge v-if="failures > 0" variant="destructive">{{ failures }}</Badge>
          </TabsTrigger>
          <TabsTrigger value="captures">
            captures
            <Badge v-if="store.unseenCaptureCount > 0" variant="destructive">
              {{ store.unseenCaptureCount }}
            </Badge>
          </TabsTrigger>
          <TabsTrigger value="resources">resources</TabsTrigger>
        </TabsList>
        <div class="flex items-center gap-1">
          <Tooltip
            v-if="store.logsTab === 'history'"
            text="Mast's own upkeep — resolving compose invocations, readiness probes, container inspection. Constant, so it is hidden by default."
          >
            <Checkbox v-model="store.historyShowBackground" class="pr-1">
              Background ({{ store.backgroundHistoryCount }})
            </Checkbox>
          </Tooltip>
          <Tooltip :text="wrap ? 'Stop wrapping — scroll sideways instead.' : 'Wrap long lines.'">
            <Button
              variant="ghost"
              size="iconSm"
              :aria-pressed="wrap"
              :class="wrap ? 'text-sky-600 dark:text-sky-400' : ''"
              @click="toggleWrap"
            >
              <TextWrap class="h-3.5 w-3.5" />
            </Button>
          </Tooltip>
          <Tooltip v-if="store.logsTab === 'output'" text="Clear the log output.">
            <Button variant="ghost" size="iconSm" @click="store.activity = []">
              <Eraser class="h-3.5 w-3.5" />
            </Button>
          </Tooltip>
          <Tooltip
            v-if="store.logsTab === 'captures'"
            text="Delete every stored capture. Captures live on disk, so this clears them for good."
          >
            <Button variant="ghost" size="iconSm" @click="store.clearCaptures()">
              <Eraser class="h-3.5 w-3.5" />
            </Button>
          </Tooltip>
          <Tooltip text="Hide the logs panel.">
            <Button variant="ghost" size="iconSm" @click="store.setLogsOpen(false)">
              <ChevronDown class="h-3.5 w-3.5" />
            </Button>
          </Tooltip>
        </div>
      </div>

      <div
        ref="scroller"
        class="min-h-0 flex-1 overflow-y-auto px-3 pb-2 text-xs leading-5"
        :class="[store.logsTab === 'output' ? 'font-mono' : '', wrap ? '' : 'overflow-x-auto']"
        @scroll="onScroll"
      >
        <TabsContent value="output">
          <p v-if="store.activity.length === 0" class="text-slate-400">
            No output yet — start, stop and command output streams here.
          </p>
          <div
            v-for="entry in store.activity"
            :key="entry.n"
            :class="[
              lineClass,
              entry.stderr
                ? 'text-amber-700 dark:text-amber-500'
                : 'text-slate-600 dark:text-slate-300',
            ]"
          >
            <AnsiText :text="entry.line" />
          </div>
        </TabsContent>

        <TabsContent value="history">
          <p v-if="store.visibleHistory.length === 0" class="text-slate-400">
            Nothing yet — every command Mast runs and every config file it writes is listed here.
          </p>
          <div
            v-for="entry in store.visibleHistory"
            :id="`history-${entry.id}`"
            :key="entry.id"
            class="border-b border-slate-100 py-1 last:border-0 dark:border-slate-800"
            :class="entry.id === store.historyFocus ? 'bg-amber-50 dark:bg-amber-950/30' : ''"
          >
            <div class="flex items-start gap-1.5">
              <button
                :class="['mt-0.5 shrink-0', ICON_BUTTON_CLASS]"
                :aria-label="expanded.has(entry.id) ? 'Collapse' : 'Expand'"
                @click="toggle(entry)"
              >
                <ChevronDown v-if="expanded.has(entry.id)" class="h-3 w-3" />
                <ChevronRight v-else class="h-3 w-3" />
              </button>
              <span
                class="mt-1.5 h-1.5 w-1.5 shrink-0 rounded-full"
                :class="
                  isFailure(entry.outcome)
                    ? 'bg-red-500'
                    : isRunning(entry.outcome)
                      ? 'animate-pulse bg-amber-400'
                      : 'bg-emerald-500'
                "
              />
              <div class="min-w-0 flex-1 cursor-pointer" @click="toggle(entry)">
                <div class="flex items-baseline gap-2">
                  <span class="truncate font-medium text-slate-700 dark:text-slate-200">
                    {{ entry.label }}
                  </span>
                  <Badge v-if="entry.origin === 'background'" variant="secondary" class="shrink-0">
                    background
                  </Badge>
                </div>
                <p class="truncate font-mono text-slate-500 dark:text-slate-400">
                  <span v-if="entry.detail.type === 'fileWrite'" class="text-slate-400"
                    >wrote
                  </span>
                  {{ commandLine(entry) }}
                </p>
              </div>
              <div class="flex shrink-0 items-center gap-1.5 pl-1">
                <span class="text-slate-400 tabular-nums">{{ formatTime(entry.atUnixMs) }}</span>
                <span v-if="entry.durationMs !== null" class="text-slate-400 tabular-nums">
                  {{ formatDuration(entry.durationMs) }}
                </span>
                <span
                  class="tabular-nums"
                  :class="
                    isFailure(entry.outcome)
                      ? 'text-red-600 dark:text-red-400'
                      : 'text-slate-500 dark:text-slate-400'
                  "
                >
                  {{ outcomeLabel(entry) }}
                </span>
                <Tooltip
                  v-if="entry.detail.type === 'command'"
                  text="Copy the command — cwd and all — so you can run it yourself."
                >
                  <button :class="ICON_BUTTON_CLASS" @click.stop="copyCommand(entry)">
                    <Check v-if="copied === entry.id" class="h-3.5 w-3.5 text-emerald-600" />
                    <Copy v-else class="h-3.5 w-3.5" />
                  </button>
                </Tooltip>
              </div>
            </div>

            <div
              v-if="expanded.has(entry.id)"
              class="mt-1 ml-6 space-y-0.5 font-mono text-slate-500 dark:text-slate-400"
            >
              <!-- The collapsed row truncates; expanding has to show the whole
                 command, or copying is the only way to read it. -->
              <p
                v-if="entry.detail.type === 'command'"
                :class="lineClass"
                class="text-slate-600 dark:text-slate-300"
              >
                {{ commandLine(entry) }}
              </p>
              <p v-if="entry.detail.type === 'command' && entry.detail.cwd">
                <span class="text-slate-400">in </span>{{ entry.detail.cwd }}
              </p>
              <p v-for="v in entry.detail.type === 'command' ? entry.detail.env : []" :key="v.key">
                <span class="text-slate-400">env </span>{{ v.key }}={{ v.value }}
              </p>
              <p
                v-for="(line, i) in entry.detail.type === 'fileWrite' ? entry.detail.summary : []"
                :key="`sum-${i}`"
              >
                {{ line }}
              </p>
              <p v-if="outcomeDetail(entry)" class="text-red-600 dark:text-red-400">
                {{ outcomeDetail(entry) }}
              </p>
              <p v-for="(line, i) in entry.output" :key="`out-${i}`" :class="lineClass">
                <AnsiText :text="line" />
              </p>
              <p
                v-if="
                  entry.detail.type === 'command' &&
                  entry.detail.streaming &&
                  isRunning(entry.outcome)
                "
                class="text-slate-400 italic"
              >
                streaming live into the Output tab
              </p>
              <p
                v-else-if="
                  entry.output.length === 0 &&
                  entry.detail.type === 'command' &&
                  !isRunning(entry.outcome)
                "
                class="text-slate-400 italic"
              >
                no output
              </p>
            </div>
          </div>
        </TabsContent>

        <TabsContent value="captures">
          <p v-if="store.captures.length === 0" class="text-slate-400">
            Nothing captured yet — when a container stops, crashes or goes unhealthy, Mast keeps its
            last minute of output here.
          </p>
          <div
            v-for="capture in store.captures"
            :key="capture.id"
            class="border-b border-slate-100 py-1 last:border-0 dark:border-slate-800"
          >
            <div class="flex items-start gap-1.5">
              <button
                :class="['mt-0.5 shrink-0', ICON_BUTTON_CLASS]"
                :aria-label="expandedCaptures.has(capture.id) ? 'Collapse' : 'Expand'"
                @click="toggleCapture(capture)"
              >
                <ChevronDown v-if="expandedCaptures.has(capture.id)" class="h-3 w-3" />
                <ChevronRight v-else class="h-3 w-3" />
              </button>
              <span
                class="mt-1.5 h-1.5 w-1.5 shrink-0 rounded-full"
                :class="
                  isPostMortem(capture.reason)
                    ? 'bg-red-500'
                    : isUnprompted(capture.reason)
                      ? 'bg-amber-400'
                      : 'bg-slate-400'
                "
              />
              <div class="min-w-0 flex-1 cursor-pointer" @click="toggleCapture(capture)">
                <div class="flex items-baseline gap-2">
                  <span class="truncate font-medium text-slate-700 dark:text-slate-200">
                    {{ capture.projectName }} · {{ capture.service }}
                  </span>
                </div>
                <p class="truncate text-slate-500 dark:text-slate-400">
                  {{ captureSummary(capture) }}
                </p>
              </div>
              <div class="flex shrink-0 items-center gap-1.5 pl-1">
                <!-- The reason carries the colour; the time stays neutral so
                   the two read as separate fields rather than one block. -->
                <span
                  class="tabular-nums"
                  :class="
                    isPostMortem(capture.reason)
                      ? 'text-red-600 dark:text-red-400'
                      : 'text-slate-500 dark:text-slate-400'
                  "
                >
                  {{ reasonLabel(capture.reason) }}
                </span>
                <span class="text-slate-400 tabular-nums">
                  {{ formatTime(capture.atUnixMs) }}
                </span>
                <Tooltip text="Copy these lines, with a header naming what they came from.">
                  <button :class="ICON_BUTTON_CLASS" @click.stop="copyCapture(capture)">
                    <Check
                      v-if="copiedCapture === capture.id"
                      class="h-3.5 w-3.5 text-emerald-600"
                    />
                    <Copy v-else class="h-3.5 w-3.5" />
                  </button>
                </Tooltip>
              </div>
            </div>

            <div v-if="expandedCaptures.has(capture.id)" class="mt-1 ml-6 space-y-0.5">
              <p v-if="capture.truncated" class="text-slate-400 italic">
                older lines dropped — this is the tail of the window
              </p>
              <div
                v-for="(line, i) in capture.lines"
                :key="`cap-${capture.id}-${i}`"
                class="flex gap-2 font-mono"
              >
                <span class="shrink-0 text-slate-400 tabular-nums">
                  {{ lineTime(line.at, capture.atUnixMs) }}
                </span>
                <span
                  :class="[
                    lineClass,
                    line.stderr
                      ? 'text-amber-700 dark:text-amber-500'
                      : 'text-slate-600 dark:text-slate-300',
                  ]"
                >
                  <AnsiText
                    :text="line.message"
                    file-links
                    @open-file="(file, ln) => openCaptureFile(capture, file, ln)"
                  />
                </span>
              </div>
            </div>
          </div>
        </TabsContent>

        <TabsContent value="resources">
          <p v-if="ranked.length === 0" class="text-slate-400">
            Nothing running to measure. Start a project and its containers appear here, busiest
            first.
          </p>
          <div v-else>
            <div
              class="flex items-center gap-2 border-b border-slate-100 pb-1 text-[11px] font-medium tracking-wide text-slate-400 dark:border-slate-800"
            >
              <button
                :class="['min-w-0 flex-1 text-left', SORT_HEADER_CLASS]"
                @click="sortBy('service')"
              >
                Service <component :is="sortIcon('service')" class="inline h-3 w-3" />
              </button>
              <button
                :class="['w-28 shrink-0 text-right', SORT_HEADER_CLASS]"
                @click="sortBy('cpu')"
              >
                CPU <component :is="sortIcon('cpu')" class="inline h-3 w-3" />
              </button>
              <button
                :class="['w-40 shrink-0 text-right', SORT_HEADER_CLASS]"
                @click="sortBy('memory')"
              >
                Memory <component :is="sortIcon('memory')" class="inline h-3 w-3" />
              </button>
              <!-- Matches the per-row stop button, so the columns line up. -->
              <span class="w-4.5 shrink-0" />
            </div>
            <div
              v-for="usage in ranked"
              :key="`${usage.project}/${usage.service}`"
              class="flex items-center gap-2 border-b border-slate-100 py-1 last:border-0 dark:border-slate-800"
            >
              <div class="min-w-0 flex-1 truncate">
                <span class="font-medium text-slate-700 dark:text-slate-200">
                  {{ usage.service }}
                </span>
                <span class="text-slate-400"> · {{ projectName(usage.project) }}</span>
              </div>

              <div class="flex w-28 shrink-0 items-center justify-end gap-1.5">
                <!-- Self-scaled with a floor, not pinned to the host's core
                   count: a single service almost never approaches a whole
                   machine, and pinning would flatten every row into a rule. -->
                <Sparkline :values="cpuHistory(usage)" :floor="SPARKLINE_FLOOR_CORES" />
                <span
                  class="w-8 text-right tabular-nums"
                  :class="
                    cpuTone(usage.cpuCores, hostCores) === 'warn'
                      ? 'text-amber-600 dark:text-amber-400'
                      : 'text-slate-500 dark:text-slate-400'
                  "
                >
                  {{ formatCores(usage.cpuCores) }}
                </span>
              </div>

              <!-- Against the limit, not just an absolute: a container at 90%
                 of a real cgroup limit is about to be OOM-killed, which is
                 exactly the disappearance the captures tab has to explain. -->
              <Tooltip
                :text="
                  usage.memoryLimited
                    ? `${formatBytes(usage.memoryBytes)} of a ${formatBytes(
                        usage.memoryLimitBytes,
                      )} limit. Passing it gets the container killed (exit 137).`
                    : `${formatBytes(usage.memoryBytes)} — no limit set, so this is its share of the machine's ${formatBytes(usage.memoryLimitBytes)}.`
                "
              >
                <div class="flex w-40 shrink-0 items-center justify-end gap-1.5">
                  <span class="tabular-nums text-slate-500 dark:text-slate-400">
                    {{ formatBytes(usage.memoryBytes) }}
                  </span>
                  <Meter
                    :value="memoryShare(usage)"
                    :tone="
                      memoryTone(usage.memoryBytes, usage.memoryLimitBytes, usage.memoryLimited)
                    "
                    width="w-12"
                  />
                  <span
                    class="w-8 text-right tabular-nums"
                    :class="
                      memoryTone(usage.memoryBytes, usage.memoryLimitBytes, usage.memoryLimited) ===
                      'danger'
                        ? 'text-red-600 dark:text-red-400'
                        : 'text-slate-400'
                    "
                  >
                    {{ usage.memoryLimited ? formatPercent(memoryShare(usage)) : "" }}
                  </span>
                </div>
              </Tooltip>

              <Tooltip text="Stop this service.">
                <button
                  :class="ICON_BUTTON_CLASS"
                  :disabled="store.readOnly"
                  @click="
                    store.runLifecycle(usage.project, `stop ${usage.service}`, {
                      type: 'stopService',
                      id: usage.project,
                      service: usage.service,
                    })
                  "
                >
                  <CircleStop class="h-3.5 w-3.5 text-red-600 dark:text-red-400" />
                </button>
              </Tooltip>
            </div>
          </div>
        </TabsContent>
      </div>
    </Tabs>
  </div>
</template>
