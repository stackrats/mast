<script setup lang="ts">
defineProps<{
  /** Currently selected — the pane on the right is showing this thing. */
  isActive?: boolean;
}>();

const base =
  "flex w-full items-center gap-2 overflow-hidden rounded-md px-2 py-1.5 text-left text-xs text-slate-700 outline-none hover:bg-slate-100 focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-slate-400 dark:text-slate-300 dark:hover:bg-slate-800";
// Same selected treatment as the logs panel's active tab, so "you are here"
// looks identical wherever it appears.
const active =
  "bg-slate-200/70 font-medium text-slate-900 hover:bg-slate-200/70 dark:bg-neutral-700/60 dark:text-slate-50 dark:hover:bg-neutral-700/60";
</script>

<template>
  <button :class="[base, isActive ? active : '']" :data-active="isActive || undefined">
    <slot />
  </button>
</template>
