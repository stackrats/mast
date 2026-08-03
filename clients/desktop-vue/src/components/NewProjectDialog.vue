<script setup lang="ts">
import { computed, ref } from "vue";
import { FolderOpen, Sparkles } from "lucide-vue-next";

import { pickDirectory } from "../lib/transport";
import { createKey, useEngineStore } from "../stores/engine";
import Button from "./ui/Button.vue";
import Chip from "./ui/Chip.vue";
import Input from "./ui/Input.vue";
import Modal from "./ui/Modal.vue";
import Select from "./ui/Select.vue";
import Tooltip from "./ui/Tooltip.vue";

const open = defineModel<boolean>("open", { default: false });
const store = useEngineStore();

const name = ref("");
const parent = ref("");
const php = ref("85");
const selected = ref<Set<string>>(new Set(["mysql", "redis", "mailpit"]));

// Sail's own service list (InteractsWithDockerComposeServices).
const SERVICES = [
  "mysql",
  "pgsql",
  "mariadb",
  "mongodb",
  "redis",
  "valkey",
  "memcached",
  "meilisearch",
  "typesense",
  "minio",
  "rustfs",
  "mailpit",
  "rabbitmq",
  "selenium",
  "soketi",
];
const PHP_VERSIONS = ["85", "84", "83", "82", "81", "80"];

const parentChoices = computed(() => store.watchedDirectories);
const nameValid = computed(() => /^[a-z0-9][a-z0-9-_]*$/.test(name.value));
const canCreate = computed(() => nameValid.value && parent.value.trim() !== "");

/** Fill the parent from a native picker; the watched-directory chips stay as shortcuts. */
async function browseParent() {
  const chosen = await pickDirectory("Choose where to create the project");
  if (chosen) parent.value = chosen;
}

function toggle(service: string) {
  const next = new Set(selected.value);
  if (next.has(service)) next.delete(service);
  else next.add(service);
  selected.value = next;
}

/// Scaffolding pulls images and a whole dependency tree, so it runs as a
/// background operation: the dialog closes immediately and progress shows up
/// in the logs panel, like every other long-running verb.
function create() {
  const projectName = name.value.trim();
  void store.runLifecycle(createKey(projectName), `create ${projectName}`, {
    type: "createProject",
    parent: parent.value.trim(),
    name: projectName,
    php: php.value,
    services: [...selected.value],
  });
  open.value = false;
  name.value = "";
}
</script>

<template>
  <Modal v-model:open="open" title="New Laravel project" wide>
    <div class="space-y-3">
      <p class="flex items-start gap-2 text-xs text-slate-500 dark:text-slate-400">
        <Sparkles class="mt-0.5 h-3.5 w-3.5 shrink-0 text-slate-400" />
        Runs the documented Sail install — composer create-project, then artisan sail:install — in
        the official composer container, and imports the result. No local PHP needed.
      </p>

      <div class="grid grid-cols-2 gap-2">
        <label class="text-xs text-slate-600 dark:text-slate-300">
          Project name
          <Input v-model="name" placeholder="my-app" class="mt-1" />
          <span v-if="name && !nameValid" class="text-red-600">
            lowercase letters, digits, - and _ only
          </span>
        </label>
        <label class="text-xs text-slate-600 dark:text-slate-300">
          PHP
          <Select
            v-model="php"
            class="mt-1"
            :options="PHP_VERSIONS.map((v) => ({ value: v, label: `${v[0]}.${v[1]}` }))"
          />
        </label>
      </div>

      <div class="text-xs text-slate-600 dark:text-slate-300">
        Create inside
        <div class="mt-1 flex gap-2">
          <Input v-model="parent" placeholder="/home/you/projects" />
          <Tooltip text="Choose a directory.">
            <Button variant="outline" size="iconLg" @click="browseParent">
              <FolderOpen class="h-3.5 w-3.5" />
            </Button>
          </Tooltip>
        </div>
        <span v-if="parentChoices.length" class="mt-1 flex flex-wrap gap-1">
          <Button
            v-for="directory in parentChoices"
            :key="directory"
            variant="outline"
            size="sm"
            @click="parent = directory"
          >
            {{ directory }}
          </Button>
        </span>
      </div>

      <div class="text-xs text-slate-600 dark:text-slate-300">
        Services
        <div class="mt-1 flex flex-wrap gap-1.5">
          <Chip
            v-for="service in SERVICES"
            :key="service"
            :active="selected.has(service)"
            @click="toggle(service)"
          >
            {{ service }}
          </Chip>
        </div>
      </div>

      <p class="text-xs text-slate-400">
        Runs in the background — follow it in the logs panel. The project appears in the sidebar
        once it is imported.
      </p>

      <div class="flex justify-end gap-2">
        <Button variant="outline" @click="open = false">Close</Button>
        <Button :disabled="!canCreate || store.readOnly" @click="create">Create project</Button>
      </div>
    </div>
  </Modal>
</template>
