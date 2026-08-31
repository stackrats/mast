<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { FolderOpen, GitBranch, Sparkles } from "lucide-vue-next";

import { pickDirectory } from "../lib/transport";
import { createKey, useEngineStore } from "../stores/engine";
import Button from "./ui/Button.vue";
import Chip from "./ui/Chip.vue";
import Input from "./ui/Input.vue";
import Modal from "./ui/Modal.vue";
import Select from "./ui/Select.vue";
import Tooltip from "./ui/Tooltip.vue";

const open = defineModel<boolean>("open", { default: false });
/** A mast://clone link's repository URL — opening with one lands in From
 * Git mode, prefilled but untouched: the person still reviews and clicks. */
const { cloneUrl = null } = defineProps<{ cloneUrl?: string | null }>();
const store = useEngineStore();

// Two ways in, one dialog: scaffold something new, or clone what the team
// already has. Both end the same way — a project in the sidebar.
const mode = ref<"create" | "clone">("create");
const name = ref("");
const parent = ref("");
const php = ref("85");
const selected = ref<Set<string>>(new Set(["mysql", "redis", "mailpit"]));
const gitUrl = ref("");
/** True once the user edits the name by hand; the URL stops driving it. */
const namedManually = ref(false);

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

// The three Sail installs that rewrite the DB_* block. Without one of them the
// project keeps the skeleton's SQLite, which is a fine default but easy to
// pick by accident — so say so rather than leaving it to be discovered.
const RELATIONAL = ["mysql", "pgsql", "mariadb"];
const usesSqlite = computed(() => !RELATIONAL.some((db) => selected.value.has(db)));

const parentChoices = computed(() => store.watchedDirectories);
const nameValid = computed(() => /^[a-z0-9][a-z0-9-_]*$/.test(name.value));
const canSubmit = computed(
  () =>
    nameValid.value &&
    parent.value.trim() !== "" &&
    (mode.value === "create" || gitUrl.value.trim() !== ""),
);

/** The repo's own name, made safe for a project directory — what the name
 * field wants to be until the user says otherwise. */
function nameFromUrl(url: string): string {
  const tail = url
    .trim()
    .replace(/\.git\/?$/, "")
    .split(/[/:]/)
    .filter(Boolean)
    .pop();
  return (tail ?? "")
    .toLowerCase()
    .replace(/[^a-z0-9-_]/g, "-")
    .replace(/^[-_]+/, "");
}

watch(gitUrl, (url) => {
  if (!namedManually.value) name.value = nameFromUrl(url);
});

watch(open, (now) => {
  if (now && cloneUrl) {
    mode.value = "clone";
    namedManually.value = false;
    gitUrl.value = cloneUrl; // the name follows via the watch above
  }
});

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

/// Scaffolding and cloning both pull images or dependency trees, so they run
/// as background operations: the dialog closes immediately and progress
/// shows up in the logs panel, like every other long-running verb.
function submit() {
  const projectName = name.value.trim();
  if (mode.value === "create") {
    void store.runLifecycle(createKey(projectName), `create ${projectName}`, {
      type: "createProject",
      parent: parent.value.trim(),
      name: projectName,
      php: php.value,
      services: [...selected.value],
    });
  } else {
    void store.runLifecycle(createKey(projectName), `clone ${projectName}`, {
      type: "cloneProject",
      url: gitUrl.value.trim(),
      parent: parent.value.trim(),
      name: projectName,
    });
  }
  open.value = false;
  name.value = "";
  gitUrl.value = "";
  namedManually.value = false;
}
</script>

<template>
  <Modal v-model:open="open" title="Add a project" wide>
    <div class="space-y-3">
      <div class="flex gap-1.5">
        <Chip :active="mode === 'create'" @click="mode = 'create'">
          <Sparkles class="h-3 w-3" /> Create new
        </Chip>
        <Chip :active="mode === 'clone'" @click="mode = 'clone'">
          <GitBranch class="h-3 w-3" /> From Git
        </Chip>
      </div>

      <p v-if="mode === 'create'" class="text-xs text-slate-500 dark:text-slate-400">
        Runs the documented Sail install — composer create-project, then artisan sail:install — in
        the official composer container, and imports the result. No local PHP needed.
      </p>
      <p v-else class="text-xs text-slate-500 dark:text-slate-400">
        Clones the repository and bootstraps whatever a fresh clone is missing — containerized
        composer install, <span class="font-mono">.env</span> from
        <span class="font-mono">.env.example</span>, an app key — then imports it. Committed
        <span class="font-mono">mast.yml</span> commands arrive with it.
      </p>

      <label v-if="mode === 'clone'" class="block text-xs text-slate-600 dark:text-slate-300">
        Repository URL
        <Input v-model="gitUrl" placeholder="git@github.com:acme/shop.git" mono class="mt-1" />
      </label>

      <!-- Create mode pairs the name with a narrow PHP picker; clone mode
           has no second column, so the name goes full width like every
           other field — a half-width input beside nothing reads as broken. -->
      <div :class="mode === 'create' ? 'grid grid-cols-[1fr_7rem] gap-2' : ''">
        <label class="block text-xs text-slate-600 dark:text-slate-300">
          Project name
          <Input v-model="name" placeholder="my-app" class="mt-1" @input="namedManually = true" />
          <span v-if="name && !nameValid" class="text-red-600">
            lowercase letters, digits, - and _ only
          </span>
        </label>
        <label v-if="mode === 'create'" class="text-xs text-slate-600 dark:text-slate-300">
          PHP
          <Select
            v-model="php"
            class="mt-1"
            :options="PHP_VERSIONS.map((v) => ({ value: v, label: `${v[0]}.${v[1]}` }))"
          />
        </label>
      </div>

      <div class="text-xs text-slate-600 dark:text-slate-300">
        {{ mode === "create" ? "Create inside" : "Clone inside" }}
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

      <div v-if="mode === 'create'" class="text-xs text-slate-600 dark:text-slate-300">
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
        <p v-if="usesSqlite" class="mt-1.5 text-slate-500 dark:text-slate-400">
          No database service — the project keeps Laravel's SQLite file at
          <code>database/database.sqlite</code>, migrated and ready.
        </p>
      </div>

      <p class="text-xs text-slate-400">
        Runs in the background — follow it in the logs panel. The project appears in the sidebar
        once it is imported.
      </p>

      <div class="flex justify-end gap-2">
        <Button variant="outline" @click="open = false">Close</Button>
        <Button :disabled="!canSubmit || store.readOnly" @click="submit">
          {{ mode === "create" ? "Create project" : "Clone project" }}
        </Button>
      </div>
    </div>
  </Modal>
</template>
