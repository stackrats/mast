<script setup lang="ts">
import { computed, nextTick, ref, watch } from "vue";

import { useEngineStore } from "../stores/engine";
import Modal from "./ui/Modal.vue";
import Select from "./ui/Select.vue";

const store = useEngineStore();

const open = computed({
  get: () => store.logs != null,
  set: (value) => {
    if (!value) void store.closeLogs();
  },
});

const project = computed(() => store.projects.find((p) => p.id === store.logs?.project));

const serviceOptions = computed(() =>
  (project.value?.services ?? [])
    .filter((s) => s.containerId != null)
    .map((s) => ({ value: s.name, label: s.name })),
);

// The Select switches which service's stream this dialog follows.
const service = computed({
  get: () => store.logs?.service ?? "",
  set: (name) => {
    const view = store.logs;
    if (view && name && name !== view.service) void store.openLogs(view.project, name);
  },
});

// Follow the tail as lines stream in.
const scroller = ref<HTMLElement | null>(null);
watch(
  () => store.logs?.lines.length,
  async () => {
    await nextTick();
    scroller.value?.scrollTo({ top: scroller.value.scrollHeight });
  },
);
</script>

<template>
  <Modal v-model:open="open" :title="`Logs — ${project?.name ?? ''}`" wide>
    <div class="space-y-2">
      <div class="w-56">
        <Select v-model="service" :options="serviceOptions" placeholder="service" />
      </div>
      <div
        ref="scroller"
        class="h-80 overflow-y-auto rounded-md border border-slate-200 bg-slate-50 p-2 font-mono text-[11px] leading-4 text-slate-600 dark:border-slate-800 dark:bg-slate-950 dark:text-slate-300"
      >
        <div
          v-for="(line, i) in store.logs?.lines ?? []"
          :key="i"
          class="break-all whitespace-pre-wrap"
          :class="{ 'text-amber-700 dark:text-amber-400': line.stderr }"
        >
          {{ line.message }}
        </div>
        <p v-if="!store.logs?.lines.length" class="text-slate-400">waiting for output…</p>
      </div>
    </div>
  </Modal>
</template>
