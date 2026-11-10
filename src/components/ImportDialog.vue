<script setup lang="ts">
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { computed, onUnmounted, ref } from "vue";
import * as api from "../api";
import { useApp } from "../composables/useApp";
import { useTransferJob } from "../composables/useTransfers";
import { baseName } from "../platform";
import { formatDuration, importStages, resumeAt, resumeChoice, runPhase, stagesPercent } from "../transfer";
import type { Driver, ImportProgress, ImportResume, ImportResumePoint } from "../types";
import ImportWorkDashboard from "./ImportWorkDashboard.vue";
import Modal from "./Modal.vue";
import TransferRoute from "./TransferRoute.vue";

const props = defineProps<{
  connectionId: string;
  driver: Driver;
  namespace: string;
  namespaceLabel: string;
  path: string;
}>();

const emit = defineEmits<{
  close: [];
  imported: [];
}>();

const { showToast, multithreadedImport } = useApp();
const job = useTransferJob(() => props.connectionId, "import");

const running = ref(false);
const cancelling = ref(false);
const error = ref("");
const resumePoint = ref<ImportResumePoint | null>(null);
const progress = ref<ImportProgress | null>(null);
/** The last progress outside a reconnect, which reports every worker at 0. */
const dashboard = ref<ImportProgress | null>(null);
/** Why the run loads in file order, when it could have loaded in parallel. */
const inOrder = ref("");
let transferId = "";
let stopProgress: UnlistenFn | null = null;

const fileName = computed(() => baseName(props.path));
const canThread = computed(() => props.driver === "mysql" || props.driver === "postgres");
/** For this import only. It starts from the setting in General. */
const multithreaded = ref(multithreadedImport.value);
const percent = computed(() => Math.round(stagesPercent(importStages(dashboard.value))));
const phase = computed(() => runPhase(progress.value));
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
  if (running.value) {
    return;
  }
  running.value = true;
  error.value = "";
  resumePoint.value = null;
  progress.value = null;
  dashboard.value = null;
  inOrder.value = "";
  transferId = job.begin(`Importing ${fileName.value} into “${props.namespace}”`);
  stopProgress = await listen<ImportProgress>("import-progress", (event) => {
    if (event.payload.transferId === transferId) {
      progress.value = event.payload;
      if (event.payload.inOrder) {
        inOrder.value = event.payload.inOrder;
      }
      if (event.payload.phase !== "reconnecting") {
        dashboard.value = event.payload;
      }
      job.progress(percent.value);
    }
  });
  try {
    const result = await api.importSql(props.connectionId, transferId, props.namespace, props.path, resume, multithreaded.value);
    const statements = `${result.statements.toLocaleString()} ${result.statements === 1 ? "statement" : "statements"}`;
    const duration = formatDuration(result.durationMs);
    showToast(`Imported ${fileName.value} into “${props.namespace}” in ${duration} (${statements})`);
    emit("imported");
    emit("close");
  } catch (err) {
    emit("imported");
    const failure = api.importFailure(err);
    error.value = failure.message;
    resumePoint.value = failure.resume;
    job.stopped();
    if (job.hidden.value) {
      const hint = failure.resume ? " Open the import to resume it." : "";
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
  <Modal v-if="!job.hidden.value" title="Import SQL file" :xwide="running" @close="close">
    <template v-if="running" #aside>
      <TransferRoute
        :file="fileName"
        :path="path"
        :file-label="/\.gz$/i.test(fileName) ? 'Gzipped SQL file' : 'SQL file'"
        :target="namespace"
        :target-label="namespaceLabel"
        :driver="driver"
      />
    </template>
    <TransferRoute
      v-if="!running"
      :file="fileName"
      :path="path"
      :file-label="/\.gz$/i.test(fileName) ? 'Gzipped SQL file' : 'SQL file'"
      :target="namespace"
      :target-label="namespaceLabel"
      :driver="driver"
    />
    <label
      v-if="!running && canThread"
      class="checkbox-row"
      title="Loads mysqldump, pg_dump, and Recon files over one connection per processor core. Set the default in General settings."
    >
      <input v-model="multithreaded" type="checkbox" :disabled="Boolean(resumePoint)" />
      Multithreaded import
    </label>
    <div v-if="running" class="transfer-progress" aria-live="polite">
      <ImportWorkDashboard :progress="dashboard" />
      <p class="muted tiny transfer-status">
        <span class="transfer-status-label">{{ cancelling ? "Cancelling…" : (phase ?? "Importing…") }}</span>
        <span v-if="progress">{{ progress.statements.toLocaleString() }} statements · {{ percent }}%</span>
      </p>
      <p v-if="inOrder" class="muted tiny">Running in order on one connection: {{ inOrder }}.</p>
      <p v-if="progress?.phase === 'reconnecting' && progress.error" class="muted tiny transfer-error">
        {{ progress.error }}
      </p>
    </div>
    <p v-if="error" class="settings-error transfer-error">{{ error }}</p>
    <p v-if="resumePoint && !running" class="muted tiny">{{ resumeChoice(resumePoint) }}</p>
    <template #actions>
      <button v-if="running" class="ghost transfer-hide" type="button" @click="job.hide">Run in background</button>
      <button class="ghost" type="button" :disabled="cancelling" @click="cancel">
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
      <button v-else-if="!error" class="primary" type="button" :disabled="running" @click="start()">
        <span v-if="running" class="spinner" aria-hidden="true" />
        {{ running ? "Importing…" : "Import" }}
      </button>
    </template>
  </Modal>
</template>
