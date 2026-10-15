<script setup lang="ts">
// One project's agent activity (ADR-0012): the sessions working in it and
// what each runs, what was left running after a session ended, process
// trees in its app container Mast did not start, and its git worktrees with
// their own containers. The Agents page shows one of these per project; the
// project's card shows its own, without the container list it already has.
import { computed, onScopeDispose, ref } from "vue";
import {
  CircleStop,
  Container,
  FolderOpen,
  FolderX,
  GitBranch,
  Loader2,
  Trash2,
} from "lucide-vue-next";

import type { ContainerRun, HostRun, ProjectSummary, Worktree } from "../bindings";
import {
  foreignContainerRuns,
  hostOwnerLabel,
  hostOwnerSentence,
  hostRunFacts,
  isLeftover,
  madeAgo,
  placeLabel,
  runsInWorktree,
  runsOf,
  sessionsIn,
  stackSummary,
  toolHint,
  unattachedRuns,
  upFor,
  worktreeFacts,
} from "../lib/agents";
import { ownerLabel, runFacts } from "../lib/containerRuns";
import { containerRunKey, hostRunKey, useEngineStore, worktreeKey } from "../stores/engine";
import RemoveWorktreeDialog from "./RemoveWorktreeDialog.vue";
import Badge from "./ui/Badge.vue";
import Button from "./ui/Button.vue";
import Hint from "./ui/Hint.vue";
import Tooltip from "./ui/Tooltip.vue";

const { project, containerRuns = true } = defineProps<{
  project: ProjectSummary;
  /** List the app container's foreign process trees — off on the card,
   * which shows them in its own "In the app container" row. */
  containerRuns?: boolean;
}>();
const store = useEngineStore();

// Ages read in minutes and hours; a coarse clock is enough.
const now = ref(Date.now());
const timer = setInterval(() => (now.value = Date.now()), 30_000);
onScopeDispose(() => clearInterval(timer));

const sessions = computed(() => project.agentSessions ?? []);
const unattached = computed(() => unattachedRuns(project));
const foreign = computed(() => (containerRuns ? foreignContainerRuns(project) : []));
const worktrees = computed(() => project.worktrees ?? []);
const missing = computed(() => worktrees.value.filter((w) => w.missing && !w.locked).length);
const pruneKey = computed(() => worktreeKey(project.id, "prune"));

const removing = ref<Worktree | null>(null);
const removeOpen = computed({
  get: () => removing.value !== null,
  set: (open: boolean) => {
    if (!open) removing.value = null;
  },
});

const sectionLabel = "text-[11px] font-medium tracking-wide text-slate-400";
const rowClass =
  "flex items-start justify-between gap-3 rounded-md px-2 py-1.5 hover:bg-slate-50 dark:hover:bg-slate-800/40";

function hostBusy(run: HostRun): boolean {
  return !!store.agentOps[hostRunKey(project.id, run.pid, run.startTicks)];
}

function stopHost(run: HostRun) {
  void store.agentClick(hostRunKey(project.id, run.pid, run.startTicks), {
    type: "stopHostRun",
    id: project.id,
    pid: run.pid,
    startTicks: run.startTicks,
  });
}

function containerBusy(run: ContainerRun): boolean {
  const key = containerRunKey(project.id, run.pid, run.startTicks);
  // The card's Stop tracks it in one map, Clean up in the other.
  return !!store.stoppingRuns[key] || !!store.agentOps[key];
}

function worktreeBusy(worktree: Worktree): boolean {
  return !!store.agentOps[worktreeKey(project.id, worktree.path)];
}

function worktreeAction(
  worktree: Worktree,
  type: "stopWorktreeContainers" | "removeWorktreeContainers",
) {
  void store.agentClick(worktreeKey(project.id, worktree.path), {
    type,
    id: project.id,
    path: worktree.path,
  });
}

function forget(worktree: Worktree) {
  void store.agentClick(worktreeKey(project.id, worktree.path), {
    type: "removeWorktree",
    id: project.id,
    path: worktree.path,
    force: false,
  });
}

function prune() {
  void store.agentClick(pruneKey.value, { type: "pruneWorktrees", id: project.id });
}
</script>

<template>
  <div>
    <!-- Sessions, each with what it has running. -->
    <div v-if="sessions.length" class="mt-3 first:mt-0">
      <p :class="sectionLabel">Working now</p>
      <div v-for="session in sessions" :key="`${session.pid}:${session.startTicks}`" class="mt-1">
        <div :class="rowClass">
          <div class="flex min-w-0 items-center gap-2">
            <span class="h-2 w-2 shrink-0 animate-pulse rounded-full bg-emerald-500" />
            <span class="text-sm font-medium text-slate-800 dark:text-slate-200">
              {{ session.agent }}
            </span>
            <span class="truncate text-xs text-slate-500 dark:text-slate-400">
              in {{ placeLabel(project, session.worktree) }} · up
              {{ upFor(session.startedAtUnix, now) }} · pid {{ session.pid }}
            </span>
          </div>
        </div>
        <div
          v-for="run in runsOf(project, session)"
          :key="`${run.pid}:${run.startTicks}`"
          class="ml-4 border-l border-slate-200 pl-2 dark:border-slate-800"
        >
          <div :class="rowClass">
            <div class="min-w-0">
              <p class="flex flex-wrap items-center gap-1.5 text-xs">
                <span class="font-medium text-slate-700 dark:text-slate-200">{{ run.title }}</span>
                <Badge v-for="kind in run.kinds" :key="kind" variant="outline" class="normal-case">
                  {{ kind }}
                </Badge>
                <span v-if="run.worktree !== session.worktree" class="text-slate-400">
                  in {{ placeLabel(project, run.worktree) }}
                </span>
              </p>
              <p class="mt-0.5 text-[11px] text-slate-400">{{ hostRunFacts(run, now) }}</p>
              <p
                class="mt-0.5 truncate font-mono text-[11px] text-slate-400"
                :title="`${run.commandLine}\n${run.cwd}`"
              >
                {{ run.commandLine }}
              </p>
            </div>
            <Tooltip :text="hostOwnerSentence(run)">
              <Button
                variant="outline"
                size="sm"
                class="shrink-0"
                :disabled="store.readOnly || hostBusy(run)"
                @click="stopHost(run)"
              >
                <Loader2 v-if="hostBusy(run)" class="h-3.5 w-3.5 animate-spin" />
                <CircleStop v-else class="h-3.5 w-3.5" />
                Stop
              </Button>
            </Tooltip>
          </div>
        </div>
      </div>
    </div>

    <!-- On this machine, with no listed session above it. -->
    <div v-if="unattached.length" class="mt-3 first:mt-0">
      <div class="flex items-center gap-1.5">
        <p :class="sectionLabel">Running on this machine</p>
        <Hint
          text="Dev servers, workers, sail and docker clients, and headless browsers working in this project's checkout or worktrees. Orphaned means whatever started it has exited — usually an agent session that finished — and it keeps running, holding memory and ports, until it is stopped."
        />
      </div>
      <div
        v-for="run in unattached"
        :key="`${run.pid}:${run.startTicks}`"
        :class="[rowClass, 'mt-1']"
      >
        <div class="flex min-w-0 items-start gap-2">
          <span
            class="mt-1.5 h-2 w-2 shrink-0 rounded-full"
            :class="isLeftover(run) ? 'bg-amber-500' : 'bg-sky-500'"
          />
          <div class="min-w-0">
            <p class="flex flex-wrap items-center gap-1.5 text-xs">
              <span class="font-medium text-slate-700 dark:text-slate-200">{{ run.title }}</span>
              <Badge v-for="kind in run.kinds" :key="kind" variant="outline" class="normal-case">
                {{ kind }}
              </Badge>
              <span class="text-slate-400">
                {{ hostOwnerLabel(run.owner) }} · in {{ placeLabel(project, run.worktree) }}
              </span>
              <Badge v-if="run.cwdDeleted" variant="warning" class="normal-case">
                directory deleted
              </Badge>
            </p>
            <p class="mt-0.5 text-[11px] text-slate-400">{{ hostRunFacts(run, now) }}</p>
            <p
              class="mt-0.5 truncate font-mono text-[11px] text-slate-400"
              :title="`${run.commandLine}\n${run.cwd}`"
            >
              {{ run.commandLine }}
            </p>
          </div>
        </div>
        <Tooltip :text="hostOwnerSentence(run)">
          <Button
            variant="outline"
            size="sm"
            class="shrink-0"
            :disabled="store.readOnly || hostBusy(run)"
            @click="stopHost(run)"
          >
            <Loader2 v-if="hostBusy(run)" class="h-3.5 w-3.5 animate-spin" />
            <CircleStop v-else class="h-3.5 w-3.5" />
            Stop
          </Button>
        </Tooltip>
      </div>
    </div>

    <!-- In the app container, not started by Mast (ADR-0011). -->
    <div v-if="foreign.length" class="mt-3 first:mt-0">
      <p :class="sectionLabel">In the app container, not started by Mast</p>
      <div v-for="run in foreign" :key="`${run.pid}:${run.startTicks}`" :class="[rowClass, 'mt-1']">
        <div class="flex min-w-0 items-start gap-2">
          <span
            class="mt-1.5 h-2 w-2 shrink-0 rounded-full"
            :class="isLeftover(run) ? 'bg-amber-500' : 'bg-sky-500'"
          />
          <div class="min-w-0">
            <p class="flex flex-wrap items-center gap-1.5 text-xs">
              <span class="font-medium text-slate-700 dark:text-slate-200">{{ run.title }}</span>
              <Badge v-for="kind in run.kinds" :key="kind" variant="outline" class="normal-case">
                {{ kind }}
              </Badge>
              <span class="text-slate-400">{{ ownerLabel(run.owner) }}</span>
            </p>
            <p class="mt-0.5 text-[11px] text-slate-400">{{ runFacts(run, now) }}</p>
          </div>
        </div>
        <Button
          variant="outline"
          size="sm"
          class="shrink-0"
          :disabled="store.readOnly || containerBusy(run)"
          @click="store.stopContainerRun(project.id, run.pid, run.startTicks)"
        >
          <Loader2 v-if="containerBusy(run)" class="h-3.5 w-3.5 animate-spin" />
          <CircleStop v-else class="h-3.5 w-3.5" />
          Stop
        </Button>
      </div>
    </div>

    <!-- Worktrees. -->
    <div v-if="worktrees.length" class="mt-3 first:mt-0">
      <div class="flex items-center gap-1.5">
        <p :class="sectionLabel">Worktrees</p>
        <Hint
          text="Git worktrees of this project's repository — where agents work on several things at once. Each can start its own containers, which usually want the same ports as the project's. Removing one stops what works in it and removes its containers first; its branch is kept."
        />
        <Button
          v-if="missing > 1"
          variant="ghost"
          size="sm"
          class="ml-auto"
          :disabled="store.readOnly || !!store.agentOps[pruneKey]"
          @click="prune"
        >
          <Loader2 v-if="store.agentOps[pruneKey]" class="h-3.5 w-3.5 animate-spin" />
          <FolderX v-else class="h-3.5 w-3.5" />
          Forget all {{ missing }} missing
        </Button>
      </div>
      <div
        v-for="worktree in worktrees"
        :key="worktree.path"
        class="mt-1 rounded-md border border-slate-200 p-2 dark:border-slate-800"
        :class="worktree.missing ? 'border-dashed' : ''"
      >
        <div class="flex items-start justify-between gap-3">
          <div class="min-w-0">
            <p class="flex flex-wrap items-center gap-1.5 text-xs">
              <span class="font-mono font-medium text-slate-800 dark:text-slate-200">
                {{ worktree.name }}
              </span>
              <span
                v-if="worktree.branch"
                class="inline-flex items-center gap-1 text-slate-500 dark:text-slate-400"
              >
                <GitBranch class="h-3 w-3" /> {{ worktree.branch }}
              </span>
              <span v-else-if="worktree.head" class="text-slate-500 dark:text-slate-400">
                detached at {{ worktree.head }}
              </span>
              <Badge
                v-for="fact in worktreeFacts(worktree)"
                :key="fact.text"
                :variant="fact.tone === 'warn' ? 'warning' : 'secondary'"
                class="normal-case"
              >
                {{ fact.text }}
              </Badge>
              <span
                v-if="sessionsIn(project, worktree.path).length"
                class="inline-flex items-center gap-1 text-emerald-600 dark:text-emerald-400"
              >
                <span class="h-1.5 w-1.5 animate-pulse rounded-full bg-emerald-500" />
                {{
                  sessionsIn(project, worktree.path)
                    .map((s) => s.agent)
                    .join(", ")
                }}
                working
              </span>
            </p>
            <p class="mt-0.5 truncate font-mono text-[11px] text-slate-400" :title="worktree.path">
              {{ worktree.path }}
            </p>
            <p class="mt-0.5 text-[11px] text-slate-400">
              <template v-if="toolHint(worktree)">{{ toolHint(worktree) }} · </template>
              <template v-if="worktree.createdAtUnix">
                made {{ madeAgo(worktree.createdAtUnix, now) }} ago
              </template>
              <template v-if="runsInWorktree(project, worktree.path).length">
                · {{ runsInWorktree(project, worktree.path).length }} running on this machine
              </template>
            </p>
          </div>
          <div class="flex shrink-0 items-center gap-1">
            <Tooltip v-if="!worktree.missing" text="Show in the file manager">
              <Button
                variant="ghost"
                size="icon"
                @click="store.run({ type: 'revealPath', path: worktree.path })"
              >
                <FolderOpen class="h-3.5 w-3.5" />
              </Button>
            </Tooltip>
            <Button
              v-if="worktree.missing"
              variant="outline"
              size="sm"
              :disabled="store.readOnly || worktree.locked || worktreeBusy(worktree)"
              @click="forget(worktree)"
            >
              <Loader2 v-if="worktreeBusy(worktree)" class="h-3.5 w-3.5 animate-spin" />
              <FolderX v-else class="h-3.5 w-3.5" />
              Forget
            </Button>
            <Button
              v-else
              variant="outline"
              size="sm"
              :disabled="store.readOnly || worktree.locked || worktreeBusy(worktree)"
              @click="removing = worktree"
            >
              <Loader2 v-if="worktreeBusy(worktree)" class="h-3.5 w-3.5 animate-spin" />
              <Trash2 v-else class="h-3.5 w-3.5" />
              Remove…
            </Button>
          </div>
        </div>
        <div
          v-if="stackSummary(worktree)"
          class="mt-2 flex items-center justify-between gap-3 rounded bg-slate-50 px-2 py-1 dark:bg-slate-800/50"
        >
          <p
            class="flex min-w-0 items-center gap-1.5 text-[11px] text-slate-600 dark:text-slate-300"
          >
            <Container class="h-3.5 w-3.5 shrink-0 text-slate-400" />
            <span class="truncate">Its own containers — {{ stackSummary(worktree) }}</span>
          </p>
          <div class="flex shrink-0 gap-1">
            <Button
              v-if="worktree.containers.some((c) => c.running)"
              variant="ghost"
              size="sm"
              :disabled="store.readOnly || worktreeBusy(worktree)"
              @click="worktreeAction(worktree, 'stopWorktreeContainers')"
            >
              <CircleStop class="h-3.5 w-3.5" /> Stop
            </Button>
            <Button
              variant="ghost"
              size="sm"
              :disabled="store.readOnly || worktreeBusy(worktree)"
              @click="worktreeAction(worktree, 'removeWorktreeContainers')"
            >
              <Trash2 class="h-3.5 w-3.5" /> Remove
            </Button>
          </div>
        </div>
      </div>
    </div>

    <RemoveWorktreeDialog v-model:open="removeOpen" :project="project" :worktree="removing" />
  </div>
</template>
