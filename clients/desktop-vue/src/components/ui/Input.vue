<script setup lang="ts">
const model = defineModel<string>({ default: "" });
defineProps<{ placeholder?: string; mono?: boolean }>();
</script>

<template>
  <input
    v-model="model"
    type="text"
    :placeholder="placeholder"
    class="flex h-8 w-full rounded-md border border-slate-200 bg-white px-2.5 py-1 text-sm shadow-xs transition-colors placeholder:text-slate-400 focus-visible:border-slate-400 focus-visible:outline-none disabled:opacity-50 dark:border-slate-700 dark:bg-slate-900 dark:placeholder:text-slate-500 dark:focus-visible:border-slate-500"
    :class="{ 'font-mono text-xs': mono }"
  />
</template>
