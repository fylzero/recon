<script setup lang="ts">
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { computed, onUnmounted, ref } from "vue";
import * as api from "../api";
import { useApp } from "../composables/useApp";
import { useTransferJob } from "../composables/useTransfers";
import { baseName } from "../platform";
import { formatDuration, importStages, resumeAt, resumeChoice, runPhase, stagesPercent } from "../transfer";
import type { BackupInfo, Driver, ImportProgress, ImportResume, ImportResumePoint } from "../types";
import ImportWorkDashboard from "./ImportWorkDashboard.vue";
import Modal from "./Modal.vue";
import TransferRoute from "./TransferRoute.vue";

const props = defineProps<{
  connectionId: string;
  driver: Driver;
  namespace: string;
  namespaceLabel: string;
  path: string;
  info: BackupInfo;
}>();

const emit = defineEmits<{
  close: [];
  restored: [];
}>();

const { showToast } = useApp();
const job = useTransferJob(() => props.connectionId, "restore");

const understood = ref(false);
const running = ref(false);
const cancelling = ref(false);
const error = ref("");
const resumePoint = ref<ImportResumePoint | null>(null);
/** Resuming skips clearing, so the status starts at replaying rather than "Clearing…". */
const resuming = ref(false);
const progress = ref<ImportProgress | null>(null);
/** The last progress outside a reconnect, which reports every worker at 0. */
const dashboard = ref<ImportProgress | null>(null);
let transferId = "";
let stopProgress: UnlistenFn | null = null;

const label = computed(() => props.namespaceLabel.toLowerCase());
const atomic = computed(() => props.driver === "postgres" || props.driver === "mssql");
const fileName = computed(() => baseName(props.path));
const created = computed(() => {
  const date = new Date(props.info.createdAt);
  return Number.isNaN(date.getTime()) ? props.info.createdAt : date.toLocaleString();
});
const renamed = computed(() => Boolean(props.info.namespace) && props.info.namespace !== props.namespace);
const loss = computed(() => {
  if (props.driver === "postgres") {
    return `Grants, row-level security policies, custom collations and operators, and extended statistics in “${props.namespace}” will be removed.`;
  }
  if (props.driver === "mysql") {
    return `Events in “${props.namespace}” are kept as they are.`;
  }
  if (props.driver === "mssql") {
    return `Logins, users, and permissions in “${props.namespace}” are kept. Assemblies and Service Broker objects are removed only when a statement in the backup drops them.`;
  }
  return "";
});
const partial = computed(() =>
  atomic.value
    ? "If the restore fails or is cancelled, nothing changes."
    : `If the restore fails or is cancelled partway, “${props.namespace}” will be left partly restored. If the connection drops, Recon reconnects and picks up where it left off.`,
);
/** Clearing can't be stopped halfway outside Postgres, so cancelling waits until the replay starts. */
const canCancel = computed(() => !running.value || atomic.value || resuming.value || progress.value !== null);
const percent = computed(() => Math.round(stagesPercent(importStages(dashboard.value))));
const phase = computed(() => runPhase(progress.value));
const uncertain = computed(() => Boolean(resumePoint.value && resumePoint.value.to > resumePoint.value.from));
const uncertainWord = computed(() => (resumePoint.value && resumePoint.value.to - resumePoint.value.from > 1 ? "them" : "it"));
const statusLabel = computed(() => {
  if (cancelling.value) {
    return "Cancelling…";
  }
  return phase.value ?? (progress.value || resuming.value ? "" : "Clearing…");
});

function close() {
  if (running.value) {
    job.hide();
  } else {
    emit("close");
  }
}

async function start(resume: ImportResume | null = null) {
  if (running.value || (!understood.value && !resume)) {
    return;
  }
  running.value = true;
  resuming.value = Boolean(resume);
  error.value = "";
  resumePoint.value = null;
  progress.value = null;
  dashboard.value = null;
  transferId = job.begin(`Restoring “${props.namespace}” from ${fileName.value}`);
  stopProgress = await listen<ImportProgress>("import-progress", (event) => {
    if (event.payload.transferId === transferId) {
      progress.value = event.payload;
      if (event.payload.phase !== "reconnecting") {
        dashboard.value = event.payload;
      }
      job.progress(percent.value);
    }
  });
  try {
    const result = await api.restoreDatabase(props.connectionId, transferId, props.namespace, props.path, resume);
    const statements = `${result.statements.toLocaleString()} ${result.statements === 1 ? "statement" : "statements"}`;
    const duration = formatDuration(result.durationMs);
    showToast(`Restored “${props.namespace}” from ${fileName.value} in ${duration} (${statements})`);
    emit("restored");
    emit("close");
  } catch (err) {
    emit("restored");
    if (cancelling.value && atomic.value) {
      showToast("Restore cancelled. Nothing was changed.");
      emit("close");
    } else {
      const failure = api.importFailure(err);
      error.value = failure.message;
      resumePoint.value = failure.resume;
      job.stopped();
      if (job.hidden.value) {
        const hint = failure.resume ? " Open the restore to resume it." : "";
        showToast(`Restore of “${props.namespace}” stopped: ${error.value}${hint}`, "error");
      }
    }
  } finally {
    running.value = false;
    resuming.value = false;
    cancelling.value = false;
    stopProgress?.();
    stopProgress = null;
  }
}

function resume(skip: number) {
  if (resumePoint.value) {
    void start(resumeAt(resumePoint.value, skip));
  }
}

function cancel() {
  if (!running.value) {
    emit("close");
    return;
  }
  if (!canCancel.value) {
    return;
  }
  cancelling.value = true;
  void api.cancelTransfer(transferId);
}

onUnmounted(() => stopProgress?.());
</script>

<template>
  <Modal v-if="!job.hidden.value" :title="`Restore ${label}`" :xwide="running" @close="close">
    <template v-if="running" #aside>
      <TransferRoute
        :file="fileName"
        :path="path"
        file-label="Recon backup"
        :target="namespace"
        :target-label="namespaceLabel"
        :driver="driver"
      />
    </template>
    <TransferRoute
      v-if="!running"
      :file="fileName"
      :path="path"
      file-label="Recon backup"
      :target="namespace"
      :target-label="namespaceLabel"
      :driver="driver"
    />
    <template v-if="!running">
      <p class="muted tiny">
        Backup of “{{ info.namespace || namespace }}”<template v-if="info.server"> from {{ info.server }}</template>,
        made {{ created }}.
      </p>
      <p v-if="renamed" class="muted tiny">
        It was made from “{{ info.namespace }}” and will be restored into “{{ namespace }}”.
      </p>
      <div class="transfer-warning" role="alert">
        <strong>Everything in “{{ namespace }}” will be deleted</strong>
        <p>
          Its tables, views, and routines are replaced with the ones in this backup. {{ loss }}
          {{ partial }}
        </p>
        <template v-if="info.skipped.length">
          <p>
            These tables and routines are not in this backup. Any with these names in “{{ namespace }}” will be deleted:
          </p>
          <ul>
            <li v-for="item in info.skipped" :key="item">{{ item }}</li>
          </ul>
        </template>
      </div>
      <label class="checkbox-row">
        <input v-model="understood" type="checkbox" :disabled="Boolean(resumePoint)" />
        I understand everything in “{{ namespace }}” will be deleted
      </label>
    </template>
    <div v-if="running" class="transfer-progress" aria-live="polite">
      <ImportWorkDashboard :progress="dashboard" />
      <p v-if="statusLabel" class="muted tiny">{{ statusLabel }}</p>
      <p v-if="progress?.phase === 'reconnecting' && progress.error" class="muted tiny transfer-error">
        {{ progress.error }}
      </p>
    </div>
    <p v-if="error" class="settings-error transfer-error">{{ error }}</p>
    <p v-if="resumePoint && !running" class="muted tiny">{{ resumeChoice(resumePoint) }}</p>
    <template #actions>
      <button v-if="running" class="ghost transfer-hide" type="button" @click="job.hide">Run in background</button>
      <button class="ghost" type="button" :disabled="cancelling || !canCancel" @click="cancel">
        {{ error && !running ? "Close" : "Cancel" }}
      </button>
      <template v-if="resumePoint && !running">
        <template v-if="uncertain">
          <button class="ghost" type="button" @click="resume(resumePoint.from)">Run {{ uncertainWord }} again</button>
          <button :class="resumePoint.likelySaved ? 'primary' : 'ghost'" type="button" @click="resume(resumePoint.to)">
            Skip {{ uncertainWord }}
          </button>
        </template>
        <button v-else class="primary" type="button" @click="resume(resumePoint.from)">Resume</button>
      </template>
      <button v-else-if="!error" class="danger" type="button" :disabled="running || !understood" @click="start()">
        <span v-if="running" class="spinner" aria-hidden="true" />
        {{ running ? "Restoring…" : "Erase and restore" }}
      </button>
    </template>
  </Modal>
</template>
