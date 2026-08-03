<script setup lang="ts">
import { Check, ChevronDown } from "lucide-vue-next";
import {
  SelectContent,
  SelectItem,
  SelectItemIndicator,
  SelectItemText,
  SelectPortal,
  SelectRoot,
  SelectTrigger,
  SelectValue,
  SelectViewport,
} from "reka-ui";

const model = defineModel<string>({ default: "" });
defineProps<{
  options: { value: string; label: string }[];
  placeholder?: string;
  disabled?: boolean;
}>();
</script>

<template>
  <SelectRoot v-model="model" :disabled="disabled">
    <SelectTrigger
      class="flex h-8 w-full items-center justify-between gap-2 overflow-hidden rounded-md border border-slate-200 bg-white px-2.5 py-1 text-sm whitespace-nowrap shadow-xs transition-colors outline-none focus-visible:border-slate-400 disabled:opacity-50 data-placeholder:text-slate-400 dark:border-slate-700 dark:bg-slate-900"
    >
      <SelectValue class="truncate" :placeholder="placeholder ?? ''" />
      <ChevronDown class="h-3.5 w-3.5 shrink-0 text-slate-400" />
    </SelectTrigger>
    <SelectPortal>
      <SelectContent
        class="z-50 min-w-(--reka-select-trigger-width) rounded-md border border-slate-200 bg-white p-1 shadow-lg dark:border-slate-700 dark:bg-slate-900"
        position="popper"
        :side-offset="4"
      >
        <SelectViewport>
          <SelectItem
            v-for="option in options"
            :key="option.value"
            :value="option.value"
            class="flex cursor-default items-center justify-between gap-2 rounded px-2 py-1.5 text-xs text-slate-700 outline-none data-highlighted:bg-slate-100 dark:text-slate-200 dark:data-highlighted:bg-slate-800"
          >
            <SelectItemText>{{ option.label }}</SelectItemText>
            <SelectItemIndicator>
              <Check class="h-3 w-3" />
            </SelectItemIndicator>
          </SelectItem>
        </SelectViewport>
      </SelectContent>
    </SelectPortal>
  </SelectRoot>
</template>
