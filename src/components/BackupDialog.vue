<script setup lang="ts">
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { save } from "@tauri-apps/plugin-dialog";
import { computed, onUnmounted, ref } from "vue";
import * as api from "../api";
import { useApp } from "../composables/useApp";
import { useTransferJob } from "../composables/useTransfers";
import { backupLimits, fileSafe, formatBytes } from "../transfer";
import type { Driver, ExportProgress, ExportResult } from "../types";
import Modal from "./Modal.vue";

const props = defineProps<{
  connectionId: string;
  driver: Driver;
  namespace: string;
  namespaceLabel: string;
  tableCount: number;
}>();

const emit = defineEmits<{
  close: [];
}>();

const { showToast } = useApp();
const job = useTransferJob(() => props.connectionId, "backup");

const running = ref(false);
const cancelling = ref(false);
const error = ref("");
const progress = ref<ExportProgress | null>(null);
/** Set when the backup finished but had to leave tables out, so the dialog stays open to say so. */
const incomplete = ref<ExportResult | null>(null);
let transferId = "";
let stopProgress: UnlistenFn | null = null;

const label = computed(() => props.namespaceLabel.toLowerCase());
const summary = computed(() => {
  const count = props.tableCount.toLocaleString();
  return `Everything in “${props.namespace}” (${count} ${props.tableCount === 1 ? "table" : "tables"}): structure, data, routines, and triggers, gzipped.`;
});
const limits = computed(() => backupLimits(props.driver));
const percent = computed(() => {
  const current = progress.value;
  if (!current?.total) {
    return 0;
  }
  return Math.round(((current.index - 1) / current.total) * 100);
});

function timestamp() {
  const now = new Date();
  const pad = (value: number) => String(value).padStart(2, "0");
  return `${now.getFullYear()}${pad(now.getMonth() + 1)}${pad(now.getDate())}-${pad(now.getHours())}${pad(now.getMinutes())}`;
}

function close() {
  if (running.value) {
    job.hide();
  } else {
    emit("close");
  }
}

function describe(result: ExportResult) {
  const file = result.path.split("/").pop() ?? result.path;
  const tables = `${result.tables.toLocaleString()} ${result.tables === 1 ? "table" : "tables"}`;
  const rows = `${result.rows.toLocaleString()} ${result.rows === 1 ? "row" : "rows"}`;
  return `Backed up ${tables} (${rows}, ${formatBytes(result.bytes)}) to ${file}`;
}

async function start() {
  if (running.value) {
    return;
  }
  const path = await save({
    title: `Back up “${props.namespace}”`,
    defaultPath: `${fileSafe(props.namespace || "main")}-backup-${timestamp()}.sql.gz`,
    filters: [{ name: "Gzipped SQL", extensions: ["gz"] }],
  });
  if (!path) {
    return;
  }
  running.value = true;
  error.value = "";
  progress.value = null;
  transferId = job.begin(`Backing up “${props.namespace}”`);
  stopProgress = await listen<ExportProgress>("export-progress", (event) => {
    if (event.payload.transferId === transferId) {
      progress.value = event.payload;
      job.progress(percent.value);
    }
  });
  try {
    const result = await api.backupDatabase(props.connectionId, transferId, props.namespace, path);
    if (result.skipped.length) {
      incomplete.value = result;
      job.stopped();
      if (job.hidden.value) {
        showToast(`Backup of “${props.namespace}” is incomplete. Open it from its tab to see what was left out.`, "error");
      }
    } else {
      showToast(describe(result));
      emit("close");
    }
  } catch (err) {
    if (cancelling.value) {
      showToast("Backup cancelled");
      emit("close");
    } else {
      error.value = String(err);
      job.stopped();
      if (job.hidden.value) {
        showToast(`Backup of “${props.namespace}” stopped: ${error.value}`, "error");
      }
    }
  } finally {
    running.value = false;
    cancelling.value = false;
    stopProgress?.();
    stopProgress = null;
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
  <Modal v-if="!job.hidden.value" :title="`Back up ${label} “${namespace}”`" @close="close">
    <template v-if="incomplete">
      <p class="muted tiny transfer-summary">{{ describe(incomplete) }}.</p>
      <div class="transfer-warning" role="alert">
        <strong>This backup is incomplete.</strong>
        <p>Recon can't back up these yet, so they aren't in the file:</p>
        <ul>
          <li v-for="item in incomplete.skipped" :key="item">{{ item }}</li>
        </ul>
        <p>Restoring this backup will delete them from the {{ label }} it's restored into.</p>
      </div>
    </template>
    <template v-else>
      <p class="muted tiny transfer-summary">{{ summary }}</p>
      <p class="muted tiny">Use Restore to replace everything in a {{ label }} with this backup.</p>
      <p v-if="limits" class="muted tiny">{{ limits }}</p>
      <div v-if="running" class="transfer-progress" aria-live="polite">
        <div class="transfer-bar"><span :style="{ width: `${percent}%` }" /></div>
        <p class="muted tiny transfer-status">
          <span class="transfer-status-label">
            {{ cancelling ? "Cancelling…" : progress ? `Backing up ${progress.table}…` : "Starting…" }}
          </span>
          <span v-if="progress">
            {{ progress.rows.toLocaleString() }} rows · {{ progress.index }} of {{ progress.total }}
          </span>
        </p>
      </div>
      <p v-if="error" class="settings-error transfer-error">{{ error }}</p>
    </template>
    <template #actions>
      <button v-if="incomplete" class="primary" type="button" @click="emit('close')">Done</button>
      <template v-else>
        <button v-if="running" class="ghost transfer-hide" type="button" @click="job.hide">Run in background</button>
        <button class="ghost" type="button" :disabled="cancelling" @click="cancel">Cancel</button>
        <button class="primary" type="button" :disabled="running" @click="start">
          <span v-if="running" class="spinner" aria-hidden="true" />
          {{ running ? "Backing up…" : "Back up…" }}
        </button>
      </template>
    </template>
  </Modal>
</template>
