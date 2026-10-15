<script setup lang="ts">
// Removing a worktree is the one Agents action that can lose work, so it
// says exactly what happens first: what is stopped, which containers go,
// that the branch stays — and what is lost, which then needs its own tick.
// While an agent session works in the worktree the engine refuses, and so
// does this: pulling the directory out from under a running agent breaks
// the session without telling it.
import { computed, ref, watch } from "vue";
import { Loader2, Trash2, TriangleAlert } from "lucide-vue-next";

import type { ProjectSummary, Worktree } from "../bindings";
import { removalCost, runsInWorktree, sessionsIn, stackSummary } from "../lib/agents";
import { useEngineStore, worktreeKey } from "../stores/engine";
import Button from "./ui/Button.vue";
import Checkbox from "./ui/Checkbox.vue";
import Modal from "./ui/Modal.vue";

const open = defineModel<boolean>("open", { default: false });
const props = defineProps<{ project: ProjectSummary | null; worktree: Worktree | null }>();
const store = useEngineStore();

// Read through the store, so the dialog follows the next reconcile — a
// worktree that gains changes while the question is open must say so.
const live = computed(() => {
  if (!props.project || !props.worktree) return null;
  const project = store.projects.find((p) => p.id === props.project?.id);
  const worktree = project?.worktrees?.find((w) => w.path === props.worktree?.path);
  return project && worktree ? { project, worktree } : null;
});

const cost = computed(() => (live.value ? removalCost(live.value.worktree) : null));
const runs = computed(() =>
  live.value ? runsInWorktree(live.value.project, live.value.worktree.path) : [],
);
const sessions = computed(() =>
  live.value ? sessionsIn(live.value.project, live.value.worktree.path) : [],
);
const stack = computed(() => (live.value ? stackSummary(live.value.worktree) : null));
const busy = computed(
  () =>
    !!live.value && !!store.agentOps[worktreeKey(live.value.project.id, live.value.worktree.path)],
);

const consented = ref(false);
const failure = ref<string | null>(null);
watch(open, (isOpen) => {
  if (isOpen) {
    consented.value = false;
    failure.value = null;
  }
});
// Gone — removed here or elsewhere: nothing left to ask about.
watch(live, (now) => {
  if (open.value && !now && !busy.value) open.value = false;
});

const blocked = computed(
  () =>
    store.readOnly ||
    sessions.value.length > 0 ||
    (cost.value?.force === true && !consented.value) ||
    busy.value,
);

async function remove() {
  if (!live.value || blocked.value) return;
  const { project, worktree } = live.value;
  failure.value = await store.agentAction(worktreeKey(project.id, worktree.path), {
    type: "removeWorktree",
    id: project.id,
    path: worktree.path,
    force: cost.value?.force === true && consented.value,
  });
  if (!failure.value) open.value = false;
}
</script>

<template>
  <Modal v-model:open="open" :title="`Remove worktree ${props.worktree?.name ?? ''}`">
    <div v-if="live && cost" class="space-y-3 text-xs text-slate-600 dark:text-slate-300">
      <p class="font-mono text-[11px] break-all text-slate-500 dark:text-slate-400">
        {{ live.worktree.path }}
      </p>

      <div
        v-if="sessions.length"
        class="flex items-start gap-1.5 rounded-md border border-red-200 bg-red-50 p-2 text-red-800 dark:border-red-900 dark:bg-red-950/40 dark:text-red-200"
      >
        <TriangleAlert class="mt-0.5 h-3.5 w-3.5 shrink-0" />
        <span>
          {{ sessions.map((s) => `${s.agent} (pid ${s.pid})`).join(", ") }}
          {{ sessions.length === 1 ? "is" : "are" }} working in it. Let it finish, or exit that
          session, before removing the directory it works in.
        </span>
      </div>

      <div>
        <p class="font-medium text-slate-700 dark:text-slate-200">Removing it will</p>
        <ul class="mt-1 list-disc space-y-0.5 pl-4">
          <li v-if="runs.length">
            stop {{ runs.length === 1 ? "what is" : `the ${runs.length} things` }} running in it:
            {{ runs.map((r) => r.title).join(", ") }}
          </li>
          <li v-if="stack">remove its containers ({{ stack }}); named volumes are kept</li>
          <li>delete the directory with <code class="font-mono">git worktree remove</code></li>
        </ul>
      </div>

      <ul class="space-y-1">
        <li
          v-for="line in cost.lines"
          :key="line"
          :class="
            cost.force && !line.startsWith('The branch')
              ? 'font-medium text-amber-700 dark:text-amber-300'
              : ''
          "
        >
          {{ line }}
        </li>
      </ul>

      <Checkbox v-if="cost.force" v-model="consented" block>
        Discard them and remove the worktree anyway
      </Checkbox>

      <p
        v-if="failure"
        class="rounded-md border border-red-200 bg-red-50 p-2 text-red-800 dark:border-red-900 dark:bg-red-950/40 dark:text-red-200"
      >
        {{ failure }}
      </p>

      <div class="flex justify-end gap-2 pt-1">
        <Button variant="outline" size="sm" @click="open = false">Cancel</Button>
        <Button variant="destructive" size="sm" :disabled="blocked" @click="remove">
          <Loader2 v-if="busy" class="h-3.5 w-3.5 animate-spin" />
          <Trash2 v-else class="h-3.5 w-3.5" />
          Remove worktree
        </Button>
      </div>
    </div>
  </Modal>
</template>
