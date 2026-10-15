<script setup lang="ts">
// What coding agents are doing across every project (ADR-0012): the totals,
// a Clean up for everything they left, and each project's activity — the
// same section its card shows. Everything here keeps running after the
// session that started it is gone, which is the whole reason it needs a page.
import { computed, ref } from "vue";
import { Bot, Brush, GitBranch, TriangleAlert } from "lucide-vue-next";

import { cleanupPlan, hasActivity, hostProcessesWatched, overview } from "../lib/agents";
import { formatBytes } from "../lib/usage";
import { useEngineStore } from "../stores/engine";
import AgentsCleanupDialog from "./AgentsCleanupDialog.vue";
import ProjectAgents from "./ProjectAgents.vue";
import Button from "./ui/Button.vue";

const store = useEngineStore();

const busy = computed(() => store.projects.filter(hasActivity));
const quiet = computed(() => store.projects.filter((p) => !hasActivity(p)));
const totals = computed(() => overview(store.projects));
const watched = hostProcessesWatched();
const cleanable = computed(() => cleanupPlan(store.projects, Date.now()).length > 0);
const cleanupOpen = ref(false);

const tileClass =
  "rounded-lg border border-slate-200 bg-white p-3 dark:border-slate-800 dark:bg-slate-900";
</script>

<template>
  <div class="mx-auto max-w-4xl space-y-4">
    <header class="flex items-start justify-between gap-3">
      <div class="min-w-0">
        <h1
          class="flex items-center gap-2 text-base font-semibold text-slate-900 dark:text-slate-100"
        >
          <Bot class="h-4 w-4 text-slate-400" /> Agents
        </h1>
        <p class="mt-0.5 text-xs text-slate-500 dark:text-slate-400">
          Coding agents working in your projects, the git worktrees they make, and everything they
          start — which keeps running after their session ends.
        </p>
      </div>
      <Button
        variant="outline"
        size="sm"
        class="shrink-0"
        :disabled="store.readOnly || !cleanable"
        @click="cleanupOpen = true"
      >
        <Brush class="h-3.5 w-3.5" /> Clean up…
      </Button>
    </header>

    <p
      v-if="!watched"
      class="rounded-md border border-slate-200 bg-slate-50 p-2 text-xs text-slate-600 dark:border-slate-800 dark:bg-slate-900 dark:text-slate-300"
    >
      Processes on this machine are watched on Linux only for now. Worktrees, their containers and
      what runs in app containers are listed here everywhere.
    </p>

    <div class="grid grid-cols-3 gap-2">
      <div :class="tileClass">
        <p class="flex items-center gap-1.5 text-xs text-slate-400">
          <Bot class="h-3.5 w-3.5" /> Sessions
        </p>
        <p class="mt-1 text-xl font-semibold text-slate-900 dark:text-slate-100">
          {{ totals.sessions }}
        </p>
        <p class="text-xs text-slate-400">{{ watched ? "working now" : "not watched here" }}</p>
      </div>
      <div :class="tileClass">
        <p class="flex items-center gap-1.5 text-xs text-slate-400">
          <GitBranch class="h-3.5 w-3.5" /> Worktrees
        </p>
        <p class="mt-1 text-xl font-semibold text-slate-900 dark:text-slate-100">
          {{ totals.worktrees }}
        </p>
        <p class="text-xs" :class="totals.missingWorktrees ? 'text-amber-600' : 'text-slate-400'">
          <template v-if="totals.missingWorktrees">
            {{ totals.missingWorktrees }} with the directory gone
          </template>
          <template v-else-if="totals.worktreeStacks">
            {{ totals.worktreeStacks }} running containers
          </template>
          <template v-else>across all projects</template>
        </p>
      </div>
      <div
        :class="[
          tileClass,
          totals.leftovers
            ? 'border-amber-200 bg-amber-50/60 dark:border-amber-900 dark:bg-amber-950/30'
            : '',
        ]"
      >
        <p class="flex items-center gap-1.5 text-xs text-slate-400">
          <TriangleAlert class="h-3.5 w-3.5" /> Left running
        </p>
        <p
          class="mt-1 text-xl font-semibold"
          :class="
            totals.leftovers
              ? 'text-amber-700 dark:text-amber-300'
              : 'text-slate-900 dark:text-slate-100'
          "
        >
          {{ totals.leftovers }}
        </p>
        <p class="text-xs text-slate-400">
          {{
            totals.leftovers ? `holding ${formatBytes(totals.leftoverBytes)}` : "nothing orphaned"
          }}
        </p>
      </div>
    </div>

    <section
      v-if="busy.length === 0"
      class="rounded-lg border border-dashed border-slate-300 p-8 text-center dark:border-slate-700"
    >
      <p class="text-sm text-slate-600 dark:text-slate-300">No agent activity in your projects.</p>
      <p class="mt-1 text-xs text-slate-400">
        When an agent works in a project, makes a worktree or leaves something running, it shows up
        here.
      </p>
    </section>

    <section
      v-for="project in busy"
      :key="project.id"
      class="rounded-lg border border-slate-200 bg-white p-3 dark:border-slate-800 dark:bg-slate-900"
    >
      <button
        class="mb-3 block max-w-full min-w-0 text-left"
        @click="store.selection = { kind: 'project', id: project.id }"
      >
        <p
          class="truncate text-sm font-semibold text-slate-800 hover:underline dark:text-slate-200"
        >
          {{ project.name }}
        </p>
        <p class="truncate font-mono text-[11px] text-slate-400">{{ project.path }}</p>
      </button>
      <ProjectAgents :project="project" />
    </section>

    <p v-if="busy.length && quiet.length" class="text-xs text-slate-400">
      Nothing from agents in {{ quiet.map((p) => p.name).join(", ") }}.
    </p>

    <AgentsCleanupDialog v-model:open="cleanupOpen" />
  </div>
</template>
