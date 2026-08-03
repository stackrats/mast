<script setup lang="ts">
import { cva } from "class-variance-authority";

import { cn } from "../../lib/utils";

const { variant = "secondary" } = defineProps<{
  variant?: "default" | "secondary" | "success" | "warning" | "destructive" | "outline";
}>();

const badge = cva(
  "inline-flex h-5 items-center gap-1 rounded-full px-2 text-xs font-medium capitalize",
  {
    variants: {
      variant: {
        default: "bg-slate-900 text-slate-50 dark:bg-slate-100 dark:text-slate-900",
        secondary: "bg-slate-100 text-slate-700 dark:bg-slate-800 dark:text-slate-300",
        success: "bg-emerald-100 text-emerald-800 dark:bg-emerald-900/60 dark:text-emerald-300",
        warning: "bg-amber-100 text-amber-800 dark:bg-amber-900/60 dark:text-amber-300",
        destructive: "bg-red-100 text-red-800 dark:bg-red-900/60 dark:text-red-300",
        outline: "border border-slate-200 text-slate-600 dark:border-slate-700 dark:text-slate-400",
      },
    },
  },
);
</script>

<template>
  <span :class="cn(badge({ variant }))"><slot /></span>
</template>
