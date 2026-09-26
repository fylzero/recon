<script setup lang="ts">
import type { SQLNamespace } from "@codemirror/lang-sql";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { confirm } from "@tauri-apps/plugin-dialog";
import { computed, nextTick, onMounted, onUnmounted, ref, shallowRef, watch } from "vue";
import * as api from "../api";
import { SIDEBAR_MAX, SIDEBAR_MIN, useApp } from "../composables/useApp";
import { useConnectionForm } from "../composables/useConnectionForm";
import { registerInnerTabCloser, setLiveTitle, useTabs } from "../composables/useTabs";
import { driverLabel, type SessionInfo, type TableInfo } from "../types";
import ConnectionViewTabs, { type ConnectionViewTab } from "./ConnectionViewTabs.vue";
import DatabaseSwitcher from "./DatabaseSwitcher.vue";
import DriverIcon from "./DriverIcon.vue";
import Modal from "./Modal.vue";
import QueryEditor from "./QueryEditor.vue";
import TableView from "./TableView.vue";

type PaneTab =
  | { id: string; kind: "table"; namespace: string; table: string; tableKind: "table" | "view" }
  | { id: string; kind: "query"; key: string; title: string };

const props = defineProps<{
  connectionId: string;
  sessionId: string;
  initialNamespace?: string;
  active: boolean;
}>();

const { findConnection, sidebarWidth, previewPreferences, savePreferences, showToast } = useApp();
const { openEditConnection } = useConnectionForm();
const { openConnectionTab } = useTabs();

type ConnectionEvent = { connectionId: string; error: string | null };

const status = ref<"connecting" | "password" | "connected" | "error">("connecting");
const connectError = ref("");
const lost = ref(false);
const lostError = ref("");
const reconnecting = ref(false);
const password = ref("");
const passwordInput = ref<HTMLInputElement | null>(null);
const session = shallowRef<SessionInfo | null>(null);
const namespaces = ref<string[]>([]);
const namespace = ref("");
const tables = ref<TableInfo[]>([]);
const tablesLoading = ref(false);
const tablesError = ref("");
const filter = ref("");
const schema = shallowRef<SQLNamespace>({});
const tabs = ref<PaneTab[]>([]);
const view = ref<ConnectionViewTab>("tables");
const activeTableTabId = ref("");
const activeQueryTabId = ref("");
const tabsRestored = ref(false);
const dirtyTabs = ref(new Map<string, number>());
const saving = ref(false);
const nameDialog = ref<{ mode: "create" } | { mode: "rename"; from: string } | null>(null);
const nameValue = ref("");
const nameError = ref("");
const nameBusy = ref(false);
const nameInput = ref<HTMLInputElement | null>(null);
const sidebarEl = ref<HTMLElement | null>(null);
const editors = new Map<string, InstanceType<typeof QueryEditor>>();
const tableViews = new Map<string, InstanceType<typeof TableView>>();

const unsavedChanges = computed(() => [...dirtyTabs.value.values()].reduce((sum, count) => sum + count, 0));
const unsavedLabel = computed(() => {
  const changes = unsavedChanges.value;
  const tableCount = dirtyTabs.value.size;
  const changeText = `${changes.toLocaleString()} unsaved ${changes === 1 ? "change" : "changes"}`;
  return tableCount > 1 ? `${changeText} in ${tableCount} tables` : changeText;
});

const match = computed(() => findConnection(props.connectionId));
const extraTab = computed(() => props.sessionId !== props.connectionId);
const entry = computed(() => match.value?.connection ?? null);
const driver = computed(() => entry.value?.driver ?? "mysql");
const namespaceLabel = computed(() => session.value?.namespaceLabel ?? "Database");
const needsPassword = computed(
  () => Boolean(entry.value && entry.value.driver !== "sqlite" && !entry.value.savePassword),
);

const databaseTitle = computed(() => {
  const connection = entry.value;
  if (!connection) {
    return "";
  }
  if (connection.driver === "sqlite") {
    return connection.filePath.split("/").pop() || connection.name;
  }
  if (connection.driver === "postgres") {
    return connection.database || connection.name;
  }
  return namespace.value || connection.name;
});

watch(
  () => (status.value === "connected" ? databaseTitle.value : ""),
  (title) => setLiveTitle(props.sessionId, title),
  { immediate: true },
);

const queryTabsKey = computed(() => `recon.queryTabs.${props.sessionId}`);

const viewTabs = computed(() =>
  tabs.value.filter((tab) => (view.value === "sql" ? tab.kind === "query" : tab.kind === "table")),
);
const activeTabId = computed(() =>
  view.value === "sql" ? activeQueryTabId.value : activeTableTabId.value,
);

const filteredTables = computed(() => {
  const needle = filter.value.trim().toLowerCase();
  return needle
    ? tables.value.filter((table) => table.name.toLowerCase().includes(needle))
    : tables.value;
});

const subtitle = computed(() => {
  const connection = entry.value;
  if (!connection) {
    return "";
  }
  if (connection.driver === "sqlite") {
    return connection.filePath.split("/").pop() ?? connection.filePath;
  }
  const address = `${connection.user}@${connection.host}:${connection.port}`;
  return connection.ssh?.enabled ? `${address} via ${connection.ssh.host}` : address;
});

function nextQueryKey() {
  return `${props.sessionId}:${Date.now().toString(36)}${Math.random().toString(36).slice(2, 6)}`;
}

function queryTitle() {
  const used = new Set(
    tabs.value.flatMap((tab) => (tab.kind === "query" ? [tab.title] : [])),
  );
  let index = 1;
  while (used.has(`Query ${index}`)) {
    index += 1;
  }
  return `Query ${index}`;
}

function saveQueryTabs() {
  const saved = tabs.value.flatMap((tab) =>
    tab.kind === "query" ? [{ key: tab.key, title: tab.title }] : [],
  );
  localStorage.setItem(queryTabsKey.value, JSON.stringify(saved));
}

function restoreQueryTabs() {
  let saved: { key: string; title: string }[] = [];
  try {
    const parsed = JSON.parse(localStorage.getItem(queryTabsKey.value) ?? "[]");
    if (Array.isArray(parsed)) {
      saved = parsed.filter(
        (item) => typeof item?.key === "string" && typeof item?.title === "string",
      );
    }
  } catch {
    saved = [];
  }
  const singleKey = `recon.queryKey.${props.sessionId}`;
  const single = localStorage.getItem(singleKey);
  if (single && !saved.some((item) => item.key === single)) {
    saved = [{ key: single, title: "Query 1" }, ...saved];
  }
  localStorage.removeItem(singleKey);
  tabs.value = saved.map((item) => ({
    id: `query:${item.key}`,
    kind: "query",
    key: item.key,
    title: item.title,
  }));
  activeQueryTabId.value = tabs.value[0]?.id ?? "";
  tabsRestored.value = true;
  saveQueryTabs();
}

function openQueryTab(initialSql?: string) {
  const key = nextQueryKey();
  const tab: PaneTab = { id: `query:${key}`, kind: "query", key, title: queryTitle() };
  tabs.value = [...tabs.value, tab];
  activeQueryTabId.value = tab.id;
  view.value = "sql";
  saveQueryTabs();
  if (initialSql) {
    void nextTick(() => editors.get(tab.id)?.insertText(initialSql));
  }
}

function openTable(table: TableInfo) {
  const id = `table:${namespace.value}.${table.name}`;
  if (!tabs.value.some((tab) => tab.id === id)) {
    tabs.value = [
      ...tabs.value,
      { id, kind: "table", namespace: namespace.value, table: table.name, tableKind: table.kind },
    ];
  }
  activeTableTabId.value = id;
}

function selectTab(tab: PaneTab) {
  if (tab.kind === "query") {
    activeQueryTabId.value = tab.id;
  } else {
    activeTableTabId.value = tab.id;
  }
}

function selectView(next: ConnectionViewTab) {
  view.value = next;
  if (next === "sql" && !tabs.value.some((tab) => tab.kind === "query")) {
    openQueryTab();
  }
}

function setTabChanges(id: string, rows: number) {
  if ((dirtyTabs.value.get(id) ?? 0) === rows) {
    return;
  }
  const next = new Map(dirtyTabs.value);
  if (rows) {
    next.set(id, rows);
  } else {
    next.delete(id);
  }
  dirtyTabs.value = next;
}

function setTableViewRef(id: string, instance: unknown) {
  if (instance) {
    tableViews.set(id, instance as InstanceType<typeof TableView>);
  } else {
    tableViews.delete(id);
  }
}

function dirtyTableViews() {
  return [...dirtyTabs.value.keys()].flatMap((id) => tableViews.get(id) ?? []);
}

async function saveAll() {
  if (saving.value) {
    return;
  }
  const pending = [...tableViews.values()].map((view) => ({ view, ...view.pendingChanges() }));
  const changed = pending.filter(
    ({ request }) =>
      request.updates.length ||
      request.inserts.length ||
      request.columns.length ||
      request.newColumns.length ||
      request.indexes.length ||
      request.newIndexes.length,
  );
  if (!changed.length) {
    return;
  }
  saving.value = true;
  try {
    const count = await api.saveTableChanges(
      props.sessionId,
      changed.map((item) => item.request),
    );
    for (const item of changed) {
      item.view.markSaved(item.snapshot);
    }
    if (changed.some((item) => item.request.columns.length)) {
      void loadSchema();
    }
    const tableText = changed.length > 1 ? ` in ${changed.length} tables` : "";
    showToast(`Saved ${count.toLocaleString()} ${count === 1 ? "change" : "changes"}${tableText}`);
  } catch (err) {
    showToast(String(err), "error");
  } finally {
    saving.value = false;
  }
}

function discardAll() {
  for (const view of dirtyTableViews()) {
    view.discard();
  }
}

function onWindowKeydown(event: KeyboardEvent) {
  if (
    !props.active ||
    !(event.metaKey || event.ctrlKey) ||
    event.altKey ||
    event.shiftKey ||
    event.key.toLowerCase() !== "s"
  ) {
    return;
  }
  event.preventDefault();
  void saveAll();
}

async function confirmCloseTab(id: string, table: string) {
  const ok = await confirm(`Discard unsaved changes to “${table}”?`, {
    title: "Unsaved changes",
    kind: "warning",
    okLabel: "Discard",
    cancelLabel: "Cancel",
  });
  if (ok) {
    removeTab(id);
  }
}

function closeTab(id: string) {
  const tab = tabs.value.find((item) => item.id === id);
  if (tab?.kind === "table" && dirtyTabs.value.has(id)) {
    void confirmCloseTab(id, tab.table);
    return;
  }
  removeTab(id);
}

function removeTab(id: string) {
  const tab = tabs.value.find((item) => item.id === id);
  if (!tab) {
    return;
  }
  const siblings = tabs.value.filter((item) => item.kind === tab.kind);
  const index = siblings.indexOf(tab);
  const remaining = siblings.filter((item) => item.id !== id);
  const next = remaining[Math.min(index, remaining.length - 1)]?.id ?? "";
  tabs.value = tabs.value.filter((item) => item.id !== id);
  if (tab.kind === "query") {
    localStorage.removeItem(`recon.query.${tab.key}`);
    editors.delete(id);
    saveQueryTabs();
    if (activeQueryTabId.value === id) {
      activeQueryTabId.value = next;
    }
  } else if (activeTableTabId.value === id) {
    activeTableTabId.value = next;
  }
}

function closeActivePaneTab() {
  if (!viewTabs.value.some((tab) => tab.id === activeTabId.value)) {
    return false;
  }
  closeTab(activeTabId.value);
  return true;
}

function tabTitle(tab: PaneTab) {
  if (tab.kind === "query") {
    return tab.title;
  }
  return tab.namespace === namespace.value ? tab.table : `${tab.namespace}.${tab.table}`;
}

function setEditorRef(id: string, instance: unknown) {
  if (instance) {
    editors.set(id, instance as InstanceType<typeof QueryEditor>);
  } else {
    editors.delete(id);
  }
}

async function loadSchema() {
  try {
    const columns = await api.schemaColumns(props.sessionId, namespace.value);
    const next: Record<string, string[]> = {};
    for (const table of tables.value) {
      next[table.name] = [];
    }
    for (const item of columns) {
      (next[item.table] ??= []).push(item.column);
    }
    schema.value = next;
  } catch {
    schema.value = Object.fromEntries(tables.value.map((table) => [table.name, []]));
  }
}

async function loadTables() {
  if (!namespace.value && driver.value !== "sqlite") {
    tables.value = [];
    return;
  }
  tablesLoading.value = true;
  tablesError.value = "";
  try {
    tables.value = await api.listTables(props.sessionId, namespace.value);
    void loadSchema();
  } catch (err) {
    tablesError.value = String(err);
    tables.value = [];
  } finally {
    tablesLoading.value = false;
  }
}

async function connect(withPassword: string | null = null) {
  status.value = "connecting";
  connectError.value = "";
  try {
    const info = await api.connect(
      props.connectionId,
      withPassword,
      props.sessionId,
      props.initialNamespace ?? null,
    );
    session.value = info;
    namespaces.value = info.namespaces.items;
    namespace.value = info.namespaces.current;
    password.value = "";
    lost.value = false;
    status.value = "connected";
    if (!tabsRestored.value) {
      restoreQueryTabs();
    }
    await loadTables();
  } catch (err) {
    connectError.value = String(err);
    status.value = needsPassword.value && withPassword === null ? "password" : "error";
    if (status.value === "password") {
      void nextTick(() => passwordInput.value?.focus());
    }
  }
}

function submitPassword() {
  if (!password.value) {
    return;
  }
  void connect(password.value);
}

async function changeNamespace(next: string) {
  if (!next || next === namespace.value) {
    return;
  }
  const previous = namespace.value;
  namespace.value = next;
  filter.value = "";
  try {
    await api.setDatabase(props.sessionId, next);
    await loadTables();
  } catch (err) {
    namespace.value = previous;
    showToast(String(err), "error");
  }
}

async function refreshNamespaces() {
  try {
    const list = await api.listDatabases(props.sessionId);
    namespaces.value = list.items;
    if (list.current) {
      namespace.value = list.current;
    }
  } catch (err) {
    showToast(String(err), "error");
  }
}

function openNameDialog(dialog: NonNullable<typeof nameDialog.value>) {
  nameValue.value = dialog.mode === "rename" ? dialog.from : "";
  nameError.value = "";
  nameDialog.value = dialog;
  void nextTick(() => nameInput.value?.select());
}

function closeNameDialog() {
  if (!nameBusy.value) {
    nameDialog.value = null;
  }
}

async function submitNameDialog() {
  const dialog = nameDialog.value;
  const name = nameValue.value.trim();
  if (!dialog || !name || nameBusy.value) {
    return;
  }
  if (dialog.mode === "rename" && name === dialog.from) {
    nameDialog.value = null;
    return;
  }
  nameBusy.value = true;
  nameError.value = "";
  try {
    if (dialog.mode === "create") {
      await createNamespace(name);
    } else {
      await renameNamespace(dialog.from, name);
    }
    nameDialog.value = null;
  } catch (err) {
    nameError.value = String(err);
  } finally {
    nameBusy.value = false;
  }
}

async function createNamespace(name: string) {
  await api.createDatabase(props.sessionId, name);
  await refreshNamespaces();
  await changeNamespace(name);
  showToast(`Created ${namespaceLabel.value.toLowerCase()} “${name}”`);
}

function startRename(name: string) {
  const dirty = tabs.value.some(
    (tab) => tab.kind === "table" && tab.namespace === name && dirtyTabs.value.has(tab.id),
  );
  if (dirty) {
    showToast(`Save or discard your changes in “${name}” before renaming it.`, "error");
    return;
  }
  openNameDialog({ mode: "rename", from: name });
}

async function renameNamespace(from: string, to: string) {
  await api.renameDatabase(props.sessionId, from, to);
  tabs.value = tabs.value.map((tab) => {
    if (tab.kind !== "table" || tab.namespace !== from) {
      return tab;
    }
    const id = `table:${to}.${tab.table}`;
    if (activeTableTabId.value === tab.id) {
      activeTableTabId.value = id;
    }
    return { ...tab, id, namespace: to };
  });
  const wasCurrent = namespace.value === from;
  await refreshNamespaces();
  if (wasCurrent) {
    namespace.value = to;
    await loadTables();
  }
  showToast(`Renamed ${namespaceLabel.value.toLowerCase()} “${from}” to “${to}”`);
}

async function copyNamespaceName(name: string) {
  try {
    await navigator.clipboard.writeText(name);
    showToast(`Copied “${name}”`);
  } catch (err) {
    showToast(String(err), "error");
  }
}

function openNamespaceInNewTab(name: string) {
  openConnectionTab(props.connectionId, name, props.sessionId);
}

async function dropNamespace(name: string) {
  if (!name || name === namespace.value) {
    return;
  }
  const label = namespaceLabel.value.toLowerCase();
  const ok = await confirm(
    `Drop the ${label} “${name}”? All of its tables and data will be permanently deleted. This can't be undone.`,
    {
      title: `Drop ${label}`,
      kind: "warning",
      okLabel: "Drop",
      cancelLabel: "Cancel",
    },
  );
  if (!ok) {
    return;
  }
  try {
    await api.dropDatabase(props.sessionId, name);
    for (const tab of tabs.value.filter((item) => item.kind === "table" && item.namespace === name)) {
      setTabChanges(tab.id, 0);
      removeTab(tab.id);
    }
    await refreshNamespaces();
    showToast(`Dropped ${label} “${name}”`);
  } catch (err) {
    showToast(String(err), "error");
  }
}

async function refreshAll() {
  await refreshNamespaces();
  await loadTables();
}

/**
 * Picks up tables created or dropped outside Recon (migrations, other
 * clients). Failures are ignored so a background check never clears the list.
 */
async function refreshTablesQuietly() {
  if (status.value !== "connected" || lost.value || tablesLoading.value) {
    return;
  }
  if (!namespace.value && driver.value !== "sqlite") {
    return;
  }
  const requested = namespace.value;
  try {
    const next = await api.listTables(props.sessionId, requested);
    if (requested !== namespace.value) {
      return;
    }
    const names = (list: TableInfo[]) => list.map((table) => `${table.kind}:${table.name}`).join("\n");
    if (names(next) !== names(tables.value)) {
      tables.value = next;
      tablesError.value = "";
      void loadSchema();
    }
  } catch {
    return;
  }
}

function onWindowFocus() {
  if (props.active) {
    void refreshTablesQuietly();
  }
}

watch(
  () => props.active,
  (active) => {
    if (active) {
      void refreshTablesQuietly();
    }
  },
);

/**
 * Rebuilds the backend session in place, so open tabs and unsaved
 * table edits survive the reconnect.
 */
async function reconnect() {
  if (reconnecting.value) {
    return;
  }
  reconnecting.value = true;
  try {
    const info = await api.reconnect(props.sessionId);
    session.value = info;
    namespaces.value = info.namespaces.items;
    namespace.value = info.namespaces.current;
    lost.value = false;
    lostError.value = "";
    await loadTables();
    void tableViews.get(activeTableTabId.value)?.refresh();
  } catch (err) {
    lostError.value = String(err);
  } finally {
    reconnecting.value = false;
  }
}

function onConnectionLost(event: ConnectionEvent) {
  if (event.connectionId !== props.sessionId) {
    return;
  }
  lost.value = true;
  lostError.value = event.error ?? "";
}

function onConnectionRestored(event: ConnectionEvent) {
  if (event.connectionId === props.sessionId) {
    showToast(`Reconnected to ${entry.value?.name ?? "the database"}`);
  }
}

function onExecuted(statements: string[]) {
  let switched: string | null = null;
  let changedSchema = false;
  for (const statement of statements) {
    const use = /^\s*use\s+[`"[]?([^`"\]\s;]+)/i.exec(statement);
    if (use && driver.value === "mysql") {
      switched = use[1];
    }
    const path = /^\s*set\s+search_path\s*(?:to|=)\s*"?([^",\s;]+)/i.exec(statement);
    if (path && driver.value === "postgres") {
      switched = path[1];
    }
    if (/^\s*(create|drop|alter|rename|attach|detach)\b/i.test(statement)) {
      changedSchema = true;
    }
  }
  if (switched && switched !== namespace.value) {
    if (!namespaces.value.includes(switched)) {
      void refreshNamespaces().then(() => changeNamespace(switched!));
    } else {
      void changeNamespace(switched);
    }
    return;
  }
  if (changedSchema) {
    void refreshAll();
  }
}

function queryTable(table: TableInfo) {
  const quote = driver.value === "mysql" ? "`" : '"';
  const name = `${quote}${table.name.split(quote).join(quote + quote)}${quote}`;
  openQueryTab(`SELECT * FROM ${name} LIMIT 100;`);
}

function editConnection() {
  if (entry.value) {
    openEditConnection(entry.value, match.value?.group?.id ?? null);
  }
}

function startSidebarResize(event: PointerEvent) {
  event.preventDefault();
  const startX = event.clientX;
  const startWidth = sidebarWidth.value;
  let width = startWidth;
  document.body.classList.add("resizing-columns");
  const onMove = (move: PointerEvent) => {
    width = Math.round(Math.min(Math.max(startWidth + move.clientX - startX, SIDEBAR_MIN), SIDEBAR_MAX));
    previewPreferences({ sidebarWidth: width });
  };
  const onUp = () => {
    window.removeEventListener("pointermove", onMove);
    window.removeEventListener("pointerup", onUp);
    window.removeEventListener("pointercancel", onUp);
    document.body.classList.remove("resizing-columns");
    if (width !== startWidth) {
      void savePreferences({ sidebarWidth: width }).catch((err) =>
        showToast(String(err), "error"),
      );
    }
  };
  window.addEventListener("pointermove", onMove);
  window.addEventListener("pointerup", onUp);
  window.addEventListener("pointercancel", onUp);
}

let measureCanvas: HTMLCanvasElement | null = null;

function autoFitSidebar() {
  const sidebar = sidebarEl.value;
  const row = sidebar?.querySelector<HTMLElement>(".db-table");
  const name = row?.querySelector<HTMLElement>(".db-table-name");
  const context = (measureCanvas ??= document.createElement("canvas")).getContext("2d");
  if (!sidebar || !row || !name || !context || !tables.value.length) return;

  const font = getComputedStyle(name);
  const base = `${font.fontWeight} ${font.fontSize} ${font.fontFamily}`;
  const longest = tables.value.reduce((max, table) => {
    const dirty = dirtyTabs.value.has(`table:${namespace.value}.${table.name}`);
    context.font = dirty ? `italic ${base}` : base;
    return Math.max(max, context.measureText(table.name).width + (dirty ? 12 : 0));
  }, 0);

  /**
   * Everything around the name: sidebar edge to the name's start, plus the
   * row's right padding, list padding, and scrollbar on the other side.
   */
  const sidebarRect = sidebar.getBoundingClientRect();
  const rowRect = row.getBoundingClientRect();
  const leading = name.getBoundingClientRect().left - sidebarRect.left;
  const trailing = sidebarRect.right - rowRect.right + parseFloat(getComputedStyle(row).paddingRight);

  const width = Math.min(Math.max(Math.ceil(leading + longest + trailing) + 2, SIDEBAR_MIN), SIDEBAR_MAX);
  if (width === sidebarWidth.value) return;
  previewPreferences({ sidebarWidth: width });
  void savePreferences({ sidebarWidth: width }).catch((err) => showToast(String(err), "error"));
}

let stopLost: UnlistenFn | null = null;
let stopRestored: UnlistenFn | null = null;

onMounted(() => {
  window.addEventListener("keydown", onWindowKeydown, true);
  window.addEventListener("focus", onWindowFocus);
  void listen<ConnectionEvent>("connection-lost", (event) => onConnectionLost(event.payload)).then(
    (unlisten) => {
      stopLost = unlisten;
    },
  );
  void listen<ConnectionEvent>("connection-restored", (event) => onConnectionRestored(event.payload)).then(
    (unlisten) => {
      stopRestored = unlisten;
    },
  );
  if (needsPassword.value) {
    status.value = "password";
    void nextTick(() => passwordInput.value?.focus());
    return;
  }
  void connect(null);
});

const unregisterCloser = registerInnerTabCloser(props.sessionId, closeActivePaneTab);

onUnmounted(() => {
  window.removeEventListener("keydown", onWindowKeydown, true);
  window.removeEventListener("focus", onWindowFocus);
  stopLost?.();
  stopRestored?.();
  unregisterCloser();
  setLiveTitle(props.sessionId, "");
  void api.disconnect(props.sessionId).catch(() => undefined);
  if (extraTab.value) {
    for (const tab of tabs.value) {
      if (tab.kind === "query") {
        localStorage.removeItem(`recon.query.${tab.key}`);
      }
    }
    localStorage.removeItem(queryTabsKey.value);
  }
});
</script>

<template>
  <div class="connection-pane">
    <div v-if="status !== 'connected'" class="connect-state">
      <div class="connect-card">
        <div class="connect-title">
          <DriverIcon v-if="entry" :driver="entry.driver" />
          <strong>{{ entry?.name ?? "Connection" }}</strong>
        </div>
        <p class="muted tiny">{{ entry ? `${driverLabel(entry.driver)} · ${subtitle}` : "" }}</p>
        <template v-if="status === 'connecting'">
          <p class="action-progress"><span class="spinner" aria-hidden="true" /> Connecting…</p>
        </template>
        <form v-else-if="status === 'password'" class="connect-password" @submit.prevent="submitPassword">
          <label class="modal-label">
            <span class="muted tiny">Password for {{ entry?.user }}</span>
            <input
              ref="passwordInput"
              v-model="password"
              type="password"
              autocomplete="off"
              placeholder="Password"
            />
          </label>
          <p v-if="connectError" class="settings-error">{{ connectError }}</p>
          <div class="connect-actions">
            <button class="ghost" type="button" @click="editConnection">Edit connection</button>
            <button class="primary" type="submit" :disabled="!password">Connect</button>
          </div>
        </form>
        <template v-else>
          <p class="settings-error connect-error">{{ connectError }}</p>
          <div class="connect-actions">
            <button class="ghost" type="button" @click="editConnection">Edit connection</button>
            <button
              v-if="entry && entry.driver !== 'sqlite'"
              class="ghost"
              type="button"
              @click="status = 'password'"
            >
              Enter password
            </button>
            <button class="primary" type="button" @click="connect(null)">Try again</button>
          </div>
        </template>
      </div>
    </div>

    <template v-else>
      <header class="pane-header db-toolbar">
        <div class="db-toolbar-meta">
          <DriverIcon v-if="entry" :driver="entry.driver" />
          <strong v-if="driver !== 'mysql'" class="db-toolbar-name" :title="entry?.name">
            {{ databaseTitle }}
          </strong>
          <DatabaseSwitcher
            v-if="namespaces.length > 1 || driver !== 'sqlite'"
            :menu-id="sessionId"
            :current="namespace"
            :items="namespaces"
            :label="namespaceLabel"
            :creatable="driver !== 'sqlite'"
            :droppable="driver !== 'sqlite'"
            :renamable="driver !== 'sqlite'"
            @open="refreshNamespaces"
            @select="changeNamespace"
            @create="openNameDialog({ mode: 'create' })"
            @drop="dropNamespace"
            @rename="startRename"
            @copy="copyNamespaceName"
            @open-tab="openNamespaceInNewTab"
          />
          <span class="muted tiny db-toolbar-version" :title="session?.serverVersion">
            {{ session?.serverVersion }}
          </span>
          <span class="muted tiny db-toolbar-driver">{{ driverLabel(driver) }}</span>
          <div v-if="dirtyTabs.size" class="db-toolbar-actions">
            <span v-if="saving" class="spinner" aria-label="Saving" />
            <span class="tiny edit-status">{{ unsavedLabel }}</span>
            <button
              class="ghost tiny"
              type="button"
              title="Discard changes in every table"
              :disabled="saving"
              @click="discardAll"
            >
              Discard
            </button>
            <button
              class="primary tiny"
              type="button"
              title="Save changes in every table (⌘S)"
              :disabled="saving"
              @click="saveAll"
            >
              Save
            </button>
          </div>
        </div>
      </header>
      <div v-if="lost" class="connection-lost" role="alert">
        <p class="connection-lost-message">
          <strong>Connection lost.</strong>
          <span v-if="lostError" class="muted" :title="lostError">{{ lostError }}</span>
        </p>
        <button class="ghost tiny" type="button" @click="editConnection">Edit connection</button>
        <button class="primary tiny" type="button" :disabled="reconnecting" @click="reconnect">
          <span v-if="reconnecting" class="spinner" aria-hidden="true" />
          {{ reconnecting ? "Reconnecting…" : "Reconnect" }}
        </button>
      </div>
      <ConnectionViewTabs :active="view" :table-count="tables.length" @select="selectView" />
      <div class="db-body">
        <aside v-show="view === 'tables'" ref="sidebarEl" class="db-sidebar" :style="{ width: `${sidebarWidth}px` }">
          <div class="db-filter">
            <input v-model="filter" type="search" placeholder="Filter tables" spellcheck="false" />
          </div>
          <div class="db-table-list" role="listbox" :aria-label="`Tables in ${namespace}`">
            <p v-if="tablesLoading && !tables.length" class="muted tiny db-list-hint">
              <span class="spinner" aria-hidden="true" /> Loading tables…
            </p>
            <p v-else-if="tablesError" class="settings-error db-list-hint">{{ tablesError }}</p>
            <p v-else-if="!tables.length" class="muted tiny db-list-hint">
              {{ namespace || driver === "sqlite" ? "No tables." : `Choose a ${namespaceLabel.toLowerCase()}.` }}
            </p>
            <p v-else-if="!filteredTables.length" class="muted tiny db-list-hint">No matches.</p>
            <button
              v-for="table in filteredTables"
              :key="table.name"
              class="db-table"
              type="button"
              role="option"
              :aria-selected="activeTableTabId === `table:${namespace}.${table.name}`"
              :class="{
                active: activeTableTabId === `table:${namespace}.${table.name}`,
                view: table.kind === 'view',
                dirty: dirtyTabs.has(`table:${namespace}.${table.name}`),
              }"
              :title="
                dirtyTabs.has(`table:${namespace}.${table.name}`)
                  ? `${table.name} (unsaved changes)`
                  : table.kind === 'view'
                    ? `${table.name} (view)`
                    : table.name
              "
              @click="openTable(table)"
              @dblclick="queryTable(table)"
            >
              <svg v-if="table.kind === 'view'" viewBox="0 0 16 16" aria-hidden="true">
                <path d="M1.5 8s2.4-4.5 6.5-4.5S14.5 8 14.5 8 12.1 12.5 8 12.5 1.5 8 1.5 8Z" />
                <circle cx="8" cy="8" r="1.8" />
              </svg>
              <svg v-else viewBox="0 0 16 16" aria-hidden="true">
                <rect x="2" y="3" width="12" height="10" rx="1.5" />
                <path d="M2 6.5h12M6.5 6.5V13" />
              </svg>
              <span class="db-table-name">{{ table.name }}</span>
              <span
                v-if="dirtyTabs.has(`table:${namespace}.${table.name}`)"
                class="dirty-dot"
                aria-label="Unsaved changes"
              />
            </button>
          </div>
        </aside>
        <div
          v-show="view === 'tables'"
          class="db-sidebar-resize"
          role="separator"
          aria-orientation="vertical"
          @pointerdown="startSidebarResize"
          @dblclick="autoFitSidebar"
        />

        <section class="db-main">
          <div v-if="viewTabs.length || view === 'sql'" class="subtab-bar" role="tablist">
            <div
              v-for="tab in viewTabs"
              :key="tab.id"
              class="subtab"
              :class="{ active: activeTabId === tab.id, query: tab.kind === 'query', dirty: dirtyTabs.has(tab.id) }"
              role="tab"
              :aria-selected="activeTabId === tab.id"
              :title="tab.kind === 'table' ? `${tab.namespace}.${tab.table}` : tab.title"
              @click="selectTab(tab)"
              @auxclick.middle="closeTab(tab.id)"
            >
              <svg v-if="tab.kind === 'query'" class="subtab-icon" viewBox="0 0 16 16" aria-hidden="true">
                <path d="M5 4 1.5 8 5 12M11 4l3.5 4L11 12" />
              </svg>
              <svg v-else class="subtab-icon" viewBox="0 0 16 16" aria-hidden="true">
                <rect x="2" y="3" width="12" height="10" rx="1.5" />
                <path d="M2 6.5h12M6.5 6.5V13" />
              </svg>
              <span class="subtab-title">{{ tabTitle(tab) }}</span>
              <button
                class="subtab-close"
                type="button"
                :aria-label="`Close ${tabTitle(tab)}`"
                @click.stop="closeTab(tab.id)"
              >
                <span v-if="dirtyTabs.has(tab.id)" class="dirty-dot" aria-hidden="true" />
                <span class="subtab-close-icon">×</span>
              </button>
            </div>
            <button
              v-if="view === 'sql'"
              class="subtab-add"
              type="button"
              title="Open a new query tab"
              aria-label="Open a new query tab"
              @click="openQueryTab()"
            >
              +
            </button>
          </div>
          <div v-if="!viewTabs.length" class="db-empty muted">
            <p v-if="view === 'sql'">No open queries. Use + to start one.</p>
            <p v-else>Pick a table on the left, or double-click one to query it.</p>
          </div>
          <div
            v-for="tab in tabs"
            v-show="activeTabId === tab.id"
            :key="tab.id"
            class="db-main-pane"
          >
            <TableView
              v-if="tab.kind === 'table'"
              :connection-id="sessionId"
              :namespace="tab.namespace"
              :table="tab.table"
              :kind="tab.tableKind"
              :driver="driver"
              :ref="(instance) => setTableViewRef(tab.id, instance)"
              :active="active && view === 'tables' && activeTableTabId === tab.id"
              @changes="setTabChanges(tab.id, $event)"
            />
            <QueryEditor
              v-else
              :ref="(instance) => setEditorRef(tab.id, instance)"
              :connection-id="sessionId"
              :driver="driver"
              :schema="schema"
              :storage-key="tab.key"
              :active="active && view === 'sql' && activeQueryTabId === tab.id"
              @executed="onExecuted"
            />
          </div>
        </section>
      </div>
      <Modal
        v-if="nameDialog"
        :title="
          nameDialog.mode === 'create'
            ? `New ${namespaceLabel.toLowerCase()}`
            : `Rename ${namespaceLabel.toLowerCase()} “${nameDialog.from}”`
        "
        @close="closeNameDialog"
      >
        <form @submit.prevent="submitNameDialog">
          <label class="modal-label">
            <span class="muted tiny">Name</span>
            <input
              ref="nameInput"
              v-model="nameValue"
              type="text"
              autocomplete="off"
              spellcheck="false"
              :disabled="nameBusy"
            />
          </label>
          <p v-if="nameDialog.mode === 'rename' && driver === 'mysql'" class="muted tiny">
            MySQL renames a database by moving its tables into a new one. Privileges granted on the old
            name are not carried over.
          </p>
          <p v-if="nameError" class="settings-error">{{ nameError }}</p>
        </form>
        <template #actions>
          <button class="ghost" type="button" :disabled="nameBusy" @click="closeNameDialog">
            Cancel
          </button>
          <button
            class="primary"
            type="button"
            :disabled="!nameValue.trim() || nameBusy"
            @click="submitNameDialog"
          >
            <span v-if="nameBusy" class="spinner" aria-hidden="true" />
            <template v-if="nameDialog.mode === 'create'">{{ nameBusy ? "Creating…" : "Create" }}</template>
            <template v-else>{{ nameBusy ? "Renaming…" : "Rename" }}</template>
          </button>
        </template>
      </Modal>
    </template>
  </div>
</template>
