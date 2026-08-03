<script setup lang="ts">
import { ref, watch } from "vue";
import { Check, Plus, RefreshCw, Trash2 } from "lucide-vue-next";

import type { CatalogEntry, CustomServiceSpec, FileEditPreview, ProjectId } from "../bindings";
import {
  catalog,
  catalogPreview,
  customServicePreview,
  serviceImagePreview,
  serviceRemovePreview,
} from "../lib/transport";
import { useEngineStore } from "../stores/engine";
import Badge from "./ui/Badge.vue";
import Button from "./ui/Button.vue";
import Hint from "./ui/Hint.vue";
import Input from "./ui/Input.vue";
import Modal from "./ui/Modal.vue";
import Select from "./ui/Select.vue";
import Tooltip from "./ui/Tooltip.vue";

const open = defineModel<boolean>("open", { default: false });
const { project } = defineProps<{ project: ProjectId }>();
const store = useEngineStore();

const entries = ref<CatalogEntry[]>([]);
const preview = ref<FileEditPreview | null>(null);
const previewFor = ref<
  | { kind: "catalog"; entry: CatalogEntry; remove: boolean }
  | { kind: "custom" }
  | { kind: "image"; service: string; image: string; title: string }
  | null
>(null);
const showDiff = ref(false);

// Custom service form (anything outside the catalog).
const customOpen = ref(false);
const customName = ref("");
const customImage = ref("");
const customPorts = ref("");
const customVolume = ref("");
const customCommand = ref("");

function customSpec(): CustomServiceSpec {
  return {
    name: customName.value.trim(),
    image: customImage.value.trim(),
    ports: customPorts.value
      .split(",")
      .map((p) => p.trim())
      .filter(Boolean),
    volume: customVolume.value.trim() || null,
    command: customCommand.value.trim() || null,
  };
}

async function previewCustom() {
  previewFor.value = { kind: "custom" };
  preview.value = null;
  showDiff.value = false;
  try {
    preview.value = await customServicePreview(project, customSpec());
  } catch (e) {
    store.error = e instanceof Error ? e.message : String(e);
    previewFor.value = null;
  }
}

async function refresh() {
  try {
    entries.value = await catalog(project);
  } catch {
    // Unresolved projects have no catalog yet.
    entries.value = [];
  }
}

/// The tag the service runs, so the picker opens on its real value. Falls back
/// to the newest offered tag when the image carries no tag at all.
function currentTag(entry: CatalogEntry): string {
  const image = entry.installedImage ?? "";
  const colon = image.lastIndexOf(":");
  const tag = colon > 0 ? image.slice(colon + 1) : "";
  // A colon can introduce a registry port rather than a tag.
  return tag && !tag.includes("/") ? tag : (entry.versions[0] ?? "");
}

async function openVersionPreview(entry: CatalogEntry, tag: string) {
  const service = entry.installedService;
  const image = entry.installedImage;
  if (!service || !image || tag === currentTag(entry)) return;
  const colon = image.lastIndexOf(":");
  const repo = colon > 0 && !image.slice(colon + 1).includes("/") ? image.slice(0, colon) : image;
  const next = `${repo}:${tag}`;
  previewFor.value = { kind: "image", service, image: next, title: entry.title };
  preview.value = null;
  showDiff.value = false;
  try {
    preview.value = await serviceImagePreview(project, service, next);
  } catch (e) {
    store.error = e instanceof Error ? e.message : String(e);
    previewFor.value = null;
  }
}

/// Pull and recreate just this container — a retag only changes the file.
function rebuild(entry: CatalogEntry) {
  const service = entry.installedService;
  if (!service) return;
  void store.runLifecycle(`${project}:rebuild:${service}`, `rebuild ${service}`, {
    type: "rebuildService",
    id: project,
    service,
  });
  open.value = false;
}

watch(open, (isOpen) => {
  if (!isOpen) return;
  previewFor.value = null;
  preview.value = null;
  customOpen.value = false;
  void refresh();
});

async function openPreview(entry: CatalogEntry, remove: boolean) {
  previewFor.value = { kind: "catalog", entry, remove };
  preview.value = null;
  showDiff.value = false;
  try {
    // Foreign-named installs go through the generic as-is removal; our own
    // keys keep the three-way check.
    preview.value =
      remove && !entry.removable && entry.installedService
        ? await serviceRemovePreview(project, entry.installedService)
        : await catalogPreview(project, entry.id, remove);
  } catch (e) {
    store.error = e instanceof Error ? e.message : String(e);
    previewFor.value = null;
  }
}

async function apply() {
  const target = previewFor.value;
  if (!target) return;
  if (target.kind === "custom") {
    await store.run({ type: "addCustomService", id: project, spec: customSpec() });
  } else if (target.kind === "image") {
    await store.run({
      type: "setServiceImage",
      id: project,
      service: target.service,
      image: target.image,
    });
  } else {
    const generic = target.remove && !target.entry.removable && target.entry.installedService;
    await store.run(
      generic
        ? { type: "removeService", id: project, service: target.entry.installedService! }
        : {
            type: target.remove ? "removeCatalogService" : "addCatalogService",
            id: project,
            service: target.entry.id,
          },
    );
  }
  if (!store.error) {
    previewFor.value = null;
    customOpen.value = false;
    await refresh();
  }
}
</script>

<template>
  <Modal v-model:open="open" title="Services" wide>
    <!-- Preview / confirm view -->
    <div v-if="previewFor" class="space-y-3">
      <p class="text-xs font-medium text-slate-700 dark:text-slate-200">
        {{
          previewFor.kind === "custom"
            ? `Add ${customName || "custom service"}`
            : previewFor.kind === "image"
              ? `${previewFor.title} → ${previewFor.image}`
              : `${previewFor.remove ? "Remove" : "Add"} ${previewFor.entry.title}`
        }}
      </p>
      <p v-if="!preview" class="text-xs text-slate-500">planning…</p>
      <template v-else>
        <ul class="space-y-1">
          <li
            v-for="line in preview.summary"
            :key="line"
            class="flex items-start gap-2 text-xs text-slate-600 dark:text-slate-300"
          >
            <Check class="mt-0.5 h-3.5 w-3.5 shrink-0 text-slate-400" />
            {{ line }}
          </li>
        </ul>

        <Button variant="outline" size="sm" @click="showDiff = !showDiff">
          {{ showDiff ? "Hide" : "Show" }} file changes ({{ preview.file }})
        </Button>
        <div v-if="showDiff" class="grid grid-cols-2 gap-2">
          <pre
            class="max-h-64 overflow-auto rounded-md border border-slate-200 bg-slate-50 p-2 font-mono text-[11px] leading-4 whitespace-pre dark:border-slate-800 dark:bg-slate-900"
            >{{ preview.before }}</pre>
          <pre
            class="max-h-64 overflow-auto rounded-md border border-emerald-200 bg-emerald-50/50 p-2 font-mono text-[11px] leading-4 whitespace-pre dark:border-emerald-900 dark:bg-emerald-950/30"
            >{{ preview.after }}</pre>
        </div>
        <p class="text-xs text-slate-400">
          Applied through the write transaction: validated by docker compose, backed up, and refused
          rather than corrupted.
        </p>

        <div class="flex justify-end gap-2">
          <Button variant="outline" @click="previewFor = null">Back</Button>
          <Button :disabled="store.busy > 0" @click="apply">
            {{
              previewFor.kind === "catalog" && previewFor.remove ? "Remove service" : "Add service"
            }}
          </Button>
        </div>
      </template>
    </div>

    <!-- Catalog list view -->
    <div v-else class="space-y-2">
      <p class="flex items-center gap-1.5 text-xs text-slate-500 dark:text-slate-400">
        Standard services for this project's compose file.
        <Hint
          text="Add, remove, or change the version of a service. Every change shows the exact compose-file edit before anything is written. Remove restores the file exactly — unless you edited the service since, in which case Mast refuses rather than destroy your changes. A version change needs a rebuild to reach the running container."
        />
      </p>
      <ul class="space-y-1">
        <li
          v-for="entry in entries"
          :key="entry.id"
          class="flex items-center justify-between gap-3 rounded border border-slate-100 px-2 py-1.5 dark:border-slate-800"
        >
          <div class="min-w-0">
            <!-- Fixed-height title row: the installed badge must not change
                 row height or push the subtext (uniform rows, no shift). -->
            <p
              class="flex h-5 items-center gap-1.5 text-xs font-medium text-slate-700 dark:text-slate-200"
            >
              {{ entry.title }}
              <Badge v-if="entry.installed" variant="success">installed</Badge>
            </p>
            <p class="truncate text-xs text-slate-400">
              {{
                entry.installed && !entry.removable
                  ? "Already provided by one of this project's own services."
                  : entry.roleCoveredBy
                    ? `This role is already covered by ${entry.roleCoveredBy} — adding both may conflict.`
                    : entry.description
              }}
            </p>
          </div>
          <!-- Version picker: only for an installed service on a repo we pin
               tags for, so it never offers a tag that cannot be pulled. -->
          <div
            v-if="entry.installed && entry.versions.length && entry.installedService"
            class="flex shrink-0 items-center gap-1.5"
          >
            <Tooltip :text="`Retag ${entry.installedService} — previewed, then rebuild to apply.`">
              <div class="w-28">
                <Select
                  :model-value="currentTag(entry)"
                  :options="entry.versions.map((v) => ({ value: v, label: v }))"
                  :disabled="store.readOnly"
                  @update:model-value="(tag: string) => openVersionPreview(entry, tag)"
                />
              </div>
            </Tooltip>
            <Tooltip
              :text="`Pull ${entry.installedImage} and recreate just this container. Needed for a version change to take effect.`"
            >
              <Button
                variant="outline"
                size="sm"
                :disabled="store.readOnly || store.busy > 0"
                @click="rebuild(entry)"
              >
                <RefreshCw class="h-3.5 w-3.5" /> Rebuild
              </Button>
            </Tooltip>
          </div>
          <Button
            v-if="!entry.installed"
            variant="outline"
            size="sm"
            class="shrink-0"
            :disabled="store.readOnly"
            @click="openPreview(entry, false)"
          >
            <Plus class="h-3.5 w-3.5" /> Add
          </Button>
          <Tooltip
            v-else-if="entry.removable || entry.installedService"
            :text="
              entry.removable
                ? 'Restores the compose file exactly as before the add.'
                : `Removes your service ${entry.installedService} as it stands.`
            "
          >
            <Button
              variant="outline"
              size="sm"
              class="shrink-0"
              :disabled="store.readOnly"
              @click="openPreview(entry, true)"
            >
              <Trash2 class="h-3.5 w-3.5" /> Remove
            </Button>
          </Tooltip>
        </li>
      </ul>
      <p v-if="entries.length === 0" class="text-xs text-slate-400">
        No catalog available yet — the project has to resolve first.
      </p>

      <div class="border-t border-slate-100 pt-2 dark:border-slate-800">
        <Button variant="outline" size="sm" @click="customOpen = !customOpen">
          <Plus v-if="!customOpen" class="h-3.5 w-3.5" />
          {{ customOpen ? "Hide custom service" : "Something else? Add a custom service…" }}
        </Button>
        <div v-if="customOpen" class="mt-2 space-y-2">
          <div class="grid grid-cols-2 gap-2">
            <label class="text-xs text-slate-600 dark:text-slate-300">
              Service name
              <Input v-model="customName" placeholder="typesense" mono class="mt-1" />
            </label>
            <label class="text-xs text-slate-600 dark:text-slate-300">
              Image
              <Input
                v-model="customImage"
                placeholder="ghcr.io/acme/tool:latest"
                mono
                class="mt-1"
              />
            </label>
          </div>
          <div class="grid grid-cols-2 gap-2">
            <label class="text-xs text-slate-600 dark:text-slate-300">
              Ports (host:container, comma-separated)
              <Input v-model="customPorts" placeholder="8081:80" mono class="mt-1" />
            </label>
            <label class="text-xs text-slate-600 dark:text-slate-300">
              Data path to persist (optional)
              <Input v-model="customVolume" placeholder="/data" mono class="mt-1" />
            </label>
          </div>
          <label class="block text-xs text-slate-600 dark:text-slate-300">
            Command (optional)
            <Input v-model="customCommand" placeholder="serve --port 80" mono class="mt-1" />
          </label>
          <div class="flex justify-end">
            <Button
              :disabled="!customName.trim() || !customImage.trim() || store.readOnly"
              @click="previewCustom"
            >
              Preview changes
            </Button>
          </div>
        </div>
      </div>
    </div>
  </Modal>
</template>
