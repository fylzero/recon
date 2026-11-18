<script setup lang="ts">
import { readText as readClipboard, writeText as writeClipboard } from "@tauri-apps/plugin-clipboard-manager";
import { confirm } from "@tauri-apps/plugin-dialog";
import { computed, nextTick, ref, watch } from "vue";
import * as api from "../api";
import type { CopyFormat } from "../cells";
import { useApp } from "../composables/useApp";
import { placeAtPoint, useDismiss } from "../composables/usePopover";
import { pastedCells } from "../gridClipboard";
import { ariaModifier, shortcut } from "../platform";
import { displayBytes, formatBytesSize, formatTtl, kindLabel, prettyJson, textBytes } from "../redis";
import type { Cell, ColumnMeta, RedisBytes, RedisEdit, RedisFieldValue, RedisKeyEntry, RedisKeyInfo } from "../types";
import DataGrid, { type CellPosition, type CellRange } from "./DataGrid.vue";
import Modal from "./Modal.vue";

const PAGE = 500;

const props = defineProps<{
  sessionId: string;
  entry: RedisKeyEntry;
}>();

const emit = defineEmits<{
  deleted: [key: RedisBytes];
  renamed: [from: RedisBytes, to: RedisBytes];
  failed: [error: string];
}>();

const { showToast } = useApp();

/** One loaded item. `values` are the raw bytes behind each column, or null for read-only columns. */
interface Item {
  id: string;
  cells: Cell[];
  values: (RedisBytes | null)[];
  /** The list index or stream ID. */
  position: number | string;
}

const info = ref<RedisKeyInfo | null>(null);
const items = ref<Item[]>([]);
const cursor = ref<string | null>(null);
const loading = ref(false);
const error = ref("");
const filter = ref("");
const otherType = ref("");

const stringValue = ref<RedisBytes | null>(null);
const stringSize = ref(0);
const stringTruncated = ref(false);
const stringDraft = ref("");
const stringMode = ref<"text" | "json" | "hex">("text");
const savingString = ref(false);

const kind = computed(() => info.value?.kind ?? props.entry.kind);
const key = computed(() => props.entry.key);

const COLUMNS: Record<string, ColumnMeta[]> = {
  hash: [
    { name: "field", typeName: "text" },
    { name: "value", typeName: "text" },
  ],
  list: [
    { name: "index", typeName: "integer" },
    { name: "value", typeName: "text" },
  ],
  set: [{ name: "member", typeName: "text" }],
  zset: [
    { name: "member", typeName: "text" },
    { name: "score", typeName: "double" },
  ],
  stream: [
    { name: "id", typeName: "text" },
    { name: "fields", typeName: "json" },
  ],
};

const columns = computed(() => COLUMNS[kind.value] ?? []);
/** Columns Redis fills in: list positions and stream IDs. */
const autoColumns = computed(() => columns.value.map((column) => column.name === "index" || column.name === "id"));

const lengthLabel = computed(() => {
  const length = info.value?.length;
  if (length === null || length === undefined) return "";
  switch (kind.value) {
    case "string":
      return formatBytesSize(length);
    case "hash":
      return `${length.toLocaleString()} ${length === 1 ? "field" : "fields"}`;
    case "stream":
      return `${length.toLocaleString()} ${length === 1 ? "entry" : "entries"}`;
    default:
      return `${length.toLocaleString()} ${length === 1 ? "item" : "items"}`;
  }
});

function bytesCell(value: RedisBytes): Cell {
  return value.hex === undefined ? value.text : { bytes: value.hex.length / 2, hex: value.hex };
}

function streamFields(fields: RedisFieldValue[]) {
  return JSON.stringify(Object.fromEntries(fields.map((pair) => [pair.field.text, pair.value.text])));
}

/* Strings. */

const isBinaryString = computed(() => stringValue.value?.hex !== undefined);
const stringDirty = computed(() => {
  const value = stringValue.value;
  if (!value || stringTruncated.value) return false;
  if (stringMode.value === "hex") return stringDraft.value.replace(/\s/g, "") !== (value.hex ?? toHex(value.text));
  if (stringMode.value === "json") return stringDraft.value !== (prettyJson(value.text) ?? value.text);
  return stringDraft.value !== value.text;
});

function toHex(text: string) {
  return [...new TextEncoder().encode(text)].map((byte) => byte.toString(16).padStart(2, "0")).join("");
}

function resetStringDraft() {
  const value = stringValue.value;
  if (!value) return;
  if (stringMode.value === "hex") {
    stringDraft.value = value.hex ?? toHex(value.text);
  } else if (stringMode.value === "json") {
    stringDraft.value = prettyJson(value.text) ?? value.text;
  } else {
    stringDraft.value = value.text;
  }
}

watch(stringMode, resetStringDraft);

async function saveString() {
  const value: RedisBytes =
    stringMode.value === "hex"
      ? { text: "", hex: stringDraft.value.replace(/\s/g, "") }
      : textBytes(stringMode.value === "json" ? compactJson(stringDraft.value) : stringDraft.value);
  savingString.value = true;
  try {
    await api.redisEdit(props.sessionId, { op: "setString", key: key.value, value });
    await loadPage(true);
  } catch (err) {
    showToast(String(err), "error");
  } finally {
    savingString.value = false;
  }
}

/** Keeps JSON as compact as it was stored, unless the edit broke it. */
function compactJson(text: string) {
  try {
    return JSON.stringify(JSON.parse(text));
  } catch {
    return text;
  }
}

/* Loading. */

function fail(err: unknown) {
  const message = String(err);
  error.value = message;
  emit("failed", message);
}

async function loadInfo() {
  info.value = await api.redisKeyInfo(props.sessionId, key.value);
}

function itemsFrom(page: Awaited<ReturnType<typeof api.redisRead>>): Item[] {
  switch (page.kind) {
    case "hash":
      return page.entries.map((entry) => ({
        id: `h:${entry.field.hex ?? entry.field.text}`,
        cells: [bytesCell(entry.field), bytesCell(entry.value)],
        values: [entry.field, entry.value],
        position: 0,
      }));
    case "list":
      return page.items.map((item) => ({
        id: `l:${item.index}`,
        cells: [item.index, bytesCell(item.value)],
        values: [null, item.value],
        position: item.index,
      }));
    case "set":
      return page.members.map((member) => ({
        id: `s:${member.hex ?? member.text}`,
        cells: [bytesCell(member)],
        values: [member],
        position: 0,
      }));
    case "zset":
      return page.members.map((member) => ({
        id: `z:${member.member.hex ?? member.member.text}`,
        cells: [bytesCell(member.member), member.score],
        values: [member.member, textBytes(String(member.score))],
        position: 0,
      }));
    case "stream":
      return page.entries.map((entry) => ({
        id: `x:${entry.id}`,
        cells: [entry.id, streamFields(entry.fields)],
        values: [null, null],
        position: entry.id,
      }));
    default:
      return [];
  }
}

async function loadPage(reset: boolean) {
  if (loading.value) return;
  loading.value = true;
  error.value = "";
  try {
    if (reset) {
      await loadInfo();
      items.value = [];
      cursor.value = null;
      clearPending();
    }
    if (info.value?.ttlMs === -2) {
      error.value = "This key no longer exists.";
      return;
    }
    const page = await api.redisRead(props.sessionId, key.value, kind.value, reset ? null : cursor.value, PAGE);
    if (page.kind === "string") {
      stringValue.value = page.value;
      stringSize.value = page.size;
      stringTruncated.value = page.truncated;
      if (reset) {
        stringMode.value = page.value.hex !== undefined ? "hex" : prettyJson(page.value.text) ? "json" : "text";
      }
      resetStringDraft();
      return;
    }
    if (page.kind === "other") {
      otherType.value = page.typeName;
      return;
    }
    // HSCAN and SSCAN can return an item again on a later page.
    const seen = new Set(items.value.map((item) => item.id));
    items.value = [...items.value, ...itemsFrom(page).filter((item) => !seen.has(item.id))];
    cursor.value = page.cursor;
  } catch (err) {
    fail(err);
  } finally {
    loading.value = false;
  }
}

/* Pending grid changes, saved together like table edits. */

/** Edited cell text by item index, then column. */
const edits = ref(new Map<number, Map<number, string>>());
/** Item indexes marked for removal. */
const removed = ref(new Set<number>());
/** New rows, one string per column. */
const added = ref<string[][]>([]);
const saving = ref(false);
const grid = ref<InstanceType<typeof DataGrid> | null>(null);

function clearPending() {
  edits.value = new Map();
  removed.value = new Set();
  added.value = [];
}

const changeCount = computed(() => {
  let count = removed.value.size + added.value.length;
  for (const [index] of edits.value) {
    if (!removed.value.has(index)) count += 1;
  }
  return count;
});

const hasChanges = computed(() => changeCount.value > 0);

/** Item indexes shown in the grid, after the filter. New rows always show, after these. */
const visible = computed(() => {
  const needle = filter.value.trim().toLowerCase();
  const indexes = items.value.map((_, index) => index);
  if (!needle) return indexes;
  return indexes.filter((index) =>
    items.value[index].cells.some((cell) => typeof cell !== "object" && String(cell).toLowerCase().includes(needle)),
  );
});

function itemAt(row: number): number | null {
  return row < visible.value.length ? visible.value[row] : null;
}

const gridRows = computed<Cell[][]>(() => [
  ...visible.value.map((index) => {
    const edited = edits.value.get(index);
    return items.value[index].cells.map((cell, col) => (edited?.has(col) ? edited.get(col)! : cell));
  }),
  ...added.value.map((row) => [...row]),
]);

const modified = computed(() => {
  const map = new Map<number, Set<number>>();
  visible.value.forEach((index, row) => {
    const edited = edits.value.get(index);
    if (edited?.size) map.set(row, new Set(edited.keys()));
  });
  return map;
});

const deletedRows = computed(() => {
  const rows = new Set<number>();
  visible.value.forEach((index, row) => {
    if (removed.value.has(index)) rows.add(row);
  });
  return rows;
});

function cellEditable(row: number, col: number) {
  if (autoColumns.value[col]) return false;
  const index = itemAt(row);
  if (index === null) return true;
  if (kind.value === "stream" || removed.value.has(index)) return false;
  return items.value[index].values[col]?.hex === undefined;
}

function onEdit(row: number, col: number, text: string) {
  const index = itemAt(row);
  if (index === null) {
    const next = added.value.map((cells) => [...cells]);
    next[row - visible.value.length][col] = text;
    added.value = next;
    return;
  }
  const next = new Map(edits.value);
  const cells = new Map(next.get(index) ?? []);
  const original = items.value[index].cells[col];
  if (text === String(original)) cells.delete(col);
  else cells.set(col, text);
  if (cells.size) next.set(index, cells);
  else next.delete(index);
  edits.value = next;
}

/** Marks rows for removal, or restores them when all were already marked. New rows are dropped. */
function toggleRemove(rows: number[]) {
  const indexes = rows.map(itemAt).filter((index): index is number => index !== null);
  const newRows = rows
    .filter((row) => itemAt(row) === null)
    .map((row) => row - visible.value.length)
    .sort((a, b) => b - a);
  if (newRows.length) {
    const next = [...added.value];
    for (const at of newRows) next.splice(at, 1);
    added.value = next;
  }
  if (!indexes.length) return;
  const next = new Set(removed.value);
  const restore = indexes.every((index) => next.has(index));
  for (const index of indexes) {
    if (restore) next.delete(index);
    else next.add(index);
  }
  removed.value = next;
}

function onSetNull(cells: CellPosition[]) {
  toggleRemove([...new Set(cells.map((cell) => cell.row))]);
}

function emptyRow(): string[] {
  return columns.value.map((column) => (column.name === "score" ? "0" : column.name === "fields" ? "{}" : ""));
}

async function createRow() {
  added.value = [...added.value, emptyRow()];
  await nextTick();
  const row = gridRows.value.length - 1;
  const col = autoColumns.value.findIndex((auto) => !auto);
  grid.value?.editCell(row, Math.max(col, 0));
}

function discard() {
  clearPending();
}

function cellString(value: Cell | undefined) {
  return value === null || value === undefined ? "" : typeof value === "object" ? "" : String(value);
}

async function paste(range: CellRange | null, asNewRows: boolean) {
  grid.value?.commitEdit();
  let text: string;
  try {
    text = await readClipboard();
  } catch {
    text = "";
  }
  const pasted = pastedCells(text);
  if (!pasted?.rows.length) {
    showToast("The clipboard has no text to paste.", "error");
    return;
  }
  const editable = columns.value.map((_, col) => col).filter((col) => !autoColumns.value[col]);
  if (asNewRows || !range) {
    const rows = pasted.rows.map((source) => {
      const values = source.length > editable.length ? source.slice(source.length - editable.length) : source;
      const row = emptyRow();
      editable.forEach((col, at) => {
        if (values[at] !== undefined) row[col] = cellString(values[at]);
      });
      return row;
    });
    added.value = [...added.value, ...rows];
    showToast(rows.length === 1 ? "Added 1 row" : `Added ${rows.length.toLocaleString()} rows`);
    return;
  }
  const height = range.bottom - range.top + 1;
  const count = height > pasted.rows.length && height % pasted.rows.length === 0 ? height : pasted.rows.length;
  let skipped = 0;
  for (let at = 0; at < count; at += 1) {
    const row = range.top + at;
    if (row >= gridRows.value.length) break;
    const source = pasted.rows[at % pasted.rows.length];
    source.forEach((value, offset) => {
      const col = range.left + offset;
      if (col >= columns.value.length) return;
      if (!cellEditable(row, col)) {
        skipped += 1;
        return;
      }
      onEdit(row, col, cellString(value));
    });
  }
  if (skipped) showToast("Some cells can't be edited, so they were skipped.", "error");
}

function changeError(): string | null {
  for (const [index, cells] of edits.value) {
    if (removed.value.has(index)) continue;
    if (kind.value === "hash" && cells.get(0) === "") return "A hash field needs a name.";
    if (kind.value === "zset" && cells.has(1) && !Number.isFinite(Number(cells.get(1)))) return "A score must be a number.";
  }
  for (const row of added.value) {
    if (kind.value === "hash" && !row[0]) return "A new hash field needs a name.";
    if (kind.value === "zset" && (row[1].trim() === "" || !Number.isFinite(Number(row[1])))) {
      return "A score must be a number.";
    }
    if (kind.value === "stream") {
      const fields = streamInput(row[1]);
      if (typeof fields === "string") return fields;
    }
  }
  return null;
}

function streamInput(text: string): [RedisBytes, RedisBytes][] | string {
  let parsed: unknown;
  try {
    parsed = JSON.parse(text);
  } catch {
    return 'Stream fields are a JSON object, like {"event": "signup"}.';
  }
  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
    return 'Stream fields are a JSON object, like {"event": "signup"}.';
  }
  const pairs = Object.entries(parsed as Record<string, unknown>);
  if (!pairs.length) return "A stream entry needs at least one field.";
  return pairs.map(([field, value]) => [
    textBytes(field),
    textBytes(typeof value === "string" ? value : JSON.stringify(value)),
  ]);
}

/** The pending changes as Redis edits, in the order the batch needs. */
function pendingEdits(): RedisEdit[] {
  const k = key.value;
  const sets: RedisEdit[] = [];
  const removals: RedisEdit[] = [];
  const additions: RedisEdit[] = [];
  const removedItems = [...removed.value].map((index) => items.value[index]);
  for (const [index, cells] of edits.value) {
    if (removed.value.has(index)) continue;
    const item = items.value[index];
    const text = (col: number) => (cells.has(col) ? textBytes(cells.get(col)!) : item.values[col]!);
    switch (kind.value) {
      case "hash":
        sets.push({ op: "hashSet", key: k, field: text(0), value: text(1), original: cells.has(0) ? item.values[0] : null });
        break;
      case "list":
        sets.push({ op: "listSet", key: k, index: item.position as number, value: text(1) });
        break;
      case "set":
        sets.push({ op: "setAdd", key: k, member: text(0), original: item.values[0] });
        break;
      case "zset":
        sets.push({
          op: "zsetAdd",
          key: k,
          member: text(0),
          score: Number(cells.get(1) ?? item.cells[1]),
          original: cells.has(0) ? item.values[0] : null,
        });
        break;
    }
  }
  if (removedItems.length) {
    switch (kind.value) {
      case "hash":
        removals.push({ op: "hashDelete", key: k, fields: removedItems.map((item) => item.values[0]!) });
        break;
      case "list":
        removals.push({ op: "listRemove", key: k, indexes: removedItems.map((item) => item.position as number) });
        break;
      case "set":
        removals.push({ op: "setRemove", key: k, members: removedItems.map((item) => item.values[0]!) });
        break;
      case "zset":
        removals.push({ op: "zsetRemove", key: k, members: removedItems.map((item) => item.values[0]!) });
        break;
      case "stream":
        removals.push({ op: "streamDelete", key: k, ids: removedItems.map((item) => item.position as string) });
        break;
    }
  }
  for (const row of added.value) {
    switch (kind.value) {
      case "hash":
        additions.push({ op: "hashSet", key: k, field: textBytes(row[0]), value: textBytes(row[1]), original: null });
        break;
      case "list":
        additions.push({ op: "listPush", key: k, value: textBytes(row[1]), head: false });
        break;
      case "set":
        additions.push({ op: "setAdd", key: k, member: textBytes(row[0]), original: null });
        break;
      case "zset":
        additions.push({ op: "zsetAdd", key: k, member: textBytes(row[0]), score: Number(row[1]), original: null });
        break;
      case "stream":
        additions.push({ op: "streamAdd", key: k, fields: streamInput(row[1]) as [RedisBytes, RedisBytes][] });
        break;
    }
  }
  return [...sets, ...removals, ...additions];
}

async function save() {
  grid.value?.commitEdit();
  if (!hasChanges.value || saving.value) return;
  const problem = changeError();
  if (problem) {
    showToast(problem, "error");
    return;
  }
  saving.value = true;
  try {
    await api.redisEditBatch(props.sessionId, pendingEdits());
    await loadPage(true);
    if (info.value?.ttlMs === -2) {
      // Redis deletes a collection once its last item goes.
      emit("deleted", key.value);
    }
  } catch (err) {
    showToast(String(err), "error");
  } finally {
    saving.value = false;
  }
}

function onKeydown(event: KeyboardEvent) {
  if ((event.metaKey || event.ctrlKey) && !event.shiftKey && event.key.toLowerCase() === "s") {
    event.preventDefault();
    if (kind.value === "string") {
      if (stringDirty.value) void saveString();
    } else {
      void save();
    }
  }
}

/* Right-click menus, like the table grid's. */

const cellMenu = ref<{ rows: number[] } | null>(null);
const blankMenu = ref(false);
const menuPosition = ref({ left: 0, top: 0 });
const menuEl = ref<HTMLElement | null>(null);

function closeMenus() {
  cellMenu.value = null;
  blankMenu.value = false;
}

useDismiss(
  computed(() => Boolean(cellMenu.value || blankMenu.value)),
  () => [menuEl.value],
  closeMenus,
);

function placeMenu(event: MouseEvent) {
  menuPosition.value = { left: event.clientX, top: event.clientY };
  void nextTick(() => {
    menuPosition.value = placeAtPoint(event.clientX, event.clientY, menuEl.value);
  });
}

function onCellMenu(row: number, _col: number, event: MouseEvent) {
  closeMenus();
  const selected = grid.value?.selectedRows() ?? [];
  cellMenu.value = { rows: selected.includes(row) ? selected : [row] };
  placeMenu(event);
}

function onRowMenu(row: number, event: MouseEvent) {
  onCellMenu(row, 0, event);
}

function onBlankMenu(event: MouseEvent) {
  closeMenus();
  blankMenu.value = true;
  placeMenu(event);
}

const menuRestores = computed(() => {
  const rows = cellMenu.value?.rows ?? [];
  const indexes = rows.map(itemAt).filter((index): index is number => index !== null);
  return indexes.length === rows.length && indexes.length > 0 && indexes.every((index) => removed.value.has(index));
});

const menuRemoveLabel = computed(() => {
  const count = cellMenu.value?.rows.length ?? 1;
  const verb = menuRestores.value ? "Restore" : "Delete";
  return count === 1 ? `${verb} row` : `${verb} ${count.toLocaleString()} rows`;
});

function copyFromMenu(format: CopyFormat) {
  closeMenus();
  void grid.value?.copySelection(format);
}

function pasteFromMenu(asNewRows: boolean) {
  closeMenus();
  void paste(asNewRows ? null : (grid.value?.selectedRange() ?? null), asNewRows);
}

function removeFromMenu() {
  const rows = cellMenu.value?.rows ?? [];
  closeMenus();
  toggleRemove(rows);
}

function createFromMenu() {
  closeMenus();
  void createRow();
}

/* Key actions: rename, TTL, delete. */

const dialog = ref<
  | { type: "rename"; name: string; overwrite: boolean; error: string }
  | { type: "ttl"; seconds: string; forever: boolean; error: string }
  | null
>(null);
const busy = ref(false);

function focusDialog() {
  void nextTick(() => {
    const field = document.querySelector<HTMLInputElement>(".redis-dialog input[type='text']:not(:disabled)");
    field?.focus();
    field?.select();
  });
}

function openRename() {
  dialog.value = { type: "rename", name: key.value.text, overwrite: false, error: "" };
  focusDialog();
}

function openTtl() {
  const ttl = info.value?.ttlMs ?? -1;
  dialog.value = {
    type: "ttl",
    seconds: ttl > 0 ? String(Math.round(ttl / 1000)) : "3600",
    forever: ttl < 0,
    error: "",
  };
  focusDialog();
}

async function submitRename() {
  const current = dialog.value;
  if (current?.type !== "rename") return;
  if (!current.name) {
    current.error = "A key needs a name.";
    return;
  }
  const to = textBytes(current.name);
  if (to.text === key.value.text && key.value.hex === undefined) {
    dialog.value = null;
    return;
  }
  busy.value = true;
  try {
    await api.redisEdit(props.sessionId, { op: "rename", key: key.value, to, overwrite: current.overwrite });
    dialog.value = null;
    emit("renamed", key.value, to);
  } catch (err) {
    current.error = String(err);
  } finally {
    busy.value = false;
  }
}

async function submitTtl() {
  const current = dialog.value;
  if (current?.type !== "ttl") return;
  let seconds: number | null = null;
  if (!current.forever) {
    seconds = Number(current.seconds);
    if (!Number.isInteger(seconds) || seconds < 1) {
      current.error = "Enter a whole number of seconds, 1 or more.";
      return;
    }
  }
  busy.value = true;
  try {
    await api.redisEdit(props.sessionId, { op: "expire", key: key.value, seconds });
    dialog.value = null;
    await loadInfo();
  } catch (err) {
    current.error = String(err);
  } finally {
    busy.value = false;
  }
}

async function deleteKey() {
  const ok = await confirm(`Delete the key “${key.value.text}”? This can't be undone.`, {
    title: "Delete key",
    kind: "warning",
    okLabel: "Delete",
  });
  if (!ok) return;
  try {
    await api.redisEdit(props.sessionId, { op: "delete", keys: [key.value] });
    emit("deleted", key.value);
  } catch (err) {
    showToast(String(err), "error");
  }
}

async function copyName() {
  try {
    await writeClipboard(key.value.text);
    showToast("Copied the key name.");
  } catch (err) {
    showToast(String(err), "error");
  }
}

function reload() {
  void loadPage(true);
}

watch(
  () => [props.sessionId, props.entry.key.text, props.entry.key.hex],
  () => {
    filter.value = "";
    stringValue.value = null;
    otherType.value = "";
    info.value = null;
    closeMenus();
    reload();
  },
  { immediate: true },
);

defineExpose({ reload, hasChanges: () => hasChanges.value || stringDirty.value });
</script>

<template>
  <section class="redis-key" @keydown="onKeydown">
    <header class="redis-key-header">
      <div class="redis-key-title">
        <span class="redis-kind-tag" :class="`kind-${kind}`">{{ kindLabel(kind) }}</span>
        <strong class="redis-key-name" :title="displayBytes(key, 2000)">{{ displayBytes(key, 300) }}</strong>
      </div>
      <div class="redis-key-actions">
        <button class="ghost tiny" type="button" title="Copy the key name" @click="copyName">Copy name</button>
        <button class="ghost tiny" type="button" :disabled="key.hex !== undefined" @click="openRename">Rename</button>
        <button class="ghost tiny" type="button" @click="openTtl">TTL</button>
        <button
          class="ghost tiny"
          type="button"
          :title="hasChanges ? 'Save or discard your changes first' : 'Reload this key'"
          :disabled="loading || hasChanges"
          @click="reload"
        >
          Refresh
        </button>
        <button class="ghost tiny danger" type="button" @click="deleteKey">Delete</button>
      </div>
      <div class="redis-key-meta muted tiny">
        <span v-if="info" :class="{ 'redis-ttl-set': info.ttlMs >= 0 }">TTL {{ formatTtl(info.ttlMs) }}</span>
        <span v-if="lengthLabel">{{ lengthLabel }}</span>
        <span v-if="info?.memory != null">{{ formatBytesSize(info.memory) }} in memory</span>
        <span v-if="info?.encoding">{{ info.encoding }}</span>
      </div>
    </header>

    <p v-if="error" class="settings-error redis-key-error">{{ error }}</p>

    <div v-else-if="kind === 'string'" class="redis-string">
      <div class="redis-string-bar">
        <div class="segmented" role="radiogroup" aria-label="Show the value as">
          <button
            v-for="mode in (['text', 'json', 'hex'] as const)"
            :key="mode"
            type="button"
            role="radio"
            :aria-checked="stringMode === mode"
            :class="{ active: stringMode === mode }"
            :disabled="mode === 'text' && isBinaryString"
            @click="stringMode = mode"
          >
            {{ mode === "json" ? "JSON" : mode === "hex" ? "Hex" : "Text" }}
          </button>
        </div>
        <span v-if="stringTruncated" class="muted tiny">
          Showing the first 1 MB of {{ formatBytesSize(stringSize) }}, so it can't be edited here.
        </span>
        <span v-else-if="stringMode === 'json' && stringValue && !prettyJson(stringValue.text)" class="muted tiny">
          Not valid JSON.
        </span>
        <div v-if="stringDirty" class="redis-string-actions">
          <button class="ghost tiny" type="button" :disabled="savingString" @click="resetStringDraft">Discard</button>
          <button
            class="primary tiny"
            type="button"
            :title="`Save (${shortcut('⌘S')})`"
            :disabled="savingString"
            @click="saveString"
          >
            Save
          </button>
        </div>
      </div>
      <textarea
        v-model="stringDraft"
        class="redis-string-editor"
        spellcheck="false"
        :readonly="stringTruncated"
        :aria-label="`Value of ${key.text}`"
      />
    </div>

    <div v-else-if="otherType" class="redis-key-empty">
      <p class="muted">
        Recon can't show {{ otherType }} keys yet. Use the console to read this key with its module's commands.
      </p>
    </div>

    <div v-else class="redis-collection">
      <div class="redis-collection-bar">
        <input v-model="filter" type="search" placeholder="Filter loaded items" spellcheck="false" />
        <div v-if="hasChanges" class="redis-save-bar">
          <span v-if="saving" class="spinner" aria-label="Saving" />
          <span class="tiny edit-status">
            {{ changeCount === 1 ? "1 unsaved change" : `${changeCount.toLocaleString()} unsaved changes` }}
          </span>
          <button class="ghost tiny" type="button" :disabled="saving" @click="discard">Discard</button>
          <button
            class="primary tiny"
            type="button"
            :title="`Save (${shortcut('⌘S')})`"
            :disabled="saving"
            @click="save"
          >
            Save
          </button>
        </div>
      </div>
      <div class="redis-grid">
        <DataGrid
          ref="grid"
          :columns="columns"
          :rows="gridRows"
          :cell-editable="cellEditable"
          :new-row-start="visible.length"
          :new-row-auto="autoColumns"
          :modified="modified"
          :deleted-rows="deletedRows"
          creatable
          deletable
          pastable
          context-menus
          @edit="onEdit"
          @set-null="onSetNull"
          @delete-rows="toggleRemove"
          @create="createRow"
          @cell-menu="onCellMenu"
          @row-menu="onRowMenu"
          @blank-menu="onBlankMenu"
          @paste="paste"
        />
      </div>
      <div class="redis-grid-footer">
        <span class="muted tiny">
          <template v-if="loading"><span class="spinner" aria-hidden="true" /> Loading…</template>
          <template v-else-if="filter && visible.length !== items.length">
            {{ visible.length.toLocaleString() }} of {{ items.length.toLocaleString() }} loaded items match
          </template>
          <template v-else-if="cursor">
            {{ items.length.toLocaleString() }} of {{ info?.length?.toLocaleString() ?? "?" }} loaded
          </template>
          <template v-else>{{ items.length.toLocaleString() }} {{ items.length === 1 ? "item" : "items" }}</template>
        </span>
        <button v-if="cursor && !loading" class="ghost tiny" type="button" @click="loadPage(false)">Load more</button>
      </div>
    </div>

    <Teleport to="body">
      <div
        v-if="cellMenu || blankMenu"
        ref="menuEl"
        class="overflow-menu-dropdown table-context-menu"
        role="menu"
        :style="{ left: `${menuPosition.left}px`, top: `${menuPosition.top}px` }"
        @contextmenu.prevent
      >
        <template v-if="cellMenu">
          <button class="overflow-menu-item" type="button" role="menuitem" :aria-keyshortcuts="`${ariaModifier}+C`" @click="copyFromMenu('tsv')">
            Copy
          </button>
          <button class="overflow-menu-item" type="button" role="menuitem" @click="copyFromMenu('csv')">
            Copy as CSV
          </button>
          <button class="overflow-menu-item" type="button" role="menuitem" @click="copyFromMenu('json')">
            Copy as JSON
          </button>
          <button class="overflow-menu-item" type="button" role="menuitem" :aria-keyshortcuts="`${ariaModifier}+V`" @click="pasteFromMenu(false)">
            Paste
          </button>
          <button class="overflow-menu-item" type="button" role="menuitem" :aria-keyshortcuts="`${ariaModifier}+Shift+V`" @click="pasteFromMenu(true)">
            Paste as new rows
          </button>
          <div class="overflow-menu-divider" role="separator" />
          <button
            class="overflow-menu-item"
            :class="{ danger: !menuRestores }"
            type="button"
            role="menuitem"
            aria-keyshortcuts="Backspace"
            @click="removeFromMenu"
          >
            {{ menuRemoveLabel }}
          </button>
        </template>
        <template v-else>
          <button class="overflow-menu-item" type="button" role="menuitem" @click="createFromMenu">New row</button>
          <button class="overflow-menu-item" type="button" role="menuitem" :aria-keyshortcuts="`${ariaModifier}+Shift+V`" @click="pasteFromMenu(true)">
            Paste as new rows
          </button>
        </template>
      </div>
    </Teleport>

    <Modal v-if="dialog?.type === 'rename'" title="Rename key" @close="dialog = null">
      <form class="redis-dialog" @submit.prevent="submitRename">
        <label class="modal-label">
          <span class="muted tiny">New name</span>
          <input v-model="dialog.name" type="text" spellcheck="false" />
        </label>
        <label class="checkbox-row">
          <input v-model="dialog.overwrite" type="checkbox" />
          <span>Replace a key that already has this name</span>
        </label>
        <p v-if="dialog.error" class="settings-error">{{ dialog.error }}</p>
        <div class="modal-actions">
          <button class="ghost" type="button" @click="dialog = null">Cancel</button>
          <button class="primary" type="submit" :disabled="busy">Rename</button>
        </div>
      </form>
    </Modal>

    <Modal v-if="dialog?.type === 'ttl'" title="Time to live" @close="dialog = null">
      <form class="redis-dialog" @submit.prevent="submitTtl">
        <label class="checkbox-row">
          <input v-model="dialog.forever" type="checkbox" />
          <span>Never expire</span>
        </label>
        <label class="modal-label">
          <span class="muted tiny">Expire in (seconds)</span>
          <input v-model="dialog.seconds" type="text" inputmode="numeric" :disabled="dialog.forever" />
        </label>
        <p v-if="dialog.error" class="settings-error">{{ dialog.error }}</p>
        <div class="modal-actions">
          <button class="ghost" type="button" @click="dialog = null">Cancel</button>
          <button class="primary" type="submit" :disabled="busy">Save</button>
        </div>
      </form>
    </Modal>
  </section>
</template>
