<script setup lang="ts">
import { cva } from "class-variance-authority";
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

import { cn } from "../../lib/utils";

const model = defineModel<string>({ default: "" });
const { size = "default" } = defineProps<{
  options: { value: string; label: string }[];
  placeholder?: string;
  disabled?: boolean;
  size?: "default" | "sm";
}>();

// SelectRoot renders no element, so fallthrough attrs used to vanish
// silently — a caller's `class="mt-1"` applied to the Input beside this
// but not here, and every Select under a label sat 4px higher than its
// neighbors. Land them on the trigger, the element a caller means.
defineOptions({ inheritAttrs: false });

// Sizes mirror Button's, so a Select in a row with buttons sits flush with
// them: `default` pairs with Button `default`, `sm` with Button `sm`.
const trigger = cva(
  "flex w-full items-center justify-between gap-2 overflow-hidden rounded-md border border-slate-200 bg-white whitespace-nowrap shadow-xs transition-colors outline-none focus-visible:border-slate-400 disabled:opacity-50 data-placeholder:text-slate-400 dark:border-slate-700 dark:bg-slate-900",
  {
    variants: {
      size: {
        default: "h-8 px-2.5 py-1 text-sm",
        sm: "h-7 px-2 py-0.5 text-xs",
      },
    },
  },
);
</script>

<template>
  <SelectRoot v-model="model" :disabled="disabled">
    <SelectTrigger
      v-bind="{ ...$attrs, class: undefined }"
      :class="cn(trigger({ size }), ($attrs.class ?? '') as string)"
    >
      <SelectValue class="truncate" :placeholder="placeholder ?? ''" />
      <ChevronDown
        class="shrink-0 text-slate-400"
        :class="size === 'sm' ? 'h-3 w-3' : 'h-3.5 w-3.5'"
      />
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
