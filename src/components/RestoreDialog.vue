<script setup lang="ts">
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { computed, onUnmounted, ref } from "vue";
import * as api from "../api";
import { useApp } from "../composables/useApp";
import { useTransferJob } from "../composables/useTransfers";
import type { BackupInfo, Driver, ImportProgress } from "../types";
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

const { showToast } = useApp();
const job = useTransferJob(() => props.connectionId, "restore");

const understood = ref(false);
const running = ref(false);
const cancelling = ref(false);
const error = ref("");
const progress = ref<ImportProgress | null>(null);
let transferId = "";
let stopProgress: UnlistenFn | null = null;

const label = computed(() => props.namespaceLabel.toLowerCase());
const atomic = computed(() => props.driver === "postgres");
const fileName = computed(() => props.path.split("/").pop() ?? props.path);
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
const partial = computed(() =>
  atomic.value
    ? "If the restore fails or is cancelled, nothing changes."
    : `If the restore fails or is cancelled partway, “${props.namespace}” will be left partly restored.`,
);
/** Clearing can't be stopped halfway outside Postgres, so cancelling waits until the replay starts. */
const canCancel = computed(() => !running.value || atomic.value || progress.value !== null);
const percent = computed(() => {
  const current = progress.value;
  return current?.totalBytes ? Math.min(100, Math.round((current.bytes / current.totalBytes) * 100)) : 0;
});

function close() {
  if (running.value) {
    job.hide();
  } else {
    emit("close");
  }
}

async function start() {
  if (running.value || !understood.value) {
    return;
  }
  running.value = true;
  error.value = "";
  progress.value = null;
  transferId = job.begin(`Restoring “${props.namespace}” from ${fileName.value}`);
  stopProgress = await listen<ImportProgress>("import-progress", (event) => {
    if (event.payload.transferId === transferId) {
      progress.value = event.payload;
      job.progress(percent.value);
    }
  });
  try {
    const result = await api.restoreDatabase(props.connectionId, transferId, props.namespace, props.path);
    const statements = `${result.statements.toLocaleString()} ${result.statements === 1 ? "statement" : "statements"}`;
    showToast(`Restored “${props.namespace}” from ${fileName.value} (${statements} in ${(result.durationMs / 1000).toFixed(1)}s)`);
    emit("restored");
    emit("close");
  } catch (err) {
    emit("restored");
    if (cancelling.value && atomic.value) {
      showToast("Restore cancelled. Nothing was changed.");
      emit("close");
    } else {
      error.value = String(err);
      job.stopped();
      if (job.hidden.value) {
        showToast(`Restore of “${props.namespace}” stopped: ${error.value}`, "error");
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
  if (!canCancel.value) {
    return;
  }
  cancelling.value = true;
  void api.cancelTransfer(transferId);
}

onUnmounted(() => stopProgress?.());
</script>

<template>
  <Modal v-if="!job.hidden.value" :title="`Restore ${label} “${namespace}”`" @close="close">
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
      <input v-model="understood" type="checkbox" :disabled="running" />
      I understand everything in “{{ namespace }}” will be deleted
    </label>
    <div v-if="running" class="transfer-progress" aria-live="polite">
      <div class="transfer-bar"><span :style="{ width: `${percent}%` }" /></div>
      <p class="muted tiny transfer-status">
        <span class="transfer-status-label">
          {{ cancelling ? "Cancelling…" : progress ? "Restoring…" : "Clearing…" }}
        </span>
        <span v-if="progress">{{ progress.statements.toLocaleString() }} statements · {{ percent }}%</span>
      </p>
    </div>
    <p v-if="error" class="settings-error transfer-error">{{ error }}</p>
    <template #actions>
      <button v-if="running" class="ghost transfer-hide" type="button" @click="job.hide">Run in background</button>
      <button class="ghost" type="button" :disabled="cancelling || !canCancel" @click="cancel">
        {{ error && !running ? "Close" : "Cancel" }}
      </button>
      <button v-if="!error" class="danger" type="button" :disabled="running || !understood" @click="start">
        <span v-if="running" class="spinner" aria-hidden="true" />
        {{ running ? "Restoring…" : "Erase and restore" }}
      </button>
    </template>
  </Modal>
</template>
