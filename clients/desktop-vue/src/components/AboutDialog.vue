<script setup lang="ts">
// About Mast. Beyond the version it carries the two facts a bug report always
// needs and nobody can produce from memory — which docker endpoint this
// instance resolved, and whether it actually owns mutation — with one button
// to put the lot on the clipboard.
import { computed, ref, watch } from "vue";
import { Check, Copy } from "lucide-vue-next";

import { appVersion } from "../lib/transport";
import { useEngineStore } from "../stores/engine";
import Modal from "./ui/Modal.vue";

const open = defineModel<boolean>("open", { default: false });
const store = useEngineStore();

const BANNER = [
  " ██╗██╗        ███╗   ███╗  █████╗  ███████╗ ████████╗",
  " ██║█████╗     ████╗ ████║ ██╔══██╗ ██╔════╝ ╚══██╔══╝",
  " ██║████████╗  ██╔████╔██║ ███████║ ███████╗    ██║",
  " ██║█████╔═╝   ██║╚██╔╝██║ ██╔══██║ ╚════██║    ██║",
  " ██║██╔═╝      ██║ ╚═╝ ██║ ██║  ██║ ███████║    ██║",
  " ╚═╝╚═╝        ╚═╝     ╚═╝ ╚═╝  ╚═╝ ╚══════╝    ╚═╝",
].join("\n");

/** Read from the bundle, so it cannot drift from what was shipped. */
const version = ref<string | null>(null);

// Fetched when the dialog opens rather than at startup: it is one IPC call
// that nothing else needs, and an About box is opened rarely.
watch(open, async (isOpen) => {
  if (isOpen && version.value === null) {
    try {
      version.value = await appVersion();
    } catch {
      version.value = "unknown";
    }
  }
});

const facts = computed(() => [
  { label: "Version", value: version.value ?? "…" },
  {
    label: "Docker",
    value: store.docker?.available ? (store.docker.contextName ?? "connected") : "not connected",
  },
  { label: "Endpoint", value: store.docker?.endpoint ?? "—" },
  // A second instance observes but cannot mutate, which explains a whole
  // class of "the buttons do nothing" reports.
  { label: "Mutation", value: store.readOnly ? "read-only (another instance owns it)" : "owned" },
]);

const copied = ref(false);
let timer: ReturnType<typeof setTimeout> | null = null;

async function copyDetails() {
  try {
    await navigator.clipboard.writeText(
      facts.value.map((f) => `${f.label}: ${f.value}`).join("\n"),
    );
    copied.value = true;
    if (timer) clearTimeout(timer);
    timer = setTimeout(() => (copied.value = false), 1500);
  } catch {
    // Clipboard access can be refused; the values are on screen anyway.
  }
}
</script>

<template>
  <Modal v-model:open="open" title="About Mast">
    <div class="space-y-4">
      <!-- v-text, not interpolation: inside `white-space: pre` the template's
           own indentation would become part of the art. -->
      <pre
        class="overflow-hidden font-mono text-[5px] leading-none text-slate-900 select-none dark:text-slate-100"
        role="img"
        aria-label="Mast"
        v-text="BANNER"
      />

      <p class="text-xs text-slate-500 dark:text-slate-400">
        A Linux-first desktop control center for Laravel Sail. Docker stays the source of truth —
        close Mast and your terminal workflow is exactly where you left it.
      </p>

      <dl class="space-y-1 text-xs">
        <div v-for="fact in facts" :key="fact.label" class="flex gap-2">
          <dt class="w-20 shrink-0 text-slate-400">{{ fact.label }}</dt>
          <dd class="min-w-0 flex-1 break-all text-slate-600 tabular-nums dark:text-slate-300">
            {{ fact.value }}
          </dd>
        </div>
      </dl>

      <button
        class="flex items-center gap-1.5 rounded-md px-2 py-1 text-xs text-slate-500 hover:bg-slate-100 hover:text-slate-700 dark:text-slate-400 dark:hover:bg-slate-800 dark:hover:text-slate-200"
        @click="copyDetails"
      >
        <Check v-if="copied" class="h-3.5 w-3.5 text-emerald-600" />
        <Copy v-else class="h-3.5 w-3.5" />
        {{ copied ? "Copied" : "Copy details" }}
      </button>
    </div>
  </Modal>
</template>
