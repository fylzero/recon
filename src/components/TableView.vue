<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, shallowRef, watch } from "vue";
import * as api from "../api";
import { isNumericColumn } from "../cells";
import { useApp } from "../composables/useApp";
import type {
  BrowseResult,
  Cell,
  ColumnMeta,
  EditValue,
  RowValues,
  SaveRequest,
  SortDirection,
  TableStructure as Structure,
} from "../types";
import DataGrid, { type CellPosition } from "./DataGrid.vue";
import TableStructure from "./TableStructure.vue";

interface PendingCell {
  key: Cell[];
  column: string;
  original: Cell;
  value: Cell;
}

interface CellChange extends PendingCell {
  before: Cell;
}

const props = defineProps<{
  connectionId: string;
  namespace: string;
  table: string;
  kind: "table" | "view";
  active: boolean;
}>();

const emit = defineEmits<{
  changes: [rows: number];
}>();

const { pageSize, showToast } = useApp();

const mode = ref<"data" | "structure">("data");
const page = ref(0);
const sortColumn = ref<string | null>(null);
const sortDir = ref<SortDirection>("asc");
const result = shallowRef<BrowseResult | null>(null);
const total = ref<number | null>(null);
const structure = shallowRef<Structure | null>(null);
const loading = ref(false);
const loadingStructure = ref(false);
const error = ref("");
const structureError = ref("");
const grid = ref<InstanceType<typeof DataGrid> | null>(null);
const pending = shallowRef(new Map<string, PendingCell>());
const undoStack: CellChange[][] = [];
const redoStack: CellChange[][] = [];
let requestId = 0;

const rows = computed(() => result.value?.rows ?? []);
const columns = computed<ColumnMeta[]>(() => result.value?.columns ?? []);
const columnIndex = computed(
  () => new Map(columns.value.map((column, index) => [column.name, index])),
);
const keyColumns = computed(() =>
  (structure.value?.columns ?? []).filter((column) => column.primaryKey).map((column) => column.name),
);
const keyIndexes = computed(() => {
  const indexes = keyColumns.value.map((name) => columnIndex.value.get(name) ?? -1);
  return indexes.includes(-1) ? [] : indexes;
});
const nullable = computed(
  () => new Map((structure.value?.columns ?? []).map((column) => [column.name, column.nullable])),
);
const editable = computed(() => props.kind === "table" && keyIndexes.value.length > 0);
const readOnlyReason = computed(() => {
  if (props.kind === "view") {
    return "Views are read-only";
  }
  if (structure.value && result.value && !keyIndexes.value.length) {
    return "Read-only: this table has no primary key";
  }
  return "";
});

function rowKey(row: RowValues | undefined): Cell[] | null {
  if (!row || !keyIndexes.value.length) {
    return null;
  }
  const key = keyIndexes.value.map((index) => row[index] ?? null);
  return key.some((value) => typeof value === "object" && value !== null) ? null : key;
}

function cellId(key: Cell[], column: string) {
  return JSON.stringify([key, column]);
}

const pageKeys = computed(() => rows.value.map((row) => rowKey(row)));
const pendingByRow = computed(() => {
  const byRow = new Map<string, PendingCell[]>();
  for (const cell of pending.value.values()) {
    const id = JSON.stringify(cell.key);
    const list = byRow.get(id);
    if (list) {
      list.push(cell);
    } else {
      byRow.set(id, [cell]);
    }
  }
  return byRow;
});
const pageEdits = computed(() =>
  pageKeys.value.map((key) => (key ? pendingByRow.value.get(JSON.stringify(key)) : undefined)),
);
const displayRows = computed(() =>
  rows.value.map((row, index) => {
    const edits = pageEdits.value[index];
    if (!edits) {
      return row;
    }
    const next = [...row];
    for (const edit of edits) {
      const col = columnIndex.value.get(edit.column);
      if (col !== undefined) {
        next[col] = edit.value;
      }
    }
    return next;
  }),
);
const modified = computed(() => {
  const cells = new Map<number, Set<number>>();
  pageEdits.value.forEach((edits, index) => {
    if (edits) {
      cells.set(
        index,
        new Set(edits.flatMap((edit) => columnIndex.value.get(edit.column) ?? [])),
      );
    }
  });
  return cells;
});
const dirty = computed(() => pending.value.size > 0);
const offset = computed(() => page.value * pageSize.value);
const pageCount = computed(() =>
  total.value === null ? null : Math.max(Math.ceil(total.value / pageSize.value), 1),
);
const hasNext = computed(() =>
  total.value === null
    ? rows.value.length === pageSize.value
    : offset.value + rows.value.length < total.value,
);
const rangeLabel = computed(() => {
  if (!result.value) {
    return "";
  }
  if (!rows.value.length) {
    return total.value ? `No rows on this page of ${total.value.toLocaleString()}` : "No rows";
  }
  const first = offset.value + 1;
  const last = offset.value + rows.value.length;
  const of = total.value === null ? "" : ` of ${total.value.toLocaleString()}`;
  return `${first.toLocaleString()}–${last.toLocaleString()}${of}`;
});

async function loadData(count = false, scrollToTop = true) {
  const id = ++requestId;
  loading.value = true;
  error.value = "";
  try {
    const next = await api.browseTable(props.connectionId, {
      namespace: props.namespace,
      table: props.table,
      offset: offset.value,
      limit: pageSize.value,
      orderBy: sortColumn.value,
      orderDir: sortColumn.value ? sortDir.value : null,
      count,
    });
    if (id !== requestId) {
      return;
    }
    result.value = next;
    if (count) {
      total.value = next.total;
    }
    if (scrollToTop) {
      grid.value?.scrollToTop();
    }
  } catch (err) {
    if (id === requestId) {
      error.value = String(err);
    }
  } finally {
    if (id === requestId) {
      loading.value = false;
    }
  }
}

async function loadStructure() {
  loadingStructure.value = true;
  structureError.value = "";
  try {
    structure.value = await api.tableStructure(props.connectionId, props.namespace, props.table);
  } catch (err) {
    structureError.value = String(err);
  } finally {
    loadingStructure.value = false;
  }
}

function refresh() {
  if (mode.value === "structure") {
    void loadStructure();
  } else {
    void loadData(true);
  }
}

function goToPage(next: number) {
  const last = pageCount.value === null ? Infinity : pageCount.value - 1;
  const clamped = Math.min(Math.max(next, 0), last);
  if (clamped === page.value) {
    return;
  }
  page.value = clamped;
  void loadData();
}

function onSort(column: string) {
  if (sortColumn.value !== column) {
    sortColumn.value = column;
    sortDir.value = "asc";
  } else if (sortDir.value === "asc") {
    sortDir.value = "desc";
  } else {
    sortColumn.value = null;
    sortDir.value = "asc";
  }
  page.value = 0;
  void loadData();
}

function parseInput(text: string, reference: Cell, column: ColumnMeta): Cell {
  const trimmed = text.trim();
  if (typeof reference === "boolean") {
    const lower = trimmed.toLowerCase();
    if (["true", "t", "1", "yes"].includes(lower)) {
      return true;
    }
    if (["false", "f", "0", "no"].includes(lower)) {
      return false;
    }
    return text;
  }
  const numeric = typeof reference === "number" || (reference === null && isNumericColumn(column));
  if (numeric && trimmed && Number.isFinite(Number(trimmed)) && String(Number(trimmed)) === trimmed) {
    return Number(trimmed);
  }
  return text;
}

function applyChanges(changes: CellChange[], side: "before" | "value") {
  const next = new Map(pending.value);
  for (const change of changes) {
    const id = cellId(change.key, change.column);
    const value = change[side];
    if (value === change.original) {
      next.delete(id);
    } else {
      next.set(id, { key: change.key, column: change.column, original: change.original, value });
    }
  }
  pending.value = next;
}

function recordChanges(changes: CellChange[]) {
  if (!changes.length) {
    return;
  }
  applyChanges(changes, "value");
  undoStack.push(changes);
  redoStack.length = 0;
}

function changeAt(row: number, col: number, value: Cell): CellChange | null {
  const key = pageKeys.value[row];
  const column = columns.value[col];
  if (!key || !column) {
    return null;
  }
  const before = displayRows.value[row]?.[col] ?? null;
  if (before === value) {
    return null;
  }
  const original = rows.value[row]?.[col] ?? null;
  return { key, column: column.name, original, before, value };
}

function onEdit(row: number, col: number, text: string) {
  const column = columns.value[col];
  if (!column) {
    return;
  }
  const change = changeAt(row, col, parseInput(text, rows.value[row]?.[col] ?? null, column));
  recordChanges(change ? [change] : []);
}

function onSetNull(cells: CellPosition[]) {
  const allowed = cells.filter((cell) => nullable.value.get(columns.value[cell.col]?.name ?? "") !== false);
  if (!allowed.length) {
    showToast(cells.length === 1 ? "This column cannot be NULL." : "These columns cannot be NULL.", "error");
    return;
  }
  recordChanges(allowed.flatMap((cell) => changeAt(cell.row, cell.col, null) ?? []));
}

function undo() {
  const changes = undoStack.pop();
  if (changes) {
    applyChanges(changes, "before");
    redoStack.push(changes);
  }
}

function redo() {
  const changes = redoStack.pop();
  if (changes) {
    applyChanges(changes, "value");
    undoStack.push(changes);
  }
}

function discard() {
  grid.value?.commitEdit();
  recordChanges(
    [...pending.value.values()].map((cell) => ({ ...cell, before: cell.value, value: cell.original })),
  );
}

function pendingChanges() {
  grid.value?.commitEdit();
  const snapshot = pending.value;
  const request: SaveRequest = {
    namespace: props.namespace,
    table: props.table,
    updates: [...pendingByRow.value.values()].map((cells) => ({
      key: keyColumns.value.map((column, index) => ({
        column,
        value: cells[0].key[index] as EditValue,
      })),
      changes: cells.map((cell) => ({ column: cell.column, value: cell.value as EditValue })),
    })),
  };
  return { request, snapshot };
}

function markSaved(sent: Map<string, PendingCell>) {
  if (result.value) {
    const saved = rows.value.map((row, index) => {
      const key = pageKeys.value[index];
      let next: RowValues | null = null;
      for (const [col, column] of columns.value.entries()) {
        const cell = key ? sent.get(cellId(key, column.name)) : undefined;
        if (cell) {
          next ??= [...row];
          next[col] = cell.value;
        }
      }
      return next ?? row;
    });
    result.value = { ...result.value, rows: saved };
  }
  const remaining = new Map(pending.value);
  for (const [id, cell] of sent) {
    if (remaining.get(id)?.value === cell.value) {
      remaining.delete(id);
    }
  }
  pending.value = remaining;
  undoStack.length = 0;
  redoStack.length = 0;
  void loadData(false, false);
}

function onWindowKeydown(event: KeyboardEvent) {
  if (!props.active || !(event.metaKey || event.ctrlKey) || event.altKey) {
    return;
  }
  const key = event.key.toLowerCase();
  if (key === "r" && !event.shiftKey) {
    event.preventDefault();
    refresh();
    return;
  }
  if (key !== "z" || mode.value !== "data" || !editable.value) {
    return;
  }
  const target = event.target;
  if (target instanceof Element && target.closest("input, textarea, select, [contenteditable]")) {
    return;
  }
  event.preventDefault();
  if (event.shiftKey) {
    redo();
  } else {
    undo();
  }
}

watch(mode, (next) => {
  if (next === "structure" && !structure.value && !loadingStructure.value) {
    void loadStructure();
  }
});

watch(pageSize, () => {
  page.value = 0;
  void loadData();
});

watch(
  () => pendingByRow.value.size,
  (count) => emit("changes", count),
);

onMounted(() => {
  void loadData(true);
  if (props.kind === "table") {
    void loadStructure();
  }
  window.addEventListener("keydown", onWindowKeydown, true);
});

onBeforeUnmount(() => {
  window.removeEventListener("keydown", onWindowKeydown, true);
  if (dirty.value) {
    emit("changes", 0);
  }
});

defineExpose({ refresh, pendingChanges, markSaved, discard });
</script>

<template>
  <div class="table-view">
    <div class="pane-toolbar">
      <div class="segmented" role="group" aria-label="Table view">
        <button
          type="button"
          :class="{ active: mode === 'data' }"
          :aria-pressed="mode === 'data'"
          @click="mode = 'data'"
        >
          Data
        </button>
        <button
          type="button"
          :class="{ active: mode === 'structure' }"
          :aria-pressed="mode === 'structure'"
          @click="mode = 'structure'"
        >
          Structure
        </button>
      </div>
      <button class="ghost tiny" type="button" title="Reload (⌘R)" @click="refresh">
        <svg class="button-icon" viewBox="0 0 24 24" aria-hidden="true">
          <path
            d="M16.023 9.348h4.992v-.001M2.985 19.644v-4.992m0 0h4.992m-4.993 0 3.181 3.183a8.25 8.25 0 0 0 13.803-3.7M4.031 9.865a8.25 8.25 0 0 1 13.803-3.7l3.181 3.182m0-4.991v4.99"
          />
        </svg>
        Reload
      </button>
      <div class="pane-toolbar-end">
        <span v-if="loading || loadingStructure" class="spinner" aria-label="Loading" />
        <template v-if="mode === 'data'">
          <span class="muted tiny pager-label">{{ rangeLabel }}</span>
          <div class="pager">
            <button
              class="ghost tiny"
              type="button"
              title="First page"
              :disabled="page === 0 || loading"
              @click="goToPage(0)"
            >
              «
            </button>
            <button
              class="ghost tiny"
              type="button"
              title="Previous page"
              :disabled="page === 0 || loading"
              @click="goToPage(page - 1)"
            >
              ‹
            </button>
            <span class="muted tiny pager-page">
              Page {{ page + 1 }}<template v-if="pageCount !== null"> of {{ pageCount.toLocaleString() }}</template>
            </span>
            <button
              class="ghost tiny"
              type="button"
              title="Next page"
              :disabled="!hasNext || loading"
              @click="goToPage(page + 1)"
            >
              ›
            </button>
            <button
              class="ghost tiny"
              type="button"
              title="Last page"
              :disabled="pageCount === null || page >= pageCount - 1 || loading"
              @click="goToPage((pageCount ?? 1) - 1)"
            >
              »
            </button>
          </div>
        </template>
      </div>
    </div>

    <div v-show="mode === 'data'" class="table-view-body">
      <p v-if="error" class="pane-error">{{ error }}</p>
      <DataGrid
        v-else-if="result"
        ref="grid"
        :columns="result.columns"
        :rows="displayRows"
        :row-number-offset="offset"
        sortable
        :sort-column="sortColumn"
        :sort-dir="sortDir"
        :editable="editable"
        :modified="modified"
        @sort="onSort"
        @edit="onEdit"
        @set-null="onSetNull"
      />
    </div>
    <div v-if="mode === 'structure'" class="table-view-body scroll">
      <p v-if="structureError" class="pane-error">{{ structureError }}</p>
      <TableStructure v-else-if="structure" :structure="structure" />
    </div>
    <div class="pane-status muted tiny">
      <template v-if="mode === 'data' && result">
        {{ result.columns.length }} columns · loaded in {{ result.durationMs }}ms
        <template v-if="sortColumn"> · sorted by {{ sortColumn }} {{ sortDir }}</template>
        <template v-if="readOnlyReason"> · {{ readOnlyReason }}</template>
        <template v-else-if="editable"> · double-click a cell to edit</template>
      </template>
      <template v-else-if="mode === 'structure' && structure">
        {{ structure.columns.length }} columns · {{ structure.indexes.length }} indexes
      </template>
    </div>
  </div>
</template>
