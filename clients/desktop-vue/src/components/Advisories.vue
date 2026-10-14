<script setup lang="ts">
// Problems the engine found that a click can fix: a daemon running twice, a
// page getting its assets from somewhere other than the project's Vite.
// Shown with the card's warnings, because that is where something being
// wrong is looked for — and with the fix beside the sentence, because "your
// scheduler runs every task twice" is not something to leave to a menu.
import { computed } from "vue";
import { CircleStop, Loader2, RotateCw, TriangleAlert } from "lucide-vue-next";

import type { AdvisoryAction, ProjectSummary, RepairOffer } from "../bindings";
import { runAge, runAt } from "../lib/containerRuns";
import { commandKey, containerRunKey, useEngineStore } from "../stores/engine";
import Button from "./ui/Button.vue";
import FixButton from "./ui/FixButton.vue";

const { project } = defineProps<{ project: ProjectSummary }>();
const store = useEngineStore();

const advisories = computed(() => project.advisories ?? []);

type StopAction = Extract<AdvisoryAction, { type: "stopContainerRun" }>;
type RepairAction = Extract<AdvisoryAction, { type: "applyRepair" }>;

function stopping(action: StopAction): boolean {
  return !!store.stoppingRuns[containerRunKey(project.id, action.pid, action.startTicks)];
}

/** Two copies of one stack carry the same name; how long each has been
 * running is what tells them apart. */
function age(action: StopAction): string | null {
  const run = runAt(project.containerRuns ?? [], action.pid, action.startTicks);
  return run ? runAge(run, Date.now()) : null;
}

/** A repair an advisory offers goes through the same preview-and-confirm
 * dialog as any other: clicking shows exactly what will change first. */
function offer(action: RepairAction): RepairOffer {
  return { id: action.repair, title: action.label, risk: "safe", description: "", arg: action.arg };
}

function restarting(name: string): boolean {
  return !!store.operations[commandKey(project.id, name)]?.restarting;
}
</script>

<template>
  <div
    v-for="advisory in advisories"
    :key="advisory.message"
    class="mt-2 rounded-md border border-amber-200 bg-amber-50 p-2 text-xs text-amber-800 dark:border-amber-900 dark:bg-amber-950/40 dark:text-amber-200"
  >
    <p class="flex items-start gap-1.5">
      <TriangleAlert class="mt-0.5 h-3.5 w-3.5 shrink-0" />
      {{ advisory.message }}
    </p>
    <div v-if="advisory.actions.length > 0" class="mt-2 flex flex-wrap gap-2 pl-5">
      <template v-for="action in advisory.actions" :key="action.label">
        <Button
          v-if="action.type === 'stopContainerRun'"
          variant="outline"
          size="sm"
          :disabled="store.readOnly || stopping(action)"
          @click="store.stopContainerRun(project.id, action.pid, action.startTicks)"
        >
          <Loader2 v-if="stopping(action)" class="h-3.5 w-3.5 animate-spin" />
          <CircleStop v-else class="h-3.5 w-3.5" />
          {{ action.label }}
          <span v-if="age(action)" class="text-slate-400">· up {{ age(action) }}</span>
        </Button>
        <Button
          v-else-if="action.type === 'restartCommand'"
          variant="outline"
          size="sm"
          :disabled="store.readOnly || restarting(action.name)"
          @click="store.restartCommand(project.id, action.name)"
        >
          <RotateCw class="h-3.5 w-3.5" :class="{ 'animate-spin': restarting(action.name) }" />
          {{ action.label }}
        </Button>
        <FixButton v-else :repair="offer(action)" :project="project.id" />
      </template>
    </div>
  </div>
</template>
