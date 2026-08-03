<script setup lang="ts">
import { computed, ref } from "vue";
import { Network, Pencil, Play, Square, Trash2, TriangleAlert, X } from "lucide-vue-next";

import type { ProjectId, WorkspaceSummary } from "../bindings";
import { statusBadgeVariant } from "../lib/status";
import { useEngineStore } from "../stores/engine";
import NetworkAttachDialog from "./NetworkAttachDialog.vue";
import ProjectCard from "./ProjectCard.vue";
import SnapshotsCard from "./SnapshotsCard.vue";
import Badge from "./ui/Badge.vue";
import Hint from "./ui/Hint.vue";
import Button from "./ui/Button.vue";

const { workspace } = defineProps<{ workspace: WorkspaceSummary }>();
const emit = defineEmits<{ edit: [] }>();
const store = useEngineStore();

const op = computed(() => store.operations[workspace.id]);
const opRunning = computed(() => op.value != null && op.value.terminal === null);

const memberProjects = computed(() =>
  workspace.members
    .map((m) => store.projects.find((p) => p.id === m.project))
    .filter((p) => p != null),
);

/// Aggregated health across every member's services (the M6 dashboard line).
const health = computed(() => {
  const services = memberProjects.value.flatMap((p) => p.services);
  return {
    total: services.length,
    running: services.filter((s) => s.state === "running").length,
    unhealthy: services.filter((s) => s.health === "unhealthy").length,
    starting: services.filter((s) => s.health === "starting").length,
  };
});

async function remove() {
  await store.run({ type: "removeWorkspace", id: workspace.id });
  if (!store.error) store.selection = { kind: "home" };
}

const attachOpen = ref(false);
const attachProject = ref<ProjectId | null>(null);

function openAttach(project: ProjectId) {
  attachProject.value = project;
  attachOpen.value = true;
}

function projectName(id: string): string {
  return store.projects.find((p) => p.id === id)?.name ?? id;
}
</script>

<template>
  <div class="space-y-4">
    <div class="flex items-center justify-between gap-4">
      <div class="flex min-w-0 items-center gap-2 overflow-hidden">
        <h1 class="truncate text-lg font-bold tracking-tight text-slate-900 dark:text-slate-100">
          {{ workspace.name }}
        </h1>
        <Badge :variant="statusBadgeVariant[workspace.status]" class="shrink-0">
          {{ workspace.status }}
        </Badge>
        <Hint
          text="A workspace groups projects that belong together. Start all brings them up in dependency order, waiting for each project to be genuinely ready (healthchecks, then Laravel's /up) before its dependents start; Stop all shuts down in reverse."
        />
      </div>
      <div v-if="!store.readOnly" class="flex gap-2">
        <template v-if="opRunning">
          <Button variant="destructive" @click="store.cancelLifecycle(workspace.id)">
            <X class="h-3.5 w-3.5" /> Cancel
          </Button>
        </template>
        <template v-else>
          <Button
            v-if="workspace.status !== 'running'"
            :disabled="workspace.graphError != null || store.busy > 0"
            @click="
              store.runLifecycle(workspace.id, 'start workspace', {
                type: 'startWorkspace',
                id: workspace.id,
              })
            "
          >
            <Play class="h-3.5 w-3.5" /> Start all
          </Button>
          <Button
            v-if="workspace.status !== 'stopped'"
            variant="destructive"
            :disabled="store.busy > 0"
            @click="
              store.runLifecycle(workspace.id, 'stop workspace', {
                type: 'stopWorkspace',
                id: workspace.id,
              })
            "
          >
            <Square class="h-3.5 w-3.5" /> Stop all
          </Button>
          <Button variant="outline" @click="emit('edit')"
            ><Pencil class="h-3.5 w-3.5" /> Edit</Button
          >
          <Button variant="ghost" @click="remove"><Trash2 class="h-3.5 w-3.5" /></Button>
        </template>
      </div>
    </div>

    <p v-if="health.total > 0" class="text-xs text-slate-400">
      {{ health.total }} service{{ health.total === 1 ? "" : "s" }} ·
      {{ health.running }} running<template v-if="health.starting">
        · {{ health.starting }} health-starting</template
      ><template v-if="health.unhealthy">
        · <span class="text-red-500">{{ health.unhealthy }} unhealthy</span></template
      >
    </p>

    <p
      v-if="workspace.graphError"
      class="flex items-start gap-2 rounded-md border border-red-200 bg-red-50 p-2.5 text-xs text-red-800 dark:border-red-900 dark:bg-red-950/40 dark:text-red-200"
    >
      <TriangleAlert class="mt-0.5 h-3.5 w-3.5 shrink-0" />
      {{ workspace.graphError }} — use Edit to untangle the dependencies.
    </p>

    <p
      v-for="warning in workspace.warnings"
      :key="warning"
      class="flex items-start gap-2 rounded-md border border-amber-200 bg-amber-50 p-2.5 text-xs text-amber-800 dark:border-amber-900 dark:bg-amber-950/40 dark:text-amber-200"
    >
      <TriangleAlert class="mt-0.5 h-3.5 w-3.5 shrink-0" />
      {{ warning }}
    </p>

    <div
      v-if="op && (opRunning || op.terminal === 'failed' || op.terminal === 'cancelled')"
      class="rounded-md border border-slate-200 bg-slate-50 p-2 dark:border-slate-800 dark:bg-neutral-800/50"
    >
      <p class="text-xs font-medium text-slate-600 dark:text-slate-300">
        {{ op.label }}
        <span v-if="opRunning" class="text-amber-600">running…</span>
        <span v-else-if="op.terminal === 'cancelled'" class="text-amber-700">cancelled</span>
        <span v-else class="text-red-700">failed: {{ op.error }}</span>
      </p>
    </div>

    <div
      class="rounded-md border border-slate-200 bg-white p-3 dark:border-slate-800 dark:bg-slate-900"
    >
      <p class="flex items-center gap-1.5 text-xs font-medium text-slate-600 dark:text-slate-300">
        <Network class="h-3.5 w-3.5 text-slate-400" /> Shared network
        <Hint
          text="Mast creates one docker network for this workspace so containers from different member projects can talk to each other by service name (e.g. the api project reaching this project's redis). Click a member to preview the compose change that attaches its services — nothing is written without your ok."
        />
      </p>
      <div class="mt-2 flex flex-wrap gap-1.5">
        <Button
          v-for="member in workspace.members"
          :key="member.project"
          variant="outline"
          size="sm"
          :disabled="store.readOnly"
          @click="openAttach(member.project)"
        >
          {{ projectName(member.project) }}
        </Button>
      </div>
    </div>

    <SnapshotsCard :workspace="workspace.id" />

    <ProjectCard v-for="project in memberProjects" :key="project.id" :project="project" />

    <NetworkAttachDialog
      v-model:open="attachOpen"
      :workspace="workspace.id"
      :project="attachProject"
    />
  </div>
</template>
