<script setup lang="ts">
import { computed, nextTick, ref, shallowRef, watch } from "vue";
import { useApp } from "../composables/useApp";
import DataGrid, { type CellPosition } from "./DataGrid.vue";
import type {
  Cell,
  ColumnChange,
  ColumnMeta,
  Driver,
  IndexChange,
  RowValues,
  TableStructure,
} from "../types";

type Section = "columns" | "indexes";
type Field = "name" | "dataType" | "nullable" | "defaultValue" | "columns" | "unique";

interface PendingField {
  section: Section;
  key: string;
  field: Field;
  original: Cell;
  value: Cell;
}

interface FieldChange extends PendingField {
  before: Cell;
}

interface NewRow {
  section: Section;
  key: string;
}

interface UndoEntry {
  changes: FieldChange[];
  created?: NewRow[];
  removed?: NewRow[];
}

const SHEETS: Record<Section, { headers: string[]; fields: (Field | null)[] }> = {
  columns: {
    headers: ["name", "type", "nullable", "default", "key", "extra"],
    fields: ["name", "dataType", "nullable", "defaultValue", null, null],
  },
  indexes: {
    headers: ["name", "columns", "kind"],
    fields: ["name", "columns", "unique"],
  },
};

const props = defineProps<{
  structure: TableStructure;
  section: Section;
  driver: Driver;
  editable: boolean;
}>();

const emit = defineEmits<{
  changes: [count: number];
}>();

const { showToast } = useApp();

const grid = ref<InstanceType<typeof DataGrid> | null>(null);
const pending = shallowRef(new Map<string, PendingField>());
const undoStack: UndoEntry[] = [];
const redoStack: UndoEntry[] = [];
const newKeys = ref<Record<Section, string[]>>({ columns: [], indexes: [] });
let nextNewId = 0;

// Prefix for keys of rows being created, which no real name can start with.
const NEW_ROW = "\u0000new:";

const BLANK_ROWS: Record<Section, RowValues> = {
  columns: ["", "", "YES", null, "", ""],
  indexes: ["", "", "INDEX"],
};

function fieldId(section: Section, key: string, field: Field) {
  return JSON.stringify([section, key, field]);
}

function isNewKey(key: string) {
  return key.startsWith(NEW_ROW);
}

const keys = computed<Record<Section, string[]>>(() => ({
  columns: [...props.structure.columns.map((column) => column.name), ...newKeys.value.columns],
  indexes: [...props.structure.indexes.map((index) => index.name), ...newKeys.value.indexes],
}));

const existingCount = computed<Record<Section, number>>(() => ({
  columns: props.structure.columns.length,
  indexes: props.structure.indexes.length,
}));

const baseRows = computed<Record<Section, RowValues[]>>(() => ({
  columns: [
    ...props.structure.columns.map((column): RowValues => [
      column.name,
      column.dataType,
      column.nullable ? "YES" : "NO",
      column.defaultValue,
      column.primaryKey ? "PRI" : "",
      column.extra,
    ]),
    ...newKeys.value.columns.map(() => [...BLANK_ROWS.columns]),
  ],
  indexes: [
    ...props.structure.indexes.map((index): RowValues => [
      index.name,
      index.columns,
      index.primary ? "PRIMARY" : index.unique ? "UNIQUE" : "INDEX",
    ]),
    ...newKeys.value.indexes.map(() => [...BLANK_ROWS.indexes]),
  ],
}));

const sheet = computed(() => SHEETS[props.section]);
const sheetKeys = computed(() => keys.value[props.section]);

const rows = computed(() =>
  baseRows.value[props.section].map((row, index) => {
    const key = sheetKeys.value[index];
    let next: RowValues | null = null;
    sheet.value.fields.forEach((field, col) => {
      const edit = field ? pending.value.get(fieldId(props.section, key, field)) : undefined;
      if (edit) {
        next ??= [...row];
        next[col] = edit.value;
      }
    });
    return next ?? row;
  }),
);

const modified = computed(() => {
  const cells = new Map<number, Set<number>>();
  sheetKeys.value.forEach((key, row) => {
    sheet.value.fields.forEach((field, col) => {
      if (field && pending.value.has(fieldId(props.section, key, field))) {
        const set = cells.get(row) ?? new Set<number>();
        set.add(col);
        cells.set(row, set);
      }
    });
  });
  return cells;
});

const columns = computed<ColumnMeta[]>(() =>
  sheet.value.headers.map((name) => ({ name, typeName: "text" })),
);

function cellEditable(row: number, col: number) {
  const field = sheet.value.fields[col];
  if (!props.editable || !field) {
    return false;
  }
  if (row >= existingCount.value[props.section]) {
    return true;
  }
  if (props.section === "indexes") {
    return !props.structure.indexes[row]?.primary;
  }
  return field === "name" || props.driver !== "sqlite";
}

const changedCount = computed(
  () =>
    new Set(
      [...pending.value.values()]
        .filter((edit) => !isNewKey(edit.key))
        .map((edit) => JSON.stringify([edit.section, edit.key])),
    ).size +
    newKeys.value.columns.length +
    newKeys.value.indexes.length,
);

async function create() {
  if (!props.editable) {
    return;
  }
  const section = props.section;
  recordEntry({ changes: [], created: [{ section, key: `${NEW_ROW}${nextNewId++}` }] });
  await nextTick();
  grid.value?.editCell(keys.value[section].length - 1, 0);
}

function parseInput(field: Field, text: string): Cell | undefined {
  const trimmed = text.trim();
  const lower = trimmed.toLowerCase();
  if (field === "nullable") {
    if (["yes", "y", "true", "t", "1", "null"].includes(lower)) {
      return "YES";
    }
    if (["no", "n", "false", "f", "0", "not null"].includes(lower)) {
      return "NO";
    }
    showToast("Nullable must be YES or NO.", "error");
    return undefined;
  }
  if (field === "unique") {
    if (["unique", "u", "yes", "true", "1"].includes(lower)) {
      return "UNIQUE";
    }
    if (["index", "i", "no", "false", "0", ""].includes(lower)) {
      return "INDEX";
    }
    showToast("Kind must be UNIQUE or INDEX.", "error");
    return undefined;
  }
  if (field === "defaultValue") {
    return trimmed ? text : null;
  }
  if (!trimmed) {
    const messages: Record<string, string> = {
      name: "A name can't be empty.",
      dataType: "A column needs a type.",
      columns: "An index needs at least one column.",
    };
    showToast(messages[field], "error");
    return undefined;
  }
  return trimmed;
}

function applyChanges(changes: FieldChange[], side: "before" | "value") {
  const next = new Map(pending.value);
  for (const change of changes) {
    const id = fieldId(change.section, change.key, change.field);
    const value = change[side];
    if (value === change.original) {
      next.delete(id);
    } else {
      const { before: _before, ...edit } = change;
      next.set(id, { ...edit, value });
    }
  }
  pending.value = next;
}

function newRowOrder(key: string) {
  return Number(key.slice(NEW_ROW.length));
}

function updateNewRows(add: NewRow[], remove: NewRow[]) {
  const next: Record<Section, string[]> = { columns: [...newKeys.value.columns], indexes: [...newKeys.value.indexes] };
  for (const row of remove) {
    next[row.section] = next[row.section].filter((key) => key !== row.key);
  }
  for (const row of add) {
    next[row.section].push(row.key);
  }
  next.columns.sort((a, b) => newRowOrder(a) - newRowOrder(b));
  next.indexes.sort((a, b) => newRowOrder(a) - newRowOrder(b));
  newKeys.value = next;
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

function recordChanges(changes: FieldChange[]) {
  recordEntry({ changes });
}

function changeAt(row: number, col: number, value: Cell): FieldChange | null {
  const key = sheetKeys.value[row];
  const field = sheet.value.fields[col];
  if (key === undefined || !field) {
    return null;
  }
  const before = rows.value[row]?.[col] ?? null;
  if (before === value) {
    return null;
  }
  const original = baseRows.value[props.section][row]?.[col] ?? null;
  return { section: props.section, key, field, original, before, value };
}

function onEdit(row: number, col: number, text: string) {
  const field = sheet.value.fields[col];
  const value = field ? parseInput(field, text) : undefined;
  if (value === undefined) {
    return;
  }
  const change = changeAt(row, col, value);
  recordChanges(change ? [change] : []);
}

function onSetNull(cells: CellPosition[]) {
  const allowed = cells.filter((cell) => sheet.value.fields[cell.col] === "defaultValue");
  if (!allowed.length) {
    showToast("Only a column's default can be set to NULL, which removes it.", "error");
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
    changes: [...pending.value.values()].map((edit) => ({
      ...edit,
      before: edit.value,
      value: edit.original,
    })),
    removed: [
      ...newKeys.value.columns.map((key): NewRow => ({ section: "columns", key })),
      ...newKeys.value.indexes.map((key): NewRow => ({ section: "indexes", key })),
    ],
  });
}

function currentValue(section: Section, key: string, field: Field) {
  const edit = pending.value.get(fieldId(section, key, field));
  if (edit) {
    return edit.value;
  }
  return BLANK_ROWS[section][SHEETS[section].fields.indexOf(field)] ?? null;
}

function newRows() {
  const text = (value: Cell) => (value === null ? "" : String(value));
  return {
    newColumns: newKeys.value.columns.map((key) => {
      const defaultValue = currentValue("columns", key, "defaultValue");
      return {
        name: text(currentValue("columns", key, "name")),
        dataType: text(currentValue("columns", key, "dataType")),
        nullable: currentValue("columns", key, "nullable") === "YES",
        defaultValue: defaultValue === null ? null : String(defaultValue),
      };
    }),
    newIndexes: newKeys.value.indexes.map((key) => ({
      name: text(currentValue("indexes", key, "name")),
      columns: text(currentValue("indexes", key, "columns")),
      unique: currentValue("indexes", key, "unique") === "UNIQUE",
    })),
  };
}

function pendingChanges() {
  grid.value?.commitEdit();
  const columnChanges = new Map<string, ColumnChange>();
  const indexChanges = new Map<string, IndexChange>();
  for (const edit of pending.value.values()) {
    const text = edit.value === null ? null : String(edit.value);
    if (isNewKey(edit.key)) {
      continue;
    }
    if (edit.section === "indexes") {
      const change = indexChanges.get(edit.key) ?? { index: edit.key };
      if (edit.field === "unique") {
        change.unique = edit.value === "UNIQUE";
      } else if (edit.field === "name" || edit.field === "columns") {
        change[edit.field] = text ?? "";
      }
      indexChanges.set(edit.key, change);
      continue;
    }
    const change = columnChanges.get(edit.key) ?? { column: edit.key };
    if (edit.field === "nullable") {
      change.nullable = edit.value === "YES";
    } else if (edit.field === "defaultValue") {
      change.defaultValue = text;
    } else if (edit.field === "name" || edit.field === "dataType") {
      change[edit.field] = text ?? "";
    }
    columnChanges.set(edit.key, change);
  }
  return {
    columns: [...columnChanges.values()],
    indexes: [...indexChanges.values()],
    ...newRows(),
    snapshot: {
      fields: pending.value,
      created: [...newKeys.value.columns, ...newKeys.value.indexes],
    },
  };
}

function markSaved(sent: { fields: Map<string, PendingField>; created: string[] }) {
  const remaining = new Map(pending.value);
  const created = new Set(sent.created);
  for (const [id, edit] of sent.fields) {
    if (!isNewKey(edit.key) && remaining.get(id)?.value === edit.value) {
      remaining.delete(id);
    }
  }
  for (const [id, edit] of remaining) {
    if (created.has(edit.key)) {
      remaining.delete(id);
    }
  }
  newKeys.value = {
    columns: newKeys.value.columns.filter((key) => !created.has(key)),
    indexes: newKeys.value.indexes.filter((key) => !created.has(key)),
  };
  pending.value = remaining;
  undoStack.length = 0;
  redoStack.length = 0;
}

watch(changedCount, (count) => emit("changes", count));

defineExpose({ pendingChanges, markSaved, discard, undo, redo, create });
</script>

<template>
  <DataGrid
    ref="grid"
    :columns="columns"
    :rows="rows"
    :cell-editable="cellEditable"
    :new-row-start="existingCount[section]"
    :creatable="editable"
    :modified="modified"
    @edit="onEdit"
    @set-null="onSetNull"
    @create="create"
  />
</template>
