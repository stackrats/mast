<script setup lang="ts">
// Ctrl/Cmd-K. Everything the app can do, one keystroke from anywhere.
//
// A deliberately plain overlay rather than the shared Modal: a palette has
// no title bar, it is focused on open, and Escape and Enter belong to the
// list rather than to a dialog frame.
import { computed, nextTick, ref, watch } from "vue";
import { CornerDownLeft } from "lucide-vue-next";

import { buildPalette, filterPalette, groupPalette, type PaletteEffect } from "../lib/palette";
import { useEngineStore } from "../stores/engine";

const open = defineModel<boolean>("open", { required: true });
const emit = defineEmits<{ dialog: [what: "settings" | "newProject" | "newWorkspace"] }>();

const store = useEngineStore();
const query = ref("");
const active = ref(0);
const input = ref<HTMLInputElement | null>(null);
const listbox = ref<HTMLElement | null>(null);

const items = computed(() => buildPalette(store.projects, store.workspaces));
const results = computed(() => filterPalette(items.value, query.value));
const groups = computed(() => groupPalette(results.value));
/** Flat ranked order — what the arrow keys walk, regardless of grouping. */
const flat = computed(() => groups.value.flatMap((g) => g.items));

watch(open, async (isOpen) => {
  if (!isOpen) return;
  query.value = "";
  active.value = 0;
  await nextTick();
  input.value?.focus();
});
watch(query, () => (active.value = 0));

function move(delta: number) {
  const count = flat.value.length;
  if (count === 0) return;
  // Wraps, because a palette with four entries should not need four presses
  // to get back to the top.
  active.value = (active.value + delta + count) % count;
  void nextTick(() => {
    listbox.value
      ?.querySelector(`[data-index="${active.value}"]`)
      ?.scrollIntoView({ block: "nearest" });
  });
}

function perform(effect: PaletteEffect) {
  open.value = false;
  switch (effect.kind) {
    case "select":
      store.selection = { kind: "project", id: effect.id };
      break;
    case "home":
      store.selection = { kind: "home" };
      break;
    case "workspace":
      store.selection = { kind: "workspace", id: effect.id };
      break;
    case "action":
      void store.runLifecycle(effect.project, effect.label, effect.action);
      break;
    case "run":
      void store.run(effect.action);
      break;
    case "dialog":
      emit("dialog", effect.what);
      break;
  }
}

function choose() {
  const item = flat.value[active.value];
  if (item) perform(item.effect);
}

/** Index of an item in the flat ranked order, for highlight and scrolling. */
function indexOf(id: string): number {
  return flat.value.findIndex((i) => i.id === id);
}
</script>

<template>
  <div
    v-if="open"
    class="fixed inset-0 z-50 flex items-start justify-center bg-slate-900/40 p-4 pt-[12vh] backdrop-blur-[2px]"
    @click.self="open = false"
  >
    <div
      class="w-full max-w-xl overflow-hidden rounded-xl border border-slate-200 bg-white shadow-2xl dark:border-slate-700 dark:bg-slate-900"
      role="dialog"
      aria-modal="true"
      aria-label="Command palette"
    >
      <input
        ref="input"
        v-model="query"
        type="text"
        placeholder="Search projects, actions, commands…"
        aria-label="Command palette search"
        role="combobox"
        aria-expanded="true"
        aria-controls="palette-listbox"
        class="w-full border-b border-slate-200 bg-transparent px-4 py-3 text-sm text-slate-900 outline-none placeholder:text-slate-400 dark:border-slate-700 dark:text-slate-100"
        @keydown.down.prevent="move(1)"
        @keydown.up.prevent="move(-1)"
        @keydown.enter.prevent="choose"
        @keydown.esc.prevent="open = false"
      />

      <div v-if="flat.length === 0" class="px-4 py-6 text-center text-sm text-slate-500">
        Nothing matches “{{ query }}”.
      </div>

      <div
        v-else
        id="palette-listbox"
        ref="listbox"
        role="listbox"
        class="max-h-[52vh] overflow-y-auto py-1"
      >
        <div v-for="group in groups" :key="group.group">
          <p
            class="px-4 pt-2 pb-1 text-[0.68rem] font-medium tracking-wide text-slate-400 uppercase"
          >
            {{ group.group }}
          </p>
          <button
            v-for="item in group.items"
            :key="item.id"
            type="button"
            role="option"
            :data-index="indexOf(item.id)"
            :aria-selected="indexOf(item.id) === active"
            class="flex w-full items-center justify-between gap-3 px-4 py-1.5 text-left text-sm"
            :class="
              indexOf(item.id) === active
                ? 'bg-slate-100 text-slate-900 dark:bg-slate-800 dark:text-slate-100'
                : 'text-slate-700 dark:text-slate-300'
            "
            @mouseenter="active = indexOf(item.id)"
            @click="perform(item.effect)"
          >
            <span class="truncate">{{ item.title }}</span>
            <span
              v-if="item.hint"
              class="shrink-0 font-mono text-xs text-slate-400 dark:text-slate-500"
            >
              {{ item.hint }}
            </span>
          </button>
        </div>
      </div>

      <div
        class="flex items-center gap-3 border-t border-slate-200 px-4 py-1.5 text-[0.7rem] text-slate-400 dark:border-slate-700"
      >
        <span class="flex items-center gap-1"><CornerDownLeft class="h-3 w-3" /> run</span>
        <span>↑↓ move</span>
        <span>esc close</span>
      </div>
    </div>
  </div>
</template>
