<script setup lang="ts">
// The HTTPS dialog: every address a project serves through the local proxy
// — the app, and each dev server it runs on this machine (a separate
// frontend's `vp dev`) — in one place, because the question that matters
// across them is whether they are one site, and only a view of all of them
// can answer it. Each row is its own operation; the output below follows
// whichever row was acted on last.
import { computed, reactive, ref, watch } from "vue";
import { Check, Copy, FileCog, Globe, Lock, TriangleAlert } from "lucide-vue-next";

import type { ProjectCommand, ProjectSummary, ProxyCa } from "../bindings";
import {
  domainFromName,
  isHostCommand,
  isLocalDomain,
  siteFix,
  suggestCommandDomain,
} from "../lib/localDomains";
import { iconButtonClass } from "../lib/menu";
import { proxyCa } from "../lib/transport";
import { commandDomainKey, domainKey, useEngineStore } from "../stores/engine";
import Button from "./ui/Button.vue";
import FixButton from "./ui/FixButton.vue";
import Input from "./ui/Input.vue";
import Modal from "./ui/Modal.vue";
import Tooltip from "./ui/Tooltip.vue";

const open = defineModel<boolean>("open", { default: false });
const {
  project,
  locked,
  focus = null,
} = defineProps<{
  project: ProjectSummary;
  locked: boolean;
  /** The command whose row the dialog was opened for, from its chip. */
  focus?: string | null;
}>();
const store = useEngineStore();

const commands = computed(() => project.commands ?? []);
const claims = computed(() => project.commandDomains ?? []);
const hostCommands = computed(() => commands.value.filter(isHostCommand));
const containerCommands = computed(() => commands.value.filter((c) => !isHostCommand(c)));
/** Claims whose command is gone — renamed or deleted. They keep the domain
 * until released, or until another command here claims it. */
const orphans = computed(() =>
  claims.value.filter((c) => !commands.value.some((cmd) => cmd.name === c.command)),
);
function claimOf(name: string) {
  return claims.value.find((c) => c.command === name) ?? null;
}
function announced(name: string): number | null {
  return project.commandPorts?.find((p) => p.command === name)?.port ?? null;
}

const appDraft = ref("");
const drafts = reactive<Record<string, string>>({});
const portDrafts = reactive<Record<string, string>>({});
/** The row acted on last; the output and Fix buttons below are its. */
const lastKey = ref<string | null>(null);

watch(
  open,
  (isOpen) => {
    if (!isOpen) return;
    appDraft.value = project.localDomain ?? domainFromName(project.name);
    for (const cmd of hostCommands.value) {
      const claim = claimOf(cmd.name);
      drafts[cmd.name] = claim?.domain ?? suggestCommandDomain(project, cmd);
      portDrafts[cmd.name] = claim?.pinned && claim.port != null ? String(claim.port) : "";
    }
    lastKey.value = focus ? commandDomainKey(project.id, focus) : domainKey(project.id);
    void loadProxyCa();
  },
  { immediate: true },
);

function normalized(domain: string): string {
  return domain.trim().toLowerCase();
}
function portValid(name: string): boolean {
  const text = portDrafts[name]?.trim() ?? "";
  if (!text) return true;
  const port = Number(text);
  return /^\d+$/.test(text) && port >= 1 && port <= 65535;
}
function busy(key: string): boolean {
  const op = store.operations[key];
  return op != null && op.terminal === null;
}

function setApp(domain: string | null) {
  const key = domainKey(project.id);
  lastKey.value = key;
  void store.runLifecycle(key, domain ? `Enable https://${domain}` : "Disable local domain", {
    type: "setLocalDomain",
    id: project.id,
    domain,
  });
}
function setCommand(name: string, domain: string | null) {
  const key = commandDomainKey(project.id, name);
  lastKey.value = key;
  const pinned = portDrafts[name]?.trim();
  void store.runLifecycle(
    key,
    domain ? `Enable https://${domain} — ${name}` : `Disable local domain — ${name}`,
    {
      type: "setCommandDomain",
      id: project.id,
      command: name,
      domain,
      port: domain && pinned ? Number(pinned) : null,
    },
  );
}
function openDomain(domain: string) {
  void store.run({ type: "openUrl", url: `https://${domain}` });
}

/** Where a command row's address leads right now, in one line. */
function targetLine(cmd: ProjectCommand): string {
  const claim = claimOf(cmd.name);
  const live = announced(cmd.name);
  if (!claim) {
    return live != null
      ? `Running on localhost:${live} — the proxy would follow that port.`
      : "The proxy follows the port it prints when it runs, or pin one.";
  }
  if (claim.port == null) {
    return `Waiting for ${cmd.name} to print its address — run it, or pin a port.`;
  }
  return claim.pinned
    ? `→ localhost:${claim.port}, pinned.`
    : `→ localhost:${claim.port}, following what ${cmd.name} prints.`;
}

/** A command on another site than the app cannot share its cookies — the
 * Sanctum SPA login, most of all. Offered while the mistake is still a
 * draft. */
function siteFixFor(name: string) {
  return siteFix(project.localDomain ?? null, normalized(drafts[name] ?? ""));
}

const lastOp = computed(() => (lastKey.value ? store.operations[lastKey.value] : undefined));

/** The proxy CA's root certificate, for the trust that Fix buttons cannot
 * reach: Firefox's import dialog wants the file, curl --cacert and
 * NODE_EXTRA_CA_CERTS want the path, other tools want the PEM itself. */
const caFile = ref<ProxyCa | null>(null);
const manualCopied = ref<"path" | "pem" | "hosts" | null>(null);
async function loadProxyCa() {
  try {
    caFile.value = await proxyCa();
  } catch {
    caFile.value = null;
  }
}
watch(
  () => lastOp.value?.terminal,
  () => {
    if (open.value) void loadProxyCa();
  },
);
/** The exact lines the add-hosts-entry repair would append, one per
 * domain this project claims — so the manual route needs no guesswork. */
const hostsLines = computed(() =>
  [project.localDomain, ...claims.value.map((c) => c.domain)]
    .filter((d): d is string => !!d)
    .map((d) => `127.0.0.1\t${d}`)
    .join("\n"),
);
async function copyManual(kind: "path" | "pem" | "hosts") {
  const value =
    kind === "hosts" ? hostsLines.value : kind === "path" ? caFile.value?.path : caFile.value?.pem;
  if (!value) return;
  await navigator.clipboard.writeText(value);
  manualCopied.value = kind;
  setTimeout(() => (manualCopied.value = null), 1500);
}

const rowClass = "space-y-1.5 rounded-md border border-slate-200 p-2.5 dark:border-slate-700";
const focusClass = "border-sky-400 dark:border-sky-500";
const sectionClass =
  "text-[11px] font-medium tracking-wide text-slate-500 uppercase dark:text-slate-400";
</script>

<template>
  <!-- The HTTPS differentiator: claim a .test domain, watch the proxy
       converge, and pick up the two system-level steps as Fix buttons —
       the operation's output stays in the dialog so "why does the browser
       still warn" has an answer in view. -->
  <Modal v-model:open="open" title="Local HTTPS" wide>
    <div class="space-y-3">
      <p class="text-xs text-slate-500 dark:text-slate-400">
        One shared Caddy proxy (ports 80/443) serves every claimed domain with certificates from its
        own local-only authority — secure cookies, service workers and other HTTPS-only features
        work in dev, and each address stays stable however the ports move.
      </p>

      <p :class="sectionClass">App</p>
      <div :class="rowClass">
        <div class="flex flex-wrap items-center gap-2">
          <Input
            v-model="appDraft"
            placeholder="myapp.test"
            class="min-w-48 flex-1 font-mono"
            aria-label="App domain"
          />
          <Button
            size="sm"
            :disabled="locked || !isLocalDomain(appDraft) || busy(domainKey(project.id))"
            @click="setApp(normalized(appDraft))"
          >
            <Lock class="h-3.5 w-3.5" />
            {{ project.localDomain ? "Update" : "Enable" }}
          </Button>
          <template v-if="project.localDomain">
            <Button variant="outline" size="sm" @click="openDomain(project.localDomain)">
              <Globe class="h-3.5 w-3.5" /> Open
            </Button>
            <Button
              variant="outline"
              size="sm"
              :disabled="locked || busy(domainKey(project.id))"
              @click="setApp(null)"
            >
              Disable
            </Button>
          </template>
        </div>
        <p
          v-if="appDraft && !isLocalDomain(appDraft)"
          class="text-xs text-amber-700 dark:text-amber-300"
        >
          Lowercase letters, digits and hyphens, ending in
          <span class="font-mono">.test</span> or <span class="font-mono">.localhost</span>.
        </p>
      </div>

      <template v-if="commands.length > 0 || orphans.length > 0">
        <p :class="sectionClass">Commands</p>
        <div
          v-for="cmd in hostCommands"
          :key="cmd.name"
          :class="[rowClass, focus === cmd.name ? focusClass : '']"
        >
          <p class="text-xs text-slate-700 dark:text-slate-200">
            <span class="font-medium">{{ cmd.name }}</span>
            <span class="font-mono text-slate-400">
              · {{ cmd.command }}<template v-if="cmd.cwd"> in {{ cmd.cwd }}</template>
            </span>
          </p>
          <div class="flex flex-wrap items-center gap-2">
            <Input
              v-model="drafts[cmd.name]"
              placeholder="web.myapp.test"
              class="min-w-48 flex-1 font-mono"
              :aria-label="`Domain for ${cmd.name}`"
            />
            <Tooltip
              text="Leave empty to follow the port the command prints when it starts. Set one to pin it."
            >
              <Input
                v-model="portDrafts[cmd.name]"
                :placeholder="String(claimOf(cmd.name)?.port ?? announced(cmd.name) ?? 'port')"
                class="w-20 font-mono"
                :aria-label="`Port for ${cmd.name}`"
              />
            </Tooltip>
            <Button
              size="sm"
              :disabled="
                locked ||
                !isLocalDomain(drafts[cmd.name] ?? '') ||
                !portValid(cmd.name) ||
                busy(commandDomainKey(project.id, cmd.name))
              "
              @click="setCommand(cmd.name, normalized(drafts[cmd.name] ?? ''))"
            >
              <Lock class="h-3.5 w-3.5" />
              {{ claimOf(cmd.name) ? "Update" : "Enable" }}
            </Button>
            <template v-if="claimOf(cmd.name)">
              <Button variant="outline" size="sm" @click="openDomain(claimOf(cmd.name)!.domain)">
                <Globe class="h-3.5 w-3.5" /> Open
              </Button>
              <Button
                variant="outline"
                size="sm"
                :disabled="locked || busy(commandDomainKey(project.id, cmd.name))"
                @click="setCommand(cmd.name, null)"
              >
                Disable
              </Button>
            </template>
          </div>
          <p class="text-xs text-slate-500 dark:text-slate-400">{{ targetLine(cmd) }}</p>
          <p v-if="!portValid(cmd.name)" class="text-xs text-amber-700 dark:text-amber-300">
            A port is a number from 1 to 65535.
          </p>
          <div
            v-if="siteFixFor(cmd.name)"
            class="flex flex-wrap items-center gap-1.5 rounded-md bg-amber-50 p-2 text-xs text-amber-800 dark:bg-amber-950/40 dark:text-amber-200"
          >
            <TriangleAlert class="h-3.5 w-3.5 shrink-0" />
            <span>
              {{ normalized(drafts[cmd.name] ?? "") }} and {{ project.localDomain }} are different
              sites, so cookies the app sets will not reach this command's pages.
            </span>
            <Button
              variant="outline"
              size="sm"
              @click="drafts[cmd.name] = siteFixFor(cmd.name)!.commandDomain"
            >
              Use {{ siteFixFor(cmd.name)!.commandDomain }}
            </Button>
            <Button
              v-if="siteFixFor(cmd.name)!.appDomain"
              variant="outline"
              size="sm"
              @click="appDraft = siteFixFor(cmd.name)!.appDomain!"
            >
              Rename the app to {{ siteFixFor(cmd.name)!.appDomain }}
            </Button>
          </div>
        </div>
        <p v-for="cmd in containerCommands" :key="cmd.name" class="px-1 text-xs text-slate-400">
          <span class="font-medium">{{ cmd.name }}</span> runs in the app container — it is reached
          through the app's domain.
        </p>
        <div v-for="claim in orphans" :key="claim.command" :class="rowClass">
          <div class="flex flex-wrap items-center gap-2 text-xs">
            <span class="font-mono text-slate-700 dark:text-slate-200">{{ claim.domain }}</span>
            <span class="text-slate-500 dark:text-slate-400">
              was {{ claim.command }}'s, which is no longer one of this project's commands. Claim it
              for another command above, or release it.
            </span>
            <Button
              variant="outline"
              size="sm"
              :disabled="locked || busy(commandDomainKey(project.id, claim.command))"
              @click="setCommand(claim.command, null)"
            >
              Release
            </Button>
          </div>
        </div>
      </template>

      <div
        v-if="lastOp && lastOp.lines.length > 0"
        class="max-h-48 overflow-y-auto rounded-md border border-slate-200 bg-slate-50 p-2 font-mono text-[11px] leading-relaxed dark:border-slate-800 dark:bg-neutral-900"
      >
        <p
          v-for="(line, i) in lastOp.lines"
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
      <p
        v-if="lastOp && lastOp.terminal === 'failed'"
        class="text-xs text-red-700 dark:text-red-300"
      >
        Failed: {{ lastOp.error }}
      </p>
      <!-- Follow-up steps, not failure fallout: these render on success
           too, because /etc/hosts and CA trust are what stand between a
           green operation and a browser that stops warning. -->
      <div v-if="lastOp && lastOp.fixes.length > 0" class="flex flex-wrap gap-1">
        <FixButton
          v-for="f in lastOp.fixes"
          :key="f.repair.id + (f.repair.arg ?? '')"
          :repair="f.repair"
          :project="f.project"
        />
      </div>
      <p class="text-xs text-slate-400">
        Two one-time system steps — the hosts-file line and trusting the certificate authority —
        appear as Fix buttons after enabling, each with a preview and an elevation prompt. Nothing
        touches your system without them.
      </p>
      <!-- The manual route, always available: elevation prompts are not an
           option everywhere (no polkit agent, hardened machines, Firefox's
           own store), so everything a person needs to do it by hand is
           copyable right here. -->
      <div v-if="hostsLines" class="space-y-2 border-t border-slate-200 pt-3 dark:border-slate-700">
        <p class="text-xs font-medium text-slate-600 dark:text-slate-300">
          Prefer to set it up yourself?
        </p>
        <p class="text-xs text-slate-500 dark:text-slate-400">
          Add these lines to your hosts file:
        </p>
        <div class="flex items-start gap-1.5">
          <pre
            class="font-mono text-xs whitespace-pre-wrap select-all text-slate-900 dark:text-slate-100"
            >{{ hostsLines }}</pre>
          <Tooltip text="Copy the hosts lines.">
            <button :class="['shrink-0', iconButtonClass]" @click="copyManual('hosts')">
              <Check v-if="manualCopied === 'hosts'" class="h-3.5 w-3.5 text-emerald-600" />
              <Copy v-else class="h-3.5 w-3.5" />
            </button>
          </Tooltip>
        </div>
        <div>
          <Button variant="outline" size="sm" @click="store.run({ type: 'openHostsFile' })">
            <FileCog class="h-3.5 w-3.5" /> Open hosts file
          </Button>
        </div>
        <template v-if="caFile">
          <p class="text-xs text-slate-500 dark:text-slate-400">
            And trust the certificate authority where you need it — Firefox's import dialog,
            <span class="font-mono">curl --cacert</span>,
            <span class="font-mono">NODE_EXTRA_CA_CERTS</span>:
          </p>
          <p
            class="flex items-baseline gap-1.5 font-mono text-xs break-all select-all text-slate-900 dark:text-slate-100"
          >
            {{ caFile.path }}
            <Tooltip text="Copy the certificate file's path.">
              <button :class="['shrink-0', iconButtonClass]" @click="copyManual('path')">
                <Check v-if="manualCopied === 'path'" class="h-3.5 w-3.5 text-emerald-600" />
                <Copy v-else class="h-3.5 w-3.5" />
              </button>
            </Tooltip>
          </p>
          <div>
            <Button variant="outline" size="sm" @click="copyManual('pem')">
              <Check v-if="manualCopied === 'pem'" class="h-3.5 w-3.5 text-emerald-600" />
              <Copy v-else class="h-3.5 w-3.5" />
              {{ manualCopied === "pem" ? "Copied" : "Copy certificate (PEM)" }}
            </Button>
          </div>
        </template>
      </div>
    </div>
  </Modal>
</template>
