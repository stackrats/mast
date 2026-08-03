<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, ref, watch } from "vue";
import { Check, ChevronDown, ChevronRight, Copy, Eraser, TextWrap } from "lucide-vue-next";

import type { HistoryEntry } from "../bindings";
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
import { useEngineStore } from "../stores/engine";
import Badge from "./ui/Badge.vue";
import Button from "./ui/Button.vue";
import Checkbox from "./ui/Checkbox.vue";
import Tabs from "./ui/Tabs.vue";
import TabsContent from "./ui/TabsContent.vue";
import TabsList from "./ui/TabsList.vue";
import TabsTrigger from "./ui/TabsTrigger.vue";
import Tooltip from "./ui/Tooltip.vue";

// The sidebar's inline icon-button recipe. `Button size="iconSm"` is 24px,
// which would set the height of these dense rows.
const ICON_BUTTON_CLASS =
  "rounded p-0.5 text-slate-400 hover:bg-slate-200 hover:text-slate-700 dark:hover:bg-slate-700 dark:hover:text-slate-200";

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
/** Entry whose command was just copied — resets the icon after a moment. */
const copied = ref<number | null>(null);
let copiedTimer: ReturnType<typeof setTimeout> | null = null;

const failures = computed(() => store.history.filter((e) => isFailure(e.outcome)).length);

function toggle(entry: HistoryEntry) {
  const next = new Set(expanded.value);
  if (!next.delete(entry.id)) next.add(entry.id);
  expanded.value = next;
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
    } else if (store.historyFocus === null) {
      // History reads newest-first, so it wants the top, not the tail.
      await nextTick();
      if (scroller.value) scroller.value.scrollTop = 0;
    }
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
            {{ entry.line }}
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
                {{ line }}
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
      </div>
    </Tabs>
  </div>
</template>
