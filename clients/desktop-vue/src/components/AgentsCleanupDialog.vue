<script setup lang="ts">
// "Clean up" is a list, not a button that guesses (ADR-0012): every
// leftover the Agents view knows about, ticked when nothing live is using
// it, and nothing runs until the list has been looked at. The list is
// taken when the dialog opens and stays put while it runs, so each row can
// say how its own stop went instead of vanishing into the next reconcile.
import { computed, ref, watch } from "vue";
import { Check, CircleStop, Loader2, X } from "lucide-vue-next";

import type { Action, ProjectId } from "../bindings";
import { cleanupPlan, type CleanupItem } from "../lib/agents";
import { formatBytes } from "../lib/usage";
import { containerRunKey, hostRunKey, useEngineStore, worktreeKey } from "../stores/engine";
import Button from "./ui/Button.vue";
import Checkbox from "./ui/Checkbox.vue";
import Modal from "./ui/Modal.vue";

const open = defineModel<boolean>("open", { default: false });
/** One project's leftovers only, from its card; every project's when unset. */
const { project = null } = defineProps<{ project?: ProjectId | null }>();
const store = useEngineStore();

const items = ref<CleanupItem[]>([]);
const ticked = ref<Record<string, boolean>>({});
const status = ref<Record<string, "running" | "done" | { failed: string }>>({});
const running = ref(false);
const finished = ref(false);

watch(open, (isOpen) => {
  if (!isOpen || running.value) return;
  const projects = project ? store.projects.filter((p) => p.id === project) : store.projects;
  items.value = cleanupPlan(projects, Date.now());
  ticked.value = Object.fromEntries(items.value.map((item) => [item.key, item.suggested]));
  status.value = {};
  finished.value = false;
});

const groups = computed(() => {
  const byProject = new Map<string, CleanupItem[]>();
  for (const item of items.value) {
    const list = byProject.get(item.projectName) ?? [];
    list.push(item);
    byProject.set(item.projectName, list);
  }
  return [...byProject.entries()];
});

const chosen = computed(() => items.value.filter((item) => ticked.value[item.key]));
const freed = computed(() => chosen.value.reduce((sum, item) => sum + item.bytes, 0));

/** The store slot each action shows its progress under, so the Agents view's
 * own rows spin while the list works through them. */
function opKey(action: Action): string {
  switch (action.type) {
    case "stopHostRun":
      return hostRunKey(action.id, action.pid, action.startTicks);
    case "stopContainerRun":
      return containerRunKey(action.id, action.pid, action.startTicks);
    case "removeWorktreeContainers":
      return worktreeKey(action.id, action.path);
    case "pruneWorktrees":
      return worktreeKey(action.id, "prune");
    default:
      return JSON.stringify(action);
  }
}

/** One at a time: a stop waits up to ten seconds for its tree to exit, and
 * twenty of them racing would make every row's outcome harder to read. */
async function runChosen() {
  running.value = true;
  try {
    for (const item of chosen.value) {
      status.value[item.key] = "running";
      const failure = await store.agentAction(opKey(item.action), item.action);
      status.value[item.key] = failure ? { failed: failure } : "done";
    }
  } finally {
    running.value = false;
    finished.value = true;
  }
}

const failures = computed(
  () => Object.values(status.value).filter((s) => typeof s === "object").length,
);
</script>

<template>
  <Modal v-model:open="open" title="Clean up after agents" wide>
    <div class="space-y-3 text-xs text-slate-600 dark:text-slate-300">
      <p>
        Ticked is what nothing live is using: processes whose session has ended, container trees
        whose starter is gone, the containers of worktrees no agent is working in, and worktrees
        whose directory has been deleted. What a running agent started, and anything started outside
        Mast in an app container, is listed but left for you to tick. Worktrees themselves are never
        removed from here.
      </p>

      <p v-if="items.length === 0" class="text-slate-400">Nothing left to clean up.</p>

      <div v-for="[name, list] in groups" :key="name">
        <p class="text-[11px] font-medium tracking-wide text-slate-400">{{ name }}</p>
        <div v-for="item in list" :key="item.key" class="flex items-start gap-2">
          <Checkbox
            v-model="ticked[item.key]"
            row
            class="min-w-0 flex-1"
            :disabled="running || finished"
          >
            <span class="min-w-0">
              <span class="block font-medium text-slate-700 dark:text-slate-200">
                {{ item.label }}
              </span>
              <span class="block text-[11px] text-slate-400">{{ item.detail }}</span>
              <span
                v-if="typeof status[item.key] === 'object'"
                class="block text-[11px] text-red-600 dark:text-red-400"
              >
                {{ (status[item.key] as { failed: string }).failed }}
              </span>
            </span>
          </Checkbox>
          <span class="mt-2 flex h-4 w-4 shrink-0 items-center justify-center">
            <Loader2 v-if="status[item.key] === 'running'" class="h-3.5 w-3.5 animate-spin" />
            <Check v-else-if="status[item.key] === 'done'" class="h-3.5 w-3.5 text-emerald-600" />
            <X v-else-if="typeof status[item.key] === 'object'" class="h-3.5 w-3.5 text-red-600" />
          </span>
        </div>
      </div>

      <div
        class="flex items-center justify-between gap-3 border-t border-slate-200 pt-3 dark:border-slate-800"
      >
        <p class="text-slate-400">
          <template v-if="finished">
            {{ failures ? `${failures} could not be cleaned up — see above.` : "Done." }}
          </template>
          <template v-else-if="freed > 0">Frees about {{ formatBytes(freed) }} of memory.</template>
        </p>
        <div class="flex gap-2">
          <Button variant="outline" size="sm" :disabled="running" @click="open = false">
            {{ finished ? "Close" : "Cancel" }}
          </Button>
          <Button
            v-if="!finished"
            size="sm"
            :disabled="store.readOnly || running || chosen.length === 0"
            @click="runChosen"
          >
            <Loader2 v-if="running" class="h-3.5 w-3.5 animate-spin" />
            <CircleStop v-else class="h-3.5 w-3.5" />
            Clean up {{ chosen.length }}
          </Button>
        </div>
      </div>
    </div>
  </Modal>
</template>
