<script setup lang="ts">
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { computed, onMounted, onUnmounted, ref, watch } from "vue";
import * as api from "../api";
import { useApp } from "../composables/useApp";
import { useTransferJob } from "../composables/useTransfers";
import { formatBytes } from "../transfer";
import type {
  ColumnDetail,
  Driver,
  ImportFileOptions,
  ImportPreview,
  ImportRowsProgress,
  ImportRowsRequest,
  TableInfo,
} from "../types";
import Modal from "./Modal.vue";

const props = defineProps<{
  connectionId: string;
  driver: Driver;
  namespace: string;
  namespaceLabel: string;
  path: string;
  tables: TableInfo[];
  /** Preselects this table as the target. */
  table?: string | null;
}>();

const emit = defineEmits<{
  close: [];
  imported: [];
}>();

const { showToast } = useApp();
const job = useTransferJob(() => props.connectionId, "import");

const DELIMITERS = [
  { value: ",", label: "Comma" },
  { value: "\t", label: "Tab" },
  { value: ";", label: "Semicolon" },
  { value: "|", label: "Pipe" },
];

const NULL_CHOICES: { id: string; label: string; markers: string[] }[] = [
  { id: "empty-n", label: "Empty fields and \\N", markers: ["", "\\N"] },
  { id: "empty", label: "Empty fields", markers: [""] },
  { id: "all", label: "Empty fields, \\N, and NULL", markers: ["", "\\N", "NULL"] },
  { id: "none", label: "Nothing (keep every value as text)", markers: [] },
];

const TYPES: Record<Driver, string[]> = {
  mysql: [
    "BIGINT", "INT", "TINYINT(1)", "DECIMAL(20,6)", "DOUBLE", "DATE", "DATETIME", "DATETIME(6)", "TIMESTAMP",
    "VARCHAR(255)", "TEXT", "LONGTEXT", "JSON", "BLOB",
  ],
  postgres: [
    "BIGINT", "INTEGER", "NUMERIC", "DOUBLE PRECISION", "BOOLEAN", "DATE", "TIMESTAMP", "TIMESTAMPTZ",
    "VARCHAR(255)", "TEXT", "JSONB", "UUID", "BYTEA",
  ],
  sqlite: ["INTEGER", "REAL", "NUMERIC", "BOOLEAN", "TEXT", "BLOB"],
};

interface NewColumn {
  include: boolean;
  name: string;
  dataType: string;
}

const fileName = computed(() => props.path.split("/").pop() ?? props.path);
const header = ref(true);
const delimiter = ref<string | null>(null);
const nullChoice = ref(NULL_CHOICES[0].id);
const preview = ref<ImportPreview | null>(null);
const loading = ref(false);
const previewError = ref("");

const targetTables = computed(() => props.tables.filter((table) => table.kind === "table"));
const mode = ref<"existing" | "new">("existing");
const tableName = ref("");
const newName = ref(suggestedName());
const targetColumns = ref<ColumnDetail[]>([]);
const columnsError = ref("");
const mapping = ref<string[]>([]);
const newColumns = ref<NewColumn[]>([]);
const idColumn = ref(true);
const emptyFirst = ref(false);
const skipConflicts = ref(false);

const running = ref(false);
const cancelling = ref(false);
const error = ref("");
const progress = ref<ImportRowsProgress | null>(null);
let transferId = "";
let stopProgress: UnlistenFn | null = null;
let previewSeq = 0;
let columnsSeq = 0;

function suggestedName() {
  const base = fileName.value.replace(/\.gz$/i, "").replace(/\.[^.]+$/, "");
  return base.replace(/[^\p{L}\p{N}_]+/gu, "_").replace(/^_+|_+$/g, "") || "imported";
}

function normalized(name: string) {
  return name.toLowerCase().replace(/[^\p{L}\p{N}]+/gu, "");
}

function isGenerated(column: ColumnDetail) {
  return /\b(virtual|stored) generated\b/i.test(column.extra);
}

const fileOptions = computed<ImportFileOptions>(() => ({
  format: preview.value?.format ?? null,
  delimiter: delimiter.value,
  header: header.value,
  nullMarkers: NULL_CHOICES.find((choice) => choice.id === nullChoice.value)?.markers ?? [],
}));
const isCsv = computed(() => preview.value?.format !== "json");
const writable = computed(() => targetColumns.value.filter((column) => !isGenerated(column)));

/** The first value in the preview for each column, to help match them up. */
const samples = computed(() =>
  (preview.value?.columns ?? []).map((_, index) => {
    const value = preview.value?.rows.map((row) => row[index]).find((cell) => cell !== null && cell !== "");
    return value ?? null;
  }),
);

async function loadPreview() {
  const seq = ++previewSeq;
  loading.value = true;
  previewError.value = "";
  try {
    const next = await api.previewImport(props.connectionId, props.path, {
      format: null,
      delimiter: delimiter.value,
      header: header.value,
      nullMarkers: fileOptions.value.nullMarkers,
    });
    if (seq !== previewSeq) {
      return;
    }
    preview.value = next;
    if (next.format === "csv") {
      delimiter.value = next.delimiter;
    }
    newColumns.value = next.columns.map((name, index) => ({ include: true, name, dataType: next.types[index] ?? "TEXT" }));
    idColumn.value = !next.columns.some((name) => name.toLowerCase() === "id");
    autoMatch();
  } catch (err) {
    if (seq === previewSeq) {
      preview.value = null;
      previewError.value = String(err);
    }
  } finally {
    if (seq === previewSeq) {
      loading.value = false;
    }
  }
}

async function loadTargetColumns() {
  const seq = ++columnsSeq;
  columnsError.value = "";
  targetColumns.value = [];
  if (!tableName.value) {
    autoMatch();
    return;
  }
  try {
    const structure = await api.tableStructure(props.connectionId, props.namespace, tableName.value);
    if (seq === columnsSeq) {
      targetColumns.value = structure.columns;
      autoMatch();
    }
  } catch (err) {
    if (seq === columnsSeq) {
      columnsError.value = String(err);
    }
  }
}

function autoMatch() {
  const byName = new Map(writable.value.map((column) => [normalized(column.name), column.name]));
  const used = new Set<string>();
  mapping.value = (preview.value?.columns ?? []).map((name) => {
    const target = byName.get(normalized(name));
    if (!target || used.has(target)) {
      return "";
    }
    used.add(target);
    return target;
  });
}

function setDelimiter(value: string) {
  delimiter.value = value;
  void loadPreview();
}

function setHeader(value: boolean) {
  header.value = value;
  void loadPreview();
}

function setNullChoice(value: string) {
  nullChoice.value = value;
  void loadPreview();
}

watch(tableName, () => void loadTargetColumns());

const matchedCount = computed(() => mapping.value.filter(Boolean).length);
const unmatchedRequired = computed(() =>
  writable.value.filter(
    (column) =>
      !column.nullable &&
      column.defaultValue === null &&
      !/auto_increment|identity/i.test(column.extra) &&
      !mapping.value.includes(column.name),
  ),
);

const problem = computed(() => {
  if (!preview.value) {
    return previewError.value || "Loading the file…";
  }
  if (!preview.value.columns.length) {
    return "The file has no columns.";
  }
  if (mode.value === "existing") {
    if (!tableName.value) {
      return "Choose a table to import into.";
    }
    if (!matchedCount.value) {
      return "Match at least one column of the file to a column of the table.";
    }
    const seen = new Set<string>();
    for (const target of mapping.value.filter(Boolean)) {
      if (seen.has(target)) {
        return `“${target}” is chosen for more than one column of the file.`;
      }
      seen.add(target);
    }
    return "";
  }
  if (!newName.value.trim()) {
    return "Name the new table.";
  }
  if (targetTables.value.some((table) => table.name === newName.value.trim())) {
    return `A table named “${newName.value.trim()}” already exists. Pick another name, or import into it.`;
  }
  const included = newColumns.value.filter((column) => column.include);
  if (!included.length) {
    return "Keep at least one column of the file.";
  }
  const seen = new Set<string>();
  for (const column of included) {
    const name = column.name.trim();
    if (!name) {
      return "Every column needs a name.";
    }
    if (!column.dataType.trim()) {
      return `Choose a type for “${name}”.`;
    }
    if (idColumn.value && name.toLowerCase() === "id") {
      return "The file already has an id column. Rename it, or turn off the added id column.";
    }
    if (seen.has(name.toLowerCase())) {
      return `“${name}” is used for more than one column.`;
    }
    seen.add(name.toLowerCase());
  }
  return "";
});

function buildRequest(): ImportRowsRequest {
  const sourceColumns = preview.value?.columns ?? [];
  const base = {
    namespace: props.namespace,
    path: props.path,
    file: fileOptions.value,
    sourceColumns,
    skipConflicts: skipConflicts.value,
  };
  if (mode.value === "existing") {
    return {
      ...base,
      table: tableName.value,
      mapping: mapping.value.flatMap((target, source) => (target ? [{ source, target }] : [])),
      emptyFirst: emptyFirst.value,
    };
  }
  const kept = newColumns.value.flatMap((column, source) =>
    column.include ? [{ source, name: column.name.trim(), dataType: column.dataType.trim() }] : [],
  );
  return {
    ...base,
    table: newName.value.trim(),
    mapping: kept.map(({ source, name }) => ({ source, target: name })),
    create: { columns: kept.map(({ name, dataType }) => ({ name, dataType })), idColumn: idColumn.value },
    emptyFirst: false,
  };
}

const percent = computed(() => {
  const current = progress.value;
  return current?.totalBytes ? Math.min(100, Math.round((current.bytes / current.totalBytes) * 100)) : 0;
});

function rowsLabel(count: number) {
  return `${count.toLocaleString()} ${count === 1 ? "row" : "rows"}`;
}

function close() {
  if (running.value) {
    job.hide();
  } else {
    emit("close");
  }
}

async function start() {
  if (running.value || problem.value) {
    return;
  }
  const request = buildRequest();
  running.value = true;
  error.value = "";
  progress.value = null;
  transferId = job.begin(`Importing ${fileName.value} into “${request.table}”`);
  stopProgress = await listen<ImportRowsProgress>("import-progress", (event) => {
    if (event.payload.transferId === transferId) {
      progress.value = event.payload;
      job.progress(percent.value);
    }
  });
  try {
    const result = await api.importRows(props.connectionId, transferId, request);
    const into = result.created ? `new table “${request.table}”` : `“${request.table}”`;
    showToast(`Imported ${rowsLabel(result.rows)} into ${into} in ${(result.durationMs / 1000).toFixed(1)}s`);
    emit("imported");
    emit("close");
  } catch (err) {
    if (cancelling.value) {
      showToast("Import cancelled. Nothing changed.");
      emit("close");
    } else {
      error.value = String(err);
      job.stopped();
      if (job.hidden.value) {
        showToast(`Import into “${request.table}” stopped: ${error.value}`, "error");
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

onMounted(() => {
  const base = normalized(suggestedName());
  const preset =
    targetTables.value.find((table) => table.name === props.table) ??
    (props.table ? undefined : targetTables.value.find((table) => normalized(table.name) === base));
  if (preset) {
    tableName.value = preset.name;
  } else {
    mode.value = "new";
  }
  void loadPreview();
});

onUnmounted(() => stopProgress?.());
</script>

<template>
  <Modal v-if="!job.hidden.value" :title="`Import rows from ${fileName}`" wide @close="close">
    <p class="muted tiny transfer-summary">
      <template v-if="preview">
        {{ preview.format === "json" ? "JSON" : "CSV" }} · {{ formatBytes(preview.totalBytes) }} ·
        {{ preview.columns.length.toLocaleString() }} {{ preview.columns.length === 1 ? "column" : "columns" }}.
        Types and samples come from the first {{ rowsLabel(preview.sampled) }}.
      </template>
      <template v-else-if="loading">Reading {{ fileName }}…</template>
    </p>
    <p v-if="previewError" class="settings-error">{{ previewError }}</p>

    <div v-if="isCsv" class="import-file-options">
      <label class="modal-label tiny">
        Delimiter
        <select
          :value="delimiter ?? ''"
          :disabled="running || loading"
          @change="setDelimiter(($event.target as HTMLSelectElement).value)"
        >
          <option v-if="delimiter === null" value="" disabled>Detecting…</option>
          <option v-for="item in DELIMITERS" :key="item.label" :value="item.value">{{ item.label }}</option>
        </select>
      </label>
      <label class="modal-label tiny">
        Treat as NULL
        <select
          :value="nullChoice"
          :disabled="running || loading"
          @change="setNullChoice(($event.target as HTMLSelectElement).value)"
        >
          <option v-for="item in NULL_CHOICES" :key="item.id" :value="item.id">{{ item.label }}</option>
        </select>
      </label>
      <label class="checkbox-row">
        <input
          type="checkbox"
          :checked="header"
          :disabled="running || loading"
          @change="setHeader(($event.target as HTMLInputElement).checked)"
        />
        The first row has column names
      </label>
    </div>

    <div class="segmented transfer-format" role="group" aria-label="Import into">
      <button
        type="button"
        :class="{ active: mode === 'existing' }"
        :aria-pressed="mode === 'existing'"
        :disabled="running || !targetTables.length"
        @click="mode = 'existing'"
      >
        Existing table
      </button>
      <button
        type="button"
        :class="{ active: mode === 'new' }"
        :aria-pressed="mode === 'new'"
        :disabled="running"
        @click="mode = 'new'"
      >
        New table
      </button>
    </div>

    <template v-if="mode === 'existing'">
      <label class="modal-label tiny">
        Table in “{{ namespace }}”
        <select v-model="tableName" :disabled="running">
          <option value="" disabled>Choose a table</option>
          <option v-for="item in targetTables" :key="item.name" :value="item.name">{{ item.name }}</option>
        </select>
      </label>
      <p v-if="columnsError" class="settings-error">{{ columnsError }}</p>
      <div v-if="preview && tableName && targetColumns.length" class="import-mapping">
        <table>
          <thead>
            <tr>
              <th>File column</th>
              <th>Goes into</th>
              <th>First value</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="(name, index) in preview.columns" :key="index">
              <td class="import-name" :title="name">{{ name }}</td>
              <td>
                <select v-model="mapping[index]" :disabled="running">
                  <option value="">Skip</option>
                  <option v-for="column in writable" :key="column.name" :value="column.name">
                    {{ column.name }} ({{ column.dataType }})
                  </option>
                </select>
              </td>
              <td class="import-sample" :class="{ muted: samples[index] === null }" :title="samples[index] ?? ''">
                {{ samples[index] ?? "NULL" }}
              </td>
            </tr>
          </tbody>
        </table>
      </div>
      <p v-if="unmatchedRequired.length" class="warn-text tiny">
        {{ unmatchedRequired.map((column) => `“${column.name}”`).join(", ") }}
        {{ unmatchedRequired.length === 1 ? "has" : "have" }} no default and can't be NULL, so every row will fail
        unless {{ unmatchedRequired.length === 1 ? "it gets" : "they get" }} a column of the file.
      </p>
      <div class="transfer-options">
        <label class="checkbox-row">
          <input v-model="emptyFirst" type="checkbox" :disabled="running" />
          Delete the table's rows first
        </label>
        <label class="checkbox-row">
          <input v-model="skipConflicts" type="checkbox" :disabled="running" />
          Skip rows whose key is already in the table
        </label>
        <p v-if="skipConflicts && driver === 'mysql'" class="muted tiny table-action-hint">
          MySQL's INSERT IGNORE also lets through values that don't fit their column, adjusted, with a warning.
        </p>
      </div>
    </template>

    <template v-else>
      <label class="modal-label tiny">
        New table in “{{ namespace }}”
        <input v-model="newName" type="text" spellcheck="false" :disabled="running" />
      </label>
      <div v-if="preview" class="import-mapping">
        <table>
          <thead>
            <tr>
              <th class="import-keep"><span class="sr-only">Keep</span></th>
              <th>Column</th>
              <th>Type</th>
              <th>First value</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="(column, index) in newColumns" :key="index" :class="{ muted: !column.include }">
              <td class="import-keep">
                <input
                  v-model="column.include"
                  type="checkbox"
                  :disabled="running"
                  :aria-label="`Import ${preview.columns[index]}`"
                />
              </td>
              <td>
                <input v-model="column.name" type="text" spellcheck="false" :disabled="running || !column.include" />
              </td>
              <td>
                <input
                  v-model="column.dataType"
                  type="text"
                  spellcheck="false"
                  list="import-column-types"
                  :disabled="running || !column.include"
                />
              </td>
              <td class="import-sample" :class="{ muted: samples[index] === null }" :title="samples[index] ?? ''">
                {{ samples[index] ?? "NULL" }}
              </td>
            </tr>
          </tbody>
        </table>
        <datalist id="import-column-types">
          <option v-for="type in TYPES[driver]" :key="type" :value="type" />
        </datalist>
      </div>
      <div class="transfer-options">
        <label class="checkbox-row">
          <input v-model="idColumn" type="checkbox" :disabled="running" />
          Add an auto-numbered id column as the primary key
        </label>
        <label class="checkbox-row">
          <input v-model="skipConflicts" type="checkbox" :disabled="running" />
          Skip rows that repeat a key
        </label>
      </div>
    </template>

    <p class="muted tiny">
      Every row goes in inside one transaction, so if a row fails or you cancel, nothing changes{{
        mode === "new" ? " and the new table is removed" : ""
      }}.
    </p>
    <div v-if="running" class="transfer-progress" aria-live="polite">
      <div class="transfer-bar"><span :style="{ width: `${percent}%` }" /></div>
      <p class="muted tiny transfer-status">
        <span class="transfer-status-label">{{ cancelling ? "Cancelling…" : "Importing…" }}</span>
        <span v-if="progress">{{ rowsLabel(progress.rows) }} · {{ percent }}%</span>
      </p>
    </div>
    <p v-if="error" class="settings-error transfer-error">{{ error }}</p>
    <p v-else-if="problem && preview && !running" class="muted tiny">{{ problem }}</p>
    <template #actions>
      <button v-if="running" class="ghost transfer-hide" type="button" @click="job.hide">Run in background</button>
      <button class="ghost" type="button" :disabled="cancelling" @click="cancel">Cancel</button>
      <button class="primary" type="button" :disabled="running || Boolean(problem)" @click="start">
        <span v-if="running" class="spinner" aria-hidden="true" />
        {{ running ? "Importing…" : "Import" }}
      </button>
    </template>
  </Modal>
</template>
