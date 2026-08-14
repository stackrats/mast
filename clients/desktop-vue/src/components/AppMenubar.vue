<script setup lang="ts">
import {
  MenubarContent,
  MenubarItem,
  MenubarMenu,
  MenubarPortal,
  MenubarRoot,
  MenubarSeparator,
  MenubarTrigger,
} from "reka-ui";
import { getCurrentWindow } from "@tauri-apps/api/window";

import { menuContentClass, menuItemClass, menuSeparatorClass } from "../lib/menu";
import { useEngineStore } from "../stores/engine";
import Tooltip from "./ui/Tooltip.vue";

const emit = defineEmits<{
  openSettings: [];
  newWorkspace: [];
  openDiagnostics: [];
  newProject: [];
  openAbout: [];
}>();
const store = useEngineStore();

// The CLI's banner (clients/mast-cli/src/main.rs), so the desktop and the
// terminal introduce the app with the same wordmark. Six rows of block glyphs
// only read as letters when the rows touch exactly — hence `leading-none` and
// a line-height tied to the font size below.
const BANNER = [
  " ██╗██╗        ███╗   ███╗  █████╗  ███████╗ ████████╗",
  " ██║█████╗     ████╗ ████║ ██╔══██╗ ██╔════╝ ╚══██╔══╝",
  " ██║████████╗  ██╔████╔██║ ███████║ ███████╗    ██║",
  " ██║█████╔═╝   ██║╚██╔╝██║ ██╔══██║ ╚════██║    ██║",
  " ██║██╔═╝      ██║ ╚═╝ ██║ ██║  ██║ ███████║    ██║",
  " ╚═╝╚═╝        ╚═╝     ╚═╝ ╚═╝  ╚═╝ ╚══════╝    ╚═╝",
].join("\n");

const triggerClass =
  "rounded px-2.5 py-1 text-xs font-medium text-slate-600 outline-none hover:bg-slate-100 data-[state=open]:bg-slate-100 dark:text-slate-300 dark:hover:bg-slate-800 dark:data-[state=open]:bg-slate-800";
const contentClass = `${menuContentClass} min-w-44`;
const itemClass = menuItemClass;

async function closeToTray() {
  await getCurrentWindow().close();
}
</script>

<template>
  <MenubarRoot
    class="flex items-center gap-0.5 border-b border-slate-200 bg-white px-2 py-1 dark:border-slate-800 dark:bg-slate-950"
  >
    <!-- v-text, not interpolation: inside `white-space: pre` the template's own
         indentation would become part of the art. -->
    <!-- role="img" + aria-label so it is announced as "Mast" rather than read
         out as box-drawing characters. -->
    <pre
      class="mr-2 px-1 font-mono text-[3px] leading-none text-slate-900 select-none dark:text-slate-100"
      role="img"
      aria-label="Mast"
      v-text="BANNER"
    />

    <MenubarMenu>
      <MenubarTrigger :class="triggerClass">App</MenubarTrigger>
      <MenubarPortal>
        <MenubarContent :class="contentClass" :side-offset="4" align="start">
          <MenubarItem :class="itemClass" @select="emit('newProject')">
            New Laravel project…
          </MenubarItem>
          <MenubarItem :class="itemClass" @select="emit('openSettings')">Settings…</MenubarItem>
          <MenubarItem :class="itemClass" @select="emit('openDiagnostics')">
            Diagnostics…
          </MenubarItem>
          <MenubarSeparator :class="menuSeparatorClass" />
          <MenubarItem :class="itemClass" @select="emit('openAbout')">About Mast</MenubarItem>
          <MenubarItem :class="itemClass" @select="closeToTray">Close to tray</MenubarItem>
        </MenubarContent>
      </MenubarPortal>
    </MenubarMenu>

    <MenubarMenu>
      <MenubarTrigger :class="triggerClass">Workspace</MenubarTrigger>
      <MenubarPortal>
        <MenubarContent :class="contentClass" :side-offset="4" align="start">
          <MenubarItem :class="itemClass" @select="emit('newWorkspace')">
            New workspace…
          </MenubarItem>
        </MenubarContent>
      </MenubarPortal>
    </MenubarMenu>

    <MenubarMenu>
      <MenubarTrigger :class="triggerClass">View</MenubarTrigger>
      <MenubarPortal>
        <MenubarContent :class="contentClass" :side-offset="4" align="start">
          <MenubarItem :class="itemClass" @select="store.run({ type: 'refreshNow' })">
            Refresh now
          </MenubarItem>
        </MenubarContent>
      </MenubarPortal>
    </MenubarMenu>

    <div class="ml-auto flex items-center gap-2 pr-1 text-[11px] text-slate-400">
      <Tooltip v-if="store.docker" :text="store.docker.endpoint ?? 'resolving endpoint…'">
        <span class="flex items-center gap-1.5">
          <span
            class="h-2 w-2 rounded-full"
            :class="store.docker.available ? 'bg-emerald-500' : 'bg-red-500'"
          />
          {{ store.docker.available ? `docker · ${store.docker.contextName}` : "docker offline" }}
        </span>
      </Tooltip>
      <span v-if="store.readOnly" class="text-amber-600 dark:text-amber-400">read-only</span>
    </div>
  </MenubarRoot>
</template>
