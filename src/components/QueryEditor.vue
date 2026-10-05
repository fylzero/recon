<script setup lang="ts">
import { sql, type SQLNamespace } from "@codemirror/lang-sql";
import { Compartment, EditorState, Prec } from "@codemirror/state";
import { EditorView, keymap, placeholder } from "@codemirror/view";
import { basicSetup } from "codemirror";
import { format as formatSql, type SqlLanguage } from "sql-formatter";
import { computed, nextTick, onMounted, onUnmounted, ref, shallowRef, watch } from "vue";
import * as api from "../api";
import { useApp } from "../composables/useApp";
import { splitStatements, statementAt } from "../sql";
import { sqlDialect, sqlEditorTheme, sqlHighlighting } from "../sqlEditor";
import { exportSqlFile } from "../transfer";
import type { Driver, RowValues, StatementResult } from "../types";
import type { CopyFormat } from "../cells";
import { useGridFind } from "../composables/useGridFind";
import { placeAtPoint, useDismiss, type PopoverPosition } from "../composables/usePopover";
import DataGrid from "./DataGrid.vue";
import ExportRowsDialog, { type ExportRowsSource } from "./ExportRowsDialog.vue";
import GridFind from "./GridFind.vue";

const FETCH_BLOCK = 500;
const EDITOR_MIN = 80;

interface ResultState {
  result: StatementResult;
  rows: (RowValues | undefined)[];
  pending: Set<number>;
}

const props = defineProps<{
  connectionId: string;
  driver: Driver;
  schema: SQLNamespace;
  storageKey: string;
  active: boolean;
  savedSql?: string | null;
  title: string;
}>();

const emit = defineEmits<{
  executed: [statements: string[]];
  modified: [modified: boolean];
}>();

const { showToast } = useApp();

const host = ref<HTMLDivElement | null>(null);
const split = ref<HTMLDivElement | null>(null);
const editorHeight = ref(Number(localStorage.getItem("recon.editorHeight")) || 220);
const running = ref(false);
const cancelling = ref(false);
const results = shallowRef<ResultState[]>([]);
const activeResult = ref(0);
const runError = ref("");
const lastRunMs = ref<number | null>(null);
const language = new Compartment();
let view: EditorView | null = null;
let saveTimer: number | undefined;
let modified = false;

const current = computed(() => results.value[activeResult.value] ?? null);
const statementCount = computed(() => results.value.length);
const grid = ref<InstanceType<typeof DataGrid> | null>(null);
const find = useGridFind(() => current.value?.rows ?? []);
const findable = computed(() => Boolean(current.value?.result.columns.length && !current.value.result.error));

function gridElement() {
  return grid.value?.$el as HTMLElement | undefined;
}

const gridMenu = ref<PopoverPosition | null>(null);
const gridMenuEl = ref<HTMLElement | null>(null);

useDismiss(
  computed(() => Boolean(gridMenu.value)),
  () => [gridMenuEl.value],
  () => (gridMenu.value = null),
);

function openGridMenu(event: MouseEvent) {
  gridMenu.value = { left: event.clientX, top: event.clientY };
  void nextTick(() => {
    gridMenu.value = placeAtPoint(event.clientX, event.clientY, gridMenuEl.value);
  });
}

function copyFromMenu(format: CopyFormat) {
  gridMenu.value = null;
  void grid.value?.copySelection(format);
}

const exportSource = ref<ExportRowsSource | null>(null);

function exportResult() {
  const state = current.value;
  if (!state?.result.resultId) {
    return;
  }
  exportSource.value = {
    kind: "result",
    resultId: state.result.resultId,
    columns: state.result.columns.map((column) => column.name),
    rowCount: state.result.rowCount,
    truncated: state.result.truncated,
  };
}

function closeFind() {
  find.close();
  gridElement()?.focus({ preventScroll: true });
}

/** Runs after CodeMirror, which keeps ⌘F, ⌘G, and ⌘E for itself while the editor has focus. */
function onWindowKeydown(event: KeyboardEvent) {
  if (!props.active || event.defaultPrevented || !(event.metaKey || event.ctrlKey) || event.altKey) {
    return;
  }
  const target = event.target;
  if (target instanceof Node && host.value?.contains(target)) {
    return;
  }
  const key = event.key.toLowerCase();
  if (key === "f" && !event.shiftKey && findable.value) {
    event.preventDefault();
    find.show(grid.value?.focusedCell()?.position);
  } else if (key === "g" && find.open.value && findable.value) {
    event.preventDefault();
    find.step(event.shiftKey ? -1 : 1);
  } else if (key === "e" && !event.shiftKey && target instanceof Node && gridElement()?.contains(target)) {
    event.preventDefault();
    const cell = grid.value?.focusedCell();
    if (cell) {
      find.search(cell.text, cell.position);
    }
  }
}

const dialect = computed(() => sqlDialect(props.driver));

const formatterLanguage = computed<SqlLanguage>(() => {
  if (props.driver === "postgres") {
    return "postgresql";
  }
  return props.driver === "sqlite" ? "sqlite" : "mysql";
});

function languageExtension() {
  return sql({ dialect: dialect.value, schema: props.schema, upperCaseKeywords: true });
}

function storageId() {
  return `recon.query.${props.storageKey}`;
}

function persistDoc(text: string) {
  window.clearTimeout(saveTimer);
  saveTimer = window.setTimeout(() => {
    try {
      localStorage.setItem(storageId(), text);
    } catch {
      /* storage full; the editor text stays in memory */
    }
  }, 400);
}

function checkModified(text: string) {
  const next = props.savedSql != null && text !== props.savedSql;
  if (next !== modified) {
    modified = next;
    emit("modified", next);
  }
}

function statementsToRun(all: boolean) {
  if (!view) {
    return [];
  }
  const state = view.state;
  const doc = state.doc.toString();
  const selection = state.selection.main;
  if (!all && !selection.empty) {
    return splitStatements(state.sliceDoc(selection.from, selection.to), props.driver).map(
      (range) => range.text,
    );
  }
  const ranges = splitStatements(doc, props.driver);
  if (all) {
    return ranges.map((range) => range.text);
  }
  const range = statementAt(ranges, selection.head);
  if (range) {
    view.dispatch({ selection: { anchor: range.from, head: range.to } });
    window.setTimeout(() => {
      if (view && view.state.selection.main.from === range.from) {
        view.dispatch({ selection: { anchor: selection.head } });
      }
    }, 280);
  }
  return range ? [range.text] : [];
}

async function releaseResults(states: ResultState[]) {
  const ids = states.flatMap((state) => (state.result.resultId ? [state.result.resultId] : []));
  if (ids.length) {
    await api.closeResults(ids).catch(() => undefined);
  }
}

async function run(all = false) {
  if (running.value) {
    return;
  }
  const statements = statementsToRun(all);
  if (!statements.length) {
    return;
  }
  const previous = results.value;
  running.value = true;
  cancelling.value = false;
  runError.value = "";
  const started = performance.now();
  try {
    void releaseResults(previous);
    const output = await api.runQuery(props.connectionId, statements);
    results.value = output.map((result) => {
      const rows: (RowValues | undefined)[] = new Array(result.rowCount);
      result.rows.forEach((row, index) => {
        rows[index] = row;
      });
      return { result, rows, pending: new Set<number>() };
    });
    const failed = output.findIndex((result) => result.error);
    const firstGrid = output.findIndex((result) => result.columns.length);
    activeResult.value = failed !== -1 ? failed : firstGrid !== -1 ? firstGrid : 0;
    emit("executed", statements);
  } catch (err) {
    results.value = [];
    runError.value = String(err);
  } finally {
    lastRunMs.value = Math.round(performance.now() - started);
    running.value = false;
    cancelling.value = false;
  }
}

async function cancel() {
  if (!running.value || cancelling.value) {
    return;
  }
  cancelling.value = true;
  try {
    await api.cancelQuery(props.connectionId);
  } catch (err) {
    runError.value = String(err);
  }
}

async function loadRows(state: ResultState, start: number, end: number) {
  const resultId = state.result.resultId;
  if (!resultId) {
    return;
  }
  const firstBlock = Math.floor(start / FETCH_BLOCK);
  const lastBlock = Math.floor((end - 1) / FETCH_BLOCK);
  for (let block = firstBlock; block <= lastBlock; block += 1) {
    const offset = block * FETCH_BLOCK;
    if (state.pending.has(block) || state.rows[offset]) {
      continue;
    }
    state.pending.add(block);
    try {
      const rows = await api.fetchRows(resultId, offset, FETCH_BLOCK);
      if (!results.value.includes(state)) {
        return;
      }
      const next = state.rows.slice();
      rows.forEach((row, index) => {
        next[offset + index] = row;
      });
      state.rows = next;
      results.value = [...results.value];
    } catch (err) {
      runError.value = String(err);
    } finally {
      state.pending.delete(block);
    }
  }
}

function resultLabel(state: ResultState, index: number) {
  if (state.result.error) {
    return `Error`;
  }
  if (state.result.columns.length) {
    return `Result ${index + 1}`;
  }
  return `Statement ${index + 1}`;
}

function startResize(event: PointerEvent) {
  event.preventDefault();
  const startY = event.clientY;
  const startHeight = editorHeight.value;
  const max = Math.max((split.value?.clientHeight ?? 600) - 120, EDITOR_MIN);
  document.body.classList.add("resizing-rows");
  const onMove = (move: PointerEvent) => {
    editorHeight.value = Math.min(Math.max(startHeight + move.clientY - startY, EDITOR_MIN), max);
  };
  const onUp = () => {
    window.removeEventListener("pointermove", onMove);
    window.removeEventListener("pointerup", onUp);
    window.removeEventListener("pointercancel", onUp);
    document.body.classList.remove("resizing-rows");
    localStorage.setItem("recon.editorHeight", String(Math.round(editorHeight.value)));
  };
  window.addEventListener("pointermove", onMove);
  window.addEventListener("pointerup", onUp);
  window.addEventListener("pointercancel", onUp);
}

function focus() {
  view?.focus();
}

function insertText(text: string) {
  if (!view) {
    return;
  }
  const doc = view.state.doc;
  const prefix = doc.length && !doc.toString().endsWith("\n") ? "\n\n" : "";
  view.dispatch({
    changes: { from: doc.length, insert: `${prefix}${text}` },
    selection: { anchor: doc.length + prefix.length + text.length },
    scrollIntoView: true,
  });
  view.focus();
}

function getText() {
  return view?.state.doc.toString() ?? "";
}

function beautify() {
  if (!view) {
    return;
  }
  const selection = view.state.selection.main;
  const from = selection.empty ? 0 : selection.from;
  const to = selection.empty ? view.state.doc.length : selection.to;
  const source = view.state.sliceDoc(from, to);
  if (!source.trim()) {
    return;
  }
  let formatted: string;
  try {
    formatted = formatSql(source, {
      language: formatterLanguage.value,
      tabWidth: 2,
      keywordCase: "upper",
      linesBetweenQueries: 1,
    });
  } catch (err) {
    showToast(`Couldn't format SQL: ${err instanceof Error ? err.message : String(err)}`, "error");
    return;
  }
  if (formatted !== source) {
    view.dispatch({
      changes: { from, to, insert: formatted },
      selection: selection.empty ? { anchor: 0 } : { anchor: from, head: from + formatted.length },
      scrollIntoView: true,
    });
  }
  view.focus();
}

async function exportSql() {
  try {
    const path = await exportSqlFile(props.title, getText());
    if (path) {
      showToast(`Exported to ${path.split("/").pop()}`);
    }
  } catch (err) {
    showToast(String(err), "error");
  }
}

watch(
  () => props.savedSql,
  () => checkModified(getText()),
);

watch([() => props.schema, dialect], () => {
  view?.dispatch({ effects: language.reconfigure(languageExtension()) });
});

watch(
  () => props.active,
  (active) => {
    if (active) {
      void nextTick(focus);
    }
  },
);

onMounted(() => {
  if (!host.value) {
    return;
  }
  const runKeys = Prec.highest(
    keymap.of([
      { key: "Mod-Enter", run: () => (void run(false), true) },
      { key: "Shift-Mod-Enter", run: () => (void run(true), true) },
    ]),
  );
  const beautifyKey = Prec.highest(
    EditorView.domEventHandlers({
      keydown: (event) => {
        // Option changes event.key on macOS (⇧⌥F reports "Ï"), so match the physical key.
        if (event.code !== "KeyF" || !event.altKey || !event.shiftKey || event.metaKey || event.ctrlKey) {
          return false;
        }
        event.preventDefault();
        beautify();
        return true;
      },
    }),
  );
  view = new EditorView({
    parent: host.value,
    state: EditorState.create({
      doc: localStorage.getItem(storageId()) ?? "",
      extensions: [
        runKeys,
        beautifyKey,
        basicSetup,
        language.of(languageExtension()),
        sqlEditorTheme,
        sqlHighlighting,
        placeholder("Write SQL here. ⌘↵ runs the statement under the cursor."),
        EditorView.updateListener.of((update) => {
          if (update.docChanged) {
            const text = update.state.doc.toString();
            persistDoc(text);
            checkModified(text);
          }
        }),
      ],
    }),
  });
  checkModified(getText());
  if (props.active) {
    focus();
  }
  window.addEventListener("keydown", onWindowKeydown);
});

onUnmounted(() => {
  window.removeEventListener("keydown", onWindowKeydown);
  window.clearTimeout(saveTimer);
  if (view) {
    try {
      localStorage.setItem(storageId(), view.state.doc.toString());
    } catch {
      /* storage full */
    }
  }
  view?.destroy();
  view = null;
  void releaseResults(results.value);
});

defineExpose({ insertText, getText, focus, run });
</script>

<template>
  <div ref="split" class="query-editor">
    <div class="pane-toolbar">
      <button
        class="primary tiny"
        type="button"
        :disabled="running"
        title="Run the statement under the cursor, or the selection (⌘↵)"
        @click="run(false)"
      >
        <svg class="button-icon" viewBox="0 0 24 24" aria-hidden="true">
          <path d="M8 6.5v11l9-5.5Z" stroke-linejoin="round" />
        </svg>
        Run
      </button>
      <button
        class="ghost tiny"
        type="button"
        :disabled="running"
        title="Run every statement in the editor (⇧⌘↵)"
        @click="run(true)"
      >
        Run all
      </button>
      <button
        v-if="running"
        class="tiny danger"
        type="button"
        :disabled="cancelling"
        @click="cancel"
      >
        {{ cancelling ? "Cancelling…" : "Cancel" }}
      </button>
      <div class="pane-toolbar-end">
        <span v-if="running" class="action-progress">
          <span class="spinner" aria-hidden="true" />
          Running…
        </span>
        <span v-else-if="lastRunMs !== null" class="muted tiny">
          {{ statementCount }} {{ statementCount === 1 ? "statement" : "statements" }} ·
          {{ lastRunMs }}ms
        </span>
        <button
          class="ghost tiny"
          type="button"
          title="Format the SQL with line breaks and indentation, or just the selection (⇧⌥F)"
          @click="beautify"
        >
          <svg class="button-icon" viewBox="0 0 24 24" aria-hidden="true">
            <path d="M10 4.5 11.6 9a2 2 0 0 0 1.4 1.4l4.5 1.6-4.5 1.6a2 2 0 0 0-1.4 1.4L10 19.5 8.4 15a2 2 0 0 0-1.4-1.4L2.5 12 7 10.4A2 2 0 0 0 8.4 9Z" stroke-linejoin="round" />
            <path d="M18 3v4M16 5h4M19 16v3M17.5 17.5h3" />
          </svg>
          Beautify
        </button>
        <button class="ghost tiny" type="button" title="Save the editor contents as a .sql file" @click="exportSql">
          Export .sql
        </button>
      </div>
    </div>
    <div ref="host" class="query-editor-host" :style="{ height: `${editorHeight}px` }" />
    <div class="split-handle" role="separator" aria-orientation="horizontal" @pointerdown="startResize" />
    <div class="query-results">
      <p v-if="runError" class="pane-error">{{ runError }}</p>
      <div v-if="results.length > 1" class="result-tabs" role="tablist">
        <button
          v-for="(state, index) in results"
          :key="index"
          class="result-tab"
          type="button"
          role="tab"
          :class="{ active: activeResult === index, bad: Boolean(state.result.error) }"
          :aria-selected="activeResult === index"
          :title="state.result.sql"
          @click="activeResult = index"
        >
          {{ resultLabel(state, index) }}
        </button>
      </div>
      <template v-if="current">
        <div v-if="current.result.error" class="query-message bad">
          <strong>Query failed</strong>
          <pre>{{ current.result.error }}</pre>
          <pre class="muted">{{ current.result.sql }}</pre>
        </div>
        <div v-else-if="!current.result.columns.length" class="query-message">
          <strong>Query OK</strong>
          <span class="muted">
            {{ (current.result.rowsAffected ?? 0).toLocaleString() }}
            {{ current.result.rowsAffected === 1 ? "row" : "rows" }} affected ·
            {{ current.result.durationMs }}ms
          </span>
          <pre class="muted">{{ current.result.sql }}</pre>
        </div>
        <template v-else>
          <GridFind v-if="find.open.value" :find="find" @close="closeFind" />
          <DataGrid
            ref="grid"
            :columns="current.result.columns"
            :rows="current.rows"
            :matches="find.byRow.value"
            :current-match="find.current.value"
            context-menus
            @need-rows="(start, end) => current && loadRows(current, start, end)"
            @cell-menu="(_row, _col, event) => openGridMenu(event)"
            @row-menu="(_row, event) => openGridMenu(event)"
          />
          <div class="pane-status muted tiny">
            {{ current.result.rowCount.toLocaleString() }}
            {{ current.result.rowCount === 1 ? "row" : "rows" }} ·
            {{ current.result.durationMs }}ms
            <span v-if="current.result.truncated" class="warn-text">
              · Cut off at the query row limit. Change it in Settings → General.
            </span>
            <button
              class="ghost tiny pane-status-action"
              type="button"
              :disabled="!current.result.resultId"
              title="Export this result as CSV or JSON"
              @click="exportResult"
            >
              Export…
            </button>
          </div>
        </template>
      </template>
      <p v-else-if="!running && !runError" class="muted tiny query-empty">
        Results appear here. Separate statements with semicolons.
      </p>
    </div>
    <Teleport to="body">
      <div
        v-if="gridMenu"
        ref="gridMenuEl"
        class="overflow-menu-dropdown table-context-menu"
        role="menu"
        :style="{ left: `${gridMenu.left}px`, top: `${gridMenu.top}px` }"
        @contextmenu.prevent
      >
        <button class="overflow-menu-item" type="button" role="menuitem" aria-keyshortcuts="Meta+C" @click="copyFromMenu('tsv')">
          Copy
        </button>
        <button class="overflow-menu-item" type="button" role="menuitem" @click="copyFromMenu('csv')">
          Copy as CSV
        </button>
        <button class="overflow-menu-item" type="button" role="menuitem" @click="copyFromMenu('json')">
          Copy as JSON
        </button>
      </div>
    </Teleport>
    <ExportRowsDialog
      v-if="exportSource"
      :connection-id="connectionId"
      title="Export query result"
      :name="title"
      :source="exportSource"
      @close="exportSource = null"
    />
  </div>
</template>
