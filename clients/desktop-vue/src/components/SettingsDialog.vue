<script setup lang="ts">
import { ref, watch } from "vue";
import { FolderOpen, Monitor, Moon, Sun, X } from "lucide-vue-next";
import { disable, enable, isEnabled } from "@tauri-apps/plugin-autostart";

import {
  applyTheme,
  loadNotificationPrefs,
  loadTheme,
  saveNotificationPrefs,
  saveTheme,
  type NotificationPrefs,
  type Theme,
} from "../lib/prefs";
import type { IntegrationSettings } from "../bindings";
import { pickDirectory } from "../lib/transport";
import { useEngineStore } from "../stores/engine";
import Button from "./ui/Button.vue";
import Checkbox from "./ui/Checkbox.vue";
import Input from "./ui/Input.vue";
import Modal from "./ui/Modal.vue";
import Tooltip from "./ui/Tooltip.vue";

const open = defineModel<boolean>("open", { default: false });
const store = useEngineStore();

const terminal = ref("");
const editor = ref("");
const autoPortRemap = ref(true);
const newDirectory = ref("");
const theme = ref<Theme>(loadTheme());
const notifications = ref<NotificationPrefs>(loadNotificationPrefs());
const autostart = ref(false);

watch(open, (isOpen) => {
  if (isOpen) {
    terminal.value = store.integrations.terminal ?? "";
    editor.value = store.integrations.editor ?? "";
    autoPortRemap.value = store.integrations.autoPortRemap;
    theme.value = loadTheme();
    notifications.value = loadNotificationPrefs();
    void isEnabled()
      .then((enabled) => (autostart.value = enabled))
      .catch(() => {});
  }
});

async function setAutostart(value: boolean) {
  try {
    if (value) await enable();
    else await disable();
    autostart.value = await isEnabled();
  } catch (e) {
    store.error = e instanceof Error ? e.message : String(e);
  }
}

function setNotification(key: keyof NotificationPrefs, value: boolean) {
  notifications.value = { ...notifications.value, [key]: value };
  saveNotificationPrefs(notifications.value);
}

function setTheme(next: Theme) {
  theme.value = next;
  saveTheme(next);
  applyTheme(next);
}

async function saveIntegrations(overrides: Partial<IntegrationSettings> = {}) {
  await store.run({
    type: "setIntegrations",
    integrations: {
      terminal: terminal.value.trim() || null,
      editor: editor.value.trim() || null,
      autoPortRemap: autoPortRemap.value,
      ...overrides,
    },
  });
}

/** Checkboxes save on click; the text inputs wait for the Save button. */
async function setAutoPortRemap(value: boolean) {
  autoPortRemap.value = value;
  await saveIntegrations({ autoPortRemap: value });
}

async function addDirectory() {
  const path = newDirectory.value.trim();
  if (!path) return;
  await store.run({ type: "addWatchedDirectory", path });
  if (!store.error) newDirectory.value = "";
}

/** Fill the input from a native picker; Watch still confirms it. */
async function browseDirectory() {
  const chosen = await pickDirectory("Choose a directory to watch");
  if (chosen) newDirectory.value = chosen;
}

const headingClass = "text-xs font-semibold text-slate-400 dark:text-slate-500";
</script>

<template>
  <Modal v-model:open="open" title="Settings" wide>
    <div class="space-y-5">
      <section class="space-y-2">
        <h4 :class="headingClass">Appearance</h4>
        <div class="flex gap-2">
          <Button
            v-for="option in [
              { value: 'system', label: 'System', icon: Monitor },
              { value: 'light', label: 'Light', icon: Sun },
              { value: 'dark', label: 'Dark', icon: Moon },
            ]"
            :key="option.value"
            :variant="theme === option.value ? 'default' : 'outline'"
            size="sm"
            @click="setTheme(option.value as Theme)"
          >
            <component :is="option.icon" class="h-3.5 w-3.5" />
            {{ option.label }}
          </Button>
        </div>
      </section>

      <section class="space-y-2">
        <h4 :class="headingClass">Watched directories</h4>
        <div class="flex gap-2">
          <Input v-model="newDirectory" placeholder="/home/you/code" @keyup.enter="addDirectory" />
          <Tooltip text="Choose a directory.">
            <Button variant="outline" size="iconLg" @click="browseDirectory">
              <FolderOpen class="h-3.5 w-3.5" />
            </Button>
          </Tooltip>
          <Button :disabled="store.busy > 0" @click="addDirectory">Watch</Button>
        </div>
        <ul class="space-y-1">
          <li
            v-for="directory in store.watchedDirectories"
            :key="directory"
            class="flex items-center justify-between rounded-md border border-slate-100 bg-slate-50 px-2 py-1 font-mono text-xs text-slate-600 dark:border-slate-800 dark:bg-neutral-800/60 dark:text-slate-300"
          >
            {{ directory }}
            <span class="flex items-center gap-1">
              <Tooltip text="Open this directory in your file manager.">
                <Button
                  variant="ghost"
                  size="iconSm"
                  @click="store.run({ type: 'revealPath', path: directory })"
                >
                  <FolderOpen class="h-3.5 w-3.5" />
                </Button>
              </Tooltip>
              <Button
                variant="ghost"
                size="iconSm"
                @click="store.run({ type: 'removeWatchedDirectory', path: directory })"
              >
                <X class="h-3.5 w-3.5" />
              </Button>
            </span>
          </li>
        </ul>
      </section>

      <section class="space-y-2">
        <h4 :class="headingClass">Startup</h4>
        <Checkbox
          :model-value="autostart"
          label="Launch Mast when you log in (starts minimized in the tray)"
          @update:model-value="setAutostart"
        />
      </section>

      <section class="space-y-2">
        <h4 :class="headingClass">Notifications</h4>
        <p class="text-xs text-slate-400">
          Native notifications fire only while the window is hidden in the tray.
        </p>
        <div class="flex flex-col gap-1.5">
          <Checkbox
            :model-value="notifications.health"
            label="Project health — a project turns unhealthy or recovers"
            @update:model-value="(v) => setNotification('health', v)"
          />
          <Checkbox
            :model-value="notifications.docker"
            label="Docker — connection lost or restored"
            @update:model-value="(v) => setNotification('docker', v)"
          />
          <Checkbox
            :model-value="notifications.operations"
            label="Operations — a start/stop/command fails"
            @update:model-value="(v) => setNotification('operations', v)"
          />
        </div>
      </section>

      <section class="space-y-2">
        <h4 :class="headingClass">Starting projects</h4>
        <Checkbox
          :model-value="autoPortRemap"
          label="Move a host port when something else already has it"
          @update:model-value="setAutoPortRemap"
        />
        <p class="text-xs text-slate-500 dark:text-slate-400">
          On start, Mast checks the ports the project publishes. If one is taken it writes a free
          port to the key that governs it (APP_PORT, VITE_PORT, FORWARD_*_PORT) and says so in the
          operation output. Your compose file is never touched.
        </p>
      </section>

      <section class="space-y-2">
        <h4 :class="headingClass">External tools</h4>
        <div class="flex items-center gap-2">
          <label class="w-20 text-xs text-slate-500">Terminal</label>
          <Input v-model="terminal" placeholder="auto-detect (ghostty, wezterm, kitty…)" />
        </div>
        <div class="flex items-center gap-2">
          <label class="w-20 text-xs text-slate-500">Editor</label>
          <Input v-model="editor" placeholder="auto-detect (code, zed, subl…)" />
        </div>
        <div class="flex justify-end">
          <Button :disabled="store.busy > 0" @click="saveIntegrations">Save tools</Button>
        </div>
      </section>
    </div>
  </Modal>
</template>
