<script setup lang="ts">
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { computed, onUnmounted, ref } from "vue";
import * as api from "../api";
import { useApp } from "../composables/useApp";
import { useTransferJob } from "../composables/useTransfers";
import { sendSystemNotification, windowFocused } from "../notifications";
import { baseName } from "../platform";
import { formatDuration, resumeAt, resumeChoice } from "../transfer";
import type { ImportProgress, ImportResume, ImportResumePoint } from "../types";
import ImportWorkDashboard from "./ImportWorkDashboard.vue";
import Modal from "./Modal.vue";

const props = defineProps<{
  connectionId: string;
  namespace: string;
  namespaceLabel: string;
  path: string;
}>();

const emit = defineEmits<{
  close: [];
  imported: [];
}>();

const { showToast, multithreadedImport, notifications } = useApp();
const job = useTransferJob(() => props.connectionId, "import");

const running = ref(false);
const cancelling = ref(false);
const error = ref("");
const resumePoint = ref<ImportResumePoint | null>(null);
const progress = ref<ImportProgress | null>(null);
let transferId = "";
let stopProgress: UnlistenFn | null = null;

const fileName = computed(() => baseName(props.path));
const percent = computed(() => {
  const current = progress.value;
  if (!current) {
    return 0;
  }
  const done = current.phase === "planning" ? current.fileBytes : current.statements;
  const total = current.phase === "planning" ? current.fileTotalBytes : current.totalStatements;
  return total ? Math.min(100, Math.round((done / total) * 100)) : 0;
});
const uncertain = computed(() => Boolean(resumePoint.value && resumePoint.value.to > resumePoint.value.from));
const uncertainWord = computed(() => (resumePoint.value && resumePoint.value.to - resumePoint.value.from > 1 ? "them" : "it"));
const continuation = computed(() =>
  multithreadedImport.value
    ? "Multithreaded import is on, so this import uses multiple threads. If the connection drops, Recon reconnects and picks up where it left off. Change it in Settings → General."
    : "Multithreaded import is off, so if this import stops, start over rather than resume. Change it in Settings → General.",
);

function close() {
  if (running.value) {
    job.hide();
  } else {
    emit("close");
  }
}

async function start(resume: ImportResume | null = null) {
  if (running.value) {
    return;
  }
  running.value = true;
  error.value = "";
  resumePoint.value = null;
  progress.value = null;
  transferId = job.begin(`Importing ${fileName.value} into “${props.namespace}”`);
  stopProgress = await listen<ImportProgress>("import-progress", (event) => {
    if (event.payload.transferId === transferId) {
      progress.value = event.payload;
      job.progress(percent.value);
    }
  });
  try {
    const result = await api.importSql(props.connectionId, transferId, props.namespace, props.path, resume);
    const statements = `${result.statements.toLocaleString()} ${result.statements === 1 ? "statement" : "statements"}`;
    const duration = formatDuration(result.durationMs);
    const summary = `Imported ${fileName.value} into “${props.namespace}” in ${duration} (${statements})`;
    showToast(summary);
    if (windowFocused.value && notifications.value === "background") {
      void sendSystemNotification(summary, "success", "Import Complete");
    }
    emit("imported");
    emit("close");
  } catch (err) {
    emit("imported");
    const failure = api.importFailure(err);
    error.value = failure.message;
    resumePoint.value = failure.resume;
    job.stopped();
    if (job.hidden.value) {
      const hint = multithreadedImport.value
        ? failure.resume
          ? " Open the import to resume it."
          : ""
        : " Start over rather than resume.";
      showToast(`Import into “${props.namespace}” stopped: ${error.value}${hint}`, "error");
    }
  } finally {
    running.value = false;
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
  cancelling.value = true;
  void api.cancelTransfer(transferId);
}

onUnmounted(() => stopProgress?.());
</script>

<template>
  <Modal v-if="!job.hidden.value" :title="`Import into “${namespace}”`" :xwide="running" @close="close">
    <p class="transfer-file" :title="path">{{ fileName }}</p>
    <p v-if="!running && !error && !resumePoint" class="muted tiny">
      Runs every statement in the file against the {{ namespaceLabel.toLowerCase() }} “{{ namespace }}”, and
      stops at the first error. Anything that ran before an error stays applied.
      {{ continuation }}
    </p>
    <div v-if="running" class="transfer-progress">
      <ImportWorkDashboard :progress="progress" />
      <p v-if="progress?.phase === 'reconnecting' && progress.error" class="muted tiny transfer-error" aria-live="polite">
        {{ progress.error }}
      </p>
    </div>
    <p v-if="error" class="settings-error transfer-error">{{ error }}</p>
    <p v-if="resumePoint && !running && multithreadedImport" class="muted tiny">{{ resumeChoice(resumePoint) }}</p>
    <template #actions>
      <button v-if="running" class="ghost transfer-hide" type="button" @click="job.hide">Run in background</button>
      <button class="ghost" type="button" :disabled="cancelling" @click="cancel">
        {{ error && !running ? "Close" : "Cancel" }}
      </button>
      <template v-if="resumePoint && !running && multithreadedImport">
        <template v-if="uncertain">
          <button class="ghost" type="button" @click="resume(resumePoint.from)">Run {{ uncertainWord }} again</button>
          <button :class="resumePoint.likelySaved ? 'primary' : 'ghost'" type="button" @click="resume(resumePoint.to)">
            Skip {{ uncertainWord }}
          </button>
        </template>
        <button v-else class="primary" type="button" @click="resume(resumePoint.from)">Resume</button>
      </template>
      <button v-else-if="!error" class="primary" type="button" :disabled="running" @click="start()">
        <span v-if="running" class="spinner" aria-hidden="true" />
        {{ running ? "Importing…" : "Import" }}
      </button>
    </template>
  </Modal>
</template>
