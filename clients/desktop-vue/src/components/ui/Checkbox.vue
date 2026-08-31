<script setup lang="ts">
import { Check } from "lucide-vue-next";

const model = defineModel<boolean>({ default: false });
const { block = false } = defineProps<{
  disabled?: boolean;
  label?: string;
  /** Fill the line rather than hugging the text, so the whole width of a
   * list row toggles — a box drawn around a row promises that, and a
   * promise the empty space to the right does not keep reads as broken. */
  block?: boolean;
}>();
</script>

<template>
  <!-- align-top + an always-rendered check (opacity toggle): the box's
       baseline must not change on tick, or the whole label nudges. -->
  <label
    class="cursor-pointer items-start gap-2 align-top text-xs leading-4 text-slate-600 select-none dark:text-slate-300"
    :class="[block ? 'flex w-full' : 'inline-flex', { 'cursor-not-allowed opacity-50': disabled }]"
  >
    <input v-model="model" type="checkbox" class="peer sr-only" :disabled="disabled" />
    <span
      class="mt-px flex h-3.5 w-3.5 shrink-0 items-center justify-center rounded border shadow-xs transition-colors peer-focus-visible:outline-2 peer-focus-visible:outline-offset-1 peer-focus-visible:outline-slate-400"
      :class="
        model
          ? 'border-slate-900 bg-slate-900 dark:border-slate-500 dark:bg-slate-600'
          : 'border-slate-300 bg-white dark:border-slate-600 dark:bg-slate-900'
      "
    >
      <Check
        class="h-2.5 w-2.5 text-slate-50 transition-opacity"
        :class="model ? 'opacity-100' : 'opacity-0'"
      />
    </span>
    <slot>{{ label }}</slot>
  </label>
</template>
