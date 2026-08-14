<script setup lang="ts">
// A tiny history strip. Built from divs rather than SVG: there is no other
// vector markup in this app, and a row of bars needs no viewBox arithmetic to
// stay crisp at any zoom.
//
// The point of it is that one CPU reading is noise. A momentary spike and a
// steady climb are the same number and completely different problems, and only
// the shape tells them apart.
import { computed } from "vue";

const {
  values,
  slots = 24,
  max,
  floor = 0,
} = defineProps<{
  /** Oldest first. Shorter than `slots` renders as leading gaps, which is
   * honest: sampling stops while the window is hidden, and inventing points
   * for that time would show activity that was never measured. */
  values: number[];
  slots?: number;
  /** Fixed ceiling — use where the denominator is meaningful, like a total
   * against the machine's core count. */
  max?: number;
  /**
   * Smallest ceiling to self-scale against, when `max` is not given.
   *
   * Pure self-scaling amplifies noise: a container idling between 0.001 and
   * 0.003 cores would draw a dramatic mountain range. Pinning to a global
   * maximum has the opposite failure — a project using 0.07 of 8 cores draws
   * every bar at the minimum height and reads as a dotted rule rather than a
   * chart. A floor gives the shape room to show while keeping small numbers
   * looking small.
   */
  floor?: number;
}>();

const bars = computed(() => {
  const recent = values.slice(-slots);
  const ceiling = max ?? Math.max(...recent, floor, 0);
  const pad = slots - recent.length;
  return [
    ...Array.from({ length: Math.max(0, pad) }, () => null),
    ...recent.map((v) => (ceiling > 0 ? Math.min(1, Math.max(0, v / ceiling)) : 0)),
  ];
});
</script>

<template>
  <div class="flex h-3.5 items-end gap-px" aria-hidden="true">
    <div
      v-for="(bar, i) in bars"
      :key="i"
      class="w-0.5 rounded-t-[1px]"
      :class="bar === null ? 'bg-transparent' : 'bg-slate-300 dark:bg-slate-600'"
      :style="{
        // A floor so a zero reading is still a visible baseline rather than a
        // hole that reads as missing data.
        height: bar === null ? '100%' : `${Math.max(8, bar * 100)}%`,
      }"
    />
  </div>
</template>
