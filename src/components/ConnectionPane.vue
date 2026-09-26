<script setup lang="ts">
import type { SQLNamespace } from "@codemirror/lang-sql";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { confirm, open as openFile } from "@tauri-apps/plugin-dialog";
import { computed, nextTick, onMounted, onUnmounted, ref, shallowRef, watch, type Ref } from "vue";
import * as api from "../api";
import { SIDEBAR_MAX, SIDEBAR_MIN, useApp } from "../composables/useApp";
import { useConnectionForm } from "../composables/useConnectionForm";
import { registerInnerTabCloser, setLiveTitle, useTabs } from "../composables/useTabs";
import {
  driverLabel,
  type CellEdit,
  type SavedQuery,
  type SessionInfo,
  type TableInfo,
  type TableLink,
} from "../types";
import ConnectionViewTabs, { type ConnectionViewTab } from "./ConnectionViewTabs.vue";
import DatabaseSwitcher from "./DatabaseSwitcher.vue";
import DriverIcon from "./DriverIcon.vue";
import ExportDialog from "./ExportDialog.vue";
import ImportDialog from "./ImportDialog.vue";
import Modal from "./Modal.vue";
import QueryEditor from "./QueryEditor.vue";
import QueryHistory from "./QueryHistory.vue";
import SavedQueries from "./SavedQueries.vue";
import TableView from "./TableView.vue";

type PaneTab =
  | {
      id: string;
      kind: "table";
      namespace: string;
      table: string;
      tableKind: "table" | "view";
      filter?: CellEdit[];
    }
  | { id: string; kind: "query"; key: string; title: string; savedId?: string };

type QueryTab = Extract<PaneTab, { kind: "query" }>;

const SAVED_TAB_ID = "saved-queries";

const props = defineProps<{
  connectionId: string;
  sessionId: string;
  initialNamespace?: string;
  active: boolean;
}>();

const {
  findConnection,
  sidebarWidth,
  previewPreferences,
  savePreferences,
  showToast,
  savedQueries,
  saveQuery,
} = useApp();
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
const selectedTables = ref(new Set<string>());
const tableMenu = ref<{ x: number; y: number; tables: string[] } | null>(null);
const tableMenuEl = ref<HTMLElement | null>(null);
const tabMenu = ref<{ x: number; y: number; tabId: string } | null>(null);
const tabMenuEl = ref<HTMLElement | null>(null);
const renamingTabId = ref("");
const renameValue = ref("");
const modifiedQueries = ref(new Set<string>());
const selectedSavedId = ref("");
const saveDialog = ref<{ tabId: string } | null>(null);
const saveName = ref("");
const saveDescription = ref("");
const saveBusy = ref(false);
const saveError = ref("");
const saveNameInput = ref<HTMLInputElement | null>(null);
const exportDialog = ref<{ tables: string[] | null } | null>(null);
const importPath = ref<string | null>(null);
let selectionAnchor = "";
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

const connectionSaved = computed(() =>
  savedQueries.value.filter((query) => query.connectionId === props.connectionId),
);
const openSavedIds = computed(
  () => new Set(tabs.value.flatMap((tab) => (tab.kind === "query" && tab.savedId ? [tab.savedId] : []))),
);
const showSavedTab = computed(() => view.value === "sql" && connectionSaved.value.length > 0);
const menuTab = computed(() => {
  const tab = tabs.value.find((item) => item.id === tabMenu.value?.tabId);
  return tab?.kind === "query" ? tab : null;
});

const viewTabs = computed(() => {
  if (view.value === "history") {
    return [];
  }
  return tabs.value.filter((tab) => (view.value === "sql" ? tab.kind === "query" : tab.kind === "table"));
});
const activeTabId = computed(() => {
  if (view.value === "history") {
    return "";
  }
  return view.value === "sql" ? activeQueryTabId.value : activeTableTabId.value;
});

const filteredTables = computed(() => {
  const needle = filter.value.trim().toLowerCase();
  return needle
    ? tables.value.filter((table) => table.name.toLowerCase().includes(needle))
    : tables.value;
});

const tableMenuExportLabel = computed(() => {
  const chosen = tableMenu.value?.tables ?? [];
  if (chosen.length !== 1) {
    return `Export ${chosen.length.toLocaleString()} tables…`;
  }
  const kind = tables.value.find((table) => table.name === chosen[0])?.kind;
  return kind === "view" ? "Export view…" : "Export table…";
});

const canTransfer = computed(() => Boolean(namespace.value || driver.value === "sqlite") && !lost.value);

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
    tab.kind === "query" ? [{ key: tab.key, title: tab.title, savedId: tab.savedId }] : [],
  );
  localStorage.setItem(queryTabsKey.value, JSON.stringify(saved));
}

function restoreQueryTabs() {
  let saved: { key: string; title: string; savedId?: unknown }[] = [];
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
  const savedIds = new Set(connectionSaved.value.map((query) => query.id));
  tabs.value = saved.map((item) => ({
    id: `query:${item.key}`,
    kind: "query",
    key: item.key,
    title: item.title,
    savedId: typeof item.savedId === "string" && savedIds.has(item.savedId) ? item.savedId : undefined,
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

function savedFor(tab: PaneTab) {
  if (tab.kind !== "query" || !tab.savedId) {
    return null;
  }
  return connectionSaved.value.find((query) => query.id === tab.savedId) ?? null;
}

function openSavedQuery(query: SavedQuery, run = false) {
  view.value = "sql";
  let tab = tabs.value.find((item): item is QueryTab => item.kind === "query" && item.savedId === query.id);
  if (!tab) {
    const key = nextQueryKey();
    try {
      localStorage.setItem(`recon.query.${key}`, query.sql);
    } catch {
      showToast("Could not open the query because local storage is full.", "error");
      return;
    }
    tab = { id: `query:${key}`, kind: "query", key, title: query.name, savedId: query.id };
    tabs.value = [...tabs.value, tab];
    saveQueryTabs();
  }
  const id = tab.id;
  activeQueryTabId.value = id;
  if (run) {
    void nextTick(() => editors.get(id)?.run(true));
  }
}

function setQueryModified(id: string, modified: boolean) {
  if (modifiedQueries.value.has(id) === modified) {
    return;
  }
  const next = new Set(modifiedQueries.value);
  if (modified) {
    next.add(id);
  } else {
    next.delete(id);
  }
  modifiedQueries.value = next;
}

function updateQueryTab(id: string, patch: Partial<QueryTab>) {
  tabs.value = tabs.value.map((tab) => (tab.id === id && tab.kind === "query" ? { ...tab, ...patch } : tab));
  saveQueryTabs();
}

watch(connectionSaved, (list) => {
  const ids = new Set(list.map((query) => query.id));
  const stale = tabs.value.filter(
    (tab): tab is QueryTab => tab.kind === "query" && Boolean(tab.savedId) && !ids.has(tab.savedId!),
  );
  for (const tab of stale) {
    updateQueryTab(tab.id, { savedId: undefined });
    setQueryModified(tab.id, false);
  }
  if (!list.length && activeQueryTabId.value === SAVED_TAB_ID) {
    const first = tabs.value.find((tab) => tab.kind === "query");
    if (first) {
      activeQueryTabId.value = first.id;
    } else if (view.value === "sql") {
      openQueryTab();
    } else {
      activeQueryTabId.value = "";
    }
  }
});

function startTabRename(tab: QueryTab) {
  closeMenus();
  selectTab(tab);
  renameValue.value = tabTitle(tab);
  renamingTabId.value = tab.id;
  void nextTick(() => {
    const input = document.querySelector<HTMLInputElement>(`[data-rename-tab="${CSS.escape(tab.id)}"]`);
    input?.focus();
    input?.select();
  });
}

function cancelTabRename() {
  renamingTabId.value = "";
}

async function commitTabRename() {
  const tab = tabs.value.find((item): item is QueryTab => item.id === renamingTabId.value && item.kind === "query");
  renamingTabId.value = "";
  const name = renameValue.value.trim();
  if (!tab || !name || name === tabTitle(tab)) {
    return;
  }
  const saved = savedFor(tab);
  updateQueryTab(tab.id, { title: name });
  if (saved) {
    try {
      await saveQuery({ ...saved, name });
    } catch (err) {
      showToast(String(err), "error");
    }
  }
}

function openSaveDialog(tab: QueryTab) {
  closeMenus();
  saveName.value = tabTitle(tab);
  saveDescription.value = "";
  saveError.value = "";
  saveDialog.value = { tabId: tab.id };
  void nextTick(() => saveNameInput.value?.select());
}

function closeSaveDialog() {
  if (!saveBusy.value) {
    saveDialog.value = null;
  }
}

async function submitSaveDialog() {
  const dialog = saveDialog.value;
  const name = saveName.value.trim();
  if (!dialog || !name || saveBusy.value) {
    return;
  }
  const sql = editors.get(dialog.tabId)?.getText() ?? "";
  if (!sql.trim()) {
    saveError.value = "Write some SQL in the tab before saving it.";
    return;
  }
  saveBusy.value = true;
  saveError.value = "";
  try {
    const saved = await saveQuery({
      id: "",
      connectionId: props.connectionId,
      name,
      description: saveDescription.value,
      sql,
      updatedAt: 0,
    });
    updateQueryTab(dialog.tabId, { title: saved.name, savedId: saved.id });
    saveDialog.value = null;
    showToast(`Saved “${saved.name}”`);
  } catch (err) {
    saveError.value = String(err);
  } finally {
    saveBusy.value = false;
  }
}

async function saveQueryTab(tab: QueryTab) {
  closeMenus();
  const saved = savedFor(tab);
  if (!saved) {
    openSaveDialog(tab);
    return;
  }
  const sql = editors.get(tab.id)?.getText() ?? saved.sql;
  try {
    await saveQuery({ ...saved, sql });
    showToast(`Saved changes to “${saved.name}”`);
  } catch (err) {
    showToast(String(err), "error");
  }
}

function showInSaved(tab: QueryTab) {
  closeMenus();
  if (tab.savedId) {
    selectedSavedId.value = tab.savedId;
    activeQueryTabId.value = SAVED_TAB_ID;
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

function activeTableName() {
  const tab = tabs.value.find((item) => item.id === activeTableTabId.value);
  return tab?.kind === "table" && tab.namespace === namespace.value ? tab.table : null;
}

/**
 * Finder-style selection: a plain click opens the table and selects only it,
 * Cmd+click toggles a table without opening it, and Shift+click selects the
 * visible range from the last clicked table.
 */
function onTableClick(event: MouseEvent, table: TableInfo) {
  if (event.metaKey || event.ctrlKey) {
    const next = new Set(selectedTables.value);
    const active = activeTableName();
    if (!next.size && !selectionAnchor && active && active !== table.name) {
      next.add(active);
    }
    if (next.has(table.name)) {
      next.delete(table.name);
    } else {
      next.add(table.name);
    }
    selectedTables.value = next;
    selectionAnchor = table.name;
    return;
  }
  const names = filteredTables.value.map((item) => item.name);
  const from = names.indexOf(selectionAnchor || activeTableName() || "");
  if (event.shiftKey && from >= 0) {
    const to = names.indexOf(table.name);
    selectedTables.value = new Set(names.slice(Math.min(from, to), Math.max(from, to) + 1));
    return;
  }
  selectedTables.value = new Set([table.name]);
  selectionAnchor = table.name;
  openTable(table);
}

function onMenuKeydown(event: KeyboardEvent) {
  if (event.key === "Escape") {
    event.stopImmediatePropagation();
    event.preventDefault();
    closeMenus();
  }
}

function onMenuPointerDown(event: PointerEvent) {
  if (!(event.target instanceof Element && event.target.closest(".table-context-menu"))) {
    closeMenus();
  }
}

function closeMenus() {
  tableMenu.value = null;
  tabMenu.value = null;
  document.removeEventListener("keydown", onMenuKeydown, true);
  document.removeEventListener("pointerdown", onMenuPointerDown, true);
  window.removeEventListener("blur", closeMenus);
  window.removeEventListener("resize", closeMenus);
}

function listenForMenuDismiss() {
  document.addEventListener("keydown", onMenuKeydown, true);
  document.addEventListener("pointerdown", onMenuPointerDown, true);
  window.addEventListener("blur", closeMenus);
  window.addEventListener("resize", closeMenus);
}

function fitMenu<T extends { x: number; y: number }>(menu: Ref<T | null>, el: Ref<HTMLElement | null>) {
  void nextTick(() => {
    const current = menu.value;
    if (!el.value || !current) {
      return;
    }
    const rect = el.value.getBoundingClientRect();
    menu.value = {
      ...current,
      x: Math.max(4, Math.min(current.x, window.innerWidth - rect.width - 4)),
      y: Math.max(4, Math.min(current.y, window.innerHeight - rect.height - 4)),
    };
  });
}

/** Right-clicking outside the selection selects just that table, as in Finder. */
function openTableMenu(event: MouseEvent, table: TableInfo) {
  event.preventDefault();
  if (!selectedTables.value.has(table.name)) {
    selectedTables.value = new Set([table.name]);
    selectionAnchor = table.name;
  }
  const chosen = tables.value.map((item) => item.name).filter((name) => selectedTables.value.has(name));
  closeMenus();
  tableMenu.value = { x: event.clientX, y: event.clientY, tables: chosen };
  listenForMenuDismiss();
  fitMenu(tableMenu, tableMenuEl);
}

function openTabMenu(event: MouseEvent, tab: PaneTab) {
  if (tab.kind !== "query") {
    return;
  }
  event.preventDefault();
  closeMenus();
  tabMenu.value = { x: event.clientX, y: event.clientY, tabId: tab.id };
  listenForMenuDismiss();
  fitMenu(tabMenu, tabMenuEl);
}

function runTableAction(action: "open" | "query" | "copy" | "export") {
  const chosen = tableMenu.value?.tables ?? [];
  closeMenus();
  const table = tables.value.find((item) => item.name === chosen[0]);
  if (!table) {
    return;
  }
  if (action === "open") {
    openTable(table);
  } else if (action === "query") {
    queryTable(table);
  } else if (action === "copy") {
    void copyNamespaceName(table.name);
  } else {
    exportDialog.value = { tables: chosen };
  }
}

async function startImport() {
  const path = await openFile({
    title: `Import into “${namespace.value}”`,
    multiple: false,
    directory: false,
    filters: [{ name: "SQL", extensions: ["sql", "gz"] }],
  });
  if (typeof path === "string") {
    importPath.value = path;
  }
}

function onImported() {
  void refreshAll();
  void tableViews.get(activeTableTabId.value)?.refresh();
}

watch(namespace, () => {
  selectedTables.value = new Set();
  selectionAnchor = "";
  closeMenus();
});

watch(tables, (list) => {
  const names = new Set(list.map((table) => table.name));
  if ([...selectedTables.value].some((name) => !names.has(name))) {
    selectedTables.value = new Set([...selectedTables.value].filter((name) => names.has(name)));
  }
});

function setTableFilter(id: string, filter: CellEdit[] | undefined) {
  tabs.value = tabs.value.map((tab) => (tab.id === id && tab.kind === "table" ? { ...tab, filter } : tab));
}

function followLink(link: TableLink) {
  const id = `table:${link.namespace}.${link.table}`;
  if (tabs.value.some((tab) => tab.id === id)) {
    setTableFilter(id, link.filter);
  } else {
    const known = link.namespace === namespace.value
      ? tables.value.find((table) => table.name === link.table)
      : undefined;
    tabs.value = [
      ...tabs.value,
      {
        id,
        kind: "table",
        namespace: link.namespace,
        table: link.table,
        tableKind: known?.kind ?? "table",
        filter: link.filter,
      },
    ];
  }
  view.value = "tables";
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
  if (next !== "sql" || tabs.value.some((tab) => tab.kind === "query")) {
    return;
  }
  if (connectionSaved.value.length) {
    activeQueryTabId.value = SAVED_TAB_ID;
  } else {
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
  if (saveDialog.value) {
    void submitSaveDialog();
    return;
  }
  const tab = tabs.value.find((item) => item.id === activeQueryTabId.value);
  if (view.value === "sql" && tab?.kind === "query") {
    void saveQueryTab(tab);
    return;
  }
  void saveAll();
}

async function confirmCloseTab(id: string, name: string) {
  const ok = await confirm(`Discard unsaved changes to “${name}”?`, {
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
  if (tab?.kind === "query" && modifiedQueries.value.has(id)) {
    void confirmCloseTab(id, tabTitle(tab));
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
    setQueryModified(id, false);
    saveQueryTabs();
    if (renamingTabId.value === id) {
      renamingTabId.value = "";
    }
    if (activeQueryTabId.value === id) {
      activeQueryTabId.value = next || (connectionSaved.value.length ? SAVED_TAB_ID : "");
    }
  } else if (activeTableTabId.value === id) {
    activeTableTabId.value = next;
  }
}

function closeActivePaneTab() {
  if (showSavedTab.value && activeTabId.value === SAVED_TAB_ID) {
    return true;
  }
  if (!viewTabs.value.some((tab) => tab.id === activeTabId.value)) {
    return false;
  }
  closeTab(activeTabId.value);
  return true;
}

function tabTitle(tab: PaneTab) {
  if (tab.kind === "query") {
    return savedFor(tab)?.name ?? tab.title;
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
  closeMenus();
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
            :hidden-key="connectionId"
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
          <div class="db-toolbar-transfer">
            <button
              class="ghost tiny"
              type="button"
              :disabled="!canTransfer"
              :title="`Run a .sql or .sql.gz file against “${namespace}”`"
              @click="startImport"
            >
              <svg viewBox="0 0 16 16" aria-hidden="true">
                <path d="M8 2.5v7.5M4.8 6.8 8 10l3.2-3.2M2.5 11.5v1a1 1 0 0 0 1 1h9a1 1 0 0 0 1-1v-1" />
              </svg>
              Import
            </button>
            <button
              class="ghost tiny"
              type="button"
              :disabled="!canTransfer"
              :title="`Export “${namespace}” to a .sql file`"
              @click="exportDialog = { tables: null }"
            >
              <svg viewBox="0 0 16 16" aria-hidden="true">
                <path d="M8 10V2.5M4.8 5.7 8 2.5l3.2 3.2M2.5 11.5v1a1 1 0 0 0 1 1h9a1 1 0 0 0 1-1v-1" />
              </svg>
              Export
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
          <div
            class="db-table-list"
            role="listbox"
            aria-multiselectable="true"
            :aria-label="`Tables in ${namespace}`"
            @scroll="closeMenus"
          >
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
              :aria-selected="selectedTables.has(table.name) || activeTableTabId === `table:${namespace}.${table.name}`"
              :class="{
                active: activeTableTabId === `table:${namespace}.${table.name}`,
                selected: selectedTables.has(table.name),
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
              @click="onTableClick($event, table)"
              @dblclick="!$event.metaKey && !$event.shiftKey && queryTable(table)"
              @contextmenu="openTableMenu($event, table)"
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

        <QueryHistory
          v-if="view === 'history'"
          :connection-id="connectionId"
          :connection-name="entry?.name ?? ''"
        />
        <section v-show="view !== 'history'" class="db-main">
          <div v-if="viewTabs.length || view === 'sql'" class="subtab-bar" role="tablist">
            <div
              v-if="showSavedTab"
              class="subtab saved-tab"
              :class="{ active: activeTabId === SAVED_TAB_ID }"
              role="tab"
              :aria-selected="activeTabId === SAVED_TAB_ID"
              title="Saved queries for this connection"
              @click="activeQueryTabId = SAVED_TAB_ID"
            >
              <svg class="subtab-icon" viewBox="0 0 16 16" aria-hidden="true">
                <path d="M4 2.5h8a.5.5 0 0 1 .5.5v10.5L8 10.75 3.5 13.5V3a.5.5 0 0 1 .5-.5Z" />
              </svg>
              <span class="subtab-title">Saved queries</span>
              <span class="file-count-badge">{{ connectionSaved.length.toLocaleString() }}</span>
            </div>
            <div
              v-for="tab in viewTabs"
              :key="tab.id"
              class="subtab"
              :class="{
                active: activeTabId === tab.id,
                query: tab.kind === 'query',
                saved: Boolean(savedFor(tab)),
                dirty: dirtyTabs.has(tab.id) || modifiedQueries.has(tab.id),
              }"
              role="tab"
              :aria-selected="activeTabId === tab.id"
              :title="
                tab.kind === 'table'
                  ? `${tab.namespace}.${tab.table}`
                  : modifiedQueries.has(tab.id)
                    ? `${tabTitle(tab)} (unsaved changes)`
                    : tabTitle(tab)
              "
              @click="selectTab(tab)"
              @dblclick="tab.kind === 'query' && startTabRename(tab)"
              @contextmenu="openTabMenu($event, tab)"
              @auxclick.middle="closeTab(tab.id)"
            >
              <svg v-if="savedFor(tab)" class="subtab-icon" viewBox="0 0 16 16" aria-hidden="true">
                <path d="M4 2.5h8a.5.5 0 0 1 .5.5v10.5L8 10.75 3.5 13.5V3a.5.5 0 0 1 .5-.5Z" />
              </svg>
              <svg v-else-if="tab.kind === 'query'" class="subtab-icon" viewBox="0 0 16 16" aria-hidden="true">
                <path d="M5 4 1.5 8 5 12M11 4l3.5 4L11 12" />
              </svg>
              <svg v-else class="subtab-icon" viewBox="0 0 16 16" aria-hidden="true">
                <rect x="2" y="3" width="12" height="10" rx="1.5" />
                <path d="M2 6.5h12M6.5 6.5V13" />
              </svg>
              <input
                v-if="renamingTabId === tab.id"
                v-model="renameValue"
                class="subtab-rename"
                type="text"
                autocomplete="off"
                spellcheck="false"
                :data-rename-tab="tab.id"
                :size="Math.max(renameValue.length, 4)"
                :aria-label="`Rename ${tabTitle(tab)}`"
                @click.stop
                @dblclick.stop
                @keydown.enter.prevent="commitTabRename"
                @keydown.esc.stop.prevent="cancelTabRename"
                @blur="commitTabRename"
              />
              <span v-else class="subtab-title">{{ tabTitle(tab) }}</span>
              <button
                class="subtab-close"
                type="button"
                :aria-label="`Close ${tabTitle(tab)}`"
                @click.stop="closeTab(tab.id)"
                @dblclick.stop
              >
                <span v-if="dirtyTabs.has(tab.id) || modifiedQueries.has(tab.id)" class="dirty-dot" aria-hidden="true" />
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
          <div v-if="!viewTabs.length && !(showSavedTab && activeTabId === SAVED_TAB_ID)" class="db-empty muted">
            <p v-if="view === 'sql'">No open queries. Use + to start one.</p>
            <p v-else>Pick a table on the left, or double-click one to query it.</p>
          </div>
          <SavedQueries
            v-if="connectionSaved.length"
            v-show="showSavedTab && activeTabId === SAVED_TAB_ID"
            v-model:selected-id="selectedSavedId"
            :queries="connectionSaved"
            :open-ids="openSavedIds"
            @open="openSavedQuery($event)"
            @run="openSavedQuery($event, true)"
          />
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
              :filter="tab.filter"
              :ref="(instance) => setTableViewRef(tab.id, instance)"
              :active="active && view === 'tables' && activeTableTabId === tab.id"
              @changes="setTabChanges(tab.id, $event)"
              @follow="followLink"
              @clear-filter="setTableFilter(tab.id, undefined)"
            />
            <QueryEditor
              v-else
              :ref="(instance) => setEditorRef(tab.id, instance)"
              :connection-id="sessionId"
              :driver="driver"
              :schema="schema"
              :storage-key="tab.key"
              :active="active && view === 'sql' && activeQueryTabId === tab.id"
              :saved-sql="savedFor(tab)?.sql ?? null"
              @executed="onExecuted"
              @modified="setQueryModified(tab.id, $event)"
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
      <Teleport to="body">
        <div
          v-if="tableMenu"
          ref="tableMenuEl"
          class="overflow-menu-dropdown table-context-menu"
          role="menu"
          :aria-label="tableMenu.tables.length === 1 ? `${tableMenu.tables[0]} actions` : 'Selected tables actions'"
          :style="{ left: `${tableMenu.x}px`, top: `${tableMenu.y}px` }"
          @contextmenu.prevent
        >
          <template v-if="tableMenu.tables.length === 1">
            <button class="overflow-menu-item" type="button" role="menuitem" @click="runTableAction('open')">
              Open
            </button>
            <button class="overflow-menu-item" type="button" role="menuitem" @click="runTableAction('query')">
              Query
            </button>
            <button class="overflow-menu-item" type="button" role="menuitem" @click="runTableAction('copy')">
              Copy name
            </button>
            <div class="overflow-menu-divider" role="separator" />
          </template>
          <button class="overflow-menu-item" type="button" role="menuitem" @click="runTableAction('export')">
            {{ tableMenuExportLabel }}
          </button>
        </div>
        <div
          v-if="tabMenu && menuTab"
          ref="tabMenuEl"
          class="overflow-menu-dropdown table-context-menu"
          role="menu"
          :aria-label="`${tabTitle(menuTab)} actions`"
          :style="{ left: `${tabMenu.x}px`, top: `${tabMenu.y}px` }"
          @contextmenu.prevent
        >
          <button class="overflow-menu-item" type="button" role="menuitem" @click="startTabRename(menuTab)">
            Rename
          </button>
          <button
            v-if="!savedFor(menuTab)"
            class="overflow-menu-item"
            type="button"
            role="menuitem"
            @click="openSaveDialog(menuTab)"
          >
            Save query…
          </button>
          <template v-else>
            <button
              class="overflow-menu-item"
              type="button"
              role="menuitem"
              :disabled="!modifiedQueries.has(menuTab.id)"
              @click="saveQueryTab(menuTab)"
            >
              Save changes
            </button>
            <button class="overflow-menu-item" type="button" role="menuitem" @click="openSaveDialog(menuTab)">
              Save as new query…
            </button>
            <button class="overflow-menu-item" type="button" role="menuitem" @click="showInSaved(menuTab)">
              Show in Saved queries
            </button>
          </template>
        </div>
      </Teleport>
      <Modal v-if="saveDialog" title="Save query" @close="closeSaveDialog">
        <form class="save-query-form" @submit.prevent="submitSaveDialog">
          <label class="modal-label">
            <span class="muted tiny">Name</span>
            <input
              ref="saveNameInput"
              v-model="saveName"
              type="text"
              autocomplete="off"
              spellcheck="false"
              :disabled="saveBusy"
            />
          </label>
          <label class="modal-label">
            <span class="muted tiny">Description (optional)</span>
            <textarea
              v-model="saveDescription"
              placeholder="What this query does, or when to use it"
              :disabled="saveBusy"
              @keydown.meta.enter.prevent="submitSaveDialog"
            />
          </label>
          <p class="muted tiny">
            Saved queries belong to this connection and appear in the Saved queries tab.
          </p>
          <p v-if="saveError" class="settings-error">{{ saveError }}</p>
        </form>
        <template #actions>
          <button class="ghost" type="button" :disabled="saveBusy" @click="closeSaveDialog">Cancel</button>
          <button class="primary" type="button" :disabled="!saveName.trim() || saveBusy" @click="submitSaveDialog">
            <span v-if="saveBusy" class="spinner" aria-hidden="true" />
            {{ saveBusy ? "Saving…" : "Save" }}
          </button>
        </template>
      </Modal>
      <ExportDialog
        v-if="exportDialog"
        :connection-id="sessionId"
        :namespace="namespace"
        :namespace-label="namespaceLabel"
        :tables="exportDialog.tables"
        :table-count="tables.length"
        @close="exportDialog = null"
      />
      <ImportDialog
        v-if="importPath"
        :connection-id="sessionId"
        :namespace="namespace"
        :namespace-label="namespaceLabel"
        :path="importPath"
        @imported="onImported"
        @close="importPath = null"
      />
    </template>
  </div>
</template>
