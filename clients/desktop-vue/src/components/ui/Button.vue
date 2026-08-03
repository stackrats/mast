<script setup lang="ts">
import { cva } from "class-variance-authority";

import { cn } from "../../lib/utils";

const {
  variant = "default",
  size = "default",
  disabled = false,
} = defineProps<{
  variant?: "default" | "secondary" | "destructive" | "outline" | "ghost";
  size?: "default" | "sm" | "iconLg" | "icon" | "iconSm";
  disabled?: boolean;
}>();

const button = cva(
  "inline-flex items-center justify-center gap-1.5 rounded-md text-sm font-medium whitespace-nowrap shadow-xs transition-colors focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-slate-400 disabled:pointer-events-none disabled:opacity-50",
  {
    variants: {
      variant: {
        default:
          "bg-slate-900 text-slate-50 hover:bg-slate-700 dark:bg-slate-700 dark:text-slate-100 dark:hover:bg-slate-600",
        secondary:
          "bg-slate-100 text-slate-900 hover:bg-slate-200 dark:bg-slate-800 dark:text-slate-100 dark:hover:bg-slate-700",
        destructive: "bg-red-600 text-white hover:bg-red-500",
        outline:
          "border border-slate-200 bg-white hover:bg-slate-50 dark:border-slate-700 dark:bg-slate-900 dark:hover:bg-slate-800",
        ghost: "shadow-none hover:bg-slate-100 dark:hover:bg-slate-800",
      },
      // Square sizes pair with a text size by height: iconLg with default,
      // icon with sm. An icon button in a row with an Input or a full-size
      // button wants iconLg, or it sits 4px short of everything beside it.
      size: {
        default: "h-8 px-3",
        sm: "h-7 px-2 text-xs",
        iconLg: "h-8 w-8 p-0",
        icon: "h-7 w-7 p-0",
        iconSm: "h-6 w-6 p-0",
      },
    },
  },
);
</script>

<template>
  <button :class="cn(button({ variant, size }))" :disabled="disabled">
    <slot />
  </button>
</template>
