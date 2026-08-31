<script setup lang="ts">
// Right-click on a project row, anywhere it appears: the verbs without the
// trip to the card. Same rule as the palette — a verb that cannot work right
// now is absent, not greyed into decoration.
import {
  ContextMenuContent,
  ContextMenuItem,
  ContextMenuPortal,
  ContextMenuRoot,
  ContextMenuSeparator,
  ContextMenuTrigger,
} from "reka-ui";
import {
  CircleStop,
  FolderOpen,
  Globe,
  Pencil,
  Play,
  RotateCw,
  SquareTerminal,
} from "lucide-vue-next";
import { computed } from "vue";

import type { ProjectId } from "../bindings";
import { menuContentClass, menuItemClass, menuSeparatorClass } from "../lib/menu";
import { useEngineStore } from "../stores/engine";

const { project } = defineProps<{ project: ProjectId }>();
const store = useEngineStore();

const summary = computed(() => store.projects.find((p) => p.id === project) ?? null);
const locked = computed(() => store.readOnly || store.hasRunningOp(project));

function lifecycle(label: string, type: "startProject" | "stopProject" | "restartProject") {
  void store.runLifecycle(project, label, { type, id: project });
}
</script>

<template>
  <ContextMenuRoot>
    <ContextMenuTrigger as-child>
      <slot />
    </ContextMenuTrigger>
    <ContextMenuPortal>
      <ContextMenuContent v-if="summary" :class="menuContentClass">
        <ContextMenuItem
          v-if="summary.status !== 'running'"
          :class="menuItemClass"
          :disabled="locked"
          @select="lifecycle('start', 'startProject')"
        >
          <Play class="h-3.5 w-3.5 text-slate-400" /> Start
        </ContextMenuItem>
        <template v-if="summary.status !== 'stopped'">
          <ContextMenuItem
            :class="menuItemClass"
            :disabled="locked"
            @select="lifecycle('stop', 'stopProject')"
          >
            <CircleStop class="h-3.5 w-3.5 text-red-600 dark:text-red-400" /> Stop
          </ContextMenuItem>
          <ContextMenuItem
            :class="menuItemClass"
            :disabled="locked"
            @select="lifecycle('restart', 'restartProject')"
          >
            <RotateCw class="h-3.5 w-3.5 text-slate-400" /> Restart
          </ContextMenuItem>
        </template>
        <ContextMenuSeparator :class="menuSeparatorClass" />
        <ContextMenuItem
          :class="menuItemClass"
          @select="store.run({ type: 'openTerminal', id: project })"
        >
          <SquareTerminal class="h-3.5 w-3.5 text-slate-400" /> Open in Terminal
        </ContextMenuItem>
        <ContextMenuItem
          :class="menuItemClass"
          @select="store.run({ type: 'openInEditor', id: project })"
        >
          <Pencil class="h-3.5 w-3.5 text-slate-400" /> Open in Editor
        </ContextMenuItem>
        <ContextMenuItem
          v-if="summary.appUrl"
          :class="menuItemClass"
          @select="store.run({ type: 'openInBrowser', id: project })"
        >
          <Globe class="h-3.5 w-3.5 text-slate-400" /> Open in Browser
        </ContextMenuItem>
        <ContextMenuItem
          :class="menuItemClass"
          @select="store.run({ type: 'revealInFileManager', id: project })"
        >
          <FolderOpen class="h-3.5 w-3.5 text-slate-400" /> Reveal in Files
        </ContextMenuItem>
      </ContextMenuContent>
    </ContextMenuPortal>
  </ContextMenuRoot>
</template>
