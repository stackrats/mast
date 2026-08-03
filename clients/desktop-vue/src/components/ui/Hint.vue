<script setup lang="ts">
import { CircleHelp } from "lucide-vue-next";

import Tooltip from "./Tooltip.vue";

defineProps<{ text: string }>();
</script>

<template>
  <Tooltip :text="text">
    <span
      class="inline-flex cursor-help items-center align-middle text-slate-400 hover:text-slate-600 dark:hover:text-slate-200"
    >
      <CircleHelp class="h-3.5 w-3.5" />
    </span>
  </Tooltip>
</template>
