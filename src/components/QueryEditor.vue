<script setup lang="ts">
import { MySQL, PostgreSQL, SQLite, sql, type SQLNamespace } from "@codemirror/lang-sql";
import { HighlightStyle, syntaxHighlighting } from "@codemirror/language";
import { Compartment, EditorState, Prec } from "@codemirror/state";
import { EditorView, keymap, placeholder } from "@codemirror/view";
import { tags as t } from "@lezer/highlight";
import { basicSetup } from "codemirror";
import { computed, nextTick, onMounted, onUnmounted, ref, shallowRef, watch } from "vue";
import * as api from "../api";
import { splitStatements, statementAt } from "../sql";
import type { Driver, RowValues, StatementResult } from "../types";
import DataGrid from "./DataGrid.vue";

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
}>();

const emit = defineEmits<{
  executed: [statements: string[]];
}>();

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

const current = computed(() => results.value[activeResult.value] ?? null);
const statementCount = computed(() => results.value.length);

const dialect = computed(() => {
  if (props.driver === "postgres") {
    return PostgreSQL;
  }
  return props.driver === "sqlite" ? SQLite : MySQL;
});

const editorTheme = EditorView.theme(
  {
    "&": {
      height: "100%",
      color: "var(--text)",
      backgroundColor: "#0d1016",
      fontSize: "var(--editor-font-size)",
    },
    "&.cm-focused": { outline: "none" },
    ".cm-scroller": {
      fontFamily: "var(--editor-font-family)",
      lineHeight: "1.55",
    },
    ".cm-content": { caretColor: "var(--text)", padding: "0.45rem 0" },
    ".cm-gutters": {
      backgroundColor: "#0d1016",
      color: "var(--muted)",
      borderRight: "1px solid var(--border)",
    },
    ".cm-activeLine": { backgroundColor: "rgba(255, 255, 255, 0.03)" },
    ".cm-activeLineGutter": { backgroundColor: "transparent" },
    ".cm-selectionBackground, &.cm-focused .cm-selectionBackground": {
      backgroundColor: "#2a3344",
    },
    ".cm-cursor": { borderLeftColor: "var(--text)" },
    ".cm-tooltip": {
      backgroundColor: "var(--bg-raised)",
      border: "1px solid var(--border)",
      color: "var(--text)",
    },
    ".cm-tooltip-autocomplete > ul > li[aria-selected]": {
      backgroundColor: "var(--bg-active)",
      color: "var(--text)",
    },
    ".cm-placeholder": { color: "var(--muted)" },
  },
  { dark: true },
);

const editorHighlight = HighlightStyle.define([
  { tag: t.keyword, color: "#c4a6ff" },
  { tag: [t.string, t.special(t.string)], color: "#3dd68c" },
  { tag: t.number, color: "#e6c07b" },
  { tag: t.bool, color: "#9ec1ff" },
  { tag: t.null, color: "#9ec1ff" },
  { tag: [t.lineComment, t.blockComment], color: "#6b7588", fontStyle: "italic" },
  { tag: t.typeName, color: "#7ee0c7" },
  { tag: [t.operator, t.punctuation], color: "#aab4c6" },
  { tag: t.special(t.name), color: "#f0a36b" },
  { tag: t.invalid, color: "var(--bad)" },
]);

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
  view = new EditorView({
    parent: host.value,
    state: EditorState.create({
      doc: localStorage.getItem(storageId()) ?? "",
      extensions: [
        runKeys,
        basicSetup,
        language.of(languageExtension()),
        editorTheme,
        syntaxHighlighting(editorHighlight),
        placeholder("Write SQL here. ⌘↵ runs the statement under the cursor."),
        EditorView.updateListener.of((update) => {
          if (update.docChanged) {
            persistDoc(update.state.doc.toString());
          }
        }),
      ],
    }),
  });
  if (props.active) {
    focus();
  }
});

onUnmounted(() => {
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

defineExpose({ insertText, focus, run });
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
          <DataGrid
            :columns="current.result.columns"
            :rows="current.rows"
            @need-rows="(start, end) => current && loadRows(current, start, end)"
          />
          <div class="pane-status muted tiny">
            {{ current.result.rowCount.toLocaleString() }}
            {{ current.result.rowCount === 1 ? "row" : "rows" }} ·
            {{ current.result.durationMs }}ms
            <span v-if="current.result.truncated" class="warn-text">
              · Cut off at the query row limit. Change it in Settings → General.
            </span>
          </div>
        </template>
      </template>
      <p v-else-if="!running && !runError" class="muted tiny query-empty">
        Results appear here. Separate statements with semicolons.
      </p>
    </div>
  </div>
</template>
