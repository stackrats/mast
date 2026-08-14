<script setup lang="ts">
// A filled bar for a bounded quantity. Supersedes the never-used ProgressBar,
// whose track had no dark variant and whose fill was hardcoded green.
//
// The tone is the caller's judgement, not a threshold baked in here: what
// counts as alarming depends on whether the denominator is a real limit or
// just the size of the machine (see `memoryTone` in lib/usage.ts).
import type { Tone } from "../../lib/usage";

const {
  value,
  tone = "neutral",
  width = "w-16",
} = defineProps<{
  /** 0–1. Values outside the range are clamped rather than overflowing. */
  value: number;
  tone?: Tone;
  /** Tailwind width class — these live inline in dense rows at several sizes. */
  width?: string;
}>();

const FILL: Record<Tone, string> = {
  neutral: "bg-slate-400 dark:bg-slate-500",
  warn: "bg-amber-400",
  danger: "bg-red-500",
};
</script>

<template>
  <div
    :class="['h-1.5 overflow-hidden rounded-full bg-slate-200 dark:bg-slate-800', width]"
    role="meter"
    :aria-valuenow="Math.round(Math.min(1, Math.max(0, value)) * 100)"
    aria-valuemin="0"
    aria-valuemax="100"
  >
    <div
      class="h-full rounded-full transition-[width] duration-300"
      :class="FILL[tone]"
      :style="{ width: `${Math.min(1, Math.max(0, value)) * 100}%` }"
    />
  </div>
</template>
