<script lang="ts">
export interface CellPosition {
  row: number;
  col: number;
}
</script>

<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { useVirtualizer } from "@tanstack/vue-virtual";
import {
  cellCopyText,
  cellDisplay,
  cellTitle,
  initialColumnWidth,
  isBytes,
  isNumericColumn,
} from "../cells";
import { useApp } from "../composables/useApp";
import type { ColumnMeta, RowValues, SortDirection } from "../types";

const props = defineProps<{
  columns: ColumnMeta[];
  rows: (RowValues | undefined)[];
  rowNumberOffset?: number;
  sortable?: boolean;
  sortColumn?: string | null;
  sortDir?: SortDirection | null;
  editable?: boolean;
  editableColumns?: boolean[];
  modified?: Map<number, Set<number>>;
}>();

const emit = defineEmits<{
  sort: [column: string];
  needRows: [start: number, end: number];
  edit: [row: number, col: number, text: string];
  setNull: [cells: CellPosition[]];
}>();

const { gridFontSize, maxAutoColumnWidth, showToast } = useApp();

const AUTO_FIT_SAMPLE = 200;

const scroller = ref<HTMLDivElement | null>(null);
const widths = ref<number[]>([]);
let autoFitPending = false;
const anchor = ref<CellPosition | null>(null);
const focus = ref<CellPosition | null>(null);
const editing = ref<CellPosition | null>(null);
const draft = ref("");
let initialDraft = "";

const rowHeight = computed(() => Math.round(gridFontSize.value * 1.85 + 2));
const headerHeight = computed(() => rowHeight.value + 4);
const charWidth = computed(() => gridFontSize.value * 0.62);
const gutterWidth = computed(() => {
  const digits = String((props.rowNumberOffset ?? 0) + props.rows.length).length;
  return Math.round(Math.max(digits, 2) * charWidth.value + 20);
});
const numeric = computed(() => props.columns.map(isNumericColumn));
const totalWidth = computed(
  () => gutterWidth.value + widths.value.reduce((sum, width) => sum + width, 0),
);
const templateColumns = computed(
  () => `${gutterWidth.value}px ${widths.value.map((width) => `${width}px`).join(" ")}`,
);

watch(
  () => props.columns.map((column) => `${column.name}:${column.typeName}`).join("\0"),
  () => {
    widths.value = props.columns.map((column) => initialColumnWidth(column, charWidth.value));
    anchor.value = null;
    focus.value = null;
    editing.value = null;
    autoFitPending = true;
    autoFitColumns();
  },
  { immediate: true },
);

watch(() => props.rows, autoFitColumns);

watch(maxAutoColumnWidth, () => {
  autoFitPending = true;
  autoFitColumns();
});

function autoFitColumns() {
  if (!autoFitPending || !props.columns.length) {
    return;
  }
  const sample: RowValues[] = [];
  for (let index = 0; index < props.rows.length && sample.length < AUTO_FIT_SAMPLE; index += 1) {
    const row = props.rows[index];
    if (row) {
      sample.push(row);
    }
  }
  if (!sample.length) {
    return;
  }
  autoFitPending = false;
  widths.value = props.columns.map((column, index) => {
    let chars = column.name.length + 2;
    for (const row of sample) {
      const value = row[index];
      if (value !== undefined) {
        chars = Math.max(chars, cellDisplay(value).length);
      }
    }
    const width = Math.round(Math.max(chars * charWidth.value + 24, 48));
    return Math.min(width, Math.max(maxAutoColumnWidth.value, 48));
  });
}

const virtualizer = useVirtualizer(
  computed(() => ({
    count: props.rows.length,
    getScrollElement: () => scroller.value,
    estimateSize: () => rowHeight.value,
    overscan: 12,
    scrollMargin: headerHeight.value,
  })),
);

watch(rowHeight, () => virtualizer.value.measure());

const virtualRows = computed(() => virtualizer.value.getVirtualItems());
const bodyHeight = computed(() => virtualizer.value.getTotalSize());

const viewportHeight = ref(0);
const fillerHeight = computed(() =>
  Math.max(viewportHeight.value - headerHeight.value - bodyHeight.value, 0),
);
const fillerRows = computed(() => Math.ceil(fillerHeight.value / rowHeight.value));

let resizeObserver: ResizeObserver | null = null;

onMounted(() => {
  const node = scroller.value;
  if (!node) {
    return;
  }
  viewportHeight.value = node.clientHeight;
  resizeObserver = new ResizeObserver(() => {
    viewportHeight.value = node.clientHeight;
  });
  resizeObserver.observe(node);
});

onBeforeUnmount(() => resizeObserver?.disconnect());

watch(
  () => {
    const items = virtualRows.value;
    if (!items.length) {
      return "";
    }
    return `${items[0].index}:${items[items.length - 1].index}:${props.rows.length}`;
  },
  () => {
    const items = virtualRows.value;
    let first = -1;
    let last = -1;
    for (const item of items) {
      if (!props.rows[item.index]) {
        if (first === -1) {
          first = item.index;
        }
        last = item.index;
      }
    }
    if (first !== -1) {
      emit("needRows", first, last + 1);
    }
  },
);

const selection = computed(() => {
  if (!anchor.value || !focus.value) {
    return null;
  }
  return {
    top: Math.min(anchor.value.row, focus.value.row),
    bottom: Math.max(anchor.value.row, focus.value.row),
    left: Math.min(anchor.value.col, focus.value.col),
    right: Math.max(anchor.value.col, focus.value.col),
  };
});

function isSelected(row: number, col: number) {
  const range = selection.value;
  return Boolean(
    range && row >= range.top && row <= range.bottom && col >= range.left && col <= range.right,
  );
}

function isFocused(row: number, col: number) {
  return focus.value?.row === row && focus.value?.col === col;
}

function isRowActive(row: number) {
  const range = selection.value;
  return Boolean(range && row >= range.top && row <= range.bottom);
}

function isEditing(row: number, col: number) {
  return editing.value?.row === row && editing.value?.col === col;
}

function isModified(row: number, col: number) {
  return Boolean(props.modified?.get(row)?.has(col));
}

function canEdit(row: number, col: number) {
  const values = props.rows[row];
  return Boolean(
    props.editable &&
      props.editableColumns?.[col] !== false &&
      values &&
      !isBytes(values[col] ?? null),
  );
}

function startEdit(row: number, col: number, initial?: string) {
  if (!canEdit(row, col)) {
    return;
  }
  const value = props.rows[row]?.[col] ?? null;
  initialDraft = value === null ? "" : String(value);
  draft.value = initial ?? initialDraft;
  editing.value = { row, col };
  anchor.value = { row, col };
  focus.value = { row, col };
  scrollCellIntoView({ row, col });
}

function commitEdit() {
  const position = editing.value;
  if (!position) {
    return;
  }
  editing.value = null;
  if (draft.value !== initialDraft) {
    emit("edit", position.row, position.col, draft.value);
  }
}

function cancelEdit() {
  editing.value = null;
}

function focusEditor(element: unknown) {
  if (element instanceof HTMLTextAreaElement && document.activeElement !== element) {
    element.focus({ preventScroll: true });
    element.setSelectionRange(element.value.length, element.value.length);
  }
}

function onEditorKeydown(event: KeyboardEvent) {
  event.stopPropagation();
  if (event.key === "Enter" && !event.shiftKey && !event.altKey) {
    event.preventDefault();
    commitEdit();
    scroller.value?.focus({ preventScroll: true });
  } else if (event.key === "Escape") {
    event.preventDefault();
    cancelEdit();
    scroller.value?.focus({ preventScroll: true });
  } else if (event.key === "Tab") {
    event.preventDefault();
    commitEdit();
    scroller.value?.focus({ preventScroll: true });
    moveFocus(0, event.shiftKey ? -1 : 1, false);
  }
}

function setSelectionNull() {
  const range = selection.value;
  if (!range) {
    return;
  }
  const cells: CellPosition[] = [];
  for (let row = range.top; row <= range.bottom; row += 1) {
    for (let col = range.left; col <= range.right; col += 1) {
      if (canEdit(row, col)) {
        cells.push({ row, col });
      }
    }
  }
  if (cells.length) {
    emit("setNull", cells);
  }
}

function selectCell(event: MouseEvent, row: number, col: number) {
  commitEdit();
  const position = { row, col };
  if (event.shiftKey && anchor.value) {
    focus.value = position;
  } else {
    anchor.value = position;
    focus.value = position;
  }
  scroller.value?.focus({ preventScroll: true });
}

function selectRow(event: MouseEvent, row: number) {
  commitEdit();
  const last = Math.max(props.columns.length - 1, 0);
  if (event.shiftKey && anchor.value) {
    focus.value = { row, col: last };
    anchor.value = { row: anchor.value.row, col: 0 };
  } else {
    anchor.value = { row, col: 0 };
    focus.value = { row, col: last };
  }
  scroller.value?.focus({ preventScroll: true });
}

function scrollCellIntoView(position: CellPosition) {
  virtualizer.value.scrollToIndex(position.row, { align: "auto" });
  const node = scroller.value;
  if (!node) {
    return;
  }
  let left = gutterWidth.value;
  for (let index = 0; index < position.col; index += 1) {
    left += widths.value[index] ?? 0;
  }
  const right = left + (widths.value[position.col] ?? 0);
  if (left - gutterWidth.value < node.scrollLeft) {
    node.scrollLeft = left - gutterWidth.value;
  } else if (right > node.scrollLeft + node.clientWidth) {
    node.scrollLeft = right - node.clientWidth;
  }
}

function moveFocus(rowDelta: number, colDelta: number, extend: boolean) {
  if (!props.rows.length || !props.columns.length) {
    return;
  }
  const current = focus.value ?? { row: 0, col: 0 };
  const next = {
    row: Math.min(Math.max(current.row + rowDelta, 0), props.rows.length - 1),
    col: Math.min(Math.max(current.col + colDelta, 0), props.columns.length - 1),
  };
  focus.value = next;
  if (!extend || !anchor.value) {
    anchor.value = next;
  }
  scrollCellIntoView(next);
}

async function copySelection() {
  const range = selection.value;
  if (!range) {
    return;
  }
  const lines: string[] = [];
  let missing = 0;
  for (let row = range.top; row <= range.bottom; row += 1) {
    const values = props.rows[row];
    if (!values) {
      missing += 1;
      continue;
    }
    const cells: string[] = [];
    for (let col = range.left; col <= range.right; col += 1) {
      cells.push(cellCopyText(values[col] ?? null));
    }
    lines.push(cells.join("\t"));
  }
  if (range.top !== range.bottom || range.left !== range.right) {
    const header = props.columns.slice(range.left, range.right + 1).map((column) => column.name);
    if (range.top === 0 && range.bottom === props.rows.length - 1) {
      lines.unshift(header.join("\t"));
    }
  }
  try {
    await navigator.clipboard.writeText(lines.join("\n"));
    const count = lines.length;
    showToast(
      missing
        ? `Copied ${count.toLocaleString()} rows. ${missing.toLocaleString()} rows were not loaded yet.`
        : count === 1 && range.left === range.right
          ? "Copied value"
          : `Copied ${count.toLocaleString()} ${count === 1 ? "row" : "rows"}`,
    );
  } catch (err) {
    showToast(String(err), "error");
  }
}

function onKeydown(event: KeyboardEvent) {
  const meta = event.metaKey || event.ctrlKey;
  if (meta && event.key.toLowerCase() === "c") {
    event.preventDefault();
    void copySelection();
    return;
  }
  if (meta && event.key.toLowerCase() === "a") {
    event.preventDefault();
    if (props.rows.length && props.columns.length) {
      anchor.value = { row: 0, col: 0 };
      focus.value = { row: props.rows.length - 1, col: props.columns.length - 1 };
    }
    return;
  }
  const page = Math.max(Math.floor((scroller.value?.clientHeight ?? 0) / rowHeight.value) - 1, 1);
  const moves: Record<string, [number, number]> = {
    ArrowUp: [-1, 0],
    ArrowDown: [1, 0],
    ArrowLeft: [0, -1],
    ArrowRight: [0, 1],
    PageUp: [-page, 0],
    PageDown: [page, 0],
    Home: [meta ? -Infinity : 0, meta ? 0 : -Infinity],
    End: [meta ? Infinity : 0, meta ? 0 : Infinity],
  };
  const move = moves[event.key];
  if (move) {
    event.preventDefault();
    const [rowDelta, colDelta] = move;
    moveFocus(
      Number.isFinite(rowDelta) ? rowDelta : rowDelta > 0 ? props.rows.length : -props.rows.length,
      Number.isFinite(colDelta)
        ? colDelta
        : colDelta > 0
          ? props.columns.length
          : -props.columns.length,
      event.shiftKey,
    );
    return;
  }
  if (event.key === "Escape" && anchor.value) {
    anchor.value = null;
    focus.value = null;
    return;
  }
  if (!props.editable || !focus.value || event.altKey || meta) {
    return;
  }
  if (event.key === "Enter" || event.key === "F2") {
    event.preventDefault();
    startEdit(focus.value.row, focus.value.col);
  } else if (event.key === "Backspace" || event.key === "Delete") {
    event.preventDefault();
    setSelectionNull();
  } else if (event.key.length === 1) {
    event.preventDefault();
    startEdit(focus.value.row, focus.value.col, event.key);
  }
}

function onHeaderClick(event: MouseEvent, column: ColumnMeta) {
  if ((event.target as Element).closest(".grid-resize")) {
    return;
  }
  if (props.sortable) {
    emit("sort", column.name);
  }
}

function startResize(event: PointerEvent, index: number) {
  event.preventDefault();
  event.stopPropagation();
  const startX = event.clientX;
  const startWidth = widths.value[index] ?? 100;
  document.body.classList.add("resizing-columns");
  const onMove = (move: PointerEvent) => {
    const next = [...widths.value];
    next[index] = Math.max(48, Math.min(startWidth + move.clientX - startX, 1600));
    widths.value = next;
  };
  const onUp = () => {
    window.removeEventListener("pointermove", onMove);
    window.removeEventListener("pointerup", onUp);
    window.removeEventListener("pointercancel", onUp);
    document.body.classList.remove("resizing-columns");
  };
  window.addEventListener("pointermove", onMove);
  window.addEventListener("pointerup", onUp);
  window.addEventListener("pointercancel", onUp);
}

function autoSize(index: number) {
  const column = props.columns[index];
  if (!column) {
    return;
  }
  let chars = column.name.length + 2;
  for (const item of virtualRows.value) {
    const value = props.rows[item.index]?.[index];
    if (value !== undefined) {
      chars = Math.max(chars, Math.min(cellDisplay(value).length, 80));
    }
  }
  const next = [...widths.value];
  next[index] = Math.round(Math.max(chars * charWidth.value + 24, 48));
  widths.value = next;
}

function scrollToTop() {
  void nextTick(() => {
    if (scroller.value) {
      scroller.value.scrollTop = 0;
      scroller.value.scrollLeft = 0;
    }
  });
}

defineExpose({ scrollToTop, commitEdit });
</script>

<template>
  <div
    ref="scroller"
    class="data-grid"
    tabindex="0"
    :style="{
      '--grid-row-height': `${rowHeight}px`,
      '--grid-header-height': `${headerHeight}px`,
      '--grid-template': templateColumns,
      '--grid-gutter-width': `${gutterWidth}px`,
      '--grid-width': `${totalWidth}px`,
    }"
    @keydown="onKeydown"
  >
    <div class="grid-header" role="row">
      <div class="grid-gutter grid-corner" />
      <div
        v-for="(column, index) in columns"
        :key="`${index}:${column.name}`"
        class="grid-header-cell"
        :class="{
          sortable,
          numeric: numeric[index],
          sorted: sortColumn === column.name,
        }"
        role="columnheader"
        :title="`${column.name} · ${column.typeName.toLowerCase()}`"
        @click="onHeaderClick($event, column)"
      >
        <span class="grid-header-name">{{ column.name }}</span>
        <span v-if="sortColumn === column.name" class="grid-sort" aria-hidden="true">
          {{ sortDir === "desc" ? "▼" : "▲" }}
        </span>
        <span
          class="grid-resize"
          aria-hidden="true"
          @pointerdown="startResize($event, index)"
          @dblclick.stop="autoSize(index)"
        />
      </div>
    </div>
    <div class="grid-body" :style="{ height: `${bodyHeight}px` }">
      <div
        v-for="item in virtualRows"
        :key="item.index"
        class="grid-row"
        :class="{
          odd: item.index % 2 === 1,
          active: isRowActive(item.index),
          modified: modified?.has(item.index),
        }"
        role="row"
        :style="{ transform: `translateY(${item.start - headerHeight}px)` }"
      >
        <div class="grid-gutter" @mousedown.prevent="selectRow($event, item.index)">
          {{ (rowNumberOffset ?? 0) + item.index + 1 }}
        </div>
        <template v-if="rows[item.index]">
          <div
            v-for="(value, col) in rows[item.index]"
            :key="col"
            class="grid-cell"
            :class="{
              null: value === null,
              numeric: numeric[col],
              selected: isSelected(item.index, col),
              focused: isFocused(item.index, col),
              modified: isModified(item.index, col),
              editing: isEditing(item.index, col),
            }"
            :title="isEditing(item.index, col) ? undefined : cellTitle(value)"
            @mousedown.prevent="selectCell($event, item.index, col)"
            @dblclick="startEdit(item.index, col)"
          >
            <textarea
              v-if="isEditing(item.index, col)"
              :ref="focusEditor"
              v-model="draft"
              class="grid-editor"
              rows="1"
              spellcheck="false"
              autocomplete="off"
              autocapitalize="off"
              :placeholder="value === null ? 'NULL' : ''"
              @keydown="onEditorKeydown"
              @blur="commitEdit"
              @mousedown.stop
              @dblclick.stop
            />
            <template v-else>{{ cellDisplay(value) }}</template>
          </div>
        </template>
        <div v-else class="grid-cell grid-loading">Loading…</div>
      </div>
    </div>
    <div
      v-if="fillerRows"
      class="grid-filler"
      aria-hidden="true"
      :style="{ height: `${fillerHeight}px` }"
    >
      <div
        v-for="index in fillerRows"
        :key="index"
        class="grid-row"
        :class="{ odd: (rows.length + index - 1) % 2 === 1 }"
      >
        <div class="grid-gutter" />
        <div v-for="col in columns.length" :key="col" class="grid-cell" />
      </div>
    </div>
    <div v-if="!rows.length" class="grid-empty muted tiny">No rows</div>
  </div>
</template>
