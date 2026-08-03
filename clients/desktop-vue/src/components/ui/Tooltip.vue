<script setup lang="ts">
import { TooltipArrow, TooltipContent, TooltipPortal, TooltipRoot, TooltipTrigger } from "reka-ui";

defineProps<{ text: string }>();
</script>

<template>
  <TooltipRoot ignore-non-keyboard-focus>
    <TooltipTrigger as-child>
      <slot />
    </TooltipTrigger>
    <TooltipPortal>
      <TooltipContent
        class="pointer-events-none z-50 w-fit max-w-72 rounded-md bg-slate-900 px-3 py-1.5 text-xs text-balance text-slate-50 dark:bg-slate-700 dark:text-slate-100"
        :side-offset="6"
      >
        {{ text }}
        <TooltipArrow class="fill-slate-900 dark:fill-slate-700" />
      </TooltipContent>
    </TooltipPortal>
  </TooltipRoot>
</template>
