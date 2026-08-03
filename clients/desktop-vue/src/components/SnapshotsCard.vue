<script setup lang="ts">
import { ref, watch } from "vue";
import { Camera, Check, FileSearch, Trash2, TriangleAlert } from "lucide-vue-next";

import type { SnapshotReport, WorkspaceSnapshot } from "../bindings";
import { listSnapshots, snapshotReport } from "../lib/transport";
import { useEngineStore } from "../stores/engine";
import Button from "./ui/Button.vue";
import Hint from "./ui/Hint.vue";
import Input from "./ui/Input.vue";
import Modal from "./ui/Modal.vue";
import Tooltip from "./ui/Tooltip.vue";

const { workspace } = defineProps<{ workspace: string }>();
const store = useEngineStore();

const snapshots = ref<WorkspaceSnapshot[]>([]);
const newName = ref("");
const report = ref<SnapshotReport | null>(null);
const reportOpen = ref(false);

async function refresh() {
  try {
    snapshots.value = await listSnapshots(workspace);
  } catch (e) {
    store.error = e instanceof Error ? e.message : String(e);
  }
}
watch(() => workspace, refresh, { immediate: true });

async function take() {
  const name = newName.value.trim() || `snapshot ${new Date().toLocaleString()}`;
  await store.run({ type: "takeSnapshot", workspace: workspace, name });
  if (!store.error) {
    newName.value = "";
    await refresh();
  }
}

async function openReport(id: string) {
  try {
    report.value = await snapshotReport(id);
    reportOpen.value = true;
  } catch (e) {
    store.error = e instanceof Error ? e.message : String(e);
  }
}

async function remove(id: string) {
  await store.run({ type: "removeSnapshot", id });
  if (!store.error) await refresh();
}

function when(unix: number): string {
  return new Date(unix * 1000).toLocaleString();
}
</script>

<template>
  <div
    class="rounded-md border border-slate-200 bg-white p-3 dark:border-slate-800 dark:bg-slate-900"
  >
    <p class="flex items-center gap-1.5 text-xs font-medium text-slate-600 dark:text-slate-300">
      <Camera class="h-3.5 w-3.5 text-slate-400" /> Snapshots
      <Hint
        text="A snapshot remembers where every member project was: its git branch and commit, whether it had uncommitted changes, and checksums of its compose files and .env. Later, the compare button shows exactly what has drifted since — which branches moved, which configs changed. It never modifies your files."
      />
    </p>

    <div class="mt-2 flex gap-2">
      <Input v-model="newName" placeholder="name (e.g. before-upgrade)" />
      <Button size="sm" :disabled="store.busy > 0 || store.readOnly" @click="take">Take</Button>
    </div>

    <ul v-if="snapshots.length" class="mt-2 space-y-1">
      <li
        v-for="snap in snapshots"
        :key="snap.id"
        class="flex items-center justify-between rounded border border-slate-100 px-2 py-1 text-xs dark:border-slate-800"
      >
        <span class="min-w-0 truncate text-slate-700 dark:text-slate-300">
          {{ snap.name }}
          <span class="ml-1 text-slate-400">{{ when(snap.takenUnix) }}</span>
        </span>
        <span class="flex shrink-0 gap-1">
          <Tooltip text="Compare with the current state.">
            <Button variant="ghost" size="iconSm" @click="openReport(snap.id)">
              <FileSearch class="h-3.5 w-3.5" />
            </Button>
          </Tooltip>
          <Tooltip text="Delete this snapshot.">
            <Button variant="ghost" size="iconSm" @click="remove(snap.id)">
              <Trash2 class="h-3.5 w-3.5" />
            </Button>
          </Tooltip>
        </span>
      </li>
    </ul>

    <Modal v-model:open="reportOpen" :title="`Drift vs ${report?.snapshot.name ?? ''}`" wide>
      <div v-if="report" class="space-y-2">
        <p
          v-if="report.clean"
          class="flex items-center gap-2 rounded-md border border-emerald-200 bg-emerald-50 p-2 text-xs text-emerald-800 dark:border-emerald-900 dark:bg-emerald-950/40 dark:text-emerald-300"
        >
          <Check class="h-3.5 w-3.5" /> Everything matches the snapshot.
        </p>
        <div v-for="delta in report.deltas" :key="delta.projectName" class="text-xs">
          <p class="font-medium text-slate-700 dark:text-slate-300">
            {{ delta.projectName }}
            <span v-if="delta.changes.length === 0" class="text-emerald-600">unchanged</span>
          </p>
          <p
            v-for="change in delta.changes"
            :key="change"
            class="mt-0.5 ml-4 flex items-start gap-1.5 text-amber-700 dark:text-amber-300"
          >
            <TriangleAlert class="mt-0.5 h-3 w-3 shrink-0" /> {{ change }}
          </p>
        </div>
      </div>
    </Modal>
  </div>
</template>
