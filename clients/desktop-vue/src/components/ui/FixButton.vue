<script setup lang="ts">
// The Fix button a failed operation can carry: the engine matched an error
// signature in the output to a concrete repair. Clicking never applies
// anything — it opens the repair's preview (exactly what will change),
// with the same risk badge and consent rules as the Diagnostics dialog.
import { ref, watch } from "vue";
import { Wrench } from "lucide-vue-next";

import type { ProjectId, RepairOffer, RepairPlan } from "../../bindings";
import { runActionCollecting, type OutputLine } from "../../lib/operations";
import { repairPreview } from "../../lib/transport";
import { useEngineStore } from "../../stores/engine";
import Button from "./Button.vue";
import Checkbox from "./Checkbox.vue";
import Modal from "./Modal.vue";

const { repair, project } = defineProps<{ repair: RepairOffer; project: ProjectId }>();
const emit = defineEmits<{ applied: [] }>();
const store = useEngineStore();

const open = ref(false);
const plan = ref<RepairPlan | null>(null);
const loading = ref(false);
const consented = ref(false);
const applying = ref(false);
const applied = ref(false);
const lines = ref<OutputLine[]>([]);
const error = ref<string | null>(null);

const riskLabel: Record<string, string> = {
  safe: "safe",
  caution: "caution",
  highRisk: "high risk",
};

async function show() {
  open.value = true;
  plan.value = null;
  consented.value = false;
  applied.value = false;
  lines.value = [];
  error.value = null;
  loading.value = true;
  try {
    plan.value = await repairPreview(repair.id, repair.arg, project);
  } catch (e) {
    error.value = e instanceof Error ? e.message : String(e);
  } finally {
    loading.value = false;
  }
}

// Emitted only when the dialog CLOSES after a successful apply: the parent
// may dismiss the failed operation on this event, which unmounts this
// component — firing it at apply time would tear the dialog away while the
// user is reading "Applied".
watch(open, (isOpen) => {
  if (!isOpen && applied.value) emit("applied");
});

async function apply() {
  if (!plan.value || plan.value.noOp) return;
  applying.value = true;
  lines.value = [];
  error.value = null;
  try {
    await runActionCollecting(
      { type: "applyRepair", repair: repair.id, arg: repair.arg, project },
      (line) => lines.value.push(line),
    );
    applied.value = true;
  } catch (e) {
    error.value = e instanceof Error ? e.message : String(e);
  } finally {
    applying.value = false;
  }
}
</script>

<template>
  <Button variant="outline" size="sm" @click="show">
    <Wrench class="h-3.5 w-3.5" /> Fix: {{ repair.title }}
  </Button>

  <Modal v-model:open="open" :title="repair.title">
    <div class="space-y-3">
      <p class="text-xs font-medium text-slate-700 dark:text-slate-200">
        <span
          class="rounded px-1.5 py-0.5 text-[10px] font-semibold tracking-wide uppercase"
          :class="
            repair.risk === 'highRisk'
              ? 'bg-red-100 text-red-700 dark:bg-red-950 dark:text-red-300'
              : repair.risk === 'caution'
                ? 'bg-amber-100 text-amber-700 dark:bg-amber-950 dark:text-amber-300'
                : 'bg-emerald-100 text-emerald-700 dark:bg-emerald-950 dark:text-emerald-300'
          "
        >
          {{ riskLabel[repair.risk] }}
        </span>
      </p>
      <p class="text-xs text-slate-500 dark:text-slate-400">{{ repair.description }}</p>

      <p v-if="loading" class="text-xs text-slate-400">Working out what would change…</p>
      <template v-else-if="plan">
        <div
          class="rounded-md border border-slate-200 bg-slate-50 p-2 text-xs dark:border-slate-800 dark:bg-neutral-900"
        >
          <p class="mb-1 font-medium text-slate-600 dark:text-slate-300">This will:</p>
          <p
            v-for="(line, i) in plan.summary"
            :key="i"
            class="font-mono text-[11px] leading-relaxed text-slate-600 dark:text-slate-300"
          >
            {{ line }}
          </p>
        </div>
        <Checkbox
          v-if="plan.repair.risk === 'highRisk' && !plan.noOp"
          block
          v-model="consented"
          label="I understand what this changes"
        />
      </template>

      <div
        v-if="lines.length > 0"
        class="max-h-32 overflow-y-auto rounded-md border border-slate-200 bg-slate-50 p-2 font-mono text-[11px] dark:border-slate-800 dark:bg-neutral-900"
      >
        <p
          v-for="(line, i) in lines"
          :key="i"
          :class="
            line.stderr
              ? 'text-amber-700 dark:text-amber-300'
              : 'text-slate-600 dark:text-slate-300'
          "
        >
          {{ line.line }}
        </p>
      </div>
      <p v-if="error" class="text-xs text-red-700 dark:text-red-300">{{ error }}</p>
      <p v-if="applied" class="text-xs text-emerald-700 dark:text-emerald-300">
        Applied — retry the operation that failed.
      </p>

      <div class="flex justify-end gap-2">
        <Button variant="outline" size="sm" @click="open = false">
          {{ applied ? "Close" : "Cancel" }}
        </Button>
        <Button
          v-if="plan && !plan.noOp && !applied"
          size="sm"
          :disabled="applying || store.readOnly || (plan.repair.risk === 'highRisk' && !consented)"
          @click="apply"
        >
          {{ applying ? "Applying…" : "Apply" }}
        </Button>
      </div>
    </div>
  </Modal>
</template>
