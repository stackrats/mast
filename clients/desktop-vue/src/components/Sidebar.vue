<script setup lang="ts">
// Built on the vendored shadcn-vue sidebar menu primitives (ui/Sidebar*.vue).
// The shell stays hand-rolled: shadcn's `SidebarProvider` drives width from a
// CSS variable with fixed expanded/collapsed states, which cannot express the
// drag-to-resize this panel has — and its offcanvas mobile drawer is dead
// weight in a desktop window.
import { onBeforeUnmount, ref } from "vue";
import { Boxes, Folder, Home, Loader2, Pencil, Plus } from "lucide-vue-next";

import type { ProjectStatus, WorkspaceSummary } from "../bindings";
import { loadSidebarWidth, saveSidebarWidth } from "../lib/prefs";
import { statusDot } from "../lib/status";
import { useEngineStore } from "../stores/engine";
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
</script>

<template>
  <aside
    class="relative flex shrink-0 flex-col border-r border-slate-200 bg-slate-50/60 dark:border-slate-800 dark:bg-neutral-900/60"
    :style="{ width: `${width}px` }"
  >
    <nav class="min-w-0 flex-1 overflow-x-hidden overflow-y-auto">
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
        <SidebarGroupLabel>Workspaces</SidebarGroupLabel>
        <Tooltip text="New workspace">
          <SidebarGroupAction @click="emit('newWorkspace')">
            <Plus class="h-3.5 w-3.5" />
          </SidebarGroupAction>
        </Tooltip>
        <SidebarMenu>
          <SidebarMenuItem v-for="ws in store.workspaces" :key="ws.id">
            <SidebarMenuButton
              :is-active="isSelected('workspace', ws.id)"
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
              <span
                v-else
                class="ml-auto mr-5 h-2 w-2 shrink-0 rounded-full"
                :class="statusDot[ws.status]"
              />
            </SidebarMenuButton>
            <Tooltip text="Edit workspace">
              <SidebarMenuAction show-on-hover @click="emit('editWorkspace', ws)">
                <Pencil class="h-3 w-3" />
              </SidebarMenuAction>
            </Tooltip>

            <SidebarMenuSub>
              <SidebarMenuItem v-for="member in ws.members" :key="member.project">
                <SidebarMenuButton
                  :is-active="isSelected('project', member.project)"
                  @click="store.selection = { kind: 'project', id: member.project }"
                >
                  <Loader2
                    v-if="store.hasRunningOp(member.project)"
                    class="h-3 w-3 shrink-0 animate-spin text-amber-500"
                  />
                  <span
                    v-else
                    class="h-1.5 w-1.5 shrink-0 rounded-full"
                    :class="statusDot[projectStatus(member.project)]"
                  />
                  <span class="truncate">{{ projectName(member.project) }}</span>
                </SidebarMenuButton>
              </SidebarMenuItem>
            </SidebarMenuSub>
          </SidebarMenuItem>
        </SidebarMenu>
      </SidebarGroup>

      <!-- Every project, workspace member or not: always individually usable. -->
      <SidebarGroup v-if="store.projects.length">
        <SidebarGroupLabel>Projects</SidebarGroupLabel>
        <SidebarMenu>
          <SidebarMenuItem v-for="project in store.projects" :key="project.id">
            <SidebarMenuButton
              :is-active="isSelected('project', project.id)"
              @click="store.selection = { kind: 'project', id: project.id }"
            >
              <Folder class="h-3.5 w-3.5 shrink-0 text-slate-400" />
              <span class="truncate">{{ project.name }}</span>
              <Loader2
                v-if="store.hasRunningOp(project.id)"
                class="ml-auto h-3 w-3 shrink-0 animate-spin text-amber-500"
              />
              <span
                v-else
                class="ml-auto h-2 w-2 shrink-0 rounded-full"
                :class="statusDot[project.status]"
              />
            </SidebarMenuButton>
          </SidebarMenuItem>
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
