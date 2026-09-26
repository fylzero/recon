<script setup lang="ts">
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { save } from "@tauri-apps/plugin-dialog";
import { computed, onUnmounted, ref, watch } from "vue";
import * as api from "../api";
import { useApp } from "../composables/useApp";
import type { ExportProgress } from "../types";
import Modal from "./Modal.vue";

const props = defineProps<{
  connectionId: string;
  namespace: string;
  namespaceLabel: string;
  /** The whole namespace when null. */
  tables: string[] | null;
  tableCount: number;
}>();

const emit = defineEmits<{
  close: [];
}>();

const { showToast } = useApp();

type ExportOptions = { structure: boolean; data: boolean; dropTables: boolean; gzip: boolean };

const OPTIONS_KEY = "recon.exportOptions";
const DEFAULT_OPTIONS: ExportOptions = { structure: true, data: true, dropTables: true, gzip: true };

function loadOptions(): ExportOptions {
  try {
    const saved = JSON.parse(localStorage.getItem(OPTIONS_KEY) ?? "{}");
    const options = { ...DEFAULT_OPTIONS };
    for (const key of Object.keys(options) as (keyof ExportOptions)[]) {
      if (typeof saved?.[key] === "boolean") {
        options[key] = saved[key];
      }
    }
    return options;
  } catch {
    return { ...DEFAULT_OPTIONS };
  }
}

const options = ref(loadOptions());
const running = ref(false);
const cancelling = ref(false);
const error = ref("");
const progress = ref<ExportProgress | null>(null);
let transferId = "";
let stopProgress: UnlistenFn | null = null;

watch(options, (value) => localStorage.setItem(OPTIONS_KEY, JSON.stringify(value)), { deep: true });

const label = computed(() => props.namespaceLabel.toLowerCase());
const title = computed(() => {
  if (props.tables === null) {
    return `Export ${label.value} “${props.namespace}”`;
  }
  return props.tables.length === 1 ? `Export “${props.tables[0]}”` : `Export ${props.tables.length} tables`;
});
const summary = computed(() => {
  if (props.tables === null) {
    const count = props.tableCount.toLocaleString();
    return `Every table and view in “${props.namespace}” (${count}).`;
  }
  if (props.tables.length === 1) {
    return `The table “${props.tables[0]}” from “${props.namespace}”.`;
  }
  const names = props.tables.slice(0, 4).join(", ");
  const more = props.tables.length > 4 ? ` and ${props.tables.length - 4} more` : "";
  return `${names}${more} from “${props.namespace}”.`;
});
const canExport = computed(() => options.value.structure || options.value.data);
const percent = computed(() => {
  const current = progress.value;
  if (!current?.total) {
    return 0;
  }
  return Math.round(((current.index - 1) / current.total) * 100);
});

function fileSafe(name: string) {
  return name.replace(/[/\\:*?"<>|]+/g, "-");
}

function defaultName() {
  if (props.tables === null) {
    return props.namespace;
  }
  return props.tables.length === 1 ? props.tables[0] : `${props.namespace}-${props.tables.length}-tables`;
}

function formatBytes(bytes: number) {
  const units = ["B", "KB", "MB", "GB"];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${unit ? value.toFixed(1) : value} ${units[unit]}`;
}

function close() {
  if (!running.value) {
    emit("close");
  }
}

async function start() {
  if (!canExport.value || running.value) {
    return;
  }
  const extension = options.value.gzip ? "sql.gz" : "sql";
  const path = await save({
    title: title.value,
    defaultPath: `${fileSafe(defaultName())}.${extension}`,
    filters: [
      options.value.gzip ? { name: "Gzipped SQL", extensions: ["gz"] } : { name: "SQL", extensions: ["sql"] },
    ],
  });
  if (!path) {
    return;
  }
  running.value = true;
  error.value = "";
  progress.value = null;
  transferId = crypto.randomUUID();
  stopProgress = await listen<ExportProgress>("export-progress", (event) => {
    if (event.payload.transferId === transferId) {
      progress.value = event.payload;
    }
  });
  try {
    const result = await api.exportSql(props.connectionId, transferId, {
      namespace: props.namespace,
      tables: props.tables,
      path,
      ...options.value,
    });
    const file = result.path.split("/").pop() ?? result.path;
    const tables = `${result.tables.toLocaleString()} ${result.tables === 1 ? "table" : "tables"}`;
    const rows = `${result.rows.toLocaleString()} ${result.rows === 1 ? "row" : "rows"}`;
    const skipped = result.skipped.length ? `. Skipped ${result.skipped.join(", ")}` : "";
    showToast(`Exported ${tables} (${rows}, ${formatBytes(result.bytes)}) to ${file}${skipped}`);
    emit("close");
  } catch (err) {
    if (cancelling.value) {
      showToast("Export cancelled");
      emit("close");
    } else {
      error.value = String(err);
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
  <Modal :title="title" @close="close">
    <p class="muted tiny transfer-summary">{{ summary }}</p>
    <div class="transfer-options">
      <label class="checkbox-row">
        <input v-model="options.structure" type="checkbox" :disabled="running" />
        Structure
      </label>
      <label class="checkbox-row">
        <input v-model="options.data" type="checkbox" :disabled="running" />
        Data
      </label>
      <label class="checkbox-row">
        <input v-model="options.dropTables" type="checkbox" :disabled="running || !options.structure" />
        Drop existing tables first
      </label>
      <label class="checkbox-row">
        <input v-model="options.gzip" type="checkbox" :disabled="running" />
        Compress with gzip (.sql.gz)
      </label>
    </div>
    <div v-if="running" class="transfer-progress" aria-live="polite">
      <div class="transfer-bar"><span :style="{ width: `${percent}%` }" /></div>
      <p class="muted tiny transfer-status">
        <span class="transfer-status-label">
          {{ cancelling ? "Cancelling…" : progress ? `Exporting ${progress.table}…` : "Starting…" }}
        </span>
        <span v-if="progress">
          {{ progress.rows.toLocaleString() }} rows · {{ progress.index }} of {{ progress.total }}
        </span>
      </p>
    </div>
    <p v-if="error" class="settings-error transfer-error">{{ error }}</p>
    <template #actions>
      <button class="ghost" type="button" :disabled="cancelling" @click="cancel">Cancel</button>
      <button class="primary" type="button" :disabled="!canExport || running" @click="start">
        <span v-if="running" class="spinner" aria-hidden="true" />
        {{ running ? "Exporting…" : "Export…" }}
      </button>
    </template>
  </Modal>
</template>
