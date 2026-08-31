<script setup lang="ts">
// Built on the vendored shadcn-vue sidebar menu primitives (ui/Sidebar*.vue).
// The shell stays hand-rolled: shadcn's `SidebarProvider` drives width from a
// CSS variable with fixed expanded/collapsed states, which cannot express the
// drag-to-resize this panel has — and its offcanvas mobile drawer is dead
// weight in a desktop window.
import { computed, onBeforeUnmount, ref } from "vue";
import { Boxes, ChevronRight, Folder, Home, Loader2, Plus, Search } from "lucide-vue-next";

import type { ProjectStatus, WorkspaceSummary } from "../bindings";
import {
  loadSidebarCollapsed,
  loadSidebarWidth,
  saveSidebarCollapsed,
  saveSidebarWidth,
} from "../lib/prefs";
import { statusDot } from "../lib/status";
import { useEngineStore } from "../stores/engine";
import ProjectContextMenu from "./ProjectContextMenu.vue";
import WorkspaceContextMenu from "./WorkspaceContextMenu.vue";
import SidebarGroup from "./ui/SidebarGroup.vue";
import SidebarGroupAction from "./ui/SidebarGroupAction.vue";
import SidebarGroupLabel from "./ui/SidebarGroupLabel.vue";
import SidebarMenu from "./ui/SidebarMenu.vue";
import SidebarMenuAction from "./ui/SidebarMenuAction.vue";
import SidebarMenuBadge from "./ui/SidebarMenuBadge.vue";
import SidebarMenuButton from "./ui/SidebarMenuButton.vue";
import SidebarMenuItem from "./ui/SidebarMenuItem.vue";
import SidebarMenuSub from "./ui/SidebarMenuSub.vue";
import Tooltip from "./ui/Tooltip.vue";

const emit = defineEmits<{ newWorkspace: []; editWorkspace: [ws: WorkspaceSummary] }>();
const store = useEngineStore();

const width = ref(loadSidebarWidth());
let dragging = false;

function startDrag(event: MouseEvent) {
  dragging = true;
  event.preventDefault();
  const move = (e: MouseEvent) => {
    if (!dragging) return;
    width.value = Math.min(480, Math.max(180, e.clientX));
  };
  const up = () => {
    dragging = false;
    saveSidebarWidth(width.value);
    window.removeEventListener("mousemove", move);
    window.removeEventListener("mouseup", up);
  };
  window.addEventListener("mousemove", move);
  window.addEventListener("mouseup", up);
}

onBeforeUnmount(() => {
  dragging = false;
});

function isSelected(kind: string, id?: string): boolean {
  const sel = store.selection;
  if (sel.kind !== kind) return false;
  if (kind === "home") return true;
  return "id" in sel && sel.id === id;
}

function projectName(id: string): string {
  return store.projects.find((p) => p.id === id)?.name ?? id;
}

function projectStatus(id: string): ProjectStatus {
  return store.projects.find((p) => p.id === id)?.status ?? "stopped";
}

// --- filter: type-to-narrow, offered whenever there is anything to narrow.
const filter = ref("");
const filterable = computed(() => store.projects.length > 0);
const needle = computed(() => (filterable.value ? filter.value.trim().toLowerCase() : ""));
const visibleProjects = computed(() => {
  if (!needle.value) return store.projects;
  return store.projects.filter((p) => p.name.toLowerCase().includes(needle.value));
});
/** A workspace stays while its own name or any member's matches. */
const visibleWorkspaces = computed(() => {
  if (!needle.value) return store.workspaces;
  return store.workspaces.filter(
    (ws) =>
      ws.name.toLowerCase().includes(needle.value) ||
      ws.members.some((m) => projectName(m.project).toLowerCase().includes(needle.value)),
  );
});

// --- collapse: sections and single workspaces fold shut and stay that way
// across restarts. A live filter overrides every fold — a match that stays
// hidden would read as the filter being broken.
const collapsed = ref(loadSidebarCollapsed());
function toggleCollapsed(key: string) {
  collapsed.value = { ...collapsed.value, [key]: !collapsed.value[key] };
  saveSidebarCollapsed(collapsed.value);
}
function isCollapsed(key: string): boolean {
  return !needle.value && collapsed.value[key] === true;
}

// --- drag & drop: reorder both lists, and move projects in and out of
// workspaces. Every drop ends in a persisted action; nothing is
// session-only. Disabled while the filter narrows the view — reordering a
// list you can only see part of writes an order you never saw.
type DragPayload =
  | { kind: "project"; id: string }
  | { kind: "workspace"; id: string }
  | { kind: "member"; id: string; ws: string };
const drag = ref<DragPayload | null>(null);
const dropHint = ref<{ key: string; edge: "before" | "after" | "into" } | null>(null);
const canDrag = computed(() => !needle.value && !store.readOnly);

function startRowDrag(payload: DragPayload, event: DragEvent) {
  drag.value = payload;
  // Firefox refuses to start a drag with an empty data store.
  event.dataTransfer?.setData("text/plain", payload.id);
  if (event.dataTransfer) event.dataTransfer.effectAllowed = "move";
}
function endRowDrag() {
  drag.value = null;
  dropHint.value = null;
}
function hintIs(key: string, edge: "before" | "after" | "into"): boolean {
  return dropHint.value?.key === key && dropHint.value.edge === edge;
}
function edgeOf(event: DragEvent): "before" | "after" {
  const rect = (event.currentTarget as HTMLElement).getBoundingClientRect();
  return event.clientY < rect.top + rect.height / 2 ? "before" : "after";
}
/** The dragged id moved next to the target, everything else untouched. */
function reorderedIds(
  list: string[],
  dragged: string,
  target: string,
  edge: "before" | "after",
): string[] {
  const without = list.filter((id) => id !== dragged);
  const at = without.indexOf(target) + (edge === "after" ? 1 : 0);
  return [...without.slice(0, at), dragged, ...without.slice(at)];
}

function overProject(id: string, event: DragEvent) {
  if (drag.value?.kind !== "project" || drag.value.id === id) return;
  event.preventDefault();
  dropHint.value = { key: `p:${id}`, edge: edgeOf(event) };
}
function dropOnProject(id: string) {
  const dragged = drag.value;
  const hint = dropHint.value;
  endRowDrag();
  if (dragged?.kind !== "project" || !hint || hint.edge === "into") return;
  void store.reorderProjects(
    reorderedIds(
      store.projects.map((p) => p.id),
      dragged.id,
      id,
      hint.edge,
    ),
  );
}

function overWorkspace(ws: WorkspaceSummary, event: DragEvent) {
  const dragged = drag.value;
  if (!dragged) return;
  if (dragged.kind === "workspace") {
    if (dragged.id === ws.id) return;
    event.preventDefault();
    dropHint.value = { key: `ws:${ws.id}`, edge: edgeOf(event) };
    return;
  }
  // A project (or another workspace's member) dropped ON a workspace joins it.
  const from = dragged.kind === "member" ? dragged.ws : null;
  if (from === ws.id) return;
  if (ws.members.some((m) => m.project === dragged.id)) return;
  event.preventDefault();
  dropHint.value = { key: `ws:${ws.id}`, edge: "into" };
}
function dropOnWorkspace(ws: WorkspaceSummary) {
  const dragged = drag.value;
  const hint = dropHint.value;
  endRowDrag();
  if (!dragged || !hint) return;
  if (dragged.kind === "workspace" && hint.edge !== "into") {
    void store.reorderWorkspaces(
      reorderedIds(
        store.workspaces.map((w) => w.id),
        dragged.id,
        ws.id,
        hint.edge,
      ),
    );
    return;
  }
  if (hint.edge === "into") {
    saveMembers(ws, [...ws.members, { project: dragged.id, dependsOn: [] }]);
    if (dragged.kind === "member") removeMember(dragged.ws, dragged.id);
  }
}

function overMember(ws: WorkspaceSummary, id: string, event: DragEvent) {
  const dragged = drag.value;
  if (dragged?.kind !== "member" || dragged.ws !== ws.id || dragged.id === id) return;
  event.preventDefault();
  dropHint.value = { key: `m:${ws.id}:${id}`, edge: edgeOf(event) };
}
function dropOnMember(ws: WorkspaceSummary, id: string) {
  const dragged = drag.value;
  const hint = dropHint.value;
  endRowDrag();
  if (dragged?.kind !== "member" || dragged.ws !== ws.id || !hint || hint.edge === "into") return;
  const order = reorderedIds(
    ws.members.map((m) => m.project),
    dragged.id,
    id,
    hint.edge,
  );
  saveMembers(
    ws,
    order.flatMap((project) => ws.members.filter((m) => m.project === project)),
  );
}

/** Dropping a member onto the Projects section takes it out of its
 * workspace — the project itself always stays in the flat list. */
function overProjectsSection(event: DragEvent) {
  if (drag.value?.kind !== "member") return;
  event.preventDefault();
  dropHint.value = { key: "section:projects", edge: "into" };
}
function dropOnProjectsSection() {
  const dragged = drag.value;
  endRowDrag();
  if (dragged?.kind !== "member") return;
  removeMember(dragged.ws, dragged.id);
}

function saveMembers(ws: WorkspaceSummary, members: WorkspaceSummary["members"]) {
  void store.run({ type: "saveWorkspace", id: ws.id, name: ws.name, members });
}
function removeMember(wsId: string, projectId: string) {
  const ws = store.workspaces.find((w) => w.id === wsId);
  if (!ws) return;
  if (ws.members.length <= 1) {
    store.pushActivity(
      "⚠ a workspace needs at least one member — edit the workspace to delete it instead",
      true,
    );
    return;
  }
  // Anything that depended on the leaver stops naming it, or the graph
  // check would refuse the save.
  saveMembers(
    ws,
    ws.members
      .filter((m) => m.project !== projectId)
      .map((m) => ({ ...m, dependsOn: m.dependsOn.filter((d) => d !== projectId) })),
  );
}

/** The insertion line between rows while a reorder drag hovers. */
const dropLineClass = "mx-2 h-0.5 rounded bg-slate-400 dark:bg-slate-500";
/** The ring around a workspace row while a join drag hovers. */
const dropIntoClass = "outline-2 -outline-offset-1 outline-slate-400 outline-dashed";
</script>

<template>
  <aside
    class="relative flex shrink-0 flex-col border-r border-slate-200 bg-slate-50/60 dark:border-slate-800 dark:bg-neutral-900/60"
    :style="{ width: `${width}px` }"
  >
    <nav class="min-w-0 flex-1 overflow-x-hidden overflow-y-auto">
      <!-- Only once the list outgrows a glance — a filter over five rows is
           chrome, over twenty it is the way in. -->
      <div v-if="filterable" class="px-2 pt-2">
        <div class="relative">
          <Search
            class="pointer-events-none absolute top-1/2 left-2 h-3 w-3 -translate-y-1/2 text-slate-400"
          />
          <input
            v-model="filter"
            type="text"
            placeholder="Filter projects…"
            class="w-full rounded-md border border-slate-200 bg-white py-1 pr-2 pl-6 text-xs text-slate-900 placeholder:text-slate-400 focus:ring-1 focus:ring-slate-400 focus:outline-none dark:border-slate-700 dark:bg-neutral-900 dark:text-slate-100"
            @keydown.escape="filter = ''"
          />
        </div>
      </div>
      <SidebarGroup>
        <SidebarMenu>
          <SidebarMenuItem>
            <SidebarMenuButton
              :is-active="isSelected('home')"
              @click="store.selection = { kind: 'home' }"
            >
              <Home class="h-3.5 w-3.5 shrink-0 text-slate-400" />
              Home
              <SidebarMenuBadge v-if="store.discovered.length">
                {{ store.discovered.length }}
              </SidebarMenuBadge>
            </SidebarMenuButton>
          </SidebarMenuItem>
        </SidebarMenu>
      </SidebarGroup>

      <SidebarGroup>
        <SidebarGroupLabel>
          <button
            class="flex h-6 w-full items-center gap-1 rounded-md px-2 pr-6 text-left hover:bg-slate-100 hover:text-slate-600 focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-slate-400 dark:hover:bg-slate-800 dark:hover:text-slate-300"
            @click="toggleCollapsed('group:workspaces')"
          >
            <ChevronRight
              class="h-3 w-3 transition-transform"
              :class="isCollapsed('group:workspaces') ? '' : 'rotate-90'"
            />
            Workspaces
          </button>
        </SidebarGroupLabel>
        <Tooltip text="New workspace">
          <SidebarGroupAction @click="emit('newWorkspace')">
            <Plus class="h-3.5 w-3.5" />
          </SidebarGroupAction>
        </Tooltip>
        <SidebarMenu v-if="!isCollapsed('group:workspaces')">
          <SidebarMenuItem v-for="ws in visibleWorkspaces" :key="ws.id">
            <div v-if="hintIs(`ws:${ws.id}`, 'before')" :class="dropLineClass" />
            <WorkspaceContextMenu :workspace="ws" @edit="emit('editWorkspace', ws)">
              <SidebarMenuButton
                :is-active="isSelected('workspace', ws.id)"
                :class="hintIs(`ws:${ws.id}`, 'into') ? dropIntoClass : ''"
                :draggable="canDrag"
                @dragstart="startRowDrag({ kind: 'workspace', id: ws.id }, $event)"
                @dragend="endRowDrag"
                @dragover="overWorkspace(ws, $event)"
                @dragleave="dropHint = null"
                @drop.prevent="dropOnWorkspace(ws)"
                @click="store.selection = { kind: 'workspace', id: ws.id }"
              >
                <Boxes class="h-3.5 w-3.5 shrink-0 text-slate-400" />
                <span class="truncate">{{ ws.name }}</span>
                <!-- While something is running, the dot would be reporting the
                     state the project is leaving, not the one it is heading
                     for. The spinner replaces it rather than joining it: one
                     glyph per row, and it is the one that is still true. -->
                <Loader2
                  v-if="store.hasRunningOp(ws.id)"
                  class="ml-auto mr-5 h-3 w-3 shrink-0 animate-spin text-amber-500"
                />
                <!-- The word, one hover away: colour alone excludes anyone who
                     cannot tell these hues apart. -->
                <Tooltip v-else :text="ws.status">
                  <span
                    class="ml-auto mr-5 h-2 w-2 shrink-0 rounded-full"
                    :class="statusDot[ws.status]"
                  />
                </Tooltip>
              </SidebarMenuButton>
            </WorkspaceContextMenu>
            <div v-if="hintIs(`ws:${ws.id}`, 'after')" :class="dropLineClass" />
            <!-- Edit moved into the right-click menu; the one action slot
                 folds the member list instead. -->
            <Tooltip :text="isCollapsed(`ws:${ws.id}`) ? 'Show projects' : 'Hide projects'">
              <SidebarMenuAction @click="toggleCollapsed(`ws:${ws.id}`)">
                <ChevronRight
                  class="h-3 w-3 transition-transform"
                  :class="isCollapsed(`ws:${ws.id}`) ? '' : 'rotate-90'"
                />
              </SidebarMenuAction>
            </Tooltip>

            <SidebarMenuSub v-if="!isCollapsed(`ws:${ws.id}`)">
              <SidebarMenuItem v-for="member in ws.members" :key="member.project">
                <div
                  v-if="hintIs(`m:${ws.id}:${member.project}`, 'before')"
                  :class="dropLineClass"
                />
                <ProjectContextMenu :project="member.project">
                  <SidebarMenuButton
                    :is-active="isSelected('project', member.project)"
                    :draggable="canDrag"
                    @dragstart="
                      startRowDrag({ kind: 'member', id: member.project, ws: ws.id }, $event)
                    "
                    @dragend="endRowDrag"
                    @dragover="overMember(ws, member.project, $event)"
                    @dragleave="dropHint = null"
                    @drop.prevent="dropOnMember(ws, member.project)"
                    @click="store.selection = { kind: 'project', id: member.project }"
                  >
                    <Loader2
                      v-if="store.hasRunningOp(member.project)"
                      class="h-3 w-3 shrink-0 animate-spin text-amber-500"
                    />
                    <Tooltip v-else :text="projectStatus(member.project)">
                      <span
                        class="h-1.5 w-1.5 shrink-0 rounded-full"
                        :class="statusDot[projectStatus(member.project)]"
                      />
                    </Tooltip>
                    <span class="truncate">{{ projectName(member.project) }}</span>
                    <!-- A toast fired while you were elsewhere is gone; the dot
                         stays until you visit the project it points at. -->
                    <Tooltip
                      v-if="store.attentionFor(member.project).length"
                      :text="`While you were away: ${store.attentionFor(member.project).join(' · ')}`"
                    >
                      <span class="ml-1 h-1.5 w-1.5 shrink-0 rounded-full bg-amber-500" />
                    </Tooltip>
                  </SidebarMenuButton>
                </ProjectContextMenu>
                <div
                  v-if="hintIs(`m:${ws.id}:${member.project}`, 'after')"
                  :class="dropLineClass"
                />
              </SidebarMenuItem>
            </SidebarMenuSub>
          </SidebarMenuItem>
        </SidebarMenu>
      </SidebarGroup>

      <!-- Every project, workspace member or not: always individually usable.
           Also the drop target that takes a member OUT of its workspace. -->
      <SidebarGroup
        v-if="store.projects.length"
        @dragover="overProjectsSection"
        @drop.prevent="dropOnProjectsSection"
      >
        <SidebarGroupLabel>
          <button
            class="flex h-6 w-full items-center gap-1 rounded-md px-2 pr-6 text-left hover:bg-slate-100 hover:text-slate-600 focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-slate-400 dark:hover:bg-slate-800 dark:hover:text-slate-300"
            :class="hintIs('section:projects', 'into') ? dropIntoClass : ''"
            @click="toggleCollapsed('group:projects')"
          >
            <ChevronRight
              class="h-3 w-3 transition-transform"
              :class="isCollapsed('group:projects') ? '' : 'rotate-90'"
            />
            Projects
          </button>
        </SidebarGroupLabel>
        <SidebarMenu v-if="!isCollapsed('group:projects')">
          <SidebarMenuItem v-for="project in visibleProjects" :key="project.id">
            <div v-if="hintIs(`p:${project.id}`, 'before')" :class="dropLineClass" />
            <ProjectContextMenu :project="project.id">
              <SidebarMenuButton
                :is-active="isSelected('project', project.id)"
                :draggable="canDrag"
                @dragstart="startRowDrag({ kind: 'project', id: project.id }, $event)"
                @dragend="endRowDrag"
                @dragover.stop="overProject(project.id, $event)"
                @dragleave="dropHint = null"
                @drop.prevent.stop="dropOnProject(project.id)"
                @click="store.selection = { kind: 'project', id: project.id }"
              >
                <Folder class="h-3.5 w-3.5 shrink-0 text-slate-400" />
                <span class="truncate">{{ project.name }}</span>
                <Tooltip
                  v-if="store.attentionFor(project.id).length"
                  :text="`While you were away: ${store.attentionFor(project.id).join(' · ')}`"
                >
                  <span class="ml-1 h-1.5 w-1.5 shrink-0 rounded-full bg-amber-500" />
                </Tooltip>
                <Loader2
                  v-if="store.hasRunningOp(project.id)"
                  class="ml-auto h-3 w-3 shrink-0 animate-spin text-amber-500"
                />
                <Tooltip v-else :text="project.status">
                  <span
                    class="ml-auto h-2 w-2 shrink-0 rounded-full"
                    :class="statusDot[project.status]"
                  />
                </Tooltip>
              </SidebarMenuButton>
            </ProjectContextMenu>
            <div v-if="hintIs(`p:${project.id}`, 'after')" :class="dropLineClass" />
          </SidebarMenuItem>
          <p v-if="needle && visibleProjects.length === 0" class="px-2 py-1 text-xs text-slate-400">
            No project matches "{{ filter.trim() }}".
          </p>
        </SidebarMenu>
      </SidebarGroup>
    </nav>

    <!-- Drag handle -->
    <div
      class="absolute top-0 -right-0.5 z-10 h-full w-1.5 cursor-col-resize hover:bg-slate-300/60 dark:hover:bg-neutral-600/60"
      @mousedown="startDrag"
    />
  </aside>
</template>
