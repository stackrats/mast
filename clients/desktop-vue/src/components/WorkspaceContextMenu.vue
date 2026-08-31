<script setup lang="ts">
// Right-click on a workspace row: the same rule as the project menu — verbs
// that cannot work are absent — plus Edit, which used to be a hover pencil
// and now lives here so the row's one action slot can hold the collapse
// chevron instead.
import {
  ContextMenuContent,
  ContextMenuItem,
  ContextMenuPortal,
  ContextMenuRoot,
  ContextMenuSeparator,
  ContextMenuTrigger,
} from "reka-ui";
import { CircleStop, Pencil, Play } from "lucide-vue-next";
import { computed } from "vue";

import type { WorkspaceSummary } from "../bindings";
import { menuContentClass, menuItemClass, menuSeparatorClass } from "../lib/menu";
import { useEngineStore } from "../stores/engine";

const { workspace } = defineProps<{ workspace: WorkspaceSummary }>();
const emit = defineEmits<{ edit: [] }>();
const store = useEngineStore();

const locked = computed(() => store.readOnly || store.hasRunningOp(workspace.id));
</script>

<template>
  <ContextMenuRoot>
    <ContextMenuTrigger as-child>
      <slot />
    </ContextMenuTrigger>
    <ContextMenuPortal>
      <ContextMenuContent :class="menuContentClass">
        <!-- Start stays offered on a degraded/partial workspace: it brings
             up whatever is missing, in dependency order. -->
        <ContextMenuItem
          v-if="workspace.status !== 'running'"
          :class="menuItemClass"
          :disabled="locked"
          @select="
            store.runLifecycle(workspace.id, 'start workspace', {
              type: 'startWorkspace',
              id: workspace.id,
            })
          "
        >
          <Play class="h-3.5 w-3.5 text-slate-400" /> Start workspace
        </ContextMenuItem>
        <ContextMenuItem
          v-if="workspace.status !== 'stopped'"
          :class="menuItemClass"
          :disabled="locked"
          @select="
            store.runLifecycle(workspace.id, 'stop workspace', {
              type: 'stopWorkspace',
              id: workspace.id,
            })
          "
        >
          <CircleStop class="h-3.5 w-3.5 text-red-600 dark:text-red-400" /> Stop workspace
        </ContextMenuItem>
        <ContextMenuSeparator :class="menuSeparatorClass" />
        <ContextMenuItem :class="menuItemClass" @select="emit('edit')">
          <Pencil class="h-3.5 w-3.5 text-slate-400" /> Edit workspace…
        </ContextMenuItem>
      </ContextMenuContent>
    </ContextMenuPortal>
  </ContextMenuRoot>
</template>
