<script setup lang="ts">
import { computed } from "vue";
import {
  Boxes,
  Container,
  FolderPlus,
  FolderSearch,
  Import,
  Layers,
  LoaderCircle,
  Play,
  Settings,
  TriangleAlert,
  X,
} from "lucide-vue-next";

import { loadRecentWorkspaces } from "../lib/prefs";
import { statusBadgeVariant } from "../lib/status";
import { createdName, useEngineStore } from "../stores/engine";
import Badge from "./ui/Badge.vue";
import Button from "./ui/Button.vue";

const emit = defineEmits<{ openSettings: [] }>();
const store = useEngineStore();

/// Live totals across everything Mast watches.
const overview = computed(() => {
  const services = store.projects.flatMap((p) => p.services);
  return {
    projectsRunning: store.projects.filter((p) => p.status === "running").length,
    projectsTotal: store.projects.length,
    servicesRunning: services.filter((s) => s.state === "running").length,
    servicesTotal: services.length,
    unhealthy: services.filter((s) => s.health === "unhealthy").length,
    workspaces: store.workspaces.length,
    attention: store.projects.filter((p) => p.status === "degraded" || p.status === "failed"),
  };
});

const tileClass =
  "rounded-lg border border-slate-200 bg-white p-3 dark:border-slate-800 dark:bg-slate-900";

/// Recently started workspaces that still exist, newest first.
const recentWorkspaces = computed(() =>
  loadRecentWorkspaces()
    .map((id) => store.workspaces.find((w) => w.id === id))
    .filter((w) => w != null),
);

function quickStart(id: string) {
  void store.runLifecycle(id, "start workspace", { type: "startWorkspace", id });
}

/// Projects being scaffolded. They have no engine id until the import lands,
/// so until then this card is the only place they are visible.
const scaffolding = computed(() =>
  Object.entries(store.operations)
    .map(([key, op]) => ({ key, name: createdName(key), op }))
    .filter((entry) => entry.name != null && entry.op.terminal !== "completed")
    .map((entry) => ({
      key: entry.key,
      name: entry.name as string,
      running: entry.op.terminal === null,
      detail:
        entry.op.terminal === null
          ? // The tail of the stream doubles as the progress line.
            (entry.op.lines[entry.op.lines.length - 1]?.line ?? "starting…")
          : entry.op.terminal === "cancelled"
            ? "cancelled — the half-created directory was removed"
            : (entry.op.error ?? "failed"),
      // Cancellation needs the engine-assigned id, which arrives a beat later.
      cancellable: entry.op.terminal === null && entry.op.id >= 0,
    })),
);
</script>

<template>
  <div class="mx-auto max-w-2xl space-y-4">
    <div
      v-if="store.docker && !store.docker.available"
      class="rounded-lg border border-red-200 bg-red-50 p-3 text-sm text-red-800 dark:border-red-900 dark:bg-red-950/40 dark:text-red-200"
    >
      Docker unavailable{{ store.docker.error ? `: ${store.docker.error}` : "" }} — observation
      paused, retrying…
    </div>

    <div
      v-if="store.readOnly"
      class="rounded-lg border border-amber-200 bg-amber-50 p-3 text-sm text-amber-800 dark:border-amber-900 dark:bg-amber-950/40 dark:text-amber-200"
    >
      Another Mast instance owns this machine — running read-only.
    </div>

    <section v-if="scaffolding.length" class="space-y-2">
      <div
        v-for="entry in scaffolding"
        :key="entry.key"
        class="rounded-lg border p-3"
        :class="
          entry.running
            ? 'border-amber-200 bg-amber-50 dark:border-amber-900 dark:bg-amber-950/40'
            : 'border-red-200 bg-red-50 dark:border-red-900 dark:bg-red-950/40'
        "
      >
        <div class="flex items-center justify-between gap-3">
          <div class="flex min-w-0 items-center gap-2.5">
            <LoaderCircle
              v-if="entry.running"
              class="h-4 w-4 shrink-0 animate-spin text-amber-600"
            />
            <TriangleAlert v-else class="h-4 w-4 shrink-0 text-red-600" />
            <div class="min-w-0">
              <p
                class="truncate text-sm font-medium"
                :class="
                  entry.running
                    ? 'text-amber-800 dark:text-amber-200'
                    : 'text-red-800 dark:text-red-200'
                "
              >
                {{ entry.name }}
                <span v-if="entry.running" class="font-normal">initializing…</span>
              </p>
              <p
                class="truncate text-xs"
                :class="[
                  entry.running
                    ? 'font-mono text-amber-700 dark:text-amber-300'
                    : 'text-red-700 dark:text-red-300',
                ]"
              >
                {{ entry.detail }}
              </p>
            </div>
          </div>
          <Button
            v-if="entry.running"
            variant="outline"
            size="sm"
            class="shrink-0"
            :disabled="!entry.cancellable"
            @click="store.cancelLifecycle(entry.key)"
          >
            <X class="h-3.5 w-3.5" /> Cancel
          </Button>
          <Button
            v-else
            variant="outline"
            size="sm"
            class="shrink-0"
            @click="store.dismissOperation(entry.key)"
          >
            Dismiss
          </Button>
        </div>
      </div>
    </section>

    <section v-if="overview.projectsTotal > 0" class="space-y-2">
      <h2 class="text-sm font-semibold text-slate-700 dark:text-slate-300">Overview</h2>
      <div class="grid grid-cols-3 gap-2">
        <div :class="tileClass">
          <p class="flex items-center gap-1.5 text-xs text-slate-400">
            <Boxes class="h-3.5 w-3.5" /> Projects
          </p>
          <p class="mt-1 text-xl font-semibold text-slate-900 dark:text-slate-100">
            {{ overview.projectsRunning
            }}<span class="text-sm font-normal text-slate-400">/{{ overview.projectsTotal }}</span>
          </p>
          <p class="text-xs text-slate-400">running</p>
        </div>
        <div :class="tileClass">
          <p class="flex items-center gap-1.5 text-xs text-slate-400">
            <Container class="h-3.5 w-3.5" /> Services
          </p>
          <p class="mt-1 text-xl font-semibold text-slate-900 dark:text-slate-100">
            {{ overview.servicesRunning
            }}<span class="text-sm font-normal text-slate-400">/{{ overview.servicesTotal }}</span>
          </p>
          <p class="text-xs" :class="overview.unhealthy ? 'text-red-500' : 'text-slate-400'">
            {{ overview.unhealthy ? `${overview.unhealthy} unhealthy` : "running" }}
          </p>
        </div>
        <div :class="tileClass">
          <p class="flex items-center gap-1.5 text-xs text-slate-400">
            <Layers class="h-3.5 w-3.5" /> Workspaces
          </p>
          <p class="mt-1 text-xl font-semibold text-slate-900 dark:text-slate-100">
            {{ overview.workspaces }}
          </p>
          <p class="text-xs text-slate-400">configured</p>
        </div>
      </div>
      <button
        v-for="project in overview.attention"
        :key="project.id"
        class="flex w-full items-center justify-between rounded-lg border border-amber-200 bg-amber-50 p-2.5 text-left text-xs text-amber-800 hover:bg-amber-100 dark:border-amber-900 dark:bg-amber-950/40 dark:text-amber-200 dark:hover:bg-amber-950/60"
        @click="store.selection = { kind: 'project', id: project.id }"
      >
        <span class="truncate">{{ project.name }}</span>
        <Badge :variant="statusBadgeVariant[project.status]" class="shrink-0">
          {{ project.status }}
        </Badge>
      </button>
    </section>

    <section v-if="recentWorkspaces.length" class="space-y-2">
      <h2 class="text-sm font-semibold text-slate-700 dark:text-slate-300">Recent workspaces</h2>
      <div
        v-for="ws in recentWorkspaces"
        :key="ws.id"
        class="relative flex items-center justify-between gap-3 rounded-lg border border-slate-200 bg-white p-3 hover:bg-slate-50 dark:border-slate-800 dark:bg-slate-900 dark:hover:bg-slate-800/60"
      >
        <button
          class="flex min-w-0 items-center gap-2.5 text-left after:absolute after:inset-0 after:rounded-lg"
          @click="store.selection = { kind: 'workspace', id: ws.id }"
        >
          <Boxes class="h-4 w-4 shrink-0 text-slate-400" />
          <div class="min-w-0">
            <p class="truncate text-sm font-medium text-slate-800 dark:text-slate-200">
              {{ ws.name }}
            </p>
            <p class="text-xs text-slate-400">
              {{ ws.members.length }} project{{ ws.members.length === 1 ? "" : "s" }} ·
              {{ ws.status }}
            </p>
          </div>
        </button>
        <Button
          v-if="ws.status !== 'running'"
          size="sm"
          class="relative z-10"
          :disabled="store.readOnly || ws.graphError != null || store.busy > 0"
          @click="quickStart(ws.id)"
        >
          <Play class="h-3.5 w-3.5" /> Start all
        </Button>
        <Badge v-else variant="success">running</Badge>
      </div>
    </section>

    <section v-if="store.discovered.length" class="space-y-2">
      <h2 class="flex items-center gap-2 text-sm font-semibold text-slate-700 dark:text-slate-300">
        <FolderSearch class="h-4 w-4 text-slate-400" />
        Discovered projects
      </h2>
      <div
        v-for="candidate in store.discovered"
        :key="candidate.path"
        class="flex items-center justify-between rounded-lg border border-dashed border-slate-300 bg-white p-3 dark:border-slate-700 dark:bg-slate-900"
      >
        <div class="min-w-0">
          <div class="flex items-center gap-2">
            <span class="truncate text-sm font-medium text-slate-800 dark:text-slate-200">{{
              candidate.name
            }}</span>
            <Badge v-if="candidate.isSail" variant="outline">Sail</Badge>
          </div>
          <p class="truncate font-mono text-xs text-slate-400">{{ candidate.path }}</p>
        </div>
        <Button
          size="sm"
          :disabled="store.busy > 0"
          @click="store.run({ type: 'importProject', path: candidate.path })"
        >
          <Import class="h-3.5 w-3.5" /> Import
        </Button>
      </div>
    </section>

    <section
      v-if="store.projects.length === 0 && store.discovered.length === 0"
      class="rounded-lg border border-dashed border-slate-300 p-10 text-center dark:border-slate-700"
    >
      <p class="text-sm text-slate-600 dark:text-slate-300">No projects yet.</p>
      <p class="mt-1 text-xs text-slate-400">
        Add a watched directory in Settings and Mast will discover your Sail projects.
      </p>
      <Button class="mt-4" variant="outline" @click="emit('openSettings')">
        <Settings class="h-3.5 w-3.5" /> Open settings
      </Button>
    </section>

    <template v-else>
      <section
        v-if="store.watchedDirectories.length === 0"
        class="flex items-center justify-between rounded-lg border border-amber-200 bg-amber-50 p-3 text-sm text-amber-800 dark:border-amber-900 dark:bg-amber-950/40 dark:text-amber-200"
      >
        <span> Not watching any directories — new projects won't be discovered. </span>
        <Button size="sm" variant="outline" @click="emit('openSettings')">
          <FolderPlus class="h-3.5 w-3.5" /> Add directory
        </Button>
      </section>

      <section class="flex items-center justify-between text-xs text-slate-400">
        <span>
          <!-- "directory" does not pluralise by appending s, so it is picked
               whole rather than suffixed like the counts beside it. -->
          Watching {{ store.watchedDirectories.length }}
          {{ store.watchedDirectories.length === 1 ? "directory" : "directories" }} ·
          {{ store.projects.length }} project{{ store.projects.length === 1 ? "" : "s" }} ·
          {{ store.workspaces.length }} workspace{{ store.workspaces.length === 1 ? "" : "s" }}
        </span>
        <Button variant="ghost" size="sm" @click="emit('openSettings')">Manage directories</Button>
      </section>
    </template>
  </div>
</template>
