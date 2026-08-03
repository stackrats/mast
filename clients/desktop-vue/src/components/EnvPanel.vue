<script setup lang="ts">
import { ref, watch } from "vue";

import { Check, Eye, EyeOff, Plus, RefreshCw, Trash2, X } from "lucide-vue-next";

import type { EnvReport, ProjectId } from "../bindings";
import { useEngineStore } from "../stores/engine";
import Button from "./ui/Button.vue";
import Modal from "./ui/Modal.vue";
import Tooltip from "./ui/Tooltip.vue";
import Input from "./ui/Input.vue";

const open = defineModel<boolean>("open", { default: false });
const { project } = defineProps<{ project: ProjectId }>();

const store = useEngineStore();
const report = ref<EnvReport | null>(null);
const loading = ref(false);
const revealed = ref(new Set<string>());
const editing = ref<string | null>(null);
const editValue = ref("");
const newKey = ref("");
const newValue = ref("");

async function refresh() {
  loading.value = true;
  try {
    report.value = await store.fetchEnvReport(project);
  } finally {
    loading.value = false;
  }
}

watch(open, (isOpen) => {
  if (!isOpen) return;
  revealed.value = new Set();
  editing.value = null;
  newKey.value = "";
  newValue.value = "";
  void refresh();
});

function display(entry: { key: string; value: string; secret: boolean }): string {
  if (!entry.secret || revealed.value.has(entry.key)) return entry.value;
  return "•".repeat(Math.min(Math.max(entry.value.length, 6), 16));
}

function toggleReveal(key: string) {
  const next = new Set(revealed.value);
  if (next.has(key)) next.delete(key);
  else next.add(key);
  revealed.value = next;
}

function startEdit(key: string, value: string) {
  editing.value = key;
  editValue.value = value;
}

async function saveEdit() {
  if (editing.value == null) return;
  await store.run({
    type: "setEnvVar",
    id: project,
    key: editing.value,
    value: editValue.value,
  });
  editing.value = null;
  if (!store.error) await refresh();
}

async function removeKey(key: string) {
  await store.run({ type: "removeEnvVar", id: project, key });
  if (!store.error) await refresh();
}

async function addMissing(key: string) {
  await store.run({ type: "setEnvVar", id: project, key, value: "" });
  if (!store.error) await refresh();
}

async function addNew() {
  const key = newKey.value.trim();
  if (!key) return;
  await store.run({ type: "setEnvVar", id: project, key, value: newValue.value });
  if (!store.error) {
    newKey.value = "";
    newValue.value = "";
    await refresh();
  }
}

function findingsFor(key: string) {
  return report.value?.findings.filter((f) => f.key === key) ?? [];
}
</script>

<template>
  <Modal v-model:open="open" title=".env" wide>
    <div class="-mt-2 mb-2 flex justify-end">
      <Button variant="outline" size="sm" @click="refresh">
        <RefreshCw class="h-3.5 w-3.5" /> Reload
      </Button>
    </div>

    <p v-if="loading" class="mt-2 text-xs text-slate-500">loading…</p>
    <template v-else-if="report">
      <p v-if="!report.envExists" class="mt-2 text-xs text-amber-700">
        No .env file yet — adding a variable will create it.
      </p>

      <div v-if="report.findings.some((f) => f.key == null)" class="mt-2 space-y-1">
        <p
          v-for="finding in report.findings.filter((f) => f.key == null)"
          :key="finding.message"
          class="rounded border border-amber-200 bg-amber-50 p-1.5 text-xs text-amber-800"
        >
          {{ finding.message }}
        </p>
      </div>

      <table class="mt-2 w-full table-fixed text-xs">
        <tbody>
          <tr
            v-for="entry in report.entries"
            :key="entry.key"
            class="border-t border-slate-200 align-top dark:border-slate-800"
          >
            <td
              class="w-44 truncate py-1 pr-2 font-mono font-medium text-slate-700 dark:text-slate-300"
            >
              {{ entry.key }}
              <Tooltip v-if="!entry.inExample && report.exampleExists" text="Not in .env.example.">
                <span class="ml-1 text-slate-400">±</span>
              </Tooltip>
            </td>
            <td class="py-1 font-mono">
              <template v-if="editing === entry.key">
                <input
                  v-model="editValue"
                  class="w-full rounded border border-slate-300 px-1 py-0.5 dark:border-slate-700 dark:bg-slate-900"
                  @keyup.enter="saveEdit"
                  @keyup.escape="editing = null"
                />
              </template>
              <template v-else>
                <button
                  class="-mx-1 block w-full rounded px-1 py-0.5 break-all text-left text-slate-600 hover:bg-slate-100 dark:hover:bg-slate-800"
                  @click="startEdit(entry.key, entry.value)"
                >
                  {{ display(entry) || "∅" }}
                </button>
              </template>
              <p
                v-for="finding in findingsFor(entry.key)"
                :key="finding.message"
                class="mt-0.5 text-amber-700"
              >
                ⚠ {{ finding.message }}
              </p>
            </td>
            <td class="w-24 whitespace-nowrap py-1 pl-2 text-right">
              <Tooltip
                v-if="entry.secret"
                :text="revealed.has(entry.key) ? 'Hide the value.' : 'Reveal the secret value.'"
              >
                <Button variant="ghost" size="iconSm" @click="toggleReveal(entry.key)">
                  <EyeOff v-if="revealed.has(entry.key)" class="h-3.5 w-3.5" />
                  <Eye v-else class="h-3.5 w-3.5" />
                </Button>
              </Tooltip>
              <template v-if="editing === entry.key">
                <Tooltip text="Save.">
                  <Button variant="ghost" size="iconSm" @click="saveEdit">
                    <Check class="h-3.5 w-3.5 text-emerald-600" />
                  </Button>
                </Tooltip>
                <Tooltip text="Cancel.">
                  <Button variant="ghost" size="iconSm" @click="editing = null">
                    <X class="h-3.5 w-3.5" />
                  </Button>
                </Tooltip>
              </template>
              <Tooltip v-else text="Remove this variable.">
                <Button variant="ghost" size="iconSm" @click="removeKey(entry.key)">
                  <Trash2 class="h-3.5 w-3.5" />
                </Button>
              </Tooltip>
            </td>
          </tr>
        </tbody>
      </table>

      <div v-if="report.missingFromEnv.length" class="mt-3">
        <p class="text-xs font-medium text-slate-500">In .env.example but missing here:</p>
        <div class="mt-1 flex flex-wrap gap-1.5">
          <Button
            v-for="key in report.missingFromEnv"
            :key="key"
            variant="outline"
            size="sm"
            class="font-mono"
            @click="addMissing(key)"
          >
            <Plus class="h-3 w-3" /> {{ key }}
          </Button>
        </div>
      </div>

      <div class="mt-3 flex gap-2">
        <div class="w-44 shrink-0">
          <Input v-model="newKey" placeholder="KEY" mono />
        </div>
        <div class="min-w-0 flex-1">
          <Input v-model="newValue" placeholder="value" mono @keyup.enter="addNew" />
        </div>
        <Button :disabled="store.busy > 0" @click="addNew">Add</Button>
      </div>
    </template>
  </Modal>
</template>
