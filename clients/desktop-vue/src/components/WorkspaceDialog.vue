<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { TriangleAlert } from "lucide-vue-next";

import type { WorkspaceMember, WorkspaceSummary } from "../bindings";
import { findCycle } from "../lib/graph";
import { useEngineStore } from "../stores/engine";
import Button from "./ui/Button.vue";
import Checkbox from "./ui/Checkbox.vue";
import Input from "./ui/Input.vue";
import Modal from "./ui/Modal.vue";

const open = defineModel<boolean>("open", { default: false });
const { editing } = defineProps<{ editing: WorkspaceSummary | null }>();

const store = useEngineStore();
const name = ref("");
const selected = ref(new Set<string>());
const deps = ref<Record<string, Set<string>>>({});

watch(open, (isOpen) => {
  if (!isOpen) return;
  if (editing) {
    name.value = editing.name;
    selected.value = new Set(editing.members.map((m) => m.project));
    deps.value = Object.fromEntries(editing.members.map((m) => [m.project, new Set(m.dependsOn)]));
  } else {
    name.value = "";
    selected.value = new Set();
    deps.value = {};
  }
});

const members = computed<WorkspaceMember[]>(() =>
  [...selected.value].map((project) => ({
    project,
    dependsOn: [...(deps.value[project] ?? [])].filter(
      (d) => selected.value.has(d) && d !== project,
    ),
  })),
);

// Live cycle guard — the same rule the engine enforces, applied before save.
const cycle = computed(() =>
  findCycle(members.value.map((m) => ({ id: m.project, dependsOn: m.dependsOn }))),
);

const canSave = computed(
  () => name.value.trim().length > 0 && selected.value.size > 0 && cycle.value == null,
);

function projectName(id: string): string {
  return store.projects.find((p) => p.id === id)?.name ?? id;
}

function toggleMember(id: string) {
  const next = new Set(selected.value);
  if (next.has(id)) {
    next.delete(id);
    const nextDeps = { ...deps.value };
    delete nextDeps[id];
    deps.value = nextDeps;
  } else {
    next.add(id);
  }
  selected.value = next;
}

function toggleDep(member: string, dep: string) {
  const current = new Set(deps.value[member] ?? []);
  if (current.has(dep)) current.delete(dep);
  else current.add(dep);
  deps.value = { ...deps.value, [member]: current };
}

async function save() {
  await store.run({
    type: "saveWorkspace",
    id: editing?.id ?? null,
    name: name.value.trim(),
    members: members.value,
  });
  if (!store.error) open.value = false;
}
</script>

<template>
  <Modal v-model:open="open" :title="editing ? `Edit ${editing.name}` : 'New workspace'" wide>
    <div class="space-y-3">
      <Input v-model="name" placeholder="workspace name" />

      <div class="max-h-72 space-y-1.5 overflow-y-auto">
        <div
          v-for="project in store.projects"
          :key="project.id"
          class="rounded-md border border-slate-100 p-2 dark:border-slate-800"
        >
          <Checkbox
            :model-value="selected.has(project.id)"
            class="text-sm!"
            @update:model-value="toggleMember(project.id)"
          >
            <span class="font-medium text-slate-800 dark:text-slate-200">{{ project.name }}</span>
          </Checkbox>
          <div
            v-if="selected.has(project.id) && selected.size > 1"
            class="mt-1.5 ml-6 flex flex-wrap items-center gap-x-3 gap-y-1 text-xs text-slate-600 dark:text-slate-300"
          >
            <span class="text-slate-400">waits for:</span>
            <Checkbox
              v-for="other in [...selected].filter((s) => s !== project.id)"
              :key="other"
              :model-value="deps[project.id]?.has(other) ?? false"
              :label="projectName(other)"
              @update:model-value="toggleDep(project.id, other)"
            />
          </div>
        </div>
      </div>

      <p
        v-if="cycle"
        class="flex items-start gap-2 rounded-md border border-red-200 bg-red-50 p-2 text-xs text-red-800 dark:border-red-900 dark:bg-red-950/40 dark:text-red-200"
      >
        <TriangleAlert class="mt-0.5 h-3.5 w-3.5 shrink-0" />
        These dependencies form a cycle ({{ cycle.map(projectName).join(" → ") }}) — untangle them
        before saving.
      </p>

      <div class="flex justify-end gap-2">
        <Button variant="outline" @click="open = false">Cancel</Button>
        <Button :disabled="!canSave || store.busy > 0" @click="save">
          {{ editing ? "Save changes" : "Create workspace" }}
        </Button>
      </div>
    </div>
  </Modal>
</template>
