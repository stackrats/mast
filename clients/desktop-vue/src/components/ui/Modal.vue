<script setup lang="ts">
import { DialogContent, DialogOverlay, DialogPortal, DialogRoot, DialogTitle } from "reka-ui";
import { X } from "lucide-vue-next";

const open = defineModel<boolean>("open", { default: false });
defineProps<{ title: string; wide?: boolean }>();
</script>

<template>
  <DialogRoot v-model:open="open">
    <DialogPortal>
      <DialogOverlay class="fixed inset-0 z-40 bg-slate-950/40 backdrop-blur-[2px]" />
      <DialogContent
        class="fixed top-1/2 left-1/2 z-50 max-h-[85vh] w-full -translate-x-1/2 -translate-y-1/2 overflow-y-auto rounded-xl border border-slate-200 bg-white p-5 shadow-2xl focus:outline-none dark:border-slate-700 dark:bg-slate-900 dark:text-slate-100"
        :class="wide ? 'max-w-2xl' : 'max-w-md'"
      >
        <div class="mb-3 flex items-center justify-between">
          <DialogTitle class="text-sm font-semibold text-slate-900 dark:text-slate-100">
            {{ title }}
          </DialogTitle>
          <button
            class="rounded p-1 text-slate-400 hover:bg-slate-100 hover:text-slate-900 dark:hover:bg-slate-800 dark:hover:text-slate-100"
            @click="open = false"
          >
            <X class="h-4 w-4" />
          </button>
        </div>
        <slot />
      </DialogContent>
    </DialogPortal>
  </DialogRoot>
</template>
