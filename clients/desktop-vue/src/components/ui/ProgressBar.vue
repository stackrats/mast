<script setup lang="ts">
import { ProgressIndicator, ProgressRoot } from "reka-ui";

defineProps<{ value: number }>();
</script>

<template>
  <ProgressRoot
    :model-value="value"
    class="relative h-2 w-full overflow-hidden rounded-full bg-slate-200"
  >
    <ProgressIndicator
      class="h-full w-full bg-emerald-500 transition-transform duration-150"
      :style="{ transform: `translateX(-${100 - value}%)` }"
    />
  </ProgressRoot>
</template>
