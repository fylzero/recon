<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, shallowRef, watch } from "vue";
import { readText as readClipboard, writeText as writeClipboard } from "@tauri-apps/plugin-clipboard-manager";
import * as api from "../api";
import { useApp } from "../composables/useApp";
import { useGridFind } from "../composables/useGridFind";
import { useOverflowMenu } from "../composables/useOverflowMenu";
import { placeAtPoint, useDismiss, type PopoverPosition } from "../composables/usePopover";
import { isBytes, isNumericColumn, type CopyFormat } from "../cells";
import {
  compileFilter,
  formatDate,
  normalizeFilter,
  previewFilter,
  type CompiledFilter,
  type FilterColumn,
  type FilterPreviewGroup,
  type WireNode,
} from "../filters/compile";
import {
  appendChild,
  dropBlankConditions,
  emptyGroup,
  filterFromClipboard,
  filterToClipboard,
  hasConditions,
  mergeFilters,
  AUTO_REFRESH_PRESETS,
  MAX_CONDITIONS,
  MAX_TAB_PAGE_SIZE,
  TAB_PAGE_SIZES,
  clampAutoRefreshMs,
  clampTabPageSize,
  conditions as allConditions,
  formatAutoRefresh,
  formatCountdown,
  newCondition,
  newId,
  renameColumn,
  type FilterCondition,
  type FilterGroup,
  type TableViewState,
} from "../filters/model";
import { convertValue, DEFAULT_OPERATOR } from "../filters/operators";
import type {
  BrowseRequest,
  BrowseResult,
  Cell,
  CellEdit,
  ColumnDetail,
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
import ExportRowsDialog, { type ExportRowsSource } from "./ExportRowsDialog.vue";
import FilterPanel from "./FilterPanel.vue";
import FilterPopover from "./FilterPopover.vue";
import FilterSummary from "./FilterSummary.vue";
import GridFind from "./GridFind.vue";
import Modal from "./Modal.vue";
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
  /** Primary keys of saved rows marked for deletion. */
  deleted?: Cell[][];
  /** Primary keys of saved rows taken off the deletion list. */
  restored?: Cell[][];
}

const props = defineProps<{
  connectionId: string;
  driver: Driver;
  active: boolean;
  visible?: boolean;
  view: TableViewState;
}>();

const emit = defineEmits<{
  changes: [rows: number];
  follow: [link: TableLink, options?: { side?: boolean }];
  "update:view": [patch: Partial<TableViewState>];
  filterState: [state: { count: number; summary: string; preview: FilterPreviewGroup | null }];
  openSql: [sql: string];
}>();

const { pageSize: defaultPageSize, showToast, autoApplyFilters } = useApp();

/** Typing waits this long before the table reloads. */
const APPLY_DELAY = 400;
const COUNT_TIMEOUT_MS = 8_000;

interface AppliedFilter {
  key: string;
  wire: WireNode | null;
  count: number;
  summary: string;
  preview: FilterPreviewGroup | null;
}

function appliedFrom(current: CompiledFilter): AppliedFilter {
  return {
    key: current.key,
    wire: current.wire,
    count: current.appliedCount,
    summary: current.summary,
    preview: current.preview,
  };
}

const namespace = computed(() => props.view.namespace);
const table = computed(() => props.view.table);
const kind = computed(() => props.view.tableKind);

const mode = ref<"data" | "structure" | "indexes">("data");
const page = ref(0);
const sortColumn = computed(() => props.view.sort?.column ?? null);
const sortDir = computed<SortDirection>(() => props.view.sort?.dir ?? "asc");
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
const deleted = shallowRef(new Map<string, Cell[]>());
let nextNewRowId = 0;
let requestId = 0;
const panel = ref<InstanceType<typeof FilterPanel> | null>(null);
const clock = ref(new Date());
const applied = shallowRef<AppliedFilter>({ key: "", wire: null, count: 0, summary: "", preview: null });
const filterError = ref("");
const serverIssues = ref(new Map<string, string>());
const counting = ref(false);
const countTimedOut = ref(false);
const sqlPreview = ref<{ sql: string; loading: boolean; error: string } | null>(null);
const cellMenu = ref<{ x: number; y: number; row: number; col: number } | null>(null);
const rowMenu = ref<{ x: number; y: number } | null>(null);
const headerMenu = ref<{ x: number; y: number; col: number } | null>(null);
const menuRows = ref<number[]>([]);
const menuEl = ref<HTMLElement | null>(null);
const menuPosition = ref<PopoverPosition>({ left: 0, top: 0 });
let applyTimer = 0;
let debounceNext = false;
let browseToken = "";
let countToken = "";
const suggestionCache = new Map<string, Promise<string[]>>();

const filterColumns = computed<FilterColumn[] | null>(() => {
  if (!structure.value) {
    return structureError.value ? [] : null;
  }
  const indexed = new Set(
    structure.value.indexes.map((index) => index.columns.split(",")[0]?.trim().replace(/^[`"[]|[`"\]]$/g, "")),
  );
  return structure.value.columns.map((column) => ({
    name: column.name,
    kind: column.filterKind,
    nullable: column.nullable,
    enumValues: column.enumValues,
    indexed: indexed.has(column.name),
    dataType: column.dataType,
  }));
});
const columnMap = computed(() =>
  filterColumns.value ? new Map(filterColumns.value.map((column) => [column.name, column])) : null,
);
const compiled = computed(() =>
  compileFilter(props.view.filter, columnMap.value, {
    now: clock.value,
    columnsError: structureError.value || undefined,
  }),
);
const conditionCount = computed(() => allConditions(props.view.filter).filter((node) => node.column).length);
const filterPreview = computed(
  () => compiled.value.preview ?? applied.value.preview ?? previewFilter(props.view.filter, columnMap.value),
);
const filterSummaryText = computed(() => compiled.value.summary || applied.value.summary);
const filterCount = computed(
  () => applied.value.count || compiled.value.appliedCount || conditionCount.value,
);
/** Tab icon, Filter button, and collapsed chip: any filters on this tab, not only the last applied query. */
const filterActive = computed(() => filterCount.value > 0 || hasConditions(props.view.filter));
const summaryVisible = computed(() => filterActive.value && !(props.view.panelOpen && mode.value === "data"));
const summaryAnchor = ref<DOMRect | null>(null);
let summaryPopoverTimer = 0;

function showSummaryPopover(event: MouseEvent) {
  window.clearTimeout(summaryPopoverTimer);
  summaryAnchor.value = (event.currentTarget as HTMLElement).getBoundingClientRect();
}

function hideSummaryPopover() {
  window.clearTimeout(summaryPopoverTimer);
  summaryPopoverTimer = window.setTimeout(() => {
    summaryAnchor.value = null;
  }, 80);
}

// Opening the panel or clearing filters hides the summary under the pointer without a mouseleave.
watch(summaryVisible, (visible) => {
  if (!visible) {
    window.clearTimeout(summaryPopoverTimer);
    summaryAnchor.value = null;
  }
});
const unapplied = computed(() => !compiled.value.pending && compiled.value.key !== applied.value.key);

// First element of a new row's key, which no primary key value can equal.
const NEW_ROW = "\u0000new";

function isNewKey(key: Cell[]) {
  return key[0] === NEW_ROW;
}

// New rows leave these out of the INSERT so the database assigns the next value.
function isAutoIncrement(column: ColumnDetail) {
  const extra = column.extra.toLowerCase();
  return (
    extra.includes("auto_increment") ||
    extra === "identity" ||
    /^nextval\(/i.test(column.defaultValue ?? "")
  );
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
const autoColumns = computed(() => {
  const auto = new Set(
    (structure.value?.columns ?? []).filter(isAutoIncrement).map((column) => column.name),
  );
  return columns.value.map((column) => auto.has(column.name));
});
const editable = computed(() => kind.value === "table" && keyIndexes.value.length > 0);
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
    const target = key.refNamespace && key.refNamespace !== namespace.value
      ? `${key.refNamespace}.${key.refTable}`
      : key.refTable;
    return `${target}.${key.refColumns[key.columns.indexOf(column.name)]}`;
  }),
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
const find = useGridFind(() => displayRows.value);
const deletedRows = computed(() => {
  const indexes = new Set<number>();
  if (deleted.value.size) {
    pageKeys.value.forEach((key, index) => {
      if (key && deleted.value.has(JSON.stringify(key))) {
        indexes.add(index);
      }
    });
  }
  return indexes;
});
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
  () =>
    pending.value.size > 0 ||
    newRowIds.value.length > 0 ||
    deleted.value.size > 0 ||
    structureChanges.value > 0,
);
const refreshing = computed(() => (mode.value === "data" ? loading.value : loadingStructure.value));
const autoRefreshMenuId = newId("ar");
const {
  isOpen: autoRefreshMenuOpen,
  toggle: toggleAutoRefreshMenu,
  close: closeAutoRefreshMenu,
} = useOverflowMenu(() => autoRefreshMenuId);
const customRefresh = ref(false);
const customRefreshMinutes = ref(0);
const customRefreshSeconds = ref(30);
const autoRefresh = computed(() => props.view.autoRefresh);
const autoRefreshOn = computed(() => Boolean(autoRefresh.value));
const autoRefreshTitle = computed(() => {
  const current = autoRefresh.value;
  return current ? `Auto refresh every ${formatAutoRefresh(current.intervalMs)}` : "Auto refresh";
});
let autoRefreshTimer = 0;
let autoRefreshClock = 0;
let viewportObserver: IntersectionObserver | undefined;
const rootEl = ref<HTMLElement | null>(null);
const inViewport = ref(false);
const pageVisible = ref(typeof document === "undefined" || document.visibilityState === "visible");
const nextRefreshAt = ref(0);
const autoRefreshNow = ref(Date.now());
const autoRefreshArmed = computed(
  () => autoRefreshOn.value && props.visible !== false && inViewport.value && pageVisible.value,
);
const autoRefreshRemaining = computed(() => {
  if (!autoRefreshArmed.value) {
    return "";
  }
  if (loading.value && mode.value === "data") {
    return "Refreshing…";
  }
  const left = Math.max(0, nextRefreshAt.value - autoRefreshNow.value);
  if (left <= 0) {
    return "Refreshing…";
  }
  return `Refreshing in ${formatCountdown(left)}`;
});
const canInsert = computed(
  () => kind.value === "table" && Boolean(result.value) && Boolean(structure.value),
);

function cellEditable(row: number) {
  if (row >= rows.value.length) {
    return canInsert.value;
  }
  return editable.value && !deletedRows.value.has(row);
}

function rowDeletable(row: number) {
  return row >= rows.value.length ? row < allRows.value.length : editable.value && Boolean(pageKeys.value[row]);
}
const pageSize = computed(() =>
  clampTabPageSize(props.view.pageSize ?? Math.min(defaultPageSize.value, MAX_TAB_PAGE_SIZE)),
);
const pageSizeOptions = computed(() => {
  const sizes: number[] = [...TAB_PAGE_SIZES];
  if (!sizes.includes(pageSize.value)) {
    sizes.push(pageSize.value);
    sizes.sort((a, b) => a - b);
  }
  return sizes;
});
const offset = computed(() => page.value * pageSize.value);
const pageCount = computed(() =>
  total.value === null ? null : Math.max(Math.ceil(total.value / pageSize.value), 1),
);
const hasNext = computed(() =>
  total.value === null
    ? rows.value.length === pageSize.value
    : offset.value + rows.value.length < total.value,
);
const hasPrev = computed(() => page.value > 0);
const onlyOnePage = computed(
  () => pageCount.value === 1 || (pageCount.value === null && !hasPrev.value && !hasNext.value),
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
  let of = "";
  if (counting.value) {
    of = " of …";
  } else if (countTimedOut.value) {
    of = " of ?";
  } else if (total.value !== null) {
    of = ` of ${total.value.toLocaleString()}`;
  }
  return `Rows ${first.toLocaleString()}–${last.toLocaleString()}${of}`;
});
const rangeTitle = computed(() =>
  countTimedOut.value
    ? "Counting the matching rows took too long, so the total is unknown. Use › to keep paging."
    : undefined,
);

function browseRequest(extra: Partial<BrowseRequest> = {}): BrowseRequest {
  return {
    namespace: namespace.value,
    table: table.value,
    offset: offset.value,
    limit: pageSize.value,
    orderBy: sortColumn.value,
    orderDir: sortColumn.value ? sortDir.value : null,
    count: false,
    filter: applied.value.wire,
    utcOffset: -new Date().getTimezoneOffset(),
    ...extra,
  };
}

function cancelRunning() {
  for (const token of [browseToken, countToken]) {
    if (token) {
      void api.cancelBrowse(props.connectionId, token).catch(() => undefined);
    }
  }
  browseToken = "";
  countToken = "";
}

/** Backend filter errors arrive as JSON naming the condition that caused them. */
function filterErrorOf(message: string): { id: string | null; message: string } | null {
  if (!message.startsWith('{"filterError"')) {
    return null;
  }
  try {
    const parsed = JSON.parse(message) as { filterError?: { id?: string | null; message?: string } };
    return parsed.filterError ? { id: parsed.filterError.id ?? null, message: parsed.filterError.message ?? message } : null;
  } catch {
    return null;
  }
}

async function loadCount(id: number) {
  const token = newId("n");
  countToken = token;
  counting.value = true;
  countTimedOut.value = false;
  try {
    const next = await api.countRows(props.connectionId, browseRequest({ requestId: token }), COUNT_TIMEOUT_MS);
    if (id === requestId) {
      total.value = next;
      countTimedOut.value = next === null;
    }
  } catch {
    if (id === requestId) {
      total.value = null;
    }
  } finally {
    if (countToken === token) {
      countToken = "";
    }
    if (id === requestId) {
      counting.value = false;
    }
  }
}

async function loadData(count = false, scrollToTop = true) {
  cancelRunning();
  const id = ++requestId;
  const token = applied.value.wire ? newId("b") : "";
  browseToken = token;
  loading.value = true;
  error.value = "";
  if (count) {
    counting.value = false;
  }
  try {
    const next = await api.browseTable(props.connectionId, browseRequest(token ? { requestId: token } : {}));
    if (id !== requestId) {
      return;
    }
    result.value = next;
    filterError.value = "";
    serverIssues.value = new Map();
    if (scrollToTop) {
      grid.value?.scrollToTop();
    }
    if (count) {
      total.value = null;
      void loadCount(id);
    }
  } catch (err) {
    if (id !== requestId) {
      return;
    }
    const message = String(err);
    const problem = filterErrorOf(message);
    if (problem?.id) {
      serverIssues.value = new Map([[problem.id, problem.message]]);
      openPanel(false);
    } else if (problem || (applied.value.wire && result.value)) {
      filterError.value = problem?.message ?? message;
      openPanel(false);
    } else {
      error.value = message;
    }
  } finally {
    if (browseToken === token) {
      browseToken = "";
    }
    if (id === requestId) {
      loading.value = false;
    }
  }
}

/** Starts using the filter as edited. Waits for the table's columns when a filter needs them. */
function applyFilter(force = false) {
  window.clearTimeout(applyTimer);
  applyTimer = 0;
  const current = compiled.value;
  if (current.pending) {
    return;
  }
  if (!force && current.key === applied.value.key && (result.value || loading.value)) {
    return;
  }
  applied.value = appliedFrom(current);
  mode.value = "data";
  page.value = 0;
  void loadData(true);
}

function scheduleApply(delay: number) {
  window.clearTimeout(applyTimer);
  applyTimer = window.setTimeout(() => applyFilter(), delay);
}

async function loadStructure() {
  loadingStructure.value = true;
  structureError.value = "";
  try {
    structure.value = await api.tableStructure(props.connectionId, namespace.value, table.value);
  } catch (err) {
    structureError.value = String(err);
  } finally {
    loadingStructure.value = false;
  }
}

function refresh() {
  if (mode.value !== "data") {
    void loadStructure();
    return;
  }
  clock.value = new Date();
  void loadStructure();
  if (autoApplyFilters.value && !compiled.value.pending) {
    const current = compiled.value;
    applied.value = appliedFrom(current);
  }
  void loadData(true, false);
  if (autoRefreshArmed.value) {
    startAutoRefreshTimer();
  }
}

function isAutoRefreshPreset(ms: number) {
  return AUTO_REFRESH_PRESETS.some((preset) => preset.ms === ms);
}

function setAutoRefresh(next: TableViewState["autoRefresh"]) {
  emit("update:view", { autoRefresh: next });
  closeAutoRefreshMenu();
}

function disableAutoRefresh() {
  setAutoRefresh(undefined);
}

function enableAutoRefresh(intervalMs: number) {
  setAutoRefresh({ intervalMs: clampAutoRefreshMs(intervalMs) });
}

function openCustomRefresh() {
  const total = Math.round((autoRefresh.value?.intervalMs ?? 30_000) / 1000);
  customRefreshMinutes.value = Math.floor(total / 60);
  customRefreshSeconds.value = total % 60;
  customRefresh.value = true;
  closeAutoRefreshMenu();
}

function applyCustomRefresh() {
  const minutes = Math.max(0, Math.floor(Number(customRefreshMinutes.value) || 0));
  const seconds = Math.max(0, Math.floor(Number(customRefreshSeconds.value) || 0));
  enableAutoRefresh((minutes * 60 + seconds) * 1000);
  customRefresh.value = false;
}

function stopAutoRefreshTimer() {
  window.clearInterval(autoRefreshTimer);
  autoRefreshTimer = 0;
  window.clearInterval(autoRefreshClock);
  autoRefreshClock = 0;
}

function scheduleNextRefresh(intervalMs: number) {
  nextRefreshAt.value = Date.now() + intervalMs;
  autoRefreshNow.value = Date.now();
}

function autoRefreshTick() {
  if (!autoRefreshArmed.value) {
    stopAutoRefreshTimer();
    nextRefreshAt.value = 0;
    return;
  }
  const current = autoRefresh.value;
  if (current) {
    scheduleNextRefresh(current.intervalMs);
  }
  if (dirty.value || loading.value || mode.value !== "data") {
    return;
  }
  clock.value = new Date();
  if (autoApplyFilters.value && !compiled.value.pending) {
    applied.value = appliedFrom(compiled.value);
  }
  void loadData(true, false);
}

function startAutoRefreshTimer() {
  stopAutoRefreshTimer();
  const current = autoRefresh.value;
  if (!current || !autoRefreshArmed.value) {
    nextRefreshAt.value = 0;
    return;
  }
  scheduleNextRefresh(current.intervalMs);
  autoRefreshTimer = window.setInterval(autoRefreshTick, current.intervalMs);
  autoRefreshClock = window.setInterval(() => {
    autoRefreshNow.value = Date.now();
  }, 250);
}

function onPageVisibility() {
  pageVisible.value = document.visibilityState === "visible";
}

watch(
  () => [autoRefresh.value?.intervalMs, autoRefreshArmed.value] as const,
  startAutoRefreshTimer,
  { immediate: true },
);

function goToPage(next: number) {
  const last = pageCount.value === null ? Infinity : pageCount.value - 1;
  const clamped = Math.min(Math.max(next, 0), last);
  if (clamped === page.value) {
    return;
  }
  page.value = clamped;
  void loadData();
}

function setPageSize(value: number) {
  const next = clampTabPageSize(value);
  if (next === pageSize.value && props.view.pageSize === next) {
    return;
  }
  emit("update:view", { pageSize: next });
}

function onPageSizeChange(event: Event) {
  setPageSize(Number((event.target as HTMLSelectElement).value));
}

function onSort(column: string) {
  let sort: TableViewState["sort"];
  if (sortColumn.value !== column) {
    sort = { column, dir: "asc" };
  } else if (sortDir.value === "asc") {
    sort = { column, dir: "desc" };
  } else {
    sort = null;
  }
  emit("update:view", { sort });
}

function onFollow(row: number, col: number, options?: { side?: boolean }) {
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
  emit("follow", { namespace: key.refNamespace || namespace.value, table: key.refTable, filter }, options);
}

function followFromMenu(side = false) {
  if (!cellMenu.value) {
    return;
  }
  onFollow(cellMenu.value.row, cellMenu.value.col, { side });
  closeGridMenus();
}

const menuHasLink = computed(() => {
  if (!cellMenu.value) {
    return false;
  }
  const name = columns.value[cellMenu.value.col]?.name;
  return Boolean(name && foreignKeyByColumn.value.has(name));
});

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

function updateDeleted(add: Cell[][], remove: Cell[][]) {
  const next = new Map(deleted.value);
  for (const key of remove) {
    next.delete(JSON.stringify(key));
  }
  for (const key of add) {
    next.set(JSON.stringify(key), key);
  }
  deleted.value = next;
}

function applyEntry(entry: UndoEntry, side: "before" | "value") {
  applyChanges(entry.changes, side);
  const created = entry.created ?? [];
  const removed = entry.removed ?? [];
  const marked = entry.deleted ?? [];
  const restored = entry.restored ?? [];
  if (side === "value") {
    updateNewRows(created, removed);
    updateDeleted(marked, restored);
  } else {
    updateNewRows(removed, created);
    updateDeleted(restored, marked);
  }
}

function recordEntry(entry: UndoEntry) {
  if (
    !entry.changes.length &&
    !entry.created?.length &&
    !entry.removed?.length &&
    !entry.deleted?.length &&
    !entry.restored?.length
  ) {
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

/**
 * Saved rows are marked and removed on the next save; unsaved new rows go
 * away at once. When every chosen row is already marked, they're restored.
 */
function deleteRows(indexes: number[]) {
  grid.value?.commitEdit();
  const targets = indexes.filter(rowDeletable);
  if (!targets.length) {
    if (indexes.length) {
      showToast(
        kind.value === "table"
          ? "Rows can only be deleted from tables with a primary key."
          : "Rows can only be deleted from tables.",
        "error",
      );
    }
    return;
  }
  const saved = targets.filter((row) => row < rows.value.length);
  const fresh = targets.filter((row) => row >= rows.value.length);
  const keys = saved.map((row) => pageKeys.value[row] as Cell[]);
  if (!fresh.length && saved.every((row) => deletedRows.value.has(row))) {
    recordEntry({ changes: [], restored: keys });
    return;
  }
  const freshIds = fresh.map((row) => newRowIds.value[row - rows.value.length]);
  recordEntry({
    changes: freshIds.flatMap((id) =>
      (pendingByRow.value.get(JSON.stringify([NEW_ROW, id])) ?? []).map((cell) => ({
        ...cell,
        before: cell.value,
        value: cell.original,
      })),
    ),
    removed: freshIds,
    deleted: keys.filter((key) => !deleted.value.has(JSON.stringify(key))),
  });
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
    restored: [...deleted.value.values()],
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
  const col = autoColumns.value.findIndex((auto) => !auto);
  if (col !== -1) {
    grid.value?.editCell(allRows.value.length - 1, col);
  }
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
    namespace: namespace.value,
    table: table.value,
    updates: rowEdits
      .filter((cells) => !isNewKey(cells[0].key) && !deleted.value.has(JSON.stringify(cells[0].key)))
      .map((cells) => ({
        key: keyEdits(cells[0].key),
        changes: cellEdits(cells),
      })),
    inserts: newRowIds.value.map((id) => ({
      values: cellEdits(pendingByRow.value.get(JSON.stringify([NEW_ROW, id])) ?? []),
    })),
    deletes: [...deleted.value.values()].map((key) => ({ key: keyEdits(key) })),
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
      deletedIds: [...deleted.value.keys()],
      columns: structurePending?.snapshot,
    },
  };
}

function keyEdits(key: Cell[]) {
  return keyColumns.value.map((column, index) => ({ column, value: key[index] as EditValue }));
}

type SavedSnapshot = ReturnType<typeof pendingChanges>["snapshot"];

function markSaved(sent: SavedSnapshot) {
  const inserted = new Set<Cell>(sent.insertedIds);
  const removed = new Set(sent.deletedIds);
  markRowsSaved(sent.rows);
  if (inserted.size) {
    newRowIds.value = newRowIds.value.filter((id) => !inserted.has(id));
    pending.value = new Map(
      [...pending.value].filter(([, cell]) => !(isNewKey(cell.key) && inserted.has(cell.key[1]))),
    );
  }
  if (removed.size) {
    const next = new Map(deleted.value);
    for (const id of removed) {
      next.delete(id);
    }
    deleted.value = next;
    if (result.value) {
      const kept = rows.value.filter((row) => {
        const key = rowKey(row);
        return !key || !removed.has(JSON.stringify(key));
      });
      result.value = { ...result.value, rows: kept };
    }
  }
  const recount = inserted.size > 0 || removed.size > 0;
  if (!sent.columns || (!sent.columns.fields.size && !sent.columns.created.length)) {
    void loadData(recount, false);
    return;
  }
  structureView.value?.markSaved(sent.columns);
  const renamed = new Map(
    [...sent.columns.fields.values()]
      .filter((edit) => edit.section === "columns" && edit.field === "name")
      .filter((edit) => structure.value?.columns.some((column) => column.name === edit.key))
      .map((edit) => [edit.key, String(edit.value)]),
  );
  if (renamed.size) {
    let filter = props.view.filter;
    for (const [from, to] of renamed) {
      filter = renameColumn(filter, from, to);
    }
    const sort = props.view.sort && renamed.has(props.view.sort.column)
      ? { ...props.view.sort, column: renamed.get(props.view.sort.column) ?? props.view.sort.column }
      : props.view.sort;
    emit("update:view", { filter, sort });
  }
  void loadStructure();
  void loadData(recount, false);
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
  if (key === "f") {
    event.preventDefault();
    if (event.shiftKey) {
      openPanel(true);
    } else {
      openFind();
    }
    return;
  }
  if (key === "g" && find.open.value && mode.value === "data") {
    event.preventDefault();
    find.step(event.shiftKey ? -1 : 1);
    return;
  }
  if (key === "e" && !event.shiftKey && mode.value === "data" && inGrid(event.target)) {
    event.preventDefault();
    const cell = grid.value?.focusedCell();
    if (cell) {
      find.search(cell.text, cell.position);
    }
    return;
  }
  if (key !== "z" || kind.value !== "table") {
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

watch(
  () => JSON.stringify(props.view.sort),
  () => {
    page.value = 0;
    void loadData();
  },
);

watch(
  () => compiled.value.key,
  () => {
    const debounced = debounceNext;
    debounceNext = false;
    if (compiled.value.pending || !autoApplyFilters.value) {
      return;
    }
    if (debounced) {
      scheduleApply(APPLY_DELAY);
    } else {
      applyFilter();
    }
  },
);

watch(autoApplyFilters, (auto) => {
  if (auto && unapplied.value) {
    applyFilter();
  }
});

watch(
  () => ({ count: filterCount.value, summary: filterSummaryText.value, preview: filterPreview.value }),
  (state) => emit("filterState", state),
  { immediate: true },
);

watch(columnMap, (columns) => {
  if (!columns) {
    return;
  }
  const normalized = normalizeFilter(props.view.filter, columns);
  if (normalized) {
    emit("update:view", { filter: normalized });
  }
  if (!result.value && !loading.value) {
    applyFilter(true);
  }
});

function onFilterChange(root: FilterGroup, immediate: boolean) {
  debounceNext = !immediate;
  emit("update:view", { filter: root, origin: "user" });
  void nextTick(() => {
    debounceNext = false;
  });
}

function openPanel(focus: boolean) {
  if (!props.view.panelOpen) {
    emit("update:view", { panelOpen: true });
  }
  mode.value = "data";
  if (focus) {
    void nextTick(() => panel.value?.focus());
  }
}

function gridElement() {
  return grid.value?.$el as HTMLElement | undefined;
}

function inGrid(target: EventTarget | null) {
  return target instanceof Element && !target.closest("textarea") && Boolean(gridElement()?.contains(target));
}

function openFind() {
  mode.value = "data";
  find.show(grid.value?.focusedCell()?.position);
}

function closeFind() {
  find.close();
  gridElement()?.focus({ preventScroll: true });
}

function collapsePanel() {
  const hasBlank = conditionCount.value !== allConditions(props.view.filter).length;
  emit("update:view", hasBlank ? { panelOpen: false, filter: dropBlankConditions(props.view.filter) } : { panelOpen: false });
  void nextTick(() => {
    if (autoApplyFilters.value && !compiled.value.pending) {
      applyFilter();
    }
    gridElement()?.focus({ preventScroll: true });
  });
}

function togglePanel() {
  if (props.view.panelOpen && mode.value === "data") {
    collapsePanel();
  } else {
    openPanel(true);
  }
}

function clearFilters() {
  emit("update:view", { filter: emptyGroup(), origin: "user" });
}

const canPasteFilters = ref(false);

/** The filter on the clipboard, or null when it holds anything else or can't be read. */
async function clipboardFilter() {
  try {
    const pasted = filterFromClipboard(await readClipboard());
    return pasted && allConditions(pasted).length ? pasted : null;
  } catch {
    return null;
  }
}

async function checkClipboard() {
  canPasteFilters.value = Boolean(await clipboardFilter());
}

function onWindowFocus() {
  if (props.view.panelOpen) {
    void checkClipboard();
  }
}

watch(
  () => props.view.panelOpen,
  (open) => {
    if (open) {
      void checkClipboard();
    }
  },
  { immediate: true },
);

onMounted(() => window.addEventListener("focus", onWindowFocus));
onBeforeUnmount(() => window.removeEventListener("focus", onWindowFocus));

async function copyFilters() {
  const count = allConditions(dropBlankConditions(props.view.filter)).length;
  try {
    await writeClipboard(filterToClipboard(props.view.filter));
    canPasteFilters.value = true;
    showToast(`Copied ${count} ${count === 1 ? "filter" : "filters"}`);
  } catch (err) {
    showToast(String(err), "error");
  }
}

async function pasteFilters() {
  const pasted = await clipboardFilter();
  canPasteFilters.value = Boolean(pasted);
  if (!pasted) {
    showToast("The clipboard no longer has filters copied from Recon.", "error");
    return;
  }
  const added = allConditions(pasted);
  const next = mergeFilters(props.view.filter, pasted);
  if (allConditions(next).length > MAX_CONDITIONS) {
    showToast(`Filters can have at most ${MAX_CONDITIONS} conditions.`, "error");
    return;
  }
  emit("update:view", { filter: next, origin: "user" });
  const missing = new Set(added.filter((node) => columnMap.value && !columnMap.value.has(node.column)).map((node) => node.column));
  const summary = `Pasted ${added.length} ${added.length === 1 ? "filter" : "filters"}`;
  if (missing.size) {
    const names = [...missing].map((name) => `“${name}”`).join(", ");
    showToast(`${summary}. This table has no ${missing.size === 1 ? "column" : "columns"} ${names}, so those filters aren't applied.`, "error");
  } else {
    showToast(summary);
  }
  void nextTick(() => panel.value?.reveal(pasted.children[0]?.id ?? next.id));
}

function suggest(column: string, search: string) {
  const key = `${column}\u0000${search.trim().toLowerCase()}`;
  let request = suggestionCache.get(key);
  if (!request) {
    request = api
      .distinctValues(props.connectionId, namespace.value, table.value, column, search)
      .catch((err) => {
        suggestionCache.delete(key);
        throw err;
      });
    suggestionCache.set(key, request);
  }
  return request;
}

/** Adds a condition that narrows the rows, keeping an OR filter intact by nesting it. */
function addCondition(condition: FilterCondition) {
  const root = props.view.filter;
  if (allConditions(root).length >= MAX_CONDITIONS) {
    showToast(`Filters can have at most ${MAX_CONDITIONS} conditions.`, "error");
    return;
  }
  const blankless = dropBlankConditions(root);
  const next: FilterGroup = blankless.match === "all" || blankless.children.length < 2
    ? appendChild({ ...blankless, match: "all" }, blankless.id, condition)
    : { ...emptyGroup("all"), children: [blankless, condition] };
  emit("update:view", { filter: next, origin: "user" });
  void nextTick(() => panel.value?.reveal(condition.id));
}

function cellFilterValue(value: Cell): string | null {
  if (value === null || isBytes(value)) {
    return null;
  }
  return String(value);
}

const menuColumn = computed(() => {
  const col = cellMenu.value?.col ?? headerMenu.value?.col;
  const name = col === undefined ? undefined : columns.value[col]?.name;
  return name ? (columnMap.value?.get(name) ?? null) : null;
});
const menuValue = computed<Cell | undefined>(() =>
  cellMenu.value ? displayRows.value[cellMenu.value.row]?.[cellMenu.value.col] : undefined,
);
const menuValueFilterable = computed(() => {
  const column = menuColumn.value;
  const value = menuValue.value;
  if (!column || value === undefined || column.kind === "binary" || column.kind === "json") {
    return false;
  }
  return value === null || (!isBytes(value) && String(value).length <= 10_000);
});

function closeGridMenus() {
  cellMenu.value = null;
  rowMenu.value = null;
  headerMenu.value = null;
}

useDismiss(
  computed(() => Boolean(cellMenu.value || rowMenu.value || headerMenu.value)),
  () => [menuEl.value],
  closeGridMenus,
);

const menuDeletable = computed(() => menuRows.value.filter(rowDeletable));
const menuRestores = computed(
  () =>
    menuDeletable.value.length > 0 &&
    menuDeletable.value.every((row) => deletedRows.value.has(row)),
);
const menuDeleteLabel = computed(() => {
  const count = menuDeletable.value.length || menuRows.value.length;
  const verb = menuRestores.value ? "Restore" : "Delete";
  return count === 1 ? `${verb} row` : `${verb} ${count.toLocaleString()} rows`;
});

function deleteFromMenu() {
  const targets = menuRows.value;
  closeGridMenus();
  deleteRows(targets);
}

function copyFromMenu(format: CopyFormat) {
  closeGridMenus();
  void grid.value?.copySelection(format);
}

const exportOpen = ref(false);
const exportSource = computed<ExportRowsSource>(() => ({
  kind: "browse",
  request: browseRequest(),
  total: total.value,
  pageRows: rows.value.length,
  filtered: applied.value.count > 0,
}));

function placeGridMenu(x: number, y: number) {
  menuPosition.value = { left: x, top: y };
  void nextTick(() => {
    menuPosition.value = placeAtPoint(x, y, menuEl.value);
  });
}

function onCellMenu(row: number, col: number, event: MouseEvent) {
  closeGridMenus();
  menuRows.value = grid.value?.selectedRows() ?? [row];
  cellMenu.value = { x: event.clientX, y: event.clientY, row, col };
  placeGridMenu(event.clientX, event.clientY);
}

function onRowMenu(row: number, event: MouseEvent) {
  closeGridMenus();
  if (kind.value !== "table") {
    return;
  }
  menuRows.value = grid.value?.selectedRows() ?? [row];
  rowMenu.value = { x: event.clientX, y: event.clientY };
  placeGridMenu(event.clientX, event.clientY);
}

function onHeaderMenu(col: number, event: MouseEvent) {
  closeGridMenus();
  headerMenu.value = { x: event.clientX, y: event.clientY, col };
  placeGridMenu(event.clientX, event.clientY);
}

function filterByCell(action: "include" | "exclude" | "null" | "notNull") {
  const column = menuColumn.value;
  const value = menuValue.value;
  closeGridMenus();
  if (!column || value === undefined) {
    return;
  }
  const text = cellFilterValue(value);
  let condition: FilterCondition;
  if (action === "null" || action === "notNull" || text === null) {
    const isNull = action === "null" || (text === null && action === "include");
    condition = newCondition(column.name, isNull ? "isNull" : "isNotNull", { type: "none" });
  } else if (column.kind === "boolean") {
    const truthy = ["true", "1", "t"].includes(text.toLowerCase());
    condition = newCondition(column.name, truthy === (action === "include") ? "isTrue" : "isFalse", { type: "none" });
  } else {
    condition = newCondition(column.name, action === "include" ? "eq" : "neq", { type: "single", value: text });
  }
  addCondition(condition);
}

function filterOnColumn() {
  const column = menuColumn.value;
  closeGridMenus();
  if (!column) {
    return;
  }
  const operator = DEFAULT_OPERATOR[column.kind];
  const seed = column.kind === "date" || column.kind === "datetime" ? formatDate(new Date()) : "";
  const condition = newCondition(column.name, operator, convertValue({ type: "single", value: seed }, operator));
  addCondition(condition);
  openPanel(false);
  void nextTick(() => {
    const row = document.querySelector<HTMLElement>(`[data-filter-id="${CSS.escape(condition.id)}"]`);
    const target = row?.querySelector<HTMLElement>(".filter-value input")
      ?? row?.querySelector<HTMLElement>(".filter-value select")
      ?? row?.querySelector<HTMLElement>(".filter-operator");
    target?.focus();
  });
}

function sortFromMenu(dir: SortDirection | null) {
  const name = headerMenu.value ? columns.value[headerMenu.value.col]?.name : undefined;
  closeGridMenus();
  if (name) {
    emit("update:view", { sort: dir ? { column: name, dir } : null });
  }
}

async function showSql() {
  sqlPreview.value = { sql: "", loading: true, error: "" };
  const current = compiled.value;
  try {
    const sql = await api.previewBrowseSql(
      props.connectionId,
      browseRequest({ filter: current.pending ? applied.value.wire : current.wire }),
    );
    if (sqlPreview.value) {
      sqlPreview.value = { sql, loading: false, error: "" };
    }
  } catch (err) {
    const problem = filterErrorOf(String(err));
    if (sqlPreview.value) {
      sqlPreview.value = { sql: "", loading: false, error: problem?.message ?? String(err) };
    }
  }
}

async function copySql() {
  if (!sqlPreview.value?.sql) {
    return;
  }
  try {
    await navigator.clipboard.writeText(sqlPreview.value.sql);
    showToast("Copied SQL");
  } catch (err) {
    showToast(String(err), "error");
  }
}

function openSqlInTab() {
  const sql = sqlPreview.value?.sql;
  sqlPreview.value = null;
  if (sql) {
    emit("openSql", `${sql};`);
  }
}

watch(
  () =>
    [...pendingByRow.value].filter(([id, cells]) => !isNewKey(cells[0].key) && !deleted.value.has(id)).length +
    newRowIds.value.length +
    deleted.value.size +
    structureChanges.value,
  (count) => emit("changes", count),
);

onMounted(() => {
  // A saved or linked filter needs the column types first; the columnMap watcher applies it.
  if (!hasConditions(props.view.filter)) {
    applyFilter(true);
  }
  void loadStructure();
  window.addEventListener("keydown", onWindowKeydown, true);
  document.addEventListener("visibilitychange", onPageVisibility);
  pageVisible.value = document.visibilityState === "visible";
  if (rootEl.value) {
    viewportObserver = new IntersectionObserver(
      ([entry]) => {
        inViewport.value = Boolean(entry?.isIntersecting);
      },
      { threshold: 0 },
    );
    viewportObserver.observe(rootEl.value);
  }
});

onBeforeUnmount(() => {
  window.removeEventListener("keydown", onWindowKeydown, true);
  document.removeEventListener("visibilitychange", onPageVisibility);
  viewportObserver?.disconnect();
  window.clearTimeout(applyTimer);
  stopAutoRefreshTimer();
  cancelRunning();
  if (dirty.value) {
    emit("changes", 0);
  }
});

defineExpose({ refresh, pendingChanges, markSaved, discard });
</script>

<template>
  <div ref="rootEl" class="table-view">
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
      <button
        class="ghost tiny filter-toggle"
        :class="{ active: view.panelOpen && mode === 'data', filtered: filterActive }"
        type="button"
        :aria-pressed="view.panelOpen && mode === 'data'"
        :aria-label="filterActive ? `Filter: ${filterSummaryText || 'rows'}` : 'Filter rows'"
        aria-keyshortcuts="Meta+Shift+F"
        :title="summaryVisible ? undefined : view.panelOpen && mode === 'data' ? 'Hide filters (⇧⌘F)' : 'Filter rows (⇧⌘F)'"
        @click="togglePanel"
        @mouseenter="showSummaryPopover"
        @mouseleave="hideSummaryPopover"
      >
        <svg class="button-icon" viewBox="0 0 16 16" aria-hidden="true">
          <path d="M2.5 3h11L9.2 8.2v4.3l-2.4 1.2V8.2L2.5 3Z" />
        </svg>
        Filter
        <span v-if="filterCount" class="filter-count">{{ filterCount }}</span>
      </button>
      <span
        v-if="summaryVisible"
        class="table-filter"
        :class="{ error: Boolean(filterError) }"
        role="button"
        tabindex="0"
        :aria-label="applied.count ? `Showing rows where ${applied.summary}. Click to edit.` : `Filters: ${filterSummaryText || 'click to edit'}.`"
        @click="openPanel(true)"
        @keydown.enter.prevent="openPanel(true)"
        @mouseenter="showSummaryPopover"
        @mouseleave="hideSummaryPopover"
      >
        <span class="table-filter-label">
          <FilterSummary v-if="filterPreview" :group="filterPreview" />
          <template v-else>{{ filterSummaryText }}</template>
        </span>
        <button
          class="table-filter-clear"
          type="button"
          title="Clear all filters"
          aria-label="Clear all filters and show all rows"
          @click.stop="clearFilters"
        >
          ×
        </button>
      </span>
      <div class="pane-toolbar-end">
        <button
          v-if="mode === 'data'"
          class="ghost tiny"
          type="button"
          :disabled="!result"
          title="Export these rows as CSV or JSON, with the current filters and sort"
          @click="exportOpen = true"
        >
          <svg class="button-icon" viewBox="0 0 16 16" aria-hidden="true">
            <path d="M8 10V2.5M4.8 5.7 8 2.5l3.2 3.2M2.5 11.5v1a1 1 0 0 0 1 1h9a1 1 0 0 0 1-1v-1" />
          </svg>
          Export
        </button>
        <div class="overflow-menu auto-refresh-menu refresh-split">
          <button
            class="ghost tiny refresh-main"
            type="button"
            :title="refreshing ? 'Loading…' : 'Refresh (⌘R)'"
            :aria-label="refreshing ? 'Loading' : 'Refresh'"
            aria-keyshortcuts="Meta+R"
            :disabled="refreshing"
            @click="refresh"
          >
            <span v-if="refreshing" class="spinner" aria-hidden="true" />
            <svg v-else class="button-icon" viewBox="0 0 24 24" aria-hidden="true">
              <path d="M21 2v6h-6" />
              <path d="M3 12a9 9 0 0 1 15.48-6.36L21 8" />
              <path d="M3 22v-6h6" />
              <path d="M21 12a9 9 0 0 1-15.48 6.36L3 16" />
            </svg>
            <span v-if="autoRefreshOn" class="auto-refresh-dot" aria-hidden="true" />
          </button>
          <button
            class="ghost tiny refresh-caret"
            type="button"
            :title="autoRefreshTitle"
            :aria-label="autoRefreshTitle"
            :aria-expanded="autoRefreshMenuOpen"
            aria-haspopup="menu"
            @click="toggleAutoRefreshMenu"
          >
            <svg class="button-icon" viewBox="0 0 24 24" aria-hidden="true">
              <path d="m7 10 5 5 5-5" />
            </svg>
          </button>
          <div v-if="autoRefreshMenuOpen" class="overflow-menu-dropdown" role="menu" aria-label="Auto refresh">
            <button
              class="overflow-menu-item"
              type="button"
              role="menuitemradio"
              :aria-checked="!autoRefresh"
              @click="disableAutoRefresh"
            >
              Off
              <span v-if="!autoRefresh" class="menu-check" aria-hidden="true">✓</span>
            </button>
            <div class="overflow-menu-divider" role="separator" />
            <button
              v-for="preset in AUTO_REFRESH_PRESETS"
              :key="preset.ms"
              class="overflow-menu-item"
              type="button"
              role="menuitemradio"
              :aria-checked="autoRefreshOn && autoRefresh?.intervalMs === preset.ms"
              @click="enableAutoRefresh(preset.ms)"
            >
              {{ preset.label }}
              <span
                v-if="autoRefreshOn && autoRefresh?.intervalMs === preset.ms"
                class="menu-check"
                aria-hidden="true"
              >✓</span>
            </button>
            <button
              class="overflow-menu-item"
              type="button"
              role="menuitem"
              :aria-checked="autoRefreshOn && autoRefresh ? !isAutoRefreshPreset(autoRefresh.intervalMs) : false"
              @click="openCustomRefresh"
            >
              {{
                autoRefresh && !isAutoRefreshPreset(autoRefresh.intervalMs)
                  ? formatAutoRefresh(autoRefresh.intervalMs)
                  : "Custom…"
              }}
              <span
                v-if="autoRefreshOn && autoRefresh && !isAutoRefreshPreset(autoRefresh.intervalMs)"
                class="menu-check"
                aria-hidden="true"
              >✓</span>
            </button>
          </div>
        </div>
      </div>
    </div>

    <FilterPanel
      v-if="view.panelOpen && mode === 'data' && filterColumns"
      ref="panel"
      :root="view.filter"
      :columns="filterColumns"
      :compiled="compiled"
      :server-issues="serverIssues"
      :error="filterError"
      :auto-apply="autoApplyFilters"
      :unapplied="unapplied"
      :suggest="suggest"
      @update="onFilterChange"
      @apply="applyFilter()"
      @collapse="collapsePanel"
      @clear="clearFilters"
      @show-sql="showSql"
      :can-paste="canPasteFilters"
      @copy="copyFilters"
      @paste="pasteFilters"
      @check-clipboard="checkClipboard"
    />
    <div v-else-if="view.panelOpen && mode === 'data' && loadingStructure" class="filter-panel-loading muted tiny">
      <span class="spinner" aria-hidden="true" /> Loading columns…
    </div>
    <GridFind
      v-if="find.open.value && mode === 'data'"
      :find="find"
      :scope="onlyOnePage ? undefined : 'on this page'"
      @close="closeFind"
    />

    <div
      v-show="mode === 'data'"
      class="table-view-body"
      :class="{ stale: loading && Boolean(result), 'has-pager': result || loading }"
    >
      <p v-if="error" class="pane-error">{{ error }}</p>
      <template v-else-if="result">
        <div v-if="applied.count && !rows.length && !loading" class="filter-empty">
          <span>No rows match these filters.</span>
          <button class="ghost tiny" type="button" @click="openPanel(true)">Edit filters</button>
          <button class="ghost tiny" type="button" @click="clearFilters">Clear filters</button>
        </div>
        <DataGrid
          ref="grid"
          :columns="result.columns"
          :rows="displayRows"
          :row-number-offset="offset"
          sortable
          :sort-column="sortColumn"
          :sort-dir="sortDir"
          :cell-editable="cellEditable"
          :new-row-start="rows.length"
          :new-row-auto="autoColumns"
          :creatable="canInsert"
          :modified="modified"
          :deletable="kind === 'table'"
          :deleted-rows="deletedRows"
          :links="links"
          :matches="find.byRow.value"
          :current-match="find.current.value"
          context-menus
          @sort="onSort"
          @edit="onEdit"
          @set-null="onSetNull"
          @delete-rows="deleteRows"
          @create="createRecord"
          @follow="onFollow"
          @cell-menu="onCellMenu"
          @row-menu="onRowMenu"
          @header-menu="onHeaderMenu"
        />
      </template>
      <div v-if="result || loading" class="table-pager-bar">
        <span class="muted tiny table-load-time">
          <template v-if="result">Loaded in {{ result.durationMs }}ms</template>
          <template v-if="result && autoRefreshRemaining"> · </template>
          <template v-if="autoRefreshRemaining">{{ autoRefreshRemaining }}</template>
        </span>
        <div class="table-pager-ribbon" role="navigation" aria-label="Table pages">
        <span class="muted tiny pager-label" :title="rangeTitle">{{ rangeLabel }}</span>
        <label class="pager-size" title="Rows per page">
          <span class="visually-hidden">Rows per page</span>
          <select :value="pageSize" :disabled="loading" @change="onPageSizeChange">
            <option v-for="size in pageSizeOptions" :key="size" :value="size">
              {{ size }}
            </option>
          </select>
        </label>
        <div class="pager">
          <button
            class="ghost tiny"
            type="button"
            title="First page"
            :disabled="onlyOnePage || !hasPrev || loading"
            @click="goToPage(0)"
          >
            «
          </button>
          <button
            class="ghost tiny"
            type="button"
            title="Previous page"
            :disabled="onlyOnePage || !hasPrev || loading"
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
            :disabled="onlyOnePage || !hasNext || loading"
            @click="goToPage(page + 1)"
          >
            ›
          </button>
          <button
            class="ghost tiny"
            type="button"
            title="Last page"
            :disabled="onlyOnePage || pageCount === null || !hasNext || loading"
            @click="goToPage((pageCount ?? 1) - 1)"
          >
            »
          </button>
        </div>
        </div>
      </div>
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
    <Teleport to="body">
      <div
        v-if="cellMenu || rowMenu || headerMenu"
        ref="menuEl"
        class="overflow-menu-dropdown table-context-menu"
        role="menu"
        :style="{ left: `${menuPosition.left}px`, top: `${menuPosition.top}px` }"
        @contextmenu.prevent
      >
        <template v-if="cellMenu || rowMenu">
          <button class="overflow-menu-item" type="button" role="menuitem" aria-keyshortcuts="Meta+C" @click="copyFromMenu('tsv')">
            Copy
          </button>
          <button class="overflow-menu-item" type="button" role="menuitem" @click="copyFromMenu('csv')">
            Copy as CSV
          </button>
          <button class="overflow-menu-item" type="button" role="menuitem" @click="copyFromMenu('json')">
            Copy as JSON
          </button>
          <div v-if="cellMenu || kind === 'table'" class="overflow-menu-divider" role="separator" />
        </template>
        <template v-if="cellMenu">
          <button
            class="overflow-menu-item"
            type="button"
            role="menuitem"
            :disabled="!menuValueFilterable"
            @click="filterByCell('include')"
          >
            Filter by this value
          </button>
          <button
            class="overflow-menu-item"
            type="button"
            role="menuitem"
            :disabled="!menuValueFilterable"
            @click="filterByCell('exclude')"
          >
            Exclude this value
          </button>
          <div class="overflow-menu-divider" role="separator" />
          <button
            class="overflow-menu-item"
            type="button"
            role="menuitem"
            :disabled="!menuColumn"
            @click="filterByCell('null')"
          >
            Filter where {{ menuColumn?.name ?? "column" }} is NULL
          </button>
          <button
            class="overflow-menu-item"
            type="button"
            role="menuitem"
            :disabled="!menuColumn"
            @click="filterByCell('notNull')"
          >
            Filter where {{ menuColumn?.name ?? "column" }} is not NULL
          </button>
          <template v-if="menuHasLink">
            <div class="overflow-menu-divider" role="separator" />
            <button class="overflow-menu-item" type="button" role="menuitem" @click="followFromMenu(false)">
              Open related table
            </button>
            <button class="overflow-menu-item" type="button" role="menuitem" @click="followFromMenu(true)">
              Open to the side
            </button>
          </template>
        </template>
        <template v-if="(cellMenu || rowMenu) && kind === 'table'">
          <div v-if="cellMenu" class="overflow-menu-divider" role="separator" />
          <button
            class="overflow-menu-item"
            :class="{ danger: !menuRestores }"
            type="button"
            role="menuitem"
            aria-keyshortcuts="Backspace"
            :disabled="!menuDeletable.length"
            :title="menuDeletable.length ? undefined : 'Rows can only be deleted from tables with a primary key.'"
            @click="deleteFromMenu"
          >
            {{ menuDeleteLabel }}
          </button>
        </template>
        <template v-else-if="headerMenu">
          <button class="overflow-menu-item" type="button" role="menuitem" :disabled="!menuColumn" @click="filterOnColumn">
            Filter on {{ menuColumn?.name ?? "column" }}…
          </button>
          <div class="overflow-menu-divider" role="separator" />
          <button class="overflow-menu-item" type="button" role="menuitem" @click="sortFromMenu('asc')">
            Sort ascending
          </button>
          <button class="overflow-menu-item" type="button" role="menuitem" @click="sortFromMenu('desc')">
            Sort descending
          </button>
          <button class="overflow-menu-item" type="button" role="menuitem" :disabled="!view.sort" @click="sortFromMenu(null)">
            Clear sort
          </button>
        </template>
      </div>
    </Teleport>
    <FilterPopover
      v-if="summaryAnchor && summaryVisible && active"
      :preview="filterPreview"
      :count="filterCount"
      :anchor="summaryAnchor"
      :hint="filterError || 'Click to edit filters (⇧⌘F)'"
    />
    <Modal v-if="customRefresh" title="Auto refresh" @close="customRefresh = false">
      <form @submit.prevent="applyCustomRefresh" @keydown.enter.prevent="applyCustomRefresh">
        <span class="muted tiny">Refresh every</span>
        <div class="auto-refresh-fields">
          <label class="modal-label">
            <span class="muted tiny">Minutes</span>
            <input v-model.number="customRefreshMinutes" type="number" min="0" max="60" step="1" />
          </label>
          <label class="modal-label">
            <span class="muted tiny">Seconds</span>
            <input v-model.number="customRefreshSeconds" type="number" min="0" max="59" step="1" />
          </label>
        </div>
      </form>
      <template #actions>
        <button class="ghost" type="button" @click="customRefresh = false">Cancel</button>
        <button class="primary" type="button" @click="applyCustomRefresh">Set</button>
      </template>
    </Modal>
    <Modal v-if="sqlPreview" title="SQL for this view" wide @close="sqlPreview = null">
      <p v-if="sqlPreview.loading" class="action-progress"><span class="spinner" aria-hidden="true" /> Building SQL…</p>
      <p v-else-if="sqlPreview.error" class="settings-error">{{ sqlPreview.error }}</p>
      <pre v-else class="filter-sql-preview">{{ sqlPreview.sql }}</pre>
      <p class="muted tiny">
        Values are written out for reading. Recon checks every column, operator, and value before it runs a filter.
      </p>
      <template #actions>
        <button class="ghost" type="button" @click="sqlPreview = null">Close</button>
        <button class="ghost" type="button" :disabled="!sqlPreview.sql" @click="copySql">Copy</button>
        <button class="primary" type="button" :disabled="!sqlPreview.sql" @click="openSqlInTab">Open in SQL tab</button>
      </template>
    </Modal>
    <ExportRowsDialog
      v-if="exportOpen"
      :connection-id="connectionId"
      :title="`Export “${table}”`"
      :name="table"
      :source="exportSource"
      @close="exportOpen = false"
    />
  </div>
</template>
