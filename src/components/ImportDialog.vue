<script setup lang="ts">
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { computed, onUnmounted, ref } from "vue";
import * as api from "../api";
import { useApp } from "../composables/useApp";
import type { ImportProgress } from "../types";
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

const { showToast } = useApp();

const running = ref(false);
const cancelling = ref(false);
const error = ref("");
const progress = ref<ImportProgress | null>(null);
let transferId = "";
let stopProgress: UnlistenFn | null = null;

const fileName = computed(() => props.path.split("/").pop() ?? props.path);
const percent = computed(() => {
  const current = progress.value;
  return current?.totalBytes ? Math.min(100, Math.round((current.bytes / current.totalBytes) * 100)) : 0;
});

function close() {
  if (!running.value) {
    emit("close");
  }
}

async function start() {
  if (running.value) {
    return;
  }
  running.value = true;
  error.value = "";
  progress.value = null;
  transferId = crypto.randomUUID();
  stopProgress = await listen<ImportProgress>("import-progress", (event) => {
    if (event.payload.transferId === transferId) {
      progress.value = event.payload;
    }
  });
  try {
    const result = await api.importSql(props.connectionId, transferId, props.namespace, props.path);
    const statements = `${result.statements.toLocaleString()} ${result.statements === 1 ? "statement" : "statements"}`;
    showToast(`Imported ${fileName.value} (${statements} in ${(result.durationMs / 1000).toFixed(1)}s)`);
    emit("imported");
    emit("close");
  } catch (err) {
    emit("imported");
    error.value = String(err);
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
  <Modal :title="`Import into “${namespace}”`" @close="close">
    <p class="transfer-file" :title="path">{{ fileName }}</p>
    <p class="muted tiny">
      Runs every statement in the file against the {{ namespaceLabel.toLowerCase() }} “{{ namespace }}”, in
      order, and stops at the first error. Anything that ran before an error stays applied.
    </p>
    <div v-if="running" class="transfer-progress" aria-live="polite">
      <div class="transfer-bar"><span :style="{ width: `${percent}%` }" /></div>
      <p class="muted tiny transfer-status">
        <span class="transfer-status-label">{{ cancelling ? "Cancelling…" : "Importing…" }}</span>
        <span v-if="progress">{{ progress.statements.toLocaleString() }} statements · {{ percent }}%</span>
      </p>
    </div>
    <p v-if="error" class="settings-error transfer-error">{{ error }}</p>
    <template #actions>
      <button class="ghost" type="button" :disabled="cancelling" @click="cancel">
        {{ error && !running ? "Close" : "Cancel" }}
      </button>
      <button v-if="!error" class="primary" type="button" :disabled="running" @click="start">
        <span v-if="running" class="spinner" aria-hidden="true" />
        {{ running ? "Importing…" : "Import" }}
      </button>
    </template>
  </Modal>
</template>
