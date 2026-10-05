<script setup lang="ts">
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { computed, onUnmounted, ref, watch } from "vue";
import * as api from "../api";
import { useApp } from "../composables/useApp";
import { useTransferJob } from "../composables/useTransfers";
import { chooseTabularPath, formatBytes, loadTabularOptions, saveTabularOptions, tabularExtension } from "../transfer";
import type { BrowseRequest, ExportProgress, TabularFormat, TabularOptions as TabularChoices } from "../types";
import Modal from "./Modal.vue";
import TabularOptions from "./TabularOptions.vue";

export type ExportRowsSource =
  | {
      kind: "browse";
      request: BrowseRequest;
      /** Rows matching the filters, when known. */
      total: number | null;
      pageRows: number;
      filtered: boolean;
    }
  | {
      kind: "result";
      resultId: string;
      columns: string[];
      rowCount: number;
      /** The query returned more rows than the query row limit. */
      truncated: boolean;
    };

const props = defineProps<{
  connectionId: string;
  title: string;
  /** The file name to suggest, without an extension. */
  name: string;
  source: ExportRowsSource;
}>();

const emit = defineEmits<{
  close: [];
}>();

const { showToast } = useApp();
const job = useTransferJob(() => props.connectionId, "export");

const options = ref<TabularChoices>(loadTabularOptions());
const pageOnly = ref(false);
const running = ref(false);
const cancelling = ref(false);
const error = ref("");
const progress = ref<ExportProgress | null>(null);
let transferId = "";
let stopProgress: UnlistenFn | null = null;

watch(options, (value) => saveTabularOptions(value));

function setFormat(format: TabularFormat) {
  options.value = { ...options.value, format };
}

function rowsLabel(count: number) {
  return `${count.toLocaleString()} ${count === 1 ? "row" : "rows"}`;
}

const allLabel = computed(() => {
  if (props.source.kind !== "browse") {
    return "";
  }
  const which = props.source.filtered ? "All matching rows" : "All rows";
  return props.source.total === null ? which : `${which} (${props.source.total.toLocaleString()})`;
});
/** Without a total, progress can't be a fraction, so the bar stays empty and only the count moves. */
const expected = computed(() => {
  if (props.source.kind === "result") {
    return props.source.rowCount;
  }
  return pageOnly.value ? props.source.pageRows : props.source.total;
});
const percent = computed(() => {
  const total = expected.value;
  return total && progress.value ? Math.min(100, Math.round((progress.value.rows / total) * 100)) : 0;
});

function close() {
  if (running.value) {
    job.hide();
  } else {
    emit("close");
  }
}

async function start() {
  if (running.value) {
    return;
  }
  const path = await chooseTabularPath(props.title, props.name, options.value);
  if (!path) {
    return;
  }
  running.value = true;
  error.value = "";
  progress.value = null;
  transferId = job.begin(props.title.replace(/^Export/, "Exporting"));
  stopProgress = await listen<ExportProgress>("export-progress", (event) => {
    if (event.payload.transferId === transferId) {
      progress.value = event.payload;
      job.progress(percent.value);
    }
  });
  try {
    const source = props.source;
    const result =
      source.kind === "browse"
        ? await api.exportBrowse(props.connectionId, transferId, source.request, pageOnly.value, options.value, path)
        : await api.exportResult(source.resultId, source.columns, options.value, path);
    const file = result.path.split("/").pop() ?? result.path;
    showToast(`Exported ${rowsLabel(result.rows)} (${formatBytes(result.bytes)}) to ${file}`);
    emit("close");
  } catch (err) {
    if (cancelling.value) {
      showToast("Export cancelled");
      emit("close");
    } else {
      error.value = String(err);
      job.stopped();
      if (job.hidden.value) {
        showToast(`Export stopped: ${error.value}`, "error");
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
    <div class="segmented transfer-format" role="group" aria-label="Format">
      <button
        v-for="item in (['csv', 'json'] as const)"
        :key="item"
        type="button"
        :class="{ active: options.format === item }"
        :aria-pressed="options.format === item"
        :disabled="running"
        @click="setFormat(item)"
      >
        {{ item.toUpperCase() }}
      </button>
    </div>
    <div v-if="source.kind === 'browse'" class="transfer-options">
      <label class="checkbox-row">
        <input v-model="pageOnly" type="radio" :value="false" :disabled="running" />
        {{ allLabel }}{{ source.request.orderBy ? ", in the current sort order" : "" }}
      </label>
      <label class="checkbox-row">
        <input v-model="pageOnly" type="radio" :value="true" :disabled="running" />
        Only this page ({{ rowsLabel(source.pageRows) }})
      </label>
    </div>
    <p v-else class="muted tiny transfer-summary">
      The {{ rowsLabel(source.rowCount) }} in this result.
      <span v-if="source.truncated" class="warn-text">
        The query returned more, but results stop at the query row limit. Raise it in Settings → General to export more.
      </span>
    </p>
    <TabularOptions v-model="options" :disabled="running" />
    <label class="checkbox-row">
      <input v-model="options.gzip" type="checkbox" :disabled="running" />
      Compress with gzip (.{{ tabularExtension({ ...options, gzip: true }) }})
    </label>
    <div v-if="running" class="transfer-progress" aria-live="polite">
      <div class="transfer-bar"><span :style="{ width: `${percent}%` }" /></div>
      <p class="muted tiny transfer-status">
        <span class="transfer-status-label">{{ cancelling ? "Cancelling…" : "Exporting…" }}</span>
        <span v-if="progress">{{ rowsLabel(progress.rows) }}</span>
      </p>
    </div>
    <p v-if="error" class="settings-error transfer-error">{{ error }}</p>
    <template #actions>
      <button v-if="running && source.kind === 'browse'" class="ghost transfer-hide" type="button" @click="job.hide">
        Run in background
      </button>
      <button class="ghost" type="button" :disabled="cancelling || (running && source.kind === 'result')" @click="cancel">
        Cancel
      </button>
      <button class="primary" type="button" :disabled="running" @click="start">
        <span v-if="running" class="spinner" aria-hidden="true" />
        {{ running ? "Exporting…" : "Export…" }}
      </button>
    </template>
  </Modal>
</template>
