<script setup lang="ts">
// One output line, with its ANSI colours honoured.
//
// Segments are interpolated, never injected, and their classes come from the
// fixed tables in `lib/ansi` — tool output cannot name its own styles. A run
// with no colour of its own renders classless and inherits whatever the row
// already sets (the amber Mast gives stderr, the muted grey it gives stdout),
// so colour from the tool wins only where the tool actually asked for it.
import { computed } from "vue";

import { parseAnsi } from "../../lib/ansi";

const { text } = defineProps<{ text: string }>();

const segments = computed(() => parseAnsi(text));
</script>

<template>
  <span v-for="(segment, i) in segments" :key="i" :class="segment.class">{{ segment.text }}</span>
</template>
