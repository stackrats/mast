<script setup lang="ts">
// Hand-rolled menubar, deliberately NOT reka-ui's Menubar: on the packaged
// macOS build (WKWebView) the reka triggers never opened their menus while
// every other platform worked, and a menu of five static items does not need
// a primitive that can fail per-webview. Click toggles, hover switches while
// one is open, Escape or a pointerdown anywhere else closes — no portal, no
// focus dance. Styling mirrors lib/menu so it still reads as the one family.
import { onBeforeUnmount, onMounted, ref } from "vue";
import { getCurrentWindow } from "@tauri-apps/api/window";

import { useEngineStore } from "../stores/engine";
import Tooltip from "./ui/Tooltip.vue";

const emit = defineEmits<{
  openPalette: [];
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

type MenuId = "app" | "workspace" | "view";

const openMenu = ref<MenuId | null>(null);
const rootEl = ref<HTMLElement | null>(null);

function toggle(menu: MenuId) {
  openMenu.value = openMenu.value === menu ? null : menu;
}

// Desktop-menubar convention: once one menu is open, sliding along the bar
// opens siblings without another click.
function slideTo(menu: MenuId) {
  if (openMenu.value !== null && openMenu.value !== menu) openMenu.value = menu;
}

function select(action: () => void) {
  openMenu.value = null;
  action();
}

function onDocumentPointerdown(event: PointerEvent) {
  if (openMenu.value === null) return;
  if (rootEl.value && !rootEl.value.contains(event.target as Node)) openMenu.value = null;
}

function onDocumentKeydown(event: KeyboardEvent) {
  if (event.key === "Escape") openMenu.value = null;
}

onMounted(() => {
  document.addEventListener("pointerdown", onDocumentPointerdown);
  document.addEventListener("keydown", onDocumentKeydown);
});
onBeforeUnmount(() => {
  document.removeEventListener("pointerdown", onDocumentPointerdown);
  document.removeEventListener("keydown", onDocumentKeydown);
});

const triggerClass =
  "rounded px-2.5 py-1 text-xs font-medium text-slate-600 outline-none hover:bg-slate-100 dark:text-slate-300 dark:hover:bg-slate-800";
const triggerOpenClass = "bg-slate-100 dark:bg-slate-800";
// lib/menu's content/item recipes, with hover: variants standing in for the
// data-highlighted state reka would have managed.
const panelClass =
  "absolute top-full left-0 z-50 mt-1 min-w-44 rounded-md border border-slate-200 bg-white p-1 shadow-lg dark:border-slate-700 dark:bg-slate-900";
const itemClass =
  "flex w-full cursor-default items-center gap-2 rounded px-2 py-1.5 text-left text-xs text-slate-700 outline-none hover:bg-slate-100 dark:text-slate-200 dark:hover:bg-slate-800";
const separatorClass = "my-1 h-px bg-slate-100 dark:bg-slate-800";
const accelClass = "ml-auto pl-4 font-mono text-[0.68rem] text-slate-400 dark:text-slate-500";
// Mac writes the modifier as a symbol and everyone else spells it. Read off
// the platform once — a menu that says Ctrl on a Mac teaches the wrong key.
const isMac = /mac/i.test(globalThis.navigator?.platform ?? globalThis.navigator?.userAgent ?? "");
const paletteAccel = isMac ? "⌘K" : "Ctrl K";

const MENUS: { id: MenuId; title: string }[] = [
  { id: "app", title: "App" },
  { id: "workspace", title: "Workspace" },
  { id: "view", title: "View" },
];

async function closeToTray() {
  await getCurrentWindow().close();
}
</script>

<template>
  <div
    ref="rootEl"
    role="menubar"
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

    <div v-for="menu in MENUS" :key="menu.id" class="relative">
      <button
        type="button"
        role="menuitem"
        aria-haspopup="menu"
        :aria-expanded="openMenu === menu.id"
        :class="[triggerClass, openMenu === menu.id && triggerOpenClass]"
        @click="toggle(menu.id)"
        @pointerenter="slideTo(menu.id)"
      >
        {{ menu.title }}
      </button>

      <div v-if="openMenu === menu.id" role="menu" :class="panelClass">
        <template v-if="menu.id === 'app'">
          <button
            type="button"
            role="menuitem"
            :class="itemClass"
            @click="select(() => emit('openPalette'))"
          >
            Commands…
            <span :class="accelClass">{{ paletteAccel }}</span>
          </button>
          <div :class="separatorClass" />
          <button
            type="button"
            role="menuitem"
            :class="itemClass"
            @click="select(() => emit('newProject'))"
          >
            New Laravel project…
          </button>
          <button
            type="button"
            role="menuitem"
            :class="itemClass"
            @click="select(() => emit('openSettings'))"
          >
            Settings…
          </button>
          <button
            type="button"
            role="menuitem"
            :class="itemClass"
            @click="select(() => emit('openDiagnostics'))"
          >
            Diagnostics…
          </button>
          <div :class="separatorClass" />
          <button
            type="button"
            role="menuitem"
            :class="itemClass"
            @click="select(() => emit('openAbout'))"
          >
            About Mast
          </button>
          <button type="button" role="menuitem" :class="itemClass" @click="select(closeToTray)">
            Close to tray
          </button>
        </template>
        <template v-else-if="menu.id === 'workspace'">
          <button
            type="button"
            role="menuitem"
            :class="itemClass"
            @click="select(() => emit('newWorkspace'))"
          >
            New workspace…
          </button>
        </template>
        <template v-else>
          <button
            type="button"
            role="menuitem"
            :class="itemClass"
            @click="select(() => store.run({ type: 'refreshNow' }))"
          >
            Refresh now
          </button>
        </template>
      </div>
    </div>

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
  </div>
</template>
