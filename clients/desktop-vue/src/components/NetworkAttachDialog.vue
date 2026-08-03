<script setup lang="ts">
import { ref, watch } from "vue";
import { Check, Network } from "lucide-vue-next";

import type { FileEditPreview, ProjectId } from "../bindings";
import { networkAttachPreview } from "../lib/transport";
import { useEngineStore } from "../stores/engine";
import Button from "./ui/Button.vue";
import Modal from "./ui/Modal.vue";

const open = defineModel<boolean>("open", { default: false });
const { workspace, project } = defineProps<{ workspace: string; project: ProjectId | null }>();

const store = useEngineStore();
const preview = ref<FileEditPreview | null>(null);
const loading = ref(false);
const showDiff = ref(false);

watch([open, () => project], async ([isOpen]) => {
  if (!isOpen || project == null) return;
  preview.value = null;
  showDiff.value = false;
  loading.value = true;
  try {
    preview.value = await networkAttachPreview(workspace, project);
  } catch (e) {
    store.error = e instanceof Error ? e.message : String(e);
    open.value = false;
  } finally {
    loading.value = false;
  }
});

async function apply() {
  if (project == null) return;
  await store.run({ type: "attachNetwork", workspace: workspace, project: project });
  if (!store.error) open.value = false;
}
</script>

<template>
  <Modal v-model:open="open" title="Attach to shared network" wide>
    <p v-if="loading" class="text-xs text-slate-500">planning…</p>
    <div v-else-if="preview" class="space-y-3">
      <ul class="space-y-1">
        <li
          v-for="line in preview.summary"
          :key="line"
          class="flex items-start gap-2 text-xs text-slate-600 dark:text-slate-300"
        >
          <Network class="mt-0.5 h-3.5 w-3.5 shrink-0 text-slate-400" />
          {{ line }}
        </li>
      </ul>

      <p
        v-if="preview.noOp"
        class="flex items-center gap-2 rounded-md border border-emerald-200 bg-emerald-50 p-2 text-xs text-emerald-800 dark:border-emerald-900 dark:bg-emerald-950/40 dark:text-emerald-300"
      >
        <Check class="h-3.5 w-3.5" /> Nothing to change — already attached.
      </p>

      <template v-else>
        <Button variant="outline" size="sm" @click="showDiff = !showDiff">
          {{ showDiff ? "Hide" : "Show" }} file changes ({{ preview.file }})
        </Button>
        <div v-if="showDiff" class="grid grid-cols-2 gap-2">
          <pre
            class="max-h-64 overflow-auto rounded-md border border-slate-200 bg-slate-50 p-2 font-mono text-[11px] leading-4 whitespace-pre dark:border-slate-800 dark:bg-slate-900"
            >{{ preview.before }}</pre>
          <pre
            class="max-h-64 overflow-auto rounded-md border border-emerald-200 bg-emerald-50/50 p-2 font-mono text-[11px] leading-4 whitespace-pre dark:border-emerald-900 dark:bg-emerald-950/30"
            >{{ preview.after }}</pre>
        </div>
        <p class="text-xs text-slate-400">
          Applied through the write transaction: validated by docker compose, backed up, and refused
          rather than corrupted.
        </p>
      </template>

      <div class="flex justify-end gap-2">
        <Button variant="outline" @click="open = false">Close</Button>
        <Button v-if="!preview.noOp" :disabled="store.busy > 0" @click="apply">
          Apply changes
        </Button>
      </div>
    </div>
  </Modal>
</template>
