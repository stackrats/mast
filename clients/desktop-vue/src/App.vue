<script setup lang="ts">
import { computed, onMounted, ref } from "vue";

import { ChevronDown, ChevronUp, SquareTerminal } from "lucide-vue-next";

import type { WorkspaceSummary } from "./bindings";
import AppMenubar from "./components/AppMenubar.vue";
import DiagnosticsDialog from "./components/DiagnosticsDialog.vue";
import LogsDialog from "./components/LogsDialog.vue";
import LogsPanel from "./components/LogsPanel.vue";
import NewProjectDialog from "./components/NewProjectDialog.vue";
import HomePane from "./components/HomePane.vue";
import ProjectCard from "./components/ProjectCard.vue";
import SettingsDialog from "./components/SettingsDialog.vue";
import Sidebar from "./components/Sidebar.vue";
import WorkspaceDetail from "./components/WorkspaceDetail.vue";
import WorkspaceDialog from "./components/WorkspaceDialog.vue";
import Button from "./components/ui/Button.vue";
import { TooltipProvider } from "reka-ui";

import { applyTheme, loadTheme } from "./lib/prefs";
import { useEngineStore } from "./stores/engine";

const store = useEngineStore();
const settingsOpen = ref(false);
const diagnosticsOpen = ref(false);
const newProjectOpen = ref(false);
const workspaceDialogOpen = ref(false);
const editingWorkspace = ref<WorkspaceSummary | null>(null);

applyTheme(loadTheme());

onMounted(() => {
  void store.connect();
});

const selectedProject = computed(() => {
  if (store.selection.kind !== "project") return null;
  const id = store.selection.id;
  return store.projects.find((p) => p.id === id) ?? null;
});

const selectedWorkspace = computed(() => {
  if (store.selection.kind !== "workspace") return null;
  const id = store.selection.id;
  return store.workspaces.find((w) => w.id === id) ?? null;
});

function newWorkspace() {
  editingWorkspace.value = null;
  workspaceDialogOpen.value = true;
}

function editWorkspace(ws: WorkspaceSummary) {
  editingWorkspace.value = ws;
  workspaceDialogOpen.value = true;
}
</script>

<template>
  <TooltipProvider :delay-duration="500" :skip-delay-duration="200" disable-hoverable-content>
    <div
      class="flex h-screen flex-col bg-white text-slate-900 dark:bg-slate-950 dark:text-slate-100"
    >
      <AppMenubar
        @open-settings="settingsOpen = true"
        @new-workspace="newWorkspace"
        @open-diagnostics="diagnosticsOpen = true"
        @new-project="newProjectOpen = true"
      />

      <div class="flex min-h-0 flex-1">
        <Sidebar @new-workspace="newWorkspace" @edit-workspace="editWorkspace" />

        <main class="min-w-0 flex-1 overflow-y-auto bg-slate-50/40 p-5 dark:bg-slate-950">
          <p
            v-if="store.error"
            class="mb-4 rounded-md border border-red-200 bg-red-50 p-3 text-sm text-red-800 dark:border-red-900 dark:bg-red-950/50 dark:text-red-200"
          >
            {{ store.error }}
          </p>

          <WorkspaceDetail
            v-if="selectedWorkspace"
            :workspace="selectedWorkspace"
            @edit="editWorkspace(selectedWorkspace)"
          />
          <div v-else-if="selectedProject" class="mx-auto max-w-2xl">
            <ProjectCard :project="selectedProject" />
          </div>
          <HomePane v-else @open-settings="settingsOpen = true" />
        </main>
      </div>

      <LogsPanel />

      <div
        class="flex items-center justify-between border-t border-slate-200 bg-white px-2 py-0.5 dark:border-slate-800 dark:bg-slate-950"
      >
        <Button variant="ghost" size="sm" @click="store.setLogsOpen(!store.logsOpen)">
          <SquareTerminal class="h-3.5 w-3.5" /> Logs
          <ChevronDown v-if="store.logsOpen" class="h-3 w-3 text-slate-400" />
          <ChevronUp v-else class="h-3 w-3 text-slate-400" />
        </Button>
        <span
          v-if="store.phase !== 'live'"
          class="rounded-full bg-amber-100 px-1.5 py-0.5 text-[11px] text-amber-700 dark:bg-amber-900/60 dark:text-amber-300"
        >
          connecting…
        </span>
      </div>

      <SettingsDialog v-model:open="settingsOpen" />
      <DiagnosticsDialog v-model:open="diagnosticsOpen" />
      <NewProjectDialog v-model:open="newProjectOpen" />
      <WorkspaceDialog v-model:open="workspaceDialogOpen" :editing="editingWorkspace" />
      <LogsDialog />
    </div>
  </TooltipProvider>
</template>
