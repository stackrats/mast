<script setup lang="ts">
import { TabsTrigger } from "reka-ui";

defineProps<{ value: string }>();

// The selected look is the sidebar's selected-item recipe, so "this one is
// active" reads the same everywhere in the app.
const base =
  "flex items-center gap-1.5 rounded-md px-2 py-0.5 text-xs font-semibold capitalize text-slate-500 hover:bg-slate-100 focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-slate-400 dark:text-slate-400 dark:hover:bg-slate-800";
const active =
  "data-[state=active]:bg-slate-200/70 data-[state=active]:text-slate-900 data-[state=active]:hover:bg-slate-200/70 dark:data-[state=active]:bg-neutral-700/60 dark:data-[state=active]:text-slate-50 dark:data-[state=active]:hover:bg-neutral-700/60";
</script>

<template>
  <TabsTrigger :value="value" :class="[base, active]">
    <slot />
  </TabsTrigger>
</template>
