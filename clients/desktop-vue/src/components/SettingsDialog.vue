<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { Check, FolderOpen, Monitor, Moon, Sun, X } from "lucide-vue-next";
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
import { SCALE_MAX, SCALE_MIN } from "../lib/prefs";
import { SCALE_SLIDER_STEP, defaultScale, scaleLabel, snapScale } from "../lib/scale";
import { comboLabel } from "../lib/shortcuts";
import { pickDirectory, pickFile } from "../lib/transport";
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
const browser = ref("");
const autoPortRemap = ref(true);
const newDirectory = ref("");
const theme = ref<Theme>(loadTheme());
const notifications = ref<NotificationPrefs>(loadNotificationPrefs());
const autostart = ref(false);

watch(open, (isOpen) => {
  if (isOpen) {
    terminal.value = store.integrations.terminal ?? "";
    editor.value = store.integrations.editor ?? "";
    browser.value = store.integrations.browser ?? "";
    autoPortRemap.value = store.integrations.autoPortRemap ?? true;
    toolsSaved.value = false;
    clearTimeout(savedTimer);
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

/** The tool values the Save button would write, for the dirty check below. */
const pendingTools = computed(() => ({
  terminal: terminal.value.trim() || null,
  editor: editor.value.trim() || null,
  browser: browser.value.trim() || null,
}));

/** Saving is a no-op until an input differs from what the engine holds. */
const toolsDirty = computed(
  () =>
    pendingTools.value.terminal !== (store.integrations.terminal ?? null) ||
    pendingTools.value.editor !== (store.integrations.editor ?? null) ||
    pendingTools.value.browser !== (store.integrations.browser ?? null),
);

async function saveIntegrations(overrides: Partial<IntegrationSettings> = {}) {
  await store.run({
    type: "setIntegrations",
    integrations: {
      ...pendingTools.value,
      autoPortRemap: autoPortRemap.value,
      ...overrides,
    },
  });
}

const toolsSaved = ref(false);
let savedTimer: ReturnType<typeof setTimeout> | undefined;

// Editing again during the "Saved" flash puts the label back to work mode.
watch(pendingTools, () => (toolsSaved.value = false));

async function saveTools() {
  await saveIntegrations();
  if (store.error) return;
  toolsSaved.value = true;
  clearTimeout(savedTimer);
  savedTimer = setTimeout(() => (toolsSaved.value = false), 2000);
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

/** Fill a tool input from a native file picker; Save tools still confirms. */
const toolInputs = { terminal, editor, browser };
async function browseTool(tool: keyof typeof toolInputs) {
  const chosen = await pickFile(`Choose a ${tool}`);
  if (chosen) toolInputs[tool].value = chosen;
}

const toolRows = [
  { key: "terminal", label: "Terminal", hint: "auto-detect (ghostty, wezterm, kitty…)" },
  { key: "editor", label: "Editor", hint: "auto-detect (code, zed, subl…)" },
  { key: "browser", label: "Browser", hint: "system default (vivaldi, firefox…)" },
] as const;

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
        <h4 :class="headingClass">Interface size</h4>
        <div class="flex items-center gap-3">
          <input
            :value="store.uiScale"
            type="range"
            :min="SCALE_MIN"
            :max="SCALE_MAX"
            :step="SCALE_SLIDER_STEP"
            aria-label="Interface size"
            class="h-1.5 min-w-0 flex-1 cursor-pointer appearance-none rounded-full bg-slate-200 accent-slate-900 dark:bg-slate-700 dark:accent-slate-100"
            @input="store.setUiScale(snapScale(Number(($event.target as HTMLInputElement).value)))"
          />
          <span
            class="w-12 shrink-0 text-right text-xs tabular-nums text-slate-600 dark:text-slate-300"
          >
            {{ scaleLabel(store.uiScale) }}
          </span>
        </div>
        <!-- Which of the two facts is true about the current number matters:
             "90% because you chose it" and "90% because this compositor tiles"
             look identical on screen and are not the same thing at all. Naming
             the compositor makes a wrong guess a legible wrong guess rather
             than a mystery shrinking of the app. -->
        <p class="text-xs text-slate-500 dark:text-slate-400">
          <template v-if="store.uiScaleIsDefault && store.sessionTiling">
            Defaulting to {{ scaleLabel(store.uiScale) }} because
            {{ store.sessionDesktop ?? "this session" }} tiles windows, so Mast rarely gets the size
            it asks for. Pick any size above and it sticks.
          </template>
          <template v-else-if="store.uiScaleIsDefault">
            Using the standard {{ scaleLabel(store.uiScale) }}. Also
            {{ comboLabel(["mod", "+"]) }} and {{ comboLabel(["mod", "−"]) }} from anywhere.
          </template>
          <template v-else>
            Set to {{ scaleLabel(store.uiScale) }}.
            <button
              type="button"
              class="underline underline-offset-2 hover:text-slate-700 dark:hover:text-slate-200"
              @click="store.resetUiScale()"
            >
              Use the default for this session
            </button>
            ({{ scaleLabel(defaultScale(store.sessionTiling)) }}).
          </template>
        </p>
      </section>

      <section class="space-y-2">
        <h4 :class="headingClass">Watched directories</h4>
        <p class="text-xs text-slate-500 dark:text-slate-400">
          Removing a directory also removes its projects from Mast unless another watched directory
          covers them. Project files stay on disk.
        </p>
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
                :aria-label="`Remove directory ${directory}`"
                :disabled="store.readOnly || store.busy > 0"
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
          row
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
            row
            :model-value="notifications.health"
            label="Project health — a project turns unhealthy or recovers"
            @update:model-value="(v) => setNotification('health', v)"
          />
          <Checkbox
            row
            :model-value="notifications.docker"
            label="Docker — connection lost or restored"
            @update:model-value="(v) => setNotification('docker', v)"
          />
          <Checkbox
            row
            :model-value="notifications.operations"
            label="Operations — a start/stop/command fails"
            @update:model-value="(v) => setNotification('operations', v)"
          />
        </div>
      </section>

      <section class="space-y-2">
        <h4 :class="headingClass">Starting projects</h4>
        <Checkbox
          row
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
        <p class="text-xs text-slate-500 dark:text-slate-400">
          A command name, or a full path picked from disk.
        </p>
        <div v-for="row in toolRows" :key="row.key" class="flex items-center gap-2">
          <label class="w-20 text-xs text-slate-500">{{ row.label }}</label>
          <Input v-model="toolInputs[row.key].value" :placeholder="row.hint" />
          <Tooltip :text="`Choose the ${row.key} on disk.`">
            <Button variant="outline" size="iconLg" @click="browseTool(row.key)">
              <FolderOpen class="h-3.5 w-3.5" />
            </Button>
          </Tooltip>
        </div>
        <div class="flex justify-end">
          <Button :disabled="store.busy > 0 || !toolsDirty" @click="saveTools">
            <Check v-if="toolsSaved" class="h-3.5 w-3.5" />
            {{ toolsSaved ? "Saved" : "Save tools" }}
          </Button>
        </div>
      </section>
    </div>
  </Modal>
</template>
