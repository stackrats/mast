<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue";

import { ChevronDown, ChevronUp, SquareTerminal } from "lucide-vue-next";

import type { ProjectSummary, WorkspaceSummary } from "./bindings";
import AboutDialog from "./components/AboutDialog.vue";
import AppMenubar from "./components/AppMenubar.vue";
import CommandPalette from "./components/CommandPalette.vue";
import DiagnosticsDialog from "./components/DiagnosticsDialog.vue";
import LogsDialog from "./components/LogsDialog.vue";
import LogsPanel from "./components/LogsPanel.vue";
import NewProjectDialog from "./components/NewProjectDialog.vue";
import HomePane from "./components/HomePane.vue";
import ProjectCard from "./components/ProjectCard.vue";
import SettingsDialog from "./components/SettingsDialog.vue";
import ShortcutsDialog from "./components/ShortcutsDialog.vue";
import Sidebar from "./components/Sidebar.vue";
import WorkspaceDetail from "./components/WorkspaceDetail.vue";
import WorkspaceDialog from "./components/WorkspaceDialog.vue";
import UsageReadout from "./components/UsageReadout.vue";
import Button from "./components/ui/Button.vue";
import { TooltipProvider } from "reka-ui";

import { parseDeepLink } from "./lib/deeplink";
import { applyTheme, loadTheme } from "./lib/prefs";
import { onDeepLink, takeDeepLinks } from "./lib/transport";
import { useEngineStore } from "./stores/engine";

const store = useEngineStore();
const settingsOpen = ref(false);
const diagnosticsOpen = ref(false);
// A project's Diagnose button scopes the dialog to it; the menubar entry
// runs the full set.
const diagnosticsScope = ref<ProjectSummary | null>(null);
function openDiagnostics(scope: ProjectSummary | null = null) {
  diagnosticsScope.value = scope;
  diagnosticsOpen.value = true;
}
const newProjectOpen = ref(false);
/** A mast://clone link's URL, waiting to prefill the add-project dialog. */
const clonePrefill = ref<string | null>(null);
// Consumed with the dialog: closing it (submitted or not) spends the link.
watch(newProjectOpen, (open) => {
  if (!open) clonePrefill.value = null;
});

// mast:// links may only navigate or prefill, never act (see lib/deeplink).
function applyDeepLink(raw: string) {
  const link = parseDeepLink(raw);
  if (!link) {
    store.pushActivity(`⚠ ignored an unrecognized link: ${raw}`, true);
    return;
  }
  if (link.kind === "clone") {
    clonePrefill.value = link.url;
    newProjectOpen.value = true;
    return;
  }
  void selectProjectSoon(link.ref);
}

/** A launch-time link races the first snapshot, so give the project list a
 * few seconds to exist before declaring the target unknown. */
async function selectProjectSoon(ref: string) {
  const deadline = Date.now() + 5000;
  while (Date.now() < deadline) {
    const match = store.projects.find((p) => p.name === ref || p.path.endsWith(ref));
    if (match) {
      store.selection = { kind: "project", id: match.id };
      return;
    }
    await new Promise((resolve) => setTimeout(resolve, 200));
  }
  store.pushActivity(`⚠ no project matches the link "${ref}"`, true);
}
const aboutOpen = ref(false);
const paletteOpen = ref(false);
const shortcutsOpen = ref(false);

// Ctrl/Cmd-K from anywhere, including from inside a text field — the palette
// is how you leave wherever you are, so it must not be capturable by the
// thing you are trying to leave. The one exception is itself.
function onKeydown(event: KeyboardEvent) {
  if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "k") {
    event.preventDefault();
    paletteOpen.value = !paletteOpen.value;
    return;
  }
  // Ctrl/Cmd-1…9 jumps to the nth sidebar project (same alphabetical order
  // the sidebar shows). Same "leave from anywhere" rule as the palette.
  if ((event.metaKey || event.ctrlKey) && !event.shiftKey && !event.altKey) {
    const n = Number(event.key);
    if (n >= 1 && n <= 9) {
      const project = store.projects[n - 1];
      if (project) {
        event.preventDefault();
        store.selection = { kind: "project", id: project.id };
      }
    }
  }
  // `?` opens the shortcut list — but only outside text fields, where the
  // character is just typing. `event.key` keeps it layout-independent.
  if (event.key === "?" && !event.metaKey && !event.ctrlKey && !event.altKey) {
    const target = event.target as HTMLElement | null;
    const editable =
      target != null &&
      (target.tagName === "INPUT" || target.tagName === "TEXTAREA" || target.isContentEditable);
    if (!editable) {
      event.preventDefault();
      shortcutsOpen.value = true;
    }
  }
}

function openPaletteDialog(what: "settings" | "newProject" | "newWorkspace" | "shortcuts") {
  if (what === "settings") settingsOpen.value = true;
  else if (what === "newProject") newProjectOpen.value = true;
  else if (what === "shortcuts") shortcutsOpen.value = true;
  else newWorkspace();
}
const workspaceDialogOpen = ref(false);
const editingWorkspace = ref<WorkspaceSummary | null>(null);

applyTheme(loadTheme());

// Sampling the machine costs real CPU, so it only runs while someone can see
// the answer. The engine does nothing while unsubscribed; this is the client
// half of that bargain.
function syncUsageToVisibility() {
  if (document.hidden) void store.disconnectUsage();
  else void store.connectUsage();
  // Coming back to a window already showing the project counts as visiting
  // it — its attention marker has been seen.
  if (!document.hidden && store.selection.kind === "project") {
    store.clearAttention(store.selection.id);
  }
}

// Navigating to a project is what clears its attention dot: the state the
// dot pointed at is now on screen.
watch(
  () => store.selection,
  (selection) => {
    if (selection.kind === "project") store.clearAttention(selection.id);
  },
);

onMounted(() => {
  void store.connect();
  document.addEventListener("visibilitychange", syncUsageToVisibility);
  window.addEventListener("keydown", onKeydown);
  // Live links first, then the parked launch-time ones — that order is what
  // keeps a link from falling between the two.
  void onDeepLink(applyDeepLink).then(async () => {
    for (const url of await takeDeepLinks()) applyDeepLink(url);
  });
});

onBeforeUnmount(() => {
  document.removeEventListener("visibilitychange", syncUsageToVisibility);
  window.removeEventListener("keydown", onKeydown);
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
      <CommandPalette v-model:open="paletteOpen" @dialog="openPaletteDialog" />

      <AppMenubar
        @open-palette="paletteOpen = true"
        @open-settings="settingsOpen = true"
        @new-workspace="newWorkspace"
        @open-diagnostics="openDiagnostics()"
        @new-project="newProjectOpen = true"
        @open-about="aboutOpen = true"
        @open-shortcuts="shortcutsOpen = true"
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
          <div v-else-if="selectedProject" class="mx-auto max-w-4xl">
            <ProjectCard :project="selectedProject" @diagnose="openDiagnostics(selectedProject)" />
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
        <div class="ml-auto flex items-center gap-3">
          <UsageReadout />
          <span
            v-if="store.phase !== 'live'"
            class="rounded-full bg-amber-100 px-1.5 py-0.5 text-[11px] text-amber-700 dark:bg-amber-900/60 dark:text-amber-300"
          >
            connecting…
          </span>
        </div>
      </div>

      <SettingsDialog v-model:open="settingsOpen" />
      <DiagnosticsDialog v-model:open="diagnosticsOpen" :scope="diagnosticsScope" />
      <NewProjectDialog v-model:open="newProjectOpen" :clone-url="clonePrefill" />
      <WorkspaceDialog v-model:open="workspaceDialogOpen" :editing="editingWorkspace" />
      <AboutDialog v-model:open="aboutOpen" />
      <ShortcutsDialog v-model:open="shortcutsOpen" />
      <LogsDialog />
    </div>
  </TooltipProvider>
</template>
