<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, shallowRef, watch } from "vue";
import * as api from "../api";
import { useApp } from "../composables/useApp";
import { cellDisplay, isNumericColumn } from "../cells";
import type {
  BrowseResult,
  Cell,
  CellEdit,
  ColumnMeta,
  Driver,
  EditValue,
  ForeignKey,
  RowValues,
  SaveRequest,
  SortDirection,
  TableLink,
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

interface UndoEntry {
  changes: CellChange[];
  created?: number[];
  removed?: number[];
}

const props = defineProps<{
  connectionId: string;
  namespace: string;
  table: string;
  kind: "table" | "view";
  driver: Driver;
  active: boolean;
  filter?: CellEdit[];
}>();

const emit = defineEmits<{
  changes: [rows: number];
  follow: [link: TableLink];
  clearFilter: [];
}>();

const { pageSize, showToast } = useApp();

const mode = ref<"data" | "structure" | "indexes">("data");
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
const structureView = ref<InstanceType<typeof TableStructure> | null>(null);
const structureChanges = ref(0);
const pending = shallowRef(new Map<string, PendingCell>());
const undoStack: UndoEntry[] = [];
const redoStack: UndoEntry[] = [];
const newRowIds = ref<number[]>([]);
let nextNewRowId = 0;
let requestId = 0;

// First element of a new row's key, which no primary key value can equal.
const NEW_ROW = "\u0000new";

function isNewKey(key: Cell[]) {
  return key[0] === NEW_ROW;
}

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
const foreignKeyByColumn = computed(() => {
  const byColumn = new Map<string, ForeignKey>();
  for (const key of structure.value?.foreignKeys ?? []) {
    for (const column of key.columns) {
      if (!byColumn.has(column)) {
        byColumn.set(column, key);
      }
    }
  }
  return byColumn;
});
const links = computed(() =>
  columns.value.map((column) => {
    const key = foreignKeyByColumn.value.get(column.name);
    if (!key) {
      return null;
    }
    const table = key.refNamespace && key.refNamespace !== props.namespace
      ? `${key.refNamespace}.${key.refTable}`
      : key.refTable;
    return `${table}.${key.refColumns[key.columns.indexOf(column.name)]}`;
  }),
);
const filterKey = computed(() => JSON.stringify(props.filter ?? []));
const filterLabel = computed(() =>
  (props.filter ?? []).map((cell) => `${cell.column} = ${cellDisplay(cell.value)}`).join(" and "),
);
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

const allRows = computed(() => [
  ...rows.value,
  ...newRowIds.value.map(() => columns.value.map(() => null)),
]);
const pageKeys = computed(() => [
  ...rows.value.map((row) => rowKey(row)),
  ...newRowIds.value.map((id): Cell[] => [NEW_ROW, id]),
]);
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
  allRows.value.map((row, index) => {
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
const dirty = computed(
  () => pending.value.size > 0 || newRowIds.value.length > 0 || structureChanges.value > 0,
);
const canInsert = computed(() => props.kind === "table" && Boolean(result.value));

function cellEditable(row: number) {
  return row >= rows.value.length ? canInsert.value : editable.value;
}
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
      filter: props.filter ?? [],
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
  if (mode.value !== "data") {
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

function onFollow(row: number, col: number) {
  const key = foreignKeyByColumn.value.get(columns.value[col]?.name ?? "");
  const values = displayRows.value[row];
  if (!key || !values) {
    return;
  }
  const filter: CellEdit[] = [];
  for (const [index, column] of key.columns.entries()) {
    const value = values[columnIndex.value.get(column) ?? -1];
    if (value === null || value === undefined || typeof value === "object") {
      showToast(`${column} is empty, so this row doesn't point to a record in ${key.refTable}.`, "error");
      return;
    }
    filter.push({ column: key.refColumns[index], value });
  }
  emit("follow", { namespace: key.refNamespace || props.namespace, table: key.refTable, filter });
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

function updateNewRows(add: number[], remove: number[]) {
  const removed = new Set(remove);
  newRowIds.value = [...newRowIds.value.filter((id) => !removed.has(id)), ...add].sort((a, b) => a - b);
}

function applyEntry(entry: UndoEntry, side: "before" | "value") {
  applyChanges(entry.changes, side);
  const created = entry.created ?? [];
  const removed = entry.removed ?? [];
  if (side === "value") {
    updateNewRows(created, removed);
  } else {
    updateNewRows(removed, created);
  }
}

function recordEntry(entry: UndoEntry) {
  if (!entry.changes.length && !entry.created?.length && !entry.removed?.length) {
    return;
  }
  applyEntry(entry, "value");
  undoStack.push(entry);
  redoStack.length = 0;
}

function recordChanges(changes: CellChange[]) {
  recordEntry({ changes });
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
  const entry = undoStack.pop();
  if (entry) {
    applyEntry(entry, "before");
    redoStack.push(entry);
  }
}

function redo() {
  const entry = redoStack.pop();
  if (entry) {
    applyEntry(entry, "value");
    undoStack.push(entry);
  }
}

function discard() {
  grid.value?.commitEdit();
  recordEntry({
    changes: [...pending.value.values()].map((cell) => ({
      ...cell,
      before: cell.value,
      value: cell.original,
    })),
    removed: [...newRowIds.value],
  });
  structureView.value?.discard();
}

async function createRecord() {
  if (!canInsert.value) {
    return;
  }
  mode.value = "data";
  recordEntry({ changes: [], created: [nextNewRowId++] });
  await nextTick();
  grid.value?.editCell(allRows.value.length - 1, 0);
}

function create() {
  if (mode.value === "data") {
    void createRecord();
  } else {
    structureView.value?.create();
  }
}

function pendingChanges() {
  grid.value?.commitEdit();
  const structurePending = structureView.value?.pendingChanges();
  const rowEdits = [...pendingByRow.value.values()];
  const cellEdits = (cells: PendingCell[]) =>
    cells.map((cell) => ({ column: cell.column, value: cell.value as EditValue }));
  const request: SaveRequest = {
    namespace: props.namespace,
    table: props.table,
    updates: rowEdits
      .filter((cells) => !isNewKey(cells[0].key))
      .map((cells) => ({
        key: keyColumns.value.map((column, index) => ({
          column,
          value: cells[0].key[index] as EditValue,
        })),
        changes: cellEdits(cells),
      })),
    inserts: newRowIds.value.map((id) => ({
      values: cellEdits(pendingByRow.value.get(JSON.stringify([NEW_ROW, id])) ?? []),
    })),
    columns: structurePending?.columns ?? [],
    newColumns: structurePending?.newColumns ?? [],
    indexes: structurePending?.indexes ?? [],
    newIndexes: structurePending?.newIndexes ?? [],
  };
  return {
    request,
    snapshot: {
      rows: pending.value,
      insertedIds: [...newRowIds.value],
      columns: structurePending?.snapshot,
    },
  };
}

type SavedSnapshot = ReturnType<typeof pendingChanges>["snapshot"];

function markSaved(sent: SavedSnapshot) {
  const inserted = new Set<Cell>(sent.insertedIds);
  markRowsSaved(sent.rows);
  if (inserted.size) {
    newRowIds.value = newRowIds.value.filter((id) => !inserted.has(id));
    pending.value = new Map(
      [...pending.value].filter(([, cell]) => !(isNewKey(cell.key) && inserted.has(cell.key[1]))),
    );
  }
  if (!sent.columns || (!sent.columns.fields.size && !sent.columns.created.length)) {
    void loadData(inserted.size > 0, false);
    return;
  }
  structureView.value?.markSaved(sent.columns);
  const renamed = new Map(
    [...sent.columns.fields.values()]
      .filter((edit) => edit.section === "columns" && edit.field === "name")
      .filter((edit) => structure.value?.columns.some((column) => column.name === edit.key))
      .map((edit) => [edit.key, String(edit.value)]),
  );
  if (sortColumn.value && renamed.has(sortColumn.value)) {
    sortColumn.value = renamed.get(sortColumn.value) ?? null;
  }
  void loadStructure();
  void loadData(inserted.size > 0, false);
}

function markRowsSaved(sent: Map<string, PendingCell>) {
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
  if (key !== "z" || props.kind !== "table") {
    return;
  }
  const target = event.target;
  if (target instanceof Element && target.closest("input, textarea, select, [contenteditable]")) {
    // An untouched grid editor has nothing to undo, so the undo goes to the grid.
    if (!(target instanceof HTMLTextAreaElement && target.dataset.pristine === "true")) {
      return;
    }
    target.blur();
  }
  event.preventDefault();
  if (mode.value !== "data") {
    if (event.shiftKey) {
      structureView.value?.redo();
    } else {
      structureView.value?.undo();
    }
  } else if (event.shiftKey) {
    redo();
  } else {
    undo();
  }
}

watch(mode, (next) => {
  if (next !== "data" && !structure.value && !loadingStructure.value) {
    void loadStructure();
  }
});

watch(pageSize, () => {
  page.value = 0;
  void loadData();
});

watch(filterKey, () => {
  mode.value = "data";
  page.value = 0;
  void loadData(true);
});

watch(
  () =>
    [...pendingByRow.value.values()].filter((cells) => !isNewKey(cells[0].key)).length +
    newRowIds.value.length +
    structureChanges.value,
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
          <span v-if="total !== null" class="group-count">{{ total.toLocaleString() }}</span>
        </button>
        <button
          type="button"
          :class="{ active: mode === 'structure' }"
          :aria-pressed="mode === 'structure'"
          @click="mode = 'structure'"
        >
          Structure
          <span v-if="structure" class="group-count">{{ structure.columns.length }}</span>
        </button>
        <button
          type="button"
          :class="{ active: mode === 'indexes' }"
          :aria-pressed="mode === 'indexes'"
          @click="mode = 'indexes'"
        >
          Indexes
          <span v-if="structure" class="group-count">{{ structure.indexes.length }}</span>
        </button>
      </div>
      <button
        v-if="kind === 'table'"
        class="ghost tiny"
        type="button"
        :disabled="mode === 'data' ? !canInsert : !structure"
        @click="create"
      >
        <svg class="button-icon" viewBox="0 0 24 24" aria-hidden="true">
          <path d="M12 4.5v15m7.5-7.5h-15" />
        </svg>
        {{ mode === "data" ? "New record" : mode === "structure" ? "New column" : "New index" }}
      </button>
      <span v-if="filterLabel && mode === 'data'" class="table-filter" :title="`Showing rows where ${filterLabel}`">
        <span class="table-filter-label">{{ filterLabel }}</span>
        <button
          class="table-filter-clear"
          type="button"
          title="Show all rows"
          aria-label="Clear filter and show all rows"
          @click="emit('clearFilter')"
        >
          ×
        </button>
      </span>
      <div class="pane-toolbar-end">
        <span v-if="loading || loadingStructure" class="spinner" aria-label="Loading" />
        <template v-if="mode === 'data'">
          <span v-if="result" class="muted tiny">Loaded in {{ result.durationMs }}ms</span>
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
        :cell-editable="cellEditable"
        :new-row-start="rows.length"
        :creatable="canInsert"
        :modified="modified"
        :links="links"
        @sort="onSort"
        @edit="onEdit"
        @set-null="onSetNull"
        @create="createRecord"
        @follow="onFollow"
      />
    </div>
    <div v-show="mode !== 'data'" class="table-view-body">
      <p v-if="structureError" class="pane-error">{{ structureError }}</p>
      <TableStructure
        v-else-if="structure"
        ref="structureView"
        :structure="structure"
        :section="mode === 'indexes' ? 'indexes' : 'columns'"
        :driver="driver"
        :editable="kind === 'table'"
        @changes="structureChanges = $event"
      />
    </div>
  </div>
</template>
