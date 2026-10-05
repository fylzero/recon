<script setup lang="ts">
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { open, save } from "@tauri-apps/plugin-dialog";
import { computed, onUnmounted, ref, watch } from "vue";
import * as api from "../api";
import { useApp } from "../composables/useApp";
import { useTransferJob } from "../composables/useTransfers";
import { fileSafe, formatBytes, loadTabularOptions, saveTabularOptions, tabularExtension, tabularFilter } from "../transfer";
import type { ExportProgress, TabularOptions as TabularChoices } from "../types";
import Modal from "./Modal.vue";
import TabularOptions from "./TabularOptions.vue";

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
const job = useTransferJob(() => props.connectionId, "export");

type ExportOptions = { structure: boolean; data: boolean; dropTables: boolean; gzip: boolean };
type ExportFormat = "sql" | "csv" | "json";

const OPTIONS_KEY = "recon.exportOptions";
const FORMAT_KEY = "recon.exportFormat";
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

function loadFormat(): ExportFormat {
  const saved = localStorage.getItem(FORMAT_KEY);
  return saved === "csv" || saved === "json" ? saved : "sql";
}

const options = ref(loadOptions());
const format = ref<ExportFormat>(loadFormat());
const tabular = ref<TabularChoices>(loadTabularOptions());
const running = ref(false);
const cancelling = ref(false);
const error = ref("");
const progress = ref<ExportProgress | null>(null);
let transferId = "";
let stopProgress: UnlistenFn | null = null;

watch(options, (value) => localStorage.setItem(OPTIONS_KEY, JSON.stringify(value)), { deep: true });
watch(format, (value) => {
  localStorage.setItem(FORMAT_KEY, value);
  if (value !== "sql") {
    tabular.value = { ...tabular.value, format: value };
  }
});
watch(tabular, (value) => saveTabularOptions(value));

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
const isTabular = computed(() => format.value !== "sql");
/** CSV and JSON hold one table each, so several tables go into a folder. */
const toFolder = computed(() => isTabular.value && (props.tables === null || props.tables.length > 1));
const tabularRequest = computed<TabularChoices>(() => ({
  ...tabular.value,
  format: format.value === "json" ? "json" : "csv",
  gzip: options.value.gzip,
}));
const canExport = computed(() => isTabular.value || options.value.structure || options.value.data);
const percent = computed(() => {
  const current = progress.value;
  if (!current?.total) {
    return 0;
  }
  return Math.round(((current.index - 1) / current.total) * 100);
});

function defaultName() {
  if (props.tables === null) {
    return props.namespace;
  }
  return props.tables.length === 1 ? props.tables[0] : `${props.namespace}-${props.tables.length}-tables`;
}

function close() {
  if (running.value) {
    job.hide();
  } else {
    emit("close");
  }
}

async function choosePath() {
  if (toFolder.value) {
    const folder = await open({ title: `${title.value}: choose a folder for one file per table`, directory: true });
    return typeof folder === "string" ? folder : null;
  }
  if (isTabular.value) {
    return save({
      title: title.value,
      defaultPath: `${fileSafe(defaultName())}.${tabularExtension(tabularRequest.value)}`,
      filters: [tabularFilter(tabularRequest.value)],
    });
  }
  const extension = options.value.gzip ? "sql.gz" : "sql";
  return save({
    title: title.value,
    defaultPath: `${fileSafe(defaultName())}.${extension}`,
    filters: [
      options.value.gzip ? { name: "Gzipped SQL", extensions: ["gz"] } : { name: "SQL", extensions: ["sql"] },
    ],
  });
}

async function start() {
  if (!canExport.value || running.value) {
    return;
  }
  const path = await choosePath();
  if (!path) {
    return;
  }
  running.value = true;
  error.value = "";
  progress.value = null;
  transferId = job.begin(title.value.replace(/^Export/, "Exporting"));
  stopProgress = await listen<ExportProgress>("export-progress", (event) => {
    if (event.payload.transferId === transferId) {
      progress.value = event.payload;
      job.progress(percent.value);
    }
  });
  try {
    const result = await api.exportSql(props.connectionId, transferId, {
      namespace: props.namespace,
      tables: props.tables,
      path,
      ...options.value,
      tabular: isTabular.value ? tabularRequest.value : null,
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
      job.stopped();
      if (job.hidden.value) {
        showToast(`Export from “${props.namespace}” stopped: ${error.value}`, "error");
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
  <Modal v-if="!job.hidden.value" :title="title" @close="close">
    <p class="muted tiny transfer-summary">{{ summary }}</p>
    <div class="segmented transfer-format" role="group" aria-label="Format">
      <button
        v-for="item in (['sql', 'csv', 'json'] as const)"
        :key="item"
        type="button"
        :class="{ active: format === item }"
        :aria-pressed="format === item"
        :disabled="running"
        @click="format = item"
      >
        {{ item.toUpperCase() }}
      </button>
    </div>
    <div v-if="!isTabular" class="transfer-options">
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
    <template v-else>
      <p class="muted tiny transfer-summary">
        Only the rows are exported, not the structure.
        <template v-if="toFolder">Each table and view gets its own file in the folder you choose.</template>
      </p>
      <TabularOptions v-model="tabular" :disabled="running" />
      <label class="checkbox-row">
        <input v-model="options.gzip" type="checkbox" :disabled="running" />
        Compress with gzip (.{{ tabularExtension({ ...tabularRequest, gzip: true }) }})
      </label>
    </template>
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
      <button v-if="running" class="ghost transfer-hide" type="button" @click="job.hide">Run in background</button>
      <button class="ghost" type="button" :disabled="cancelling" @click="cancel">Cancel</button>
      <button class="primary" type="button" :disabled="!canExport || running" @click="start">
        <span v-if="running" class="spinner" aria-hidden="true" />
        {{ running ? "Exporting…" : "Export…" }}
      </button>
    </template>
  </Modal>
</template>
