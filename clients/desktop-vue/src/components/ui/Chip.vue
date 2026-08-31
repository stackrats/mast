<script setup lang="ts">
// The one interactive pill: services/processes/commands chips, add-chips,
// toggle pills. Fixed height so rows always align regardless of content.
//
// The tooltip lives INSIDE this component (`tip` prop) so wrappers like
// DropdownMenuTrigger can as-child straight onto the button — an outer
// TooltipTrigger wrapping an inner as-child trigger swallows its handlers.
import { TooltipArrow, TooltipContent, TooltipPortal, TooltipRoot, TooltipTrigger } from "reka-ui";

defineOptions({ inheritAttrs: false });

const {
  dot,
  dashed = false,
  active = false,
  interactive = true,
  tip,
} = defineProps<{
  /** Status dot color class (e.g. "bg-emerald-500"); omitted = no dot. */
  dot?: string;
  /** Dashed affordance for "add" chips. */
  dashed?: boolean;
  /** Selected state (toggle pills). */
  active?: boolean;
  /** False for purely informational chips: no pointer cursor, no hover
   * state — a chip that looks clickable must be clickable. */
  interactive?: boolean;
  /** Hover tooltip text. */
  tip?: string;
}>();

const base =
  "inline-flex h-6 items-center gap-1.5 rounded-full border px-2.5 text-xs whitespace-nowrap focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-slate-400 disabled:cursor-default disabled:opacity-50";

const variants = {
  active:
    "cursor-pointer border-emerald-300 bg-emerald-50 text-emerald-800 dark:border-emerald-800 dark:bg-emerald-950/40 dark:text-emerald-300",
  dashed:
    "cursor-pointer border-dashed border-slate-300 text-slate-500 hover:border-slate-400 hover:bg-slate-50 dark:border-slate-600 dark:text-slate-400 dark:hover:bg-slate-800",
  default:
    "cursor-pointer border-slate-200 text-slate-700 hover:bg-slate-50 data-[state=open]:bg-slate-100 dark:border-slate-700 dark:text-slate-300 dark:hover:bg-slate-800 dark:data-[state=open]:bg-slate-800",
  static:
    "cursor-default border-slate-200 text-slate-700 dark:border-slate-700 dark:text-slate-300",
};

const variant = () =>
  !interactive
    ? variants.static
    : active
      ? variants.active
      : dashed
        ? variants.dashed
        : variants.default;
</script>

<template>
  <TooltipRoot v-if="tip" ignore-non-keyboard-focus>
    <TooltipTrigger as-child>
      <button v-bind="$attrs" :class="[base, variant()]">
        <span v-if="dot" class="h-2 w-2 shrink-0 rounded-full" :class="dot" />
        <slot />
      </button>
    </TooltipTrigger>
    <TooltipPortal>
      <TooltipContent
        class="pointer-events-none z-50 w-fit max-w-72 rounded-md bg-slate-900 px-3 py-1.5 text-xs text-balance text-slate-50 dark:bg-slate-700 dark:text-slate-100"
        :side-offset="6"
      >
        {{ tip }}
        <TooltipArrow class="fill-slate-900 dark:fill-slate-700" />
      </TooltipContent>
    </TooltipPortal>
  </TooltipRoot>
  <button v-else v-bind="$attrs" :class="[base, variant()]">
    <span v-if="dot" class="h-2 w-2 shrink-0 rounded-full" :class="dot" />
    <slot />
  </button>
</template>
