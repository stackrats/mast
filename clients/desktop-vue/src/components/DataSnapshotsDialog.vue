<script setup lang="ts">
// Point-in-time copies of a service's named volumes. Taking one is cheap and
// calm; restoring one overwrites the live data, so restore is the armed,
// two-step path — with a fresh safety snapshot offered first, checked by
// default, because the person restoring is usually mid-mistake already.
import { computed, ref, watch } from "vue";
import { Archive, RotateCcw, Trash2 } from "lucide-vue-next";

import type { ProjectSummary, ServiceState, VolumeSnapshot } from "../bindings";
import { volumeSnapshots } from "../lib/transport";
import { useEngineStore } from "../stores/engine";
import Button from "./ui/Button.vue";
import Checkbox from "./ui/Checkbox.vue";
import Modal from "./ui/Modal.vue";

const open = defineModel<boolean>("open", { required: true });
const { project, service } = defineProps<{
  project: ProjectSummary;
  service: ServiceState | null;
}>();

const store = useEngineStore();
const snapshots = ref<VolumeSnapshot[]>([]);
const loadError = ref<string | null>(null);
const loaded = ref(false);
/** Group id whose Restore has been clicked once and awaits the second. */
const arming = ref<string | null>(null);
const snapshotFirst = ref(true);
/** A restore chain in flight — the dialog's own busy, beyond the op lock. */
const chaining = ref(false);

const forService = computed(() => snapshots.value.filter((s) => s.service === service?.name));
const busy = computed(() => store.readOnly || chaining.value || store.hasRunningOp(project.id));

async function reload() {
  loadError.value = null;
  try {
    snapshots.value = await volumeSnapshots(project.id);
  } catch (e) {
    loadError.value = e instanceof Error ? e.message : String(e);
  }
  loaded.value = true;
}

watch(open, (now) => {
  if (now) {
    loaded.value = false;
    arming.value = null;
    snapshotFirst.value = true;
    void reload();
  }
});

// The take/restore operations run under the project's op key; when one
// reaches a terminal state while the dialog is up, the list is stale.
watch(
  () => store.operations[project.id]?.terminal,
  (terminal) => {
    if (terminal && open.value) void reload();
  },
);

/** Resolve when the project's current operation reaches a terminal state. */
function projectOpSettled(): Promise<string | null> {
  return new Promise((resolve) => {
    const poll = () => {
      const op = store.operations[project.id];
      if (!op || op.terminal !== null) return resolve(op?.terminal ?? null);
      setTimeout(poll, 250);
    };
    poll();
  });
}

async function takeSnapshot() {
  if (!service) return;
  await store.runLifecycle(project.id, `snapshot ${service.name} data`, {
    type: "snapshotServiceData",
    id: project.id,
    service: service.name,
  });
}

async function restore(group: string) {
  if (!service) return;
  arming.value = null;
  chaining.value = true;
  try {
    if (snapshotFirst.value) {
      await takeSnapshot();
      // A failed safety snapshot must abort the restore: proceeding would
      // destroy the very data the checkbox promised to keep.
      if ((await projectOpSettled()) !== "completed") return;
    }
    await store.runLifecycle(project.id, `restore ${service.name} data`, {
      type: "restoreServiceData",
      id: project.id,
      group,
    });
    await projectOpSettled();
  } finally {
    chaining.value = false;
    void reload();
  }
}

async function remove(group: string) {
  await store.run({ type: "removeServiceDataSnapshot", group });
  void reload();
}

function when(atUnixMs: number): string {
  return new Date(atUnixMs).toLocaleString(undefined, {
    dateStyle: "medium",
    timeStyle: "short",
  });
}
</script>

<template>
  <Modal v-model:open="open" :title="`${service?.name ?? ''} data snapshots`" wide>
    <div class="space-y-3">
      <p class="text-xs text-slate-500 dark:text-slate-400">
        A snapshot copies this service's named volumes
        <span class="font-mono">({{ (service?.dataVolumes ?? []).join(", ") }})</span> while its
        container is briefly stopped, into labeled docker volumes — insurance before a risky
        migration, and the way back after one. Nothing leaves your machine.
      </p>

      <div class="flex items-center justify-between">
        <p v-if="loadError" class="text-xs text-amber-700 dark:text-amber-300">{{ loadError }}</p>
        <p v-else-if="loaded && forService.length === 0" class="text-xs text-slate-400">
          No snapshots yet.
        </p>
        <span v-else class="text-xs text-slate-400">
          {{ forService.length }} snapshot{{ forService.length === 1 ? "" : "s" }}
        </span>
        <Button :disabled="busy" size="sm" @click="takeSnapshot">
          <Archive class="h-3.5 w-3.5" /> Take snapshot now
        </Button>
      </div>

      <ul v-if="forService.length" class="space-y-1.5">
        <li
          v-for="snapshot in forService"
          :key="snapshot.group"
          class="rounded-md border border-slate-200 p-2 dark:border-slate-800"
        >
          <div class="flex items-center gap-2">
            <div class="min-w-0 flex-1">
              <p class="text-xs text-slate-700 dark:text-slate-200">
                {{ when(snapshot.atUnixMs) }}
              </p>
              <p class="truncate font-mono text-[10px] text-slate-400">
                {{ snapshot.volumes.join(" · ") }}
              </p>
            </div>
            <Button
              v-if="arming !== snapshot.group"
              variant="outline"
              size="sm"
              :disabled="busy"
              @click="arming = snapshot.group"
            >
              <RotateCcw class="h-3.5 w-3.5" /> Restore
            </Button>
            <Button variant="ghost" size="sm" :disabled="busy" @click="remove(snapshot.group)">
              <Trash2 class="h-3.5 w-3.5 text-slate-400" />
            </Button>
          </div>
          <!-- The armed second step, in place, with the safety net opt-out. -->
          <div
            v-if="arming === snapshot.group"
            class="mt-2 space-y-2 rounded-md bg-amber-50 p-2 dark:bg-amber-950/40"
          >
            <p class="text-xs text-amber-800 dark:text-amber-200">
              Restoring overwrites {{ service?.name }}'s current data with this snapshot.
            </p>
            <Checkbox v-model="snapshotFirst" label="Snapshot the current data first" />
            <div class="flex justify-end gap-2">
              <Button variant="outline" size="sm" @click="arming = null">Cancel</Button>
              <Button size="sm" :disabled="busy" @click="restore(snapshot.group)">
                <RotateCcw class="h-3.5 w-3.5" /> Overwrite with this snapshot
              </Button>
            </div>
          </div>
        </li>
      </ul>

      <p v-if="store.readOnly" class="text-xs text-amber-700 dark:text-amber-300">
        Read-only — the controlling Mast window can manage snapshots.
      </p>
    </div>
  </Modal>
</template>
