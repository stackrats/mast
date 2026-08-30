<script setup lang="ts">
// Every project at once, and what each one is costing.
//
// The single-project view answers "what is this project doing". This answers
// the question that only exists once you have ten of them: which of these do
// I not need running. That is why the columns are costs and the only action
// is stop — starting a project is a decision you make from the project, but
// stopping one is a decision you make by comparison.
import { computed } from "vue";
import { CircleStop, Moon } from "lucide-vue-next";

import type { ProjectId } from "../bindings";
import { statusDot } from "../lib/status";
import { fleetRows, formatBytes, formatCores, QUIET_CORES_PER_CONTAINER } from "../lib/usage";
import { useEngineStore } from "../stores/engine";
import Badge from "./ui/Badge.vue";
import Button from "./ui/Button.vue";
import Tooltip from "./ui/Tooltip.vue";

const store = useEngineStore();

const rows = computed(() => fleetRows(store.projects, store.usage));
// Only projects that are up get a row. Every column here is a cost, and a
// stopped project has none — the rows were four cells of "—" repeating what
// the sidebar's status dots already say. The header keeps the total, so the
// inventory is not lost, and starting one is the sidebar's job (or Ctrl-K).
const active = computed(() => rows.value.filter((r) => r.project.status !== "stopped"));
const quiet = computed(() => rows.value.filter((r) => r.quiet));
const totals = computed(() => ({
  cpuCores: active.value.reduce((n, r) => n + r.cpuCores, 0),
  memoryBytes: active.value.reduce((n, r) => n + r.memoryBytes, 0),
  containers: active.value.reduce((n, r) => n + r.containers, 0),
}));
const hostCores = computed(() => store.latestUsage?.hostCores ?? 0);

/** Minutes behind the quiet verdict, for the tooltip's sake. */
const quietWindow = computed(() => {
  const seconds = quiet.value[0]?.quietFor ?? 0;
  return seconds >= 60 ? `${Math.round(seconds / 60)} min` : `${seconds}s`;
});

function busy(id: ProjectId): boolean {
  return store.operations[id]?.terminal === null;
}

function stop(id: ProjectId) {
  void store.runLifecycle(id, "stop", { type: "stopProject", id });
}

function open(id: ProjectId) {
  store.selection = { kind: "project", id };
}

function stopQuiet() {
  for (const row of quiet.value) {
    if (!busy(row.project.id)) stop(row.project.id);
  }
}

const cellClass = "px-3 py-2 text-sm";
</script>

<template>
  <section
    v-if="rows.length > 0"
    class="rounded-lg border border-slate-200 bg-white dark:border-slate-800 dark:bg-slate-900"
  >
    <header class="flex flex-wrap items-center justify-between gap-3 px-3 py-2.5">
      <div class="flex items-baseline gap-2">
        <h2 class="text-sm font-medium text-slate-900 dark:text-slate-100">Fleet</h2>
        <p class="text-xs text-slate-500 tabular-nums dark:text-slate-400">
          {{ active.length }} of {{ rows.length }} up
          <template v-if="active.length > 0">
            · {{ totals.containers }} container{{ totals.containers === 1 ? "" : "s" }} ·
            {{ formatCores(totals.cpuCores)
            }}<template v-if="hostCores > 0"> of {{ hostCores }} </template> cores ·
            {{ formatBytes(totals.memoryBytes) }}
          </template>
        </p>
      </div>
      <Tooltip
        v-if="quiet.length > 0 && !store.readOnly"
        :text="`Stop the ${quiet.length} project${quiet.length === 1 ? '' : 's'} that have stayed under ${QUIET_CORES_PER_CONTAINER} cores per container for ${quietWindow}. Nothing is lost — containers stop, volumes stay.`"
      >
        <Button variant="outline" size="sm" @click="stopQuiet">
          <Moon class="h-3.5 w-3.5" />
          Stop {{ quiet.length }} quiet
        </Button>
      </Tooltip>
    </header>

    <p
      v-if="active.length === 0"
      class="border-t border-slate-200 px-3 py-3 text-sm text-slate-500 dark:border-slate-800 dark:text-slate-400"
    >
      Nothing running. Start a project from the sidebar, or press
      <kbd class="rounded border border-slate-300 px-1 font-mono text-xs dark:border-slate-600">
        Ctrl</kbd
      >+<kbd class="rounded border border-slate-300 px-1 font-mono text-xs dark:border-slate-600">
        K</kbd
      >.
    </p>

    <div v-else class="overflow-x-auto border-t border-slate-200 dark:border-slate-800">
      <table class="w-full min-w-[34rem] border-collapse">
        <thead>
          <tr class="text-left text-xs text-slate-500 dark:text-slate-400">
            <th class="px-3 py-1.5 font-medium">Project</th>
            <th class="px-3 py-1.5 text-right font-medium">Containers</th>
            <th class="px-3 py-1.5 text-right font-medium">CPU</th>
            <th class="px-3 py-1.5 text-right font-medium">Memory</th>
            <th class="px-3 py-1.5"><span class="sr-only">Actions</span></th>
          </tr>
        </thead>
        <tbody>
          <tr
            v-for="row in active"
            :key="row.project.id"
            tabindex="0"
            role="button"
            :aria-label="`Open ${row.project.name}`"
            class="cursor-pointer border-t border-slate-100 hover:bg-slate-50 focus-visible:bg-slate-50 focus-visible:outline-none dark:border-slate-800/70 dark:hover:bg-slate-800/40 dark:focus-visible:bg-slate-800/40"
            @click="open(row.project.id)"
            @keydown.enter="open(row.project.id)"
            @keydown.space.prevent="open(row.project.id)"
          >
            <td :class="cellClass">
              <span class="flex min-w-0 items-center gap-2">
                <span
                  class="h-2 w-2 shrink-0 rounded-full"
                  :class="statusDot[row.project.status]"
                />
                <span class="truncate text-slate-900 dark:text-slate-100">
                  {{ row.project.name }}
                </span>
                <Tooltip
                  v-if="row.quiet"
                  :text="`Under ${QUIET_CORES_PER_CONTAINER} cores per container (${formatCores(QUIET_CORES_PER_CONTAINER * row.containers)} across its ${row.containers}) for the last ${quietWindow}. Still running — nothing has been stopped.`"
                >
                  <Badge variant="secondary">quiet</Badge>
                </Tooltip>
              </span>
            </td>
            <td :class="[cellClass, 'text-right tabular-nums text-slate-600 dark:text-slate-300']">
              {{ row.containers }}
            </td>
            <td :class="[cellClass, 'text-right tabular-nums text-slate-600 dark:text-slate-300']">
              {{ formatCores(row.cpuCores) }}
            </td>
            <td :class="[cellClass, 'text-right tabular-nums text-slate-600 dark:text-slate-300']">
              {{ formatBytes(row.memoryBytes) }}
            </td>
            <td :class="[cellClass, 'text-right']">
              <Button
                v-if="!store.readOnly"
                variant="ghost"
                size="sm"
                :disabled="busy(row.project.id)"
                :aria-label="`Stop ${row.project.name}`"
                @click.stop="stop(row.project.id)"
              >
                <CircleStop class="h-3.5 w-3.5" />
                Stop
              </Button>
            </td>
          </tr>
        </tbody>
      </table>
    </div>
  </section>
</template>
