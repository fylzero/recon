<script setup lang="ts">
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { computed, onUnmounted, ref } from "vue";
import * as api from "../api";
import { useApp } from "../composables/useApp";
import { useTransferJob } from "../composables/useTransfers";
import { sendSystemNotification, windowFocused } from "../notifications";
import { baseName } from "../platform";
import { formatDuration, resumeAt, resumeChoice } from "../transfer";
import type { BackupInfo, Driver, ImportProgress, ImportResume, ImportResumePoint } from "../types";
import ImportWorkDashboard from "./ImportWorkDashboard.vue";
import Modal from "./Modal.vue";

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

const { showToast, multithreadedImport, notifications } = useApp();
const job = useTransferJob(() => props.connectionId, "restore");

const understood = ref(false);
const running = ref(false);
const cancelling = ref(false);
const error = ref("");
const resumePoint = ref<ImportResumePoint | null>(null);
/** Resuming skips clearing, so the status starts at replaying rather than "Clearing…". */
const resuming = ref(false);
const progress = ref<ImportProgress | null>(null);
let transferId = "";
let stopProgress: UnlistenFn | null = null;

const label = computed(() => props.namespaceLabel.toLowerCase());
const atomic = computed(() => props.driver === "postgres");
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
  return "";
});
const partial = computed(() => {
  const change = "Change it in Settings → General.";
  if (atomic.value) {
    return `This restore is a single transaction. If it fails or is cancelled, nothing changes. Multithreaded import applies to other imports and to a MySQL restore. ${change}`;
  }
  const left = `If the restore fails or is cancelled partway, “${props.namespace}” will be left partly restored.`;
  if (props.driver === "mysql") {
    return multithreadedImport.value
      ? `${left} Multithreaded import is on, so this restore uses multiple threads. If the connection drops, Recon reconnects and picks up where it left off. ${change}`
      : `${left} Multithreaded import is off, so if this restore stops, start over rather than resume. ${change}`;
  }
  return multithreadedImport.value
    ? `${left} If the connection drops, Recon reconnects and picks up where it left off. Multithreaded import applies to other imports and to a MySQL restore. ${change}`
    : `${left} Multithreaded import is off, so if this restore stops, start over rather than resume. ${change}`;
});
/** Postgres restore is one transaction, so it never resumes. Other restores resume only while Multithreaded import is on. */
const canResume = computed(() => !atomic.value && multithreadedImport.value);
/** Clearing can't be stopped halfway outside Postgres, so cancelling waits until the replay starts. */
const canCancel = computed(() => !running.value || atomic.value || resuming.value || progress.value !== null);
const percent = computed(() => {
  const current = progress.value;
  if (!current) {
    return 0;
  }
  const planning = current.phase === "planning" || !current.totalStatements;
  const done = planning ? current.fileBytes : current.statements;
  const total = planning ? current.fileTotalBytes : current.totalStatements;
  return total ? Math.min(100, Math.round((done / total) * 100)) : 0;
});
const uncertain = computed(() => Boolean(resumePoint.value && resumePoint.value.to > resumePoint.value.from));
const uncertainWord = computed(() => (resumePoint.value && resumePoint.value.to - resumePoint.value.from > 1 ? "them" : "it"));

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
  transferId = job.begin(`Restoring “${props.namespace}” from ${fileName.value}`);
  stopProgress = await listen<ImportProgress>("import-progress", (event) => {
    if (event.payload.transferId === transferId) {
      progress.value = event.payload;
      job.progress(percent.value);
    }
  });
  try {
    const result = await api.restoreDatabase(props.connectionId, transferId, props.namespace, props.path, resume);
    const statements = `${result.statements.toLocaleString()} ${result.statements === 1 ? "statement" : "statements"}`;
    const duration = formatDuration(result.durationMs);
    const summary = `Restored “${props.namespace}” from ${fileName.value} in ${duration} (${statements})`;
    showToast(summary);
    if (windowFocused.value && notifications.value === "background") {
      void sendSystemNotification(summary, "success", "Restore Complete");
    }
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
        const hint = atomic.value
          ? " Nothing was changed."
          : multithreadedImport.value
            ? failure.resume
              ? " Open the restore to resume it."
              : ""
            : " Start over rather than resume.";
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
  <Modal v-if="!job.hidden.value" :title="`Restore ${label} “${namespace}”`" :xwide="running" @close="close">
    <p class="transfer-file" :title="path">{{ fileName }}</p>
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
      <input v-model="understood" type="checkbox" :disabled="running || Boolean(resumePoint)" />
      I understand everything in “{{ namespace }}” will be deleted
    </label>
    <div v-if="running" class="transfer-progress">
      <ImportWorkDashboard :progress="progress" />
      <p v-if="progress?.phase === 'reconnecting' && progress.error" class="muted tiny transfer-error" aria-live="polite">
        {{ progress.error }}
      </p>
    </div>
    <p v-if="error" class="settings-error transfer-error">{{ error }}</p>
    <p v-if="resumePoint && !running && canResume" class="muted tiny">{{ resumeChoice(resumePoint) }}</p>
    <template #actions>
      <button v-if="running" class="ghost transfer-hide" type="button" @click="job.hide">Run in background</button>
      <button class="ghost" type="button" :disabled="cancelling || !canCancel" @click="cancel">
        {{ error && !running ? "Close" : "Cancel" }}
      </button>
      <template v-if="resumePoint && !running && canResume">
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
