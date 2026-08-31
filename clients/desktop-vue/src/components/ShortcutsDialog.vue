<script setup lang="ts">
// The keyboard's table of contents. Data comes from lib/shortcuts — the
// same source the menubar accelerator and the fleet's empty-state hint
// read — so this list cannot drift from what the keys actually do.
import { keyLabel, SHORTCUTS } from "../lib/shortcuts";
import Modal from "./ui/Modal.vue";

const open = defineModel<boolean>("open", { default: false });

const kbdClass =
  "rounded border border-slate-300 bg-slate-50 px-1.5 py-0.5 font-mono text-[11px] text-slate-700 dark:border-slate-600 dark:bg-slate-800 dark:text-slate-200";
</script>

<template>
  <Modal v-model:open="open" title="Keyboard shortcuts">
    <ul class="space-y-2">
      <li
        v-for="shortcut in SHORTCUTS"
        :key="shortcut.does"
        class="flex items-center justify-between gap-4 text-xs text-slate-600 dark:text-slate-300"
      >
        <span>{{ shortcut.does }}</span>
        <span class="flex shrink-0 gap-1">
          <kbd v-for="part in shortcut.combo" :key="part" :class="kbdClass">
            {{ keyLabel(part) }}
          </kbd>
        </span>
      </li>
    </ul>
  </Modal>
</template>
