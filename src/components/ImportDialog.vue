<script setup lang="ts">
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { computed, onUnmounted, ref, watch } from "vue";
import * as api from "../api";
import { useApp } from "../composables/useApp";
import { useTransferJob } from "../composables/useTransfers";
import { sourceName, sourcePath } from "../s3";
import { formatDuration, importStages, resumeAt, resumeChoice, runPhase, stagesPercent } from "../transfer";
import type { Driver, ImportProgress, ImportResume, ImportResumePoint, TransferSource } from "../types";
import ImportWorkDashboard from "./ImportWorkDashboard.vue";
import Modal from "./Modal.vue";
import TransferRoute from "./TransferRoute.vue";

const props = defineProps<{
  connectionId: string;
  driver: Driver;
  namespace: string;
  namespaceLabel: string;
  source: TransferSource;
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

const fileName = computed(() => sourceName(props.source));
const path = computed(() => sourcePath(props.source));
const fileLabel = computed(() => {
  const kind = /\.gz$/i.test(fileName.value) ? "Gzipped SQL file" : "SQL file";
  return props.source.kind === "s3" ? `${kind} in S3` : kind;
});
const canThread = computed(() => props.driver === "mysql" || props.driver === "postgres");
/** For this import only. It starts from the setting in General. */
const multithreaded = ref(multithreadedImport.value);
/** Off every time, since it can't be undone. */
const dropTables = ref(false);
/** How many tables the drop would remove, or null until counted. */
const tableCount = ref<number | null>(null);
const dropWarning = computed(() => {
  const target = `“${props.namespace}”`;
  if (tableCount.value === null) {
    return `Every table in ${target} will be dropped before importing`;
  }
  if (tableCount.value === 0) {
    return `${target} has no tables to drop`;
  }
  const tables = tableCount.value === 1 ? "the 1 table" : `all ${tableCount.value.toLocaleString()} tables`;
  return `Drops ${tables} in ${target} before importing`;
});

watch(dropTables, async (on) => {
  if (!on || tableCount.value !== null) {
    return;
  }
  try {
    const tables = await api.listTables(props.connectionId, props.namespace);
    tableCount.value = tables.filter((table) => table.kind === "table").length;
  } catch {
    tableCount.value = null;
  }
});
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
    const result = await api.importSql(
      props.connectionId,
      transferId,
      props.namespace,
      props.source,
      resume,
      multithreaded.value,
      dropTables.value && !resume,
    );
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
        :file-label="fileLabel"
        :target="namespace"
        :target-label="namespaceLabel"
        :driver="driver"
      />
    </template>
    <TransferRoute
      v-if="!running"
      :file="fileName"
      :path="path"
      :file-label="fileLabel"
      :target="namespace"
      :target-label="namespaceLabel"
      :driver="driver"
    />
    <div v-if="!running" class="import-options">
      <label
        v-if="canThread"
        class="checkbox-row"
        title="Loads mysqldump, pg_dump, and Recon files over one connection per processor core. Set the default in General settings."
      >
        <input v-model="multithreaded" type="checkbox" :disabled="Boolean(resumePoint)" />
        Multithreaded import
      </label>
      <label
        class="checkbox-row"
        title="Removes tables the file doesn't create, so the database ends up matching it."
      >
        <input v-model="dropTables" type="checkbox" :disabled="Boolean(resumePoint)" />
        Drop existing tables first
      </label>
    </div>
    <div v-if="dropTables && !running && !resumePoint" class="transfer-warning" role="alert">
      <strong>{{ dropWarning }}</strong>
      <p>Their rows are permanently deleted, even if the import fails. This can't be undone.</p>
      <p v-if="driver === 'postgres'">Views built on them are dropped too (CASCADE). Other views are kept.</p>
      <p v-else>Foreign key checks are skipped while dropping. Views are kept.</p>
    </div>
    <div v-if="running" class="transfer-progress" aria-live="polite">
      <ImportWorkDashboard :progress="dashboard" />
      <p v-if="cancelling || phase" class="muted tiny">{{ cancelling ? "Cancelling…" : phase }}</p>
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
      <button v-else-if="!error" :class="dropTables ? 'danger' : 'primary'" type="button" :disabled="running" @click="start()">
        <span v-if="running" class="spinner" aria-hidden="true" />
        {{ running ? "Importing…" : "Import" }}
      </button>
    </template>
  </Modal>
</template>
