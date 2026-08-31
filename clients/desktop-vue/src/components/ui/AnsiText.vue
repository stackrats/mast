<script setup lang="ts">
// One output line, with its ANSI colours honoured.
//
// Segments are interpolated, never injected, and their classes come from the
// fixed tables in `lib/ansi` — tool output cannot name its own styles. A run
// with no colour of its own renders classless and inherits whatever the row
// already sets (the amber Mast gives stderr, the muted grey it gives stdout),
// so colour from the tool wins only where the tool actually asked for it.
//
// With `fileLinks`, container paths (`/var/www/html/…`) inside a segment
// become clickable and emit `openFile` with the project-relative path and
// line — the reader is one click from the code a trace names.
import { computed } from "vue";

import { parseAnsi } from "../../lib/ansi";
import { splitFileLinks, type FileLinkPart } from "../../lib/fileLinks";

const { text, fileLinks = false } = defineProps<{ text: string; fileLinks?: boolean }>();

const emit = defineEmits<{ openFile: [file: string, line: number | null] }>();

const segments = computed(() =>
  parseAnsi(text).map((segment) => ({
    class: segment.class,
    parts: fileLinks ? splitFileLinks(segment.text) : [{ text: segment.text } as FileLinkPart],
  })),
);
</script>

<template>
  <span v-for="(segment, i) in segments" :key="i" :class="segment.class"
    ><template v-for="(part, j) in segment.parts" :key="j"
      ><button
        v-if="part.file"
        type="button"
        class="cursor-pointer underline decoration-dotted underline-offset-2 hover:decoration-solid"
        :title="`Open ${part.file}${part.line ? `:${part.line}` : ''} in your editor`"
        @click.stop="emit('openFile', part.file, part.line ?? null)"
      >
        {{ part.text }}</button
      ><template v-else>{{ part.text }}</template></template
    ></span
  >
</template>
