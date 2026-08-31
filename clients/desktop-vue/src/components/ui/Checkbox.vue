<script setup lang="ts">
import { Check } from "lucide-vue-next";

const model = defineModel<boolean>({ default: false });
const { block = false, row = false } = defineProps<{
  disabled?: boolean;
  label?: string;
  /** Fill the line rather than hugging the text, so the whole width toggles.
   * The rule: a checkbox that owns its line owns the whole line — the empty
   * space beside a label looks like part of the target because it is one.
   * Leave it off only for a checkbox sharing its line with other controls. */
  block?: boolean;
  /** `block` plus the padding and hover of a list row — for stacks of
   * sibling toggles, where hover also tracks which line you are on. */
  row?: boolean;
}>();
</script>

<template>
  <!-- align-top + an always-rendered check (opacity toggle): the box's
       baseline must not change on tick, or the whole label nudges. -->
  <label
    class="cursor-pointer items-start gap-2 align-top text-xs leading-4 text-slate-600 select-none dark:text-slate-300"
    :class="[
      block ? 'flex w-full' : row ? 'flex' : 'inline-flex',
      // `-mx-2` bleeds the hover area into the surrounding padding so the
      // tick still lines up with the section heading above it — an indented
      // row would trade one misalignment for another.
      row
        ? '-mx-2 rounded-md px-2 py-1.5 transition-colors hover:bg-slate-100 dark:hover:bg-slate-800/60'
        : '',
      { 'cursor-not-allowed opacity-50': disabled },
    ]"
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
