<script setup lang="ts">
import { writeText as writeClipboard } from "@tauri-apps/plugin-clipboard-manager";
import { confirm } from "@tauri-apps/plugin-dialog";
import { computed, nextTick, onMounted, ref, watch } from "vue";
import * as api from "../api";
import { useApp } from "../composables/useApp";
import { placeAtPoint, useDismiss } from "../composables/usePopover";
import { cellEditable, documentGrid, idLabel } from "../mongo";
import { shortcut } from "../platform";
import type { MongoCollection, MongoDocument, MongoIndex } from "../types";
import DataGrid, { type CellPosition } from "./DataGrid.vue";
import Modal from "./Modal.vue";
import MongoCode from "./MongoCode.vue";

const props = defineProps<{
  sessionId: string;
  db: string;
  collection: MongoCollection;
}>();

const emit = defineEmits<{
  /** Something failed; the pane decides whether the connection was lost. */
  failed: [message: string];
  /** Documents were added or removed, so counts may have changed. */
  changed: [];
}>();

const { pageSize, showToast } = useApp();

const view = ref<"documents" | "indexes">("documents");
const layout = ref<"grid" | "json">(readLayout());

const filter = ref("");
const sort = ref("");
const projection = ref("");
const showOptions = ref(false);
const applied = ref({ filter: "", sort: "", projection: "" });
const skip = ref(0);

const documents = ref<MongoDocument[]>([]);
const total = ref<number | null>(null);
const durationMs = ref<number | null>(null);
const loading = ref(false);
const queryError = ref("");
let token = 0;

const indexes = ref<MongoIndex[]>([]);
const indexesLoading = ref(false);
const indexesError = ref("");

const grid = ref<InstanceType<typeof DataGrid> | null>(null);
const filterInput = ref<HTMLInputElement | null>(null);

const readOnly = computed(() => props.collection.kind === "view");
const limit = computed(() => Math.max(1, pageSize.value || 300));
const table = computed(() => documentGrid(documents.value));
const pageEnd = computed(() => skip.value + documents.value.length);
const hasNext = computed(() =>
  total.value !== null ? pageEnd.value < total.value : documents.value.length === limit.value,
);
const queryDirty = computed(
  () =>
    filter.value.trim() !== applied.value.filter ||
    sort.value.trim() !== applied.value.sort ||
    projection.value.trim() !== applied.value.projection,
);

function readLayout(): "grid" | "json" {
  try {
    return localStorage.getItem("recon.mongoLayout") === "json" ? "json" : "grid";
  } catch {
    return "grid";
  }
}

watch(layout, (value) => {
  try {
    localStorage.setItem("recon.mongoLayout", value);
  } catch {
    /* a convenience only */
  }
});

async function load() {
  const current = ++token;
  loading.value = true;
  queryError.value = "";
  try {
    const page = await api.mongoFind(props.sessionId, props.db, props.collection.name, {
      ...applied.value,
      skip: skip.value,
      limit: limit.value,
    });
    if (current !== token) return;
    documents.value = page.documents;
    total.value = page.total;
    durationMs.value = page.durationMs;
    void nextTick(() => grid.value?.scrollToTop());
  } catch (err) {
    if (current !== token) return;
    queryError.value = String(err);
    emit("failed", queryError.value);
  } finally {
    if (current === token) loading.value = false;
  }
}

function runQuery() {
  applied.value = { filter: filter.value.trim(), sort: sort.value.trim(), projection: projection.value.trim() };
  skip.value = 0;
  void load();
}

function resetQuery() {
  filter.value = "";
  sort.value = "";
  projection.value = "";
  runQuery();
}

function page(step: number) {
  skip.value = Math.max(0, skip.value + step * limit.value);
  void load();
}

async function loadIndexes() {
  indexesLoading.value = true;
  indexesError.value = "";
  try {
    indexes.value = await api.mongoIndexes(props.sessionId, props.db, props.collection.name);
  } catch (err) {
    indexesError.value = String(err);
    emit("failed", indexesError.value);
  } finally {
    indexesLoading.value = false;
  }
}

watch(view, (next) => {
  if (next === "indexes") void loadIndexes();
});

function refresh() {
  if (view.value === "indexes") void loadIndexes();
  else void load();
}

/* Editing in the grid. */

function canEditCell(row: number, col: number) {
  if (readOnly.value || documents.value[row]?.id === undefined) return false;
  const column = table.value.columns[col];
  return Boolean(column) && cellEditable(table.value.kinds[row]?.[col], column.name);
}

function replaceDocument(index: number, next: MongoDocument) {
  documents.value = documents.value.map((doc, at) => (at === index ? next : doc));
}

async function setField(row: number, col: number, text: string | null) {
  const doc = documents.value[row];
  const field = table.value.columns[col]?.name;
  if (!doc || !field) return;
  try {
    const saved = await api.mongoSetField(props.sessionId, props.db, props.collection.name, doc.id, field, text);
    replaceDocument(row, saved);
  } catch (err) {
    showToast(String(err), "error");
    emit("failed", String(err));
  }
}

function onEdit(row: number, col: number, text: string) {
  void setField(row, col, text);
}

async function onSetNull(cells: CellPosition[]) {
  for (const cell of cells.filter((cell) => canEditCell(cell.row, cell.col))) {
    await setField(cell.row, cell.col, null);
  }
}

/** Double-clicking a value the grid can't edit, like an array, opens the whole document. */
function onGridDblclick() {
  const focused = grid.value?.focusedCell();
  if (!focused) return;
  const { row, col } = focused.position;
  if (!canEditCell(row, col) && documents.value[row]) openEditor(row);
}

/* Document editor. */

const editor = ref<{
  mode: "edit" | "insert" | "view";
  index: number;
  text: string;
  original: string;
  error: string;
  busy: boolean;
} | null>(null);
const editorRef = ref<InstanceType<typeof MongoCode> | null>(null);

function openEditor(index: number) {
  const doc = documents.value[index];
  if (!doc) return;
  const mode = readOnly.value || doc.id === undefined ? "view" : "edit";
  editor.value = { mode, index, text: doc.text, original: doc.text, error: "", busy: false };
  void nextTick(() => editorRef.value?.focus());
}

function openInsert(text = "{\n  \n}") {
  if (readOnly.value) return;
  editor.value = { mode: "insert", index: -1, text, original: text, error: "", busy: false };
  void nextTick(() => editorRef.value?.focus());
}

function duplicate(index: number) {
  const doc = documents.value[index];
  if (!doc) return;
  // Drops the _id line so MongoDB gives the copy a new one.
  const text = doc.text.replace(/^ {2}"_id": .*\n/m, "");
  openInsert(text);
}

async function closeEditor() {
  const form = editor.value;
  if (form && form.mode !== "view" && form.text !== form.original) {
    const ok = await confirm("Discard your changes to this document?", {
      title: "Unsaved changes",
      kind: "warning",
      okLabel: "Discard",
    });
    if (!ok) return;
  }
  editor.value = null;
}

async function saveEditor() {
  const form = editor.value;
  if (!form || form.busy || form.mode === "view") return;
  form.busy = true;
  form.error = "";
  try {
    if (form.mode === "insert") {
      const created = await api.mongoInsert(props.sessionId, props.db, props.collection.name, form.text);
      documents.value = [...documents.value, created];
      if (total.value !== null) total.value += 1;
      emit("changed");
      showToast("Inserted the document.");
    } else {
      const doc = documents.value[form.index];
      const saved = await api.mongoReplace(props.sessionId, props.db, props.collection.name, doc.id, form.text);
      replaceDocument(form.index, saved);
      showToast("Saved the document.");
    }
    editor.value = null;
  } catch (err) {
    if (editor.value) editor.value.error = String(err);
  } finally {
    if (editor.value) editor.value.busy = false;
  }
}

/* Deleting. */

async function deleteRows(rows: number[]) {
  if (readOnly.value) return;
  const targets = rows.map((row) => documents.value[row]).filter((doc) => doc && doc.id !== undefined);
  if (!targets.length) return;
  const message =
    targets.length === 1
      ? `Delete the document ${idLabel(targets[0].id)}? This can't be undone.`
      : `Delete ${targets.length.toLocaleString()} documents? This can't be undone.`;
  const ok = await confirm(message, { title: "Delete documents", kind: "warning", okLabel: "Delete" });
  if (!ok) return;
  try {
    const deleted = await api.mongoDelete(
      props.sessionId,
      props.db,
      props.collection.name,
      targets.map((doc) => doc.id),
    );
    const removed = new Set(targets);
    documents.value = documents.value.filter((doc) => !removed.has(doc));
    if (total.value !== null) total.value = Math.max(0, total.value - deleted);
    emit("changed");
    showToast(deleted === 1 ? "Deleted the document." : `Deleted ${deleted.toLocaleString()} documents.`);
  } catch (err) {
    showToast(String(err), "error");
    emit("failed", String(err));
  }
}

function selectedRows() {
  if (layout.value === "json") return jsonSelected.value === null ? [] : [jsonSelected.value];
  return grid.value?.selectedRows() ?? [];
}

const jsonSelected = ref<number | null>(null);

function editSelected() {
  const [row] = selectedRows();
  if (row !== undefined) openEditor(row);
}

function deleteSelected() {
  void deleteRows(selectedRows());
}

/* Context menu. */

const menu = ref<{ rows: number[] } | null>(null);
const menuPosition = ref({ left: 0, top: 0 });
const menuEl = ref<HTMLElement | null>(null);

useDismiss(
  computed(() => Boolean(menu.value)),
  () => [menuEl.value],
  () => (menu.value = null),
);

function onCellMenu(row: number, _col: number, event: MouseEvent) {
  const selected = grid.value?.selectedRows() ?? [];
  openMenu(selected.includes(row) ? selected : [row], event);
}

function openMenu(rows: number[], event: MouseEvent) {
  menu.value = { rows };
  menuPosition.value = { left: event.clientX, top: event.clientY };
  void nextTick(() => {
    menuPosition.value = placeAtPoint(event.clientX, event.clientY, menuEl.value);
  });
}

async function copyDocuments(rows: number[]) {
  const texts = rows.map((row) => documents.value[row]?.text).filter(Boolean);
  await writeClipboard(texts.length === 1 ? texts[0] : `[\n${texts.join(",\n")}\n]`);
  showToast(texts.length === 1 ? "Copied the document." : `Copied ${texts.length} documents.`);
}

async function copyId(row: number) {
  const id = documents.value[row]?.id;
  if (id === undefined) return;
  await writeClipboard(idLabel(id));
  showToast("Copied the _id.");
}

function fromMenu(action: (rows: number[]) => void) {
  const rows = menu.value?.rows ?? [];
  menu.value = null;
  action(rows);
}

function onQueryKeydown(event: KeyboardEvent) {
  if (event.key === "Enter") {
    event.preventDefault();
    runQuery();
  }
}

function focusFilter() {
  view.value = "documents";
  void nextTick(() => {
    filterInput.value?.focus();
    filterInput.value?.select();
  });
}

function hasChanges() {
  return Boolean(editor.value && editor.value.mode !== "view" && editor.value.text !== editor.value.original);
}

watch(
  () => [props.db, props.collection.name],
  () => {
    filter.value = "";
    sort.value = "";
    projection.value = "";
    applied.value = { filter: "", sort: "", projection: "" };
    skip.value = 0;
    documents.value = [];
    total.value = null;
    jsonSelected.value = null;
    editor.value = null;
    if (view.value === "indexes") void loadIndexes();
    void load();
  },
);

onMounted(load);

defineExpose({ refresh, insert: () => openInsert(), focusFilter, hasChanges });
</script>

<template>
  <section class="redis-key mongo-collection">
    <header class="redis-key-header mongo-collection-header">
      <div class="redis-key-title">
        <span class="redis-kind-tag mongo-kind-tag" :class="`kind-${collection.kind}`">
          {{ collection.kind === "view" ? "VIEW" : collection.kind === "timeseries" ? "TS" : "COLL" }}
        </span>
        <span class="redis-key-name" :title="`${db}.${collection.name}`">{{ collection.name }}</span>
      </div>
      <div class="redis-key-actions">
        <div class="segmented" role="tablist" aria-label="Collection view">
          <button
            type="button"
            role="tab"
            :aria-selected="view === 'documents'"
            :class="{ active: view === 'documents' }"
            @click="view = 'documents'"
          >
            Documents
          </button>
          <button
            type="button"
            role="tab"
            :aria-selected="view === 'indexes'"
            :class="{ active: view === 'indexes' }"
            @click="view = 'indexes'"
          >
            Indexes
          </button>
        </div>
      </div>
    </header>

    <template v-if="view === 'documents'">
      <form class="mongo-query" @submit.prevent="runQuery">
        <div class="mongo-query-row">
          <span class="muted tiny mongo-query-label">Filter</span>
          <input
            ref="filterInput"
            v-model="filter"
            class="code-input"
            type="text"
            spellcheck="false"
            autocomplete="off"
            placeholder="{ status: 'active', age: { $gt: 30 } }"
            aria-label="Filter"
            @keydown="onQueryKeydown"
          />
          <button
            class="ghost tiny"
            type="button"
            :class="{ active: showOptions || sort || projection }"
            :aria-expanded="showOptions"
            title="Sort and projection"
            @click="showOptions = !showOptions"
          >
            Options
          </button>
          <button class="ghost tiny" type="button" :disabled="!filter && !sort && !projection && !queryDirty" @click="resetQuery">
            Reset
          </button>
          <button class="primary tiny" type="submit" :disabled="loading">Find</button>
        </div>
        <template v-if="showOptions || sort || projection">
          <div class="mongo-query-row">
            <span class="muted tiny mongo-query-label">Sort</span>
            <input
              v-model="sort"
              class="code-input"
              type="text"
              spellcheck="false"
              autocomplete="off"
              placeholder="{ createdAt: -1 }"
              aria-label="Sort"
              @keydown="onQueryKeydown"
            />
          </div>
          <div class="mongo-query-row">
            <span class="muted tiny mongo-query-label">Project</span>
            <input
              v-model="projection"
              class="code-input"
              type="text"
              spellcheck="false"
              autocomplete="off"
              placeholder="{ name: 1, email: 1 }"
              aria-label="Projection"
              @keydown="onQueryKeydown"
            />
          </div>
        </template>
        <p v-if="queryError" class="settings-error mongo-query-error">{{ queryError }}</p>
      </form>

      <div class="redis-collection-bar mongo-toolbar">
        <div class="segmented" role="radiogroup" aria-label="Layout">
          <button type="button" role="radio" :aria-checked="layout === 'grid'" :class="{ active: layout === 'grid' }" @click="layout = 'grid'">
            Table
          </button>
          <button type="button" role="radio" :aria-checked="layout === 'json'" :class="{ active: layout === 'json' }" @click="layout = 'json'">
            JSON
          </button>
        </div>
        <span v-if="readOnly" class="muted tiny">Views are read-only.</span>
        <div class="redis-save-bar">
          <template v-if="!readOnly">
            <button class="ghost tiny" type="button" @click="openInsert()">Insert</button>
            <button class="ghost tiny" type="button" :disabled="!documents.length" @click="editSelected">Edit</button>
            <button class="ghost tiny danger" type="button" :disabled="!documents.length" @click="deleteSelected">Delete</button>
          </template>
          <button v-else class="ghost tiny" type="button" :disabled="!documents.length" @click="editSelected">View</button>
        </div>
      </div>

      <div v-if="layout === 'grid'" class="redis-grid" @dblclick.capture="onGridDblclick">
        <DataGrid
          ref="grid"
          :columns="table.columns"
          :rows="table.rows"
          :row-number-offset="skip"
          :cell-editable="canEditCell"
          :deletable="!readOnly"
          context-menus
          @edit="onEdit"
          @set-null="onSetNull"
          @delete-rows="deleteRows"
          @cell-menu="onCellMenu"
          @row-menu="(row, event) => onCellMenu(row, 0, event)"
        />
      </div>
      <div v-else class="mongo-json-list">
        <article
          v-for="(doc, index) in documents"
          :key="index"
          class="mongo-json-doc"
          :class="{ selected: jsonSelected === index }"
          @click="jsonSelected = index"
          @dblclick="openEditor(index)"
          @contextmenu.prevent="openMenu([index], $event)"
        >
          <pre>{{ doc.text }}</pre>
        </article>
      </div>
      <p v-if="!loading && !queryError && !documents.length" class="muted tiny mongo-empty">
        {{ applied.filter ? "No documents match the filter." : skip ? "No more documents." : "This collection is empty." }}
      </p>

      <footer class="redis-grid-footer">
        <span class="muted tiny">
          <template v-if="loading"><span class="spinner" aria-hidden="true" /> Loading…</template>
          <template v-else-if="documents.length">
            {{ (skip + 1).toLocaleString() }}–{{ pageEnd.toLocaleString() }}
            <template v-if="total !== null"> of {{ total.toLocaleString() }}</template>
            <template v-if="durationMs !== null"> · {{ durationMs.toLocaleString() }} ms</template>
          </template>
          <template v-else-if="total !== null">{{ total.toLocaleString() }} documents</template>
        </span>
        <button class="ghost tiny" type="button" :disabled="loading || skip === 0" @click="page(-1)">Previous</button>
        <button class="ghost tiny" type="button" :disabled="loading || !hasNext" @click="page(1)">Next</button>
        <button class="ghost tiny" type="button" :title="`Refresh (${shortcut('⌘R')})`" :disabled="loading" @click="load">
          Refresh
        </button>
      </footer>
    </template>

    <div v-else class="mongo-indexes">
      <p v-if="indexesLoading && !indexes.length" class="muted tiny"><span class="spinner" aria-hidden="true" /> Loading indexes…</p>
      <p v-else-if="indexesError" class="settings-error">{{ indexesError }}</p>
      <p v-else-if="!indexes.length" class="muted tiny">
        {{ collection.kind === "view" ? "Views have no indexes of their own." : "No indexes." }}
      </p>
      <table v-else class="mongo-index-table">
        <thead>
          <tr>
            <th>Name</th>
            <th>Keys</th>
            <th>Properties</th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="index in indexes" :key="index.name" :title="index.text">
            <td class="mongo-index-name">{{ index.name }}</td>
            <td><code>{{ index.keys }}</code></td>
            <td class="mongo-index-props">
              <span v-if="index.unique" class="mongo-badge">unique</span>
              <span v-if="index.sparse" class="mongo-badge">sparse</span>
              <span v-if="index.hidden" class="mongo-badge">hidden</span>
              <span v-if="index.ttlSeconds !== null" class="mongo-badge">TTL {{ index.ttlSeconds.toLocaleString() }}s</span>
              <span v-if="index.partial" class="mongo-badge" :title="index.partial">partial</span>
            </td>
          </tr>
        </tbody>
      </table>
      <p class="muted tiny mongo-index-hint">
        Create or drop indexes in the Console, with <code>db.getCollection("{{ collection.name }}").createIndex({ field: 1 })</code>.
      </p>
    </div>

    <Teleport to="body">
      <div
        v-if="menu"
        ref="menuEl"
        class="overflow-menu-dropdown table-context-menu"
        role="menu"
        :style="{ left: `${menuPosition.left}px`, top: `${menuPosition.top}px` }"
        @contextmenu.prevent
      >
        <button
          v-if="menu.rows.length === 1"
          class="overflow-menu-item"
          type="button"
          role="menuitem"
          @click="fromMenu((rows) => openEditor(rows[0]))"
        >
          {{ readOnly ? "View document" : "Edit document" }}
        </button>
        <button
          v-if="menu.rows.length === 1 && !readOnly"
          class="overflow-menu-item"
          type="button"
          role="menuitem"
          @click="fromMenu((rows) => duplicate(rows[0]))"
        >
          Duplicate
        </button>
        <button class="overflow-menu-item" type="button" role="menuitem" @click="fromMenu(copyDocuments)">
          {{ menu.rows.length === 1 ? "Copy document" : `Copy ${menu.rows.length} documents` }}
        </button>
        <button
          v-if="menu.rows.length === 1"
          class="overflow-menu-item"
          type="button"
          role="menuitem"
          @click="fromMenu((rows) => copyId(rows[0]))"
        >
          Copy _id
        </button>
        <template v-if="!readOnly">
          <div class="overflow-menu-divider" role="separator" />
          <button class="overflow-menu-item danger" type="button" role="menuitem" @click="fromMenu(deleteRows)">
            {{ menu.rows.length === 1 ? "Delete document" : `Delete ${menu.rows.length} documents` }}
          </button>
        </template>
      </div>
    </Teleport>

    <Modal
      v-if="editor"
      :title="editor.mode === 'insert' ? 'Insert document' : editor.mode === 'view' ? 'Document' : 'Edit document'"
      wide
      @close="closeEditor"
    >
      <form class="redis-dialog mongo-editor" @submit.prevent="saveEditor">
        <MongoCode ref="editorRef" v-model="editor.text" :readonly="editor.mode === 'view'" @submit="saveEditor" />
        <p v-if="editor.error" class="settings-error">{{ editor.error }}</p>
        <p v-else-if="editor.mode !== 'view'" class="muted tiny">
          Types are kept as written, like <code>NumberLong(5)</code>, <code>ISODate("…")</code>, and
          <code>ObjectId("…")</code>.{{ editor.mode === "insert" ? " Leave out _id and MongoDB adds one." : "" }}
        </p>
        <div class="modal-actions">
          <button class="ghost" type="button" @click="closeEditor">{{ editor.mode === "view" ? "Close" : "Cancel" }}</button>
          <button v-if="editor.mode !== 'view'" class="primary" type="submit" :disabled="editor.busy" :title="shortcut('⌘S')">
            {{ editor.mode === "insert" ? "Insert" : "Save" }}
          </button>
        </div>
      </form>
    </Modal>
  </section>
</template>
