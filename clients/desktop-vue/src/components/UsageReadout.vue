<script setup lang="ts">
// The always-visible total, in the status bar. Answers "is Docker costing me
// anything right now" at a glance, and is the way in to the Resources tab when
// the answer is yes.
//
// CPU is shown in *cores*, not a percentage: Docker's percentage is of all
// cores at once, so "230%" is both correct and unreadable, while "2.3 of 8"
// needs no explanation.
import { computed } from "vue";

import { formatBytes, formatCores, rollupTotal, series } from "../lib/usage";
import { useEngineStore } from "../stores/engine";
import Meter from "./ui/Meter.vue";
import Sparkline from "./ui/Sparkline.vue";
import Tooltip from "./ui/Tooltip.vue";

const store = useEngineStore();

const latest = computed(() => store.latestUsage);
const total = computed(() => rollupTotal(latest.value));
const hostCores = computed(() => latest.value?.hostCores ?? 0);
const hostMemory = computed(() => latest.value?.hostMemoryBytes ?? 0);

const cpuHistory = computed(() =>
  series(store.usage, (sample) =>
    sample.services.reduce((sum, service) => sum + service.cpuCores, 0),
  ),
);

const memoryShare = computed(() =>
  hostMemory.value > 0 ? total.value.memoryBytes / hostMemory.value : 0,
);

/** Nothing running is not the same as nothing measured — say so rather than
 * showing a confident zero. */
const idle = computed(() => latest.value === null || latest.value.services.length === 0);
</script>

<template>
  <Tooltip
    :text="
      idle
        ? 'No containers running — nothing to measure.'
        : `${formatCores(total.cpuCores)} of ${hostCores} cores and ${formatBytes(
            total.memoryBytes,
          )} of ${formatBytes(hostMemory)} across every running container. Click for the breakdown.`
    "
  >
    <button
      class="flex items-center gap-3 rounded px-1 py-0.5 text-[11px] text-slate-400 hover:bg-slate-100 dark:hover:bg-slate-800"
      @click="store.showResources()"
    >
      <span v-if="idle" class="text-slate-400">idle</span>
      <template v-else>
        <span class="flex items-center gap-1.5">
          <span class="tabular-nums">{{ formatCores(total.cpuCores) }}</span>
          <span class="text-slate-400">/ {{ hostCores }} cores</span>
          <!-- Floored rather than pinned to the host's core count. A dev
             machine idles at a fraction of a core, and against a 14-core
             ceiling that draws a flat dotted rule with no shape in it. The
             cores figure beside it carries the absolute scale; the strip is
             here to show movement. -->
          <Sparkline :values="cpuHistory" :floor="1" />
        </span>
        <span class="flex items-center gap-1.5">
          <span class="tabular-nums">{{ formatBytes(total.memoryBytes) }}</span>
          <Meter :value="memoryShare" width="w-10" />
        </span>
      </template>
    </button>
  </Tooltip>
</template>
