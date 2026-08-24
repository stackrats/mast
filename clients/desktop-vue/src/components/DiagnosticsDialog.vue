<script setup lang="ts">
import { computed, ref, watch } from "vue";
import {
  Check,
  History,
  Info,
  RefreshCw,
  ShieldAlert,
  TriangleAlert,
  Wrench,
  XCircle,
} from "lucide-vue-next";

import type {
  DiagnosticFinding,
  DiagnosticReport,
  DiagnosticsHistory,
  ProjectSummary,
  RepairPlan,
} from "../bindings";
import { runActionCollecting } from "../lib/operations";
import { diagnosticsHistory, repairPreview, runDiagnostics } from "../lib/transport";
import { useEngineStore } from "../stores/engine";
import Button from "./ui/Button.vue";
import Checkbox from "./ui/Checkbox.vue";
import Modal from "./ui/Modal.vue";

const open = defineModel<boolean>("open", { default: false });
// When set, the run is scoped to this project: only its findings, no
// probes into the neighbours, and no history (history tracks full passes).
const { scope = null } = defineProps<{ scope?: ProjectSummary | null }>();
const store = useEngineStore();

const report = ref<DiagnosticReport | null>(null);
const running = ref(false);
const history = ref<DiagnosticsHistory | null>(null);
const showHistory = ref(false);

// Repair consent flow: pick a finding → preview → (consent) → apply.
const activeFinding = ref<DiagnosticFinding | null>(null);
const plan = ref<RepairPlan | null>(null);
const planLoading = ref(false);
const consented = ref(false);
const showDiff = ref(false);
const applying = ref(false);
const applyLines = ref<{ line: string; stderr: boolean }[]>([]);
const applyError = ref<string | null>(null);

async function refresh() {
  running.value = true;
  try {
    report.value = await runDiagnostics(scope?.id ?? null);
    history.value = scope ? null : await diagnosticsHistory();
  } catch (e) {
    store.error = e instanceof Error ? e.message : String(e);
  } finally {
    running.value = false;
  }
}

watch(open, (isOpen) => {
  if (!isOpen) return;
  closeRepair();
  showHistory.value = false;
  void refresh();
});

const counts = computed(() => {
  const findings = report.value?.findings ?? [];
  return {
    error: findings.filter((f) => f.severity === "error").length,
    warning: findings.filter((f) => f.severity === "warning").length,
    info: findings.filter((f) => f.severity === "info").length,
  };
});

async function openRepair(finding: DiagnosticFinding) {
  if (!finding.repair) return;
  activeFinding.value = finding;
  plan.value = null;
  consented.value = false;
  showDiff.value = false;
  applyLines.value = [];
  applyError.value = null;
  planLoading.value = true;
  try {
    plan.value = await repairPreview(finding.repair.id, finding.repair.arg, finding.project);
  } catch (e) {
    store.error = e instanceof Error ? e.message : String(e);
    activeFinding.value = null;
  } finally {
    planLoading.value = false;
  }
}

function closeRepair() {
  activeFinding.value = null;
  plan.value = null;
  applyLines.value = [];
  applyError.value = null;
  applying.value = false;
}

const canApply = computed(() => {
  if (!plan.value || plan.value.noOp || applying.value) return false;
  return plan.value.repair.risk !== "highRisk" || consented.value;
});

async function apply() {
  const finding = activeFinding.value;
  if (!finding?.repair || !plan.value) return;
  applying.value = true;
  applyLines.value = [];
  applyError.value = null;
  try {
    await runActionCollecting(
      {
        type: "applyRepair",
        repair: finding.repair.id,
        arg: finding.repair.arg,
        project: finding.project,
      },
      (line) => applyLines.value.push(line),
    );
    closeRepair();
    await refresh();
  } catch (e) {
    applyError.value = e instanceof Error ? e.message : String(e);
    applying.value = false;
  }
}

function when(unix: number): string {
  return new Date(unix * 1000).toLocaleString();
}

const riskLabel: Record<string, string> = {
  safe: "safe",
  caution: "caution",
  highRisk: "high risk",
};
</script>

<template>
  <Modal v-model:open="open" :title="scope ? `Diagnostics — ${scope.name}` : 'Diagnostics'" wide>
    <!-- Repair preview / consent view -->
    <div v-if="activeFinding" class="space-y-3">
      <p class="text-xs font-medium text-slate-700 dark:text-slate-200">
        {{ activeFinding.repair?.title }}
        <span
          class="ml-1 rounded px-1.5 py-0.5 text-[10px] font-semibold uppercase tracking-wide"
          :class="
            activeFinding.repair?.risk === 'highRisk'
              ? 'bg-red-100 text-red-700 dark:bg-red-950 dark:text-red-300'
              : activeFinding.repair?.risk === 'caution'
                ? 'bg-amber-100 text-amber-700 dark:bg-amber-950 dark:text-amber-300'
                : 'bg-emerald-100 text-emerald-700 dark:bg-emerald-950 dark:text-emerald-300'
          "
        >
          {{ riskLabel[activeFinding.repair?.risk ?? "safe"] }}
        </span>
      </p>
      <p class="text-xs text-slate-500 dark:text-slate-400">
        {{ activeFinding.repair?.description }}
      </p>

      <p v-if="planLoading" class="text-xs text-slate-500">planning…</p>
      <template v-else-if="plan">
        <ul class="space-y-1">
          <li
            v-for="line in plan.summary"
            :key="line"
            class="flex items-start gap-2 text-xs text-slate-600 dark:text-slate-300"
          >
            <Wrench class="mt-0.5 h-3.5 w-3.5 shrink-0 text-slate-400" />
            <span class="min-w-0 break-all">{{ line }}</span>
          </li>
        </ul>

        <p
          v-if="plan.noOp"
          class="flex items-center gap-2 rounded-md border border-emerald-200 bg-emerald-50 p-2 text-xs text-emerald-800 dark:border-emerald-900 dark:bg-emerald-950/40 dark:text-emerald-300"
        >
          <Check class="h-3.5 w-3.5" /> Nothing to change.
        </p>

        <template v-if="plan.filePreview && !plan.noOp">
          <Button variant="outline" size="sm" @click="showDiff = !showDiff">
            {{ showDiff ? "Hide" : "Show" }} file changes ({{ plan.filePreview.file }})
          </Button>
          <div v-if="showDiff" class="grid grid-cols-2 gap-2">
            <pre
              class="max-h-56 overflow-auto rounded-md border border-slate-200 bg-slate-50 p-2 font-mono text-[11px] leading-4 whitespace-pre dark:border-slate-800 dark:bg-slate-900"
              >{{ plan.filePreview.before }}</pre>
            <pre
              class="max-h-56 overflow-auto rounded-md border border-emerald-200 bg-emerald-50/50 p-2 font-mono text-[11px] leading-4 whitespace-pre dark:border-emerald-900 dark:bg-emerald-950/30"
              >{{ plan.filePreview.after }}</pre>
          </div>
        </template>

        <div
          v-if="plan.repair.risk === 'highRisk' && !plan.noOp"
          class="rounded-md border border-red-200 bg-red-50 p-2.5 text-red-800 dark:border-red-900 dark:bg-red-950/40 dark:text-red-200"
        >
          <Checkbox v-model="consented" class="text-inherit!">
            <span>
              <ShieldAlert class="mr-1 inline h-3.5 w-3.5" />
              I understand this changes system security: docker-group membership is equivalent to
              root on this machine.
            </span>
          </Checkbox>
        </div>

        <div
          v-if="applyLines.length"
          class="max-h-44 overflow-y-auto rounded-md border border-slate-200 bg-slate-50 p-2 font-mono text-[11px] leading-4 dark:border-slate-800 dark:bg-slate-900"
        >
          <div
            v-for="(line, i) in applyLines"
            :key="i"
            :class="{ 'text-amber-700 dark:text-amber-400': line.stderr }"
          >
            {{ line.line }}
          </div>
        </div>
        <p v-if="applyError" class="text-xs text-red-700 dark:text-red-300">{{ applyError }}</p>

        <div class="flex justify-end gap-2">
          <Button variant="outline" :disabled="applying" @click="closeRepair">Back</Button>
          <Button v-if="!plan.noOp" :disabled="!canApply || store.readOnly" @click="apply">
            {{ applying ? "applying…" : "Apply repair" }}
          </Button>
        </div>
      </template>
    </div>

    <!-- Findings view -->
    <div v-else class="space-y-3">
      <div class="flex items-center justify-between">
        <p class="text-xs text-slate-500 dark:text-slate-400">
          <template v-if="running">running checks…</template>
          <template v-else-if="report">
            {{ report.checksRun }} checks ·
            <span v-if="report.findings.length === 0" class="text-emerald-600">
              {{ scope ? "nothing to fix for this project" : "all passed" }}
            </span>
            <template v-else>
              <span v-if="counts.error" class="text-red-600">{{ counts.error }} errors</span>
              <span v-if="counts.error && (counts.warning || counts.info)"> · </span>
              <span v-if="counts.warning" class="text-amber-600">
                {{ counts.warning }} warnings
              </span>
              <span v-if="counts.warning && counts.info"> · </span>
              <span v-if="counts.info">{{ counts.info }} notes</span>
            </template>
          </template>
        </p>
        <Button variant="outline" size="sm" :disabled="running" @click="refresh">
          <RefreshCw class="h-3.5 w-3.5" :class="{ 'animate-spin': running }" /> Re-run
        </Button>
      </div>

      <p
        v-if="report && report.findings.length === 0 && !running"
        class="flex items-center gap-2 rounded-md border border-emerald-200 bg-emerald-50 p-2.5 text-xs text-emerald-800 dark:border-emerald-900 dark:bg-emerald-950/40 dark:text-emerald-300"
      >
        <Check class="h-4 w-4" /> Everything looks healthy.
      </p>

      <ul v-if="report" class="space-y-1.5">
        <li
          v-for="(finding, i) in report.findings"
          :key="i"
          class="rounded-md border border-slate-200 p-2.5 dark:border-slate-800"
        >
          <div class="flex items-start justify-between gap-3">
            <div class="min-w-0">
              <p
                class="flex items-start gap-1.5 text-xs font-medium text-slate-700 dark:text-slate-200"
              >
                <XCircle
                  v-if="finding.severity === 'error'"
                  class="mt-0.5 h-3.5 w-3.5 shrink-0 text-red-500"
                />
                <TriangleAlert
                  v-else-if="finding.severity === 'warning'"
                  class="mt-0.5 h-3.5 w-3.5 shrink-0 text-amber-500"
                />
                <Info
                  v-else
                  class="mt-0.5 h-3.5 w-3.5 shrink-0 text-sky-500 dark:text-neutral-400"
                />
                {{ finding.title }}
              </p>
              <p class="mt-1 ml-5 text-xs text-slate-500 dark:text-slate-400">
                {{ finding.detail }}
              </p>
            </div>
            <Button
              v-if="finding.repair"
              variant="outline"
              size="sm"
              class="shrink-0"
              :disabled="store.readOnly"
              @click="openRepair(finding)"
            >
              <Wrench class="h-3.5 w-3.5" /> Repair
            </Button>
          </div>
        </li>
      </ul>

      <div v-if="history && (history.runs.length || history.repairs.length)">
        <Button variant="outline" size="sm" @click="showHistory = !showHistory">
          <History class="h-3.5 w-3.5" /> {{ showHistory ? "Hide" : "Show" }} history
        </Button>
        <div v-if="showHistory" class="mt-2 grid grid-cols-2 gap-3 text-xs">
          <div>
            <p class="font-medium text-slate-600 dark:text-slate-300">Runs</p>
            <ul class="mt-1 space-y-0.5 text-slate-500 dark:text-slate-400">
              <li v-for="run in history.runs.slice(0, 8)" :key="run.id">
                {{ when(run.takenUnix) }} —
                <span v-if="run.errors" class="text-red-600">{{ run.errors }}E</span>
                <span v-if="run.warnings" class="text-amber-600"> {{ run.warnings }}W</span>
                <span v-if="!run.errors && !run.warnings" class="text-emerald-600">clean</span>
              </li>
            </ul>
          </div>
          <div>
            <p class="font-medium text-slate-600 dark:text-slate-300">Repairs applied</p>
            <ul class="mt-1 space-y-0.5 text-slate-500 dark:text-slate-400">
              <li v-for="(repair, i) in history.repairs.slice(0, 8)" :key="i">
                {{ when(repair.appliedUnix) }} — {{ repair.repair
                }}<template v-if="repair.projectName"> ({{ repair.projectName }})</template> ·
                {{ repair.outcome }}
              </li>
              <li v-if="history.repairs.length === 0">none yet</li>
            </ul>
          </div>
        </div>
      </div>
    </div>
  </Modal>
</template>
