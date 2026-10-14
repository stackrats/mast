<script setup lang="ts">
// Everything running in the project's app container beyond its own startup,
// one chip per process tree, with who started it. Mast's own commands show
// green; anything else — a copy an earlier Mast session left behind, a
// terminal's `sail artisan horizon`, an agent's background job — shows amber
// with a Stop, because each `docker exec` leaves its processes running when
// its client goes, and they otherwise hold memory and do their work twice
// without a trace on the card.
import { computed, onScopeDispose, ref } from "vue";
import { ChevronDown, CircleStop, Loader2 } from "lucide-vue-next";
import {
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuPortal,
  DropdownMenuRoot,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "reka-ui";

import type { ContainerRun, ProjectSummary } from "../bindings";
import {
  needsAttention,
  orderedRuns,
  ownerLabel,
  ownerSentence,
  runFacts,
} from "../lib/containerRuns";
import { menuContentClass, menuItemClass, menuSeparatorClass } from "../lib/menu";
import { formatBytes } from "../lib/usage";
import { containerRunKey, useEngineStore } from "../stores/engine";
import Chip from "./ui/Chip.vue";
import Hint from "./ui/Hint.vue";

const { project } = defineProps<{ project: ProjectSummary }>();
const store = useEngineStore();

const runs = computed(() => orderedRuns(project.containerRuns ?? []));

// Ages read in minutes and hours; a coarse clock is enough.
const now = ref(Date.now());
const timer = setInterval(() => (now.value = Date.now()), 30_000);
onScopeDispose(() => clearInterval(timer));

function stopping(run: ContainerRun): boolean {
  return !!store.stoppingRuns[containerRunKey(project.id, run.pid, run.startTicks)];
}
</script>

<template>
  <div v-if="runs.length > 0" class="mt-3">
    <div class="flex items-center gap-1.5">
      <p class="text-[11px] font-medium tracking-wide text-slate-400">In the app container</p>
      <Hint
        text="Everything running inside the app container beyond its own startup, grouped by what started it. Green is a command Mast is running. Amber is anything else — a copy an earlier Mast session left behind, something started from a terminal or by an agent — which keeps running after whatever started it is gone. Click one for what it is and to stop it."
      />
    </div>
    <div class="mt-1.5 flex flex-wrap gap-2">
      <DropdownMenuRoot v-for="run in runs" :key="`${run.pid}:${run.startTicks}`">
        <DropdownMenuTrigger as-child>
          <Chip
            :dot="
              stopping(run) ? undefined : needsAttention(run) ? 'bg-amber-500' : 'bg-emerald-500'
            "
            :tip="`${run.title} — ${ownerLabel(run.owner)} · ${runFacts(run, now)}`"
            :aria-label="`${run.title}, started by ${ownerLabel(run.owner)}`"
          >
            <Loader2 v-if="stopping(run)" class="h-3 w-3 shrink-0 animate-spin text-amber-500" />
            {{ run.title }}
            <span class="text-[10px] text-slate-400">
              {{ ownerLabel(run.owner) }} · {{ formatBytes(run.memoryBytes) }}
            </span>
            <ChevronDown class="h-3 w-3 text-slate-400" />
          </Chip>
        </DropdownMenuTrigger>
        <DropdownMenuPortal>
          <DropdownMenuContent :class="menuContentClass" :side-offset="4" align="start">
            <div class="max-w-80 space-y-1 px-2 py-1.5">
              <p class="font-mono text-[11px] break-all text-slate-600 dark:text-slate-300">
                {{ run.commandLine }}
              </p>
              <p class="text-[11px] text-slate-500 dark:text-slate-400">
                {{ runFacts(run, now) }}
                <template v-if="run.kinds.length"> · runs {{ run.kinds.join(", ") }}</template>
              </p>
              <p class="text-[11px] text-slate-600 dark:text-slate-300">
                {{ ownerSentence(run.owner) }}
                <template v-if="!needsAttention(run)"> Stop it from its chip above.</template>
              </p>
            </div>
            <template v-if="needsAttention(run)">
              <DropdownMenuSeparator :class="menuSeparatorClass" />
              <DropdownMenuItem
                :class="menuItemClass"
                :disabled="store.readOnly || stopping(run)"
                @select="store.stopContainerRun(project.id, run.pid, run.startTicks)"
              >
                <CircleStop class="h-3.5 w-3.5 text-red-600 dark:text-red-400" />
                {{ stopping(run) ? "Stopping…" : "Stop it and everything it started" }}
              </DropdownMenuItem>
            </template>
          </DropdownMenuContent>
        </DropdownMenuPortal>
      </DropdownMenuRoot>
    </div>
  </div>
</template>
