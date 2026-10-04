<script setup lang="ts">
import type { SQLNamespace } from "@codemirror/lang-sql";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { confirm, open as openFile } from "@tauri-apps/plugin-dialog";
import { computed, nextTick, onMounted, onUnmounted, ref, shallowRef, watch, type Ref } from "vue";
import * as api from "../api";
import { SIDEBAR_MAX, SIDEBAR_MIN, useApp } from "../composables/useApp";
import { useConnectionForm } from "../composables/useConnectionForm";
import { registerInnerTabCloser, setLiveTitle, useTabs } from "../composables/useTabs";
import { useTransfers } from "../composables/useTransfers";
import type { FilterPreviewGroup } from "../filters/compile";
import {
  cloneNode,
  emptyGroup,
  hasConditions,
  linkFilter,
  newId,
  clampTabPageSize,
  sanitizeAutoRefresh,
  sanitizeFilter,
  type TableViewState,
} from "../filters/model";
import {
  driverLabel,
  type BackupInfo,
  type SaveRequest,
  type SavedQuery,
  type SessionInfo,
  type TableInfo,
  type TableLink,
} from "../types";
import BackupDialog from "./BackupDialog.vue";
import ConnectionViewTabs, { type ConnectionViewTab } from "./ConnectionViewTabs.vue";
import DatabaseSwitcher from "./DatabaseSwitcher.vue";
import DriverIcon from "./DriverIcon.vue";
import ExportDialog from "./ExportDialog.vue";
import FilterPopover from "./FilterPopover.vue";
import ImportDialog from "./ImportDialog.vue";
import Modal from "./Modal.vue";
import QueryEditor from "./QueryEditor.vue";
import QueryHistory from "./QueryHistory.vue";
import SchemaDiagram, { type DiagramScope } from "./SchemaDiagram.vue";
import RestoreDialog from "./RestoreDialog.vue";
import SavedQueries from "./SavedQueries.vue";
import SplitWorkspace from "./SplitWorkspace.vue";
import TablePaneFrame, { type PaneTabInfo } from "./TablePaneFrame.vue";
import TableView from "./TableView.vue";
import {
  activateTab,
  addTabToPane,
  canSplit,
  defaultSizesFor,
  checkSplitPaneAt,
  DROP_EDGE_ZONE,
  dropEdge,
  emptyPane,
  emptyWorkspace,
  findPane,
  focusedActiveTabId,
  focusPane,
  MAX_PANES,
  MAX_PANES_ACROSS,
  MAX_PANES_DOWN,
  mergeAllPanes,
  insertionIndex,
  moveTab,
  nextSplitDirection,
  nodeAtPath,
  paneForTab,
  removePane,
  removeTabFromWorkspace,
  restoreWorkspace,
  splitFocused,
  splitPaneAt,
  tabStripDrop,
  updateSplitSizes,
  type DropEdge,
  type SplitDirection,
  type TableWorkspace,
} from "../workspace/split";

/** Table tab ids are opaque, so several tabs can show the same table with different filters. */
type TableTab = { id: string; kind: "table" } & TableViewState;

type PaneTab = TableTab | { id: string; kind: "query"; key: string; title: string; savedId?: string };

type QueryTab = Extract<PaneTab, { kind: "query" }>;

interface FilterIndicator {
  count: number;
  summary: string;
  preview: FilterPreviewGroup | null;
}

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
const { revealKind } = useTransfers();

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
const tableFilterInput = ref<HTMLInputElement | null>(null);
const tableListEl = ref<HTMLElement | null>(null);
/** Set while a sidebar table name is the thing the user is acting on, even if the webview left keyboard focus on the grid. */
let tableTyping = false;
const schema = shallowRef<SQLNamespace>({});
const tabs = ref<PaneTab[]>([]);
const view = ref<ConnectionViewTab>("tables");
const diagramOpened = ref(false);
const diagramScope = ref<DiagramScope>("schema");
type TableListView = "tables" | "diagram";
const TABLE_LIST_KEY = "recon.tableListOpen";
/** The table list beside the diagram picks tables for it rather than opening them. */
const listForDiagram = computed(() => view.value === "diagram");
const tableListOpen = ref<Record<TableListView, boolean>>(loadTableListOpen());
const tableListView = computed<TableListView | null>(() =>
  view.value === "tables" || view.value === "diagram" ? view.value : null,
);
const tableListVisible = computed(() => (tableListView.value ? tableListOpen.value[tableListView.value] : false));

function loadTableListOpen(): Record<TableListView, boolean> {
  try {
    const saved = JSON.parse(localStorage.getItem(TABLE_LIST_KEY) ?? "{}") ?? {};
    return { tables: saved.tables !== false, diagram: saved.diagram !== false };
  } catch {
    return { tables: true, diagram: true };
  }
}

function toggleTableList() {
  const current = tableListView.value;
  if (!current) {
    return;
  }
  tableListOpen.value = { ...tableListOpen.value, [current]: !tableListOpen.value[current] };
  localStorage.setItem(TABLE_LIST_KEY, JSON.stringify(tableListOpen.value));
}
const workspace = ref<TableWorkspace>(emptyWorkspace());
const activeQueryTabId = ref("");
const tabsRestored = ref(false);
const splitHost = ref<HTMLElement | null>(null);
const flashPaneId = ref("");
let flashPaneTimer = 0;
const draggingTabId = ref("");
const dropTarget = ref<{ paneId: string; afterId?: string; atStart?: boolean; edge?: DropEdge } | null>(null);
const dirtyTabs = ref(new Map<string, number>());
const filteredTabs = ref(new Map<string, FilterIndicator>());
const filterPopover = ref<{ tabId: string; anchor: DOMRect } | null>(null);
let filterPopoverTimer = 0;
const popoverFilters = computed(() =>
  filterPopover.value ? (filteredTabs.value.get(filterPopover.value.tabId) ?? null) : null,
);
const tabActivity = new Map<string, number>();
let tableTabsSaveTimer = 0;
const saving = ref(false);
const nameDialog = ref<{ mode: "create" } | { mode: "rename"; from: string } | null>(null);
const nameValue = ref("");
const nameError = ref("");
const nameBusy = ref(false);
const nameInput = ref<HTMLInputElement | null>(null);
const sidebarEl = ref<HTMLElement | null>(null);
const diagramEl = ref<InstanceType<typeof SchemaDiagram> | null>(null);
const subtabBar = ref<HTMLElement | null>(null);

const activeTableTabId = computed({
  get: () => focusedActiveTabId(workspace.value),
  set: (id: string) => {
    if (!id) {
      return;
    }
    workspace.value = activateTab(workspace.value, id);
  },
});
const selectedTables = ref(new Set<string>());
const selectedTableNames = computed(() =>
  tables.value.map((table) => table.name).filter((name) => selectedTables.value.has(name)),
);
const tableMenu = ref<{ x: number; y: number; tables: string[] } | null>(null);
const tableMenuEl = ref<HTMLElement | null>(null);
const tabMenu = ref<{ x: number; y: number; tabId: string } | null>(null);
const tabMenuEl = ref<HTMLElement | null>(null);
const renamingTabId = ref("");
const renameValue = ref("");
const modifiedQueries = ref(new Set<string>());
const selectedSavedId = ref("");
const saveDialog = ref<{ tabId: string } | null>(null);
const savedQueriesEl = ref<InstanceType<typeof SavedQueries> | null>(null);
const saveName = ref("");
const saveDescription = ref("");
const saveBusy = ref(false);
const saveError = ref("");
const saveNameInput = ref<HTMLInputElement | null>(null);
// Each keeps the database it was opened on, since its transfer can keep running while you switch away.
const exportDialog = ref<{ namespace: string; tables: string[] | null; tableCount: number } | null>(null);
const importTarget = ref<{ namespace: string; path: string } | null>(null);
const backupTarget = ref<{ namespace: string; tableCount: number } | null>(null);
const restoreTarget = ref<{ namespace: string; path: string; info: BackupInfo } | null>(null);
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
const systemNamespaces = computed(() => session.value?.systemNamespaces ?? []);
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
const tableTabsKey = computed(() => `recon.tableTabs.${props.sessionId}`);

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
const menuTableTab = computed(() => {
  const tab = tabs.value.find((item) => item.id === tabMenu.value?.tabId);
  return tab?.kind === "table" ? tab : null;
});

function tableKey(tableNamespace: string, table: string) {
  return `${tableNamespace}\u0000${table}`;
}

const activeTableKey = computed(() => {
  if (listForDiagram.value) {
    return "";
  }
  const tab = tabs.value.find((item) => item.id === activeTableTabId.value);
  return tab?.kind === "table" ? tableKey(tab.namespace, tab.table) : "";
});
const dirtyTableKeys = computed(
  () =>
    new Set(
      tabs.value.flatMap((tab) =>
        tab.kind === "table" && dirtyTabs.value.has(tab.id) ? [tableKey(tab.namespace, tab.table)] : [],
      ),
    ),
);

/** Tabs on the same table without a custom name are numbered in tab order: orders, orders · 2. */
const tableTitleSuffix = computed(() => {
  const seen = new Map<string, number>();
  const suffix = new Map<string, string>();
  for (const tab of tabs.value) {
    if (tab.kind !== "table" || tab.title) {
      continue;
    }
    const key = tableKey(tab.namespace, tab.table);
    const count = (seen.get(key) ?? 0) + 1;
    seen.set(key, count);
    if (count > 1) {
      suffix.set(tab.id, ` · ${count}`);
    }
  }
  return suffix;
});

const queryTabs = computed(() => tabs.value.filter((tab): tab is QueryTab => tab.kind === "query"));
const viewTabs = computed(() => {
  if (view.value === "history" || view.value === "diagram") {
    return [];
  }
  return tabs.value.filter((tab) => (view.value === "sql" ? tab.kind === "query" : tab.kind === "table"));
});
const activeTabId = computed(() => {
  if (view.value === "history" || view.value === "diagram") {
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
const canRestore = computed(
  () => canTransfer.value && (driver.value !== "sqlite" || !namespace.value || namespace.value === "main"),
);

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

function saveTableTabs() {
  window.clearTimeout(tableTabsSaveTimer);
  tableTabsSaveTimer = 0;
  if (!tabsRestored.value) {
    return;
  }
  const saved = {
    active: activeTableTabId.value,
    focusedPaneId: workspace.value.focusedPaneId,
    twoPaneAxis: workspace.value.twoPaneAxis,
    layout: workspace.value.layout,
    panes: workspace.value.panes,
    tabs: tabs.value.flatMap((tab) =>
      tab.kind === "table"
        ? [
            {
              id: tab.id,
              namespace: tab.namespace,
              table: tab.table,
              tableKind: tab.tableKind,
              filter: tab.filter,
              sort: tab.sort,
              panelOpen: tab.panelOpen,
              origin: tab.origin,
              title: tab.title,
              pageSize: tab.pageSize != null ? clampTabPageSize(tab.pageSize) : undefined,
              autoRefresh: tab.autoRefresh,
            },
          ]
        : [],
    ),
  };
  try {
    localStorage.setItem(tableTabsKey.value, JSON.stringify(saved));
  } catch {
    // Storage is full; the tabs still work for this session.
  }
}

function scheduleSaveTableTabs() {
  window.clearTimeout(tableTabsSaveTimer);
  tableTabsSaveTimer = window.setTimeout(saveTableTabs, 300);
}

function restoredTableTab(value: unknown): TableTab | null {
  if (!value || typeof value !== "object") {
    return null;
  }
  const item = value as Record<string, unknown>;
  if (typeof item.id !== "string" || typeof item.namespace !== "string" || typeof item.table !== "string") {
    return null;
  }
  const sort = item.sort as Record<string, unknown> | null | undefined;
  return {
    id: item.id,
    kind: "table",
    namespace: item.namespace,
    table: item.table,
    tableKind: item.tableKind === "view" ? "view" : "table",
    filter: sanitizeFilter(item.filter),
    sort:
      sort && typeof sort.column === "string" && (sort.dir === "asc" || sort.dir === "desc")
        ? { column: sort.column, dir: sort.dir }
        : null,
    panelOpen: item.panelOpen === true,
    origin: item.origin === "link" ? "link" : "user",
    title: typeof item.title === "string" && item.title ? item.title : undefined,
    pageSize:
      typeof item.pageSize === "number" && Number.isFinite(item.pageSize)
        ? clampTabPageSize(item.pageSize)
        : undefined,
    autoRefresh: sanitizeAutoRefresh(item.autoRefresh),
  };
}

function restoreTableTabs() {
  let saved: Record<string, unknown> = {};
  try {
    saved = JSON.parse(localStorage.getItem(tableTabsKey.value) ?? "{}") ?? {};
  } catch {
    saved = {};
  }
  const restored = (Array.isArray(saved.tabs) ? saved.tabs : [])
    .map(restoredTableTab)
    .filter((tab): tab is TableTab => tab !== null);
  tabs.value = [...tabs.value, ...restored];
  const active = restored.find((tab) => tab.id === saved.active) ?? restored[0];
  workspace.value = restoreWorkspace(
    {
      layout: saved.layout,
      panes: saved.panes,
      focusedPaneId: saved.focusedPaneId,
      twoPaneAxis: saved.twoPaneAxis,
    },
    restored.map((tab) => tab.id),
    active?.id ?? "",
  );
}

function openQueryTab(initialSql?: string) {
  const key = nextQueryKey();
  const tab: PaneTab = { id: `query:${key}`, kind: "query", key, title: queryTitle() };
  tabs.value = [...tabs.value, tab];
  activeQueryTabId.value = tab.id;
  view.value = "sql";
  saveQueryTabs();
  flashTab(tab.id);
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
    flashTab(tab.id);
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

function startTabRename(tab: PaneTab) {
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
  const tableTab = tabs.value.find((item): item is TableTab => item.id === renamingTabId.value && item.kind === "table");
  if (tableTab) {
    renamingTabId.value = "";
    const name = renameValue.value.trim();
    const title = name && name !== baseTableTitle(tableTab) ? name : undefined;
    if (title !== tableTab.title) {
      updateView(tableTab.id, { title });
    }
    return;
  }
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

function newTableTab(
  tableNamespace: string,
  table: string,
  tableKind: "table" | "view",
  patch: Partial<TableViewState> = {},
): TableTab {
  return {
    id: `table:${newId("")}`,
    kind: "table",
    namespace: tableNamespace,
    table,
    tableKind,
    filter: emptyGroup(),
    sort: null,
    panelOpen: false,
    origin: "user",
    ...patch,
  };
}

function tabsForTable(tableNamespace: string, table: string) {
  return tabs.value.filter(
    (tab): tab is TableTab => tab.kind === "table" && tab.namespace === tableNamespace && tab.table === table,
  );
}

function mostRecentTab(list: TableTab[]) {
  let best: TableTab | undefined;
  for (const tab of list) {
    if (!best || (tabActivity.get(tab.id) ?? 0) >= (tabActivity.get(best.id) ?? 0)) {
      best = tab;
    }
  }
  return best;
}

/** Adds a table tab right after `afterId`, or at the end, and focuses it. */
function insertTableTab(tab: TableTab, afterId?: string, paneId?: string) {
  const next = [...tabs.value];
  const index = afterId ? next.findIndex((item) => item.id === afterId) : -1;
  next.splice(index >= 0 ? index + 1 : next.length, 0, tab);
  tabs.value = next;
  const target = paneId && findPane(workspace.value, paneId) ? paneId : workspace.value.focusedPaneId;
  const pane = findPane(workspace.value, target);
  const afterInPane = afterId && pane?.tabIds.includes(afterId) ? afterId : undefined;
  workspace.value = addTabToPane(workspace.value, target, tab.id, afterInPane);
  scheduleSaveTableTabs();
  flashTab(tab.id);
}

/** Turns a vertical mouse wheel into horizontal scrolling while the pointer is over the tab strip. */
function onSubtabWheel(event: WheelEvent) {
  const strip = event.currentTarget as HTMLElement;
  if (strip.scrollWidth <= strip.clientWidth || Math.abs(event.deltaY) <= Math.abs(event.deltaX)) {
    return;
  }
  event.preventDefault();
  const scale = event.deltaMode === WheelEvent.DOM_DELTA_LINE ? 16 : event.deltaMode === WheelEvent.DOM_DELTA_PAGE ? strip.clientWidth : 1;
  strip.scrollLeft += event.deltaY * scale;
}

/** Matches the `new-item-flash` animation in styles.css. */
const TAB_FLASH_MS = 1800;
const flashTabId = ref("");
let flashTabTimer = 0;

/** Scrolls a newly opened tab into view in the tab strip and briefly highlights it. */
function flashTab(id: string) {
  flashTabId.value = id;
  window.clearTimeout(flashTabTimer);
  flashTabTimer = window.setTimeout(() => {
    flashTabId.value = "";
  }, TAB_FLASH_MS);
  void nextTick(() => {
    const root = view.value === "sql" ? subtabBar.value : splitHost.value;
    const tab = root?.querySelector<HTMLElement>(`[data-tab-id="${CSS.escape(id)}"]`);
    const strip = tab?.closest(".subtab-bar");
    if (!strip || !tab) {
      return;
    }
    const box = strip.getBoundingClientRect();
    const rect = tab.getBoundingClientRect();
    if (rect.left < box.left) {
      strip.scrollLeft -= box.left - rect.left + 8;
    } else if (rect.right > box.right) {
      strip.scrollLeft += rect.right - box.right + 8;
    }
  });
}

function flashPane(id: string) {
  flashPaneId.value = id;
  window.clearTimeout(flashPaneTimer);
  flashPaneTimer = window.setTimeout(() => {
    flashPaneId.value = "";
  }, TAB_FLASH_MS);
}

/**
 * Focuses the most recently used tab on `table`, unless `newTab` asks for
 * another view of it. A new tab goes after the table's other tabs, or after
 * every tab when `atEnd` is set. Returns whether a tab was created.
 */
function openTable(table: TableInfo, newTab = false, atEnd = false) {
  const existing = tabsForTable(namespace.value, table.name);
  if (!newTab) {
    const focused = findPane(workspace.value, workspace.value.focusedPaneId);
    const inFocused = existing.filter((tab) => focused?.tabIds.includes(tab.id));
    const recentFocused = mostRecentTab(inFocused);
    if (recentFocused) {
      workspace.value = activateTab(workspace.value, recentFocused.id);
      scheduleSaveTableTabs();
      return false;
    }
    const recent = mostRecentTab(existing);
    if (recent) {
      workspace.value = activateTab(workspace.value, recent.id);
      scheduleSaveTableTabs();
      return false;
    }
  }
  insertTableTab(
    newTableTab(namespace.value, table.name, table.kind),
    atEnd ? undefined : existing[existing.length - 1]?.id,
  );
  return true;
}

/** The table whose tab the latest single click created, so the double-click that follows doesn't add another. */
let clickCreatedTab = "";

function onTableDblclick(event: MouseEvent, table: TableInfo) {
  if (event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) {
    return;
  }
  if (listForDiagram.value) {
    openFromDiagram(table.name);
    return;
  }
  if (clickCreatedTab === table.name) {
    clickCreatedTab = "";
    return;
  }
  openTable(table, true, true);
}

function updateView(id: string, patch: Partial<TableViewState>) {
  tabs.value = tabs.value.map((tab) => (tab.id === id && tab.kind === "table" ? { ...tab, ...patch } : tab));
  scheduleSaveTableTabs();
}

function setFilterIndicator(id: string, indicator: FilterIndicator) {
  const current = filteredTabs.value.get(id);
  if (
    current?.count === indicator.count &&
    current.summary === indicator.summary &&
    JSON.stringify(current.preview) === JSON.stringify(indicator.preview)
  ) {
    return;
  }
  const next = new Map(filteredTabs.value);
  if (indicator.count) {
    next.set(id, indicator);
  } else if (current) {
    next.delete(id);
  } else {
    return;
  }
  filteredTabs.value = next;
}

function resetTableTitle(tab: TableTab) {
  closeMenus();
  updateView(tab.id, { title: undefined });
}

function closeTableTab(tab: TableTab) {
  closeMenus();
  closeTab(tab.id);
}

type CloseScope = "others" | "left" | "right";

/** Table tabs beside `tab` in this pane's tab-bar order. */
function tableTabsBeside(tab: TableTab, scope: CloseScope) {
  const pane = paneForTab(workspace.value, tab.id);
  const list = (pane?.tabIds ?? []).flatMap((id) => {
    const item = tabs.value.find((entry): entry is TableTab => entry.id === id && entry.kind === "table");
    return item ? [item] : [];
  });
  const index = list.findIndex((item) => item.id === tab.id);
  if (scope === "left") {
    return list.slice(0, index);
  }
  if (scope === "right") {
    return list.slice(index + 1);
  }
  return list.filter((item) => item.id !== tab.id);
}

/** Closes several table tabs, asking once if any of them have unsaved changes. */
async function closeTableTabs(tab: TableTab, scope: CloseScope) {
  closeMenus();
  const closing = tableTabsBeside(tab, scope);
  if (!closing.length) {
    return;
  }
  const dirty = closing.filter((item) => dirtyTabs.value.has(item.id));
  if (dirty.length) {
    const names = dirty.slice(0, 3).map((item) => `“${tabTitle(item)}”`).join(", ");
    const more = dirty.length > 3 ? ` and ${dirty.length - 3} more` : "";
    const ok = await confirm(`Discard unsaved changes to ${names}${more}?`, {
      title: "Unsaved changes",
      kind: "warning",
      okLabel: "Discard",
      cancelLabel: "Cancel",
    });
    if (!ok) {
      return;
    }
  }
  for (const item of closing) {
    setTabChanges(item.id, 0);
    removeTab(item.id);
  }
  activeTableTabId.value = tab.id;
}

function duplicateTableTab(tab: TableTab) {
  closeMenus();
  insertTableTab({ ...tab, id: `table:${newId("")}`, filter: cloneNode(tab.filter), origin: "user", title: undefined }, tab.id);
}

watch(activeTableTabId, (id) => {
  if (id) {
    tabActivity.set(id, Date.now());
  }
  scheduleSaveTableTabs();
});

function armTableTyping() {
  tableTyping = true;
}

function onTableTypingPointerDown(event: PointerEvent) {
  const target = event.target;
  if (!(target instanceof Node) || !tableListEl.value?.contains(target)) {
    tableTyping = false;
    return;
  }
  if (target instanceof Element && target.closest(".db-table")) {
    tableTyping = true;
  }
}

/**
 * A table name is a button, and clicking it often leaves keyboard focus on the
 * grid. While the name is the active target, send typing to Filter tables.
 */
function onTableTypingKeydown(event: KeyboardEvent) {
  if (!props.active || !tableTyping || !tableListVisible.value) {
    return;
  }
  if (event.metaKey || event.ctrlKey || event.altKey || event.isComposing) {
    return;
  }
  if (document.querySelector(".modal-layer, [role='menu']")) {
    return;
  }
  const target = event.target;
  if (target instanceof HTMLElement && target.closest("input, textarea, select, [contenteditable]")) {
    return;
  }
  if (event.key === "Tab" || event.key === "Escape" || event.key === "Enter" || event.key.startsWith("Arrow")) {
    if (!(target instanceof Node) || !tableListEl.value?.contains(target)) {
      tableTyping = false;
    }
    return;
  }
  const character = event.key.length === 1;
  const backspace = event.key === "Backspace" && filter.value.length > 0;
  if (!character && !backspace) {
    return;
  }
  event.preventDefault();
  event.stopPropagation();
  const next = backspace ? filter.value.slice(0, -1) : filter.value + event.key;
  filter.value = next;
  const input = tableFilterInput.value;
  if (!input) {
    return;
  }
  input.value = next;
  void nextTick(() => {
    if (!tableTyping || !tableListVisible.value) {
      return;
    }
    input.focus({ preventScroll: true });
    input.setSelectionRange(input.value.length, input.value.length);
  });
}

function activeTableName() {
  if (listForDiagram.value) {
    return null;
  }
  const tab = tabs.value.find((item) => item.id === activeTableTabId.value);
  return tab?.kind === "table" && tab.namespace === namespace.value ? tab.table : null;
}

/**
 * Finder-style selection: a plain click opens the table and selects only it,
 * Cmd+click toggles a table without opening it, and Shift+click selects the
 * visible range from the last clicked table. Option+click and double-click
 * open another tab on the table. Beside the diagram, a plain click only
 * selects (and centers the table when the whole schema is showing), and
 * double-click leaves the diagram to open it.
 */
function onTableClick(event: MouseEvent, table: TableInfo) {
  if (event.altKey) {
    selectedTables.value = new Set([table.name]);
    selectionAnchor = table.name;
    if (listForDiagram.value) {
      view.value = "tables";
    }
    openTable(table, true);
    return;
  }
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
  if (listForDiagram.value) {
    clickCreatedTab = "";
    if (diagramScope.value === "schema") {
      diagramEl.value?.reveal(table.name);
    }
    return;
  }
  const created = openTable(table);
  if (event.detail <= 1) {
    clickCreatedTab = created ? table.name : "";
  }
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
  event.preventDefault();
  closeMenus();
  tabMenu.value = { x: event.clientX, y: event.clientY, tabId: tab.id };
  listenForMenuDismiss();
  fitMenu(tabMenu, tabMenuEl);
}

function runTableAction(action: "open" | "openNew" | "openSide" | "query" | "copy" | "diagram" | "export") {
  const chosen = tableMenu.value?.tables ?? [];
  const table = tables.value.find((item) => item.name === chosen[0]);
  if (action === "openSide" && table) {
    openTableToTheSide(table);
    return;
  }
  closeMenus();
  if (action === "diagram") {
    diagramScope.value = "selection";
    selectView("diagram");
    if (chosen.length === 1) {
      void nextTick(() => diagramEl.value?.isolate(chosen[0]));
    }
    return;
  }
  if (!table) {
    return;
  }
  if (action === "open" || action === "openNew") {
    openTable(table, action === "openNew");
  } else if (action === "query") {
    queryTable(table);
  } else if (action === "copy") {
    void copyNamespaceName(table.name);
  } else {
    openExport(chosen);
  }
}

function openExport(chosen: string[] | null) {
  if (!revealKind(props.sessionId, "export")) {
    exportDialog.value = { namespace: namespace.value, tables: chosen, tableCount: tables.value.length };
  }
}

function openBackup() {
  if (!revealKind(props.sessionId, "backup")) {
    backupTarget.value = { namespace: namespace.value, tableCount: tables.value.length };
  }
}

async function startImport() {
  if (revealKind(props.sessionId, "import")) {
    return;
  }
  const path = await openFile({
    title: `Import into “${namespace.value}”`,
    multiple: false,
    directory: false,
    filters: [{ name: "SQL", extensions: ["sql", "gz"] }],
  });
  if (typeof path === "string") {
    importTarget.value = { namespace: namespace.value, path };
  }
}

async function startRestore() {
  if (revealKind(props.sessionId, "restore")) {
    return;
  }
  const path = await openFile({
    title: `Restore “${namespace.value}” from a backup`,
    multiple: false,
    directory: false,
    filters: [{ name: "Recon backup", extensions: ["gz"] }],
  });
  if (typeof path !== "string") {
    return;
  }
  try {
    const target = namespace.value;
    restoreTarget.value = { namespace: target, path, info: await api.readBackupInfo(path) };
  } catch (err) {
    showToast(String(err), "error");
  }
}

function refreshVisibleTables() {
  for (const pane of workspace.value.panes) {
    tableViews.get(pane.activeTabId)?.refresh();
  }
}

function onImported() {
  void refreshAll();
  refreshVisibleTables();
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

/**
 * Reuses a tab on the target table only when that tab has no filters or its
 * filter also came from an arrow, so filters the user built are never replaced.
 */
function onTableFollow(link: TableLink, options?: { side?: boolean }) {
  followLink(link, Boolean(options?.side));
}

function followLink(link: TableLink, side = false) {
  const filter = linkFilter(link.filter);
  view.value = "tables";
  if (side) {
    followLinkToSide(link, filter);
    return;
  }
  const focused = findPane(workspace.value, workspace.value.focusedPaneId);
  const reusable = mostRecentTab(
    tabsForTable(link.namespace, link.table).filter(
      (tab) =>
        Boolean(focused?.tabIds.includes(tab.id)) && (tab.origin === "link" || !hasConditions(tab.filter)),
    ),
  );
  if (reusable) {
    updateView(reusable.id, { filter, origin: "link" });
    workspace.value = activateTab(workspace.value, reusable.id);
    scheduleSaveTableTabs();
    return;
  }
  const known =
    link.namespace === namespace.value ? tables.value.find((table) => table.name === link.table) : undefined;
  insertTableTab(
    newTableTab(link.namespace, link.table, known?.kind ?? "table", { filter, origin: "link" }),
    focused?.activeTabId,
  );
}

function followLinkToSide(link: TableLink, filter: ReturnType<typeof linkFilter>) {
  if (!ensureCanSplit(nextSplitDirection(workspace.value) ?? "right")) {
    followLink(link, false);
    return;
  }
  const known =
    link.namespace === namespace.value ? tables.value.find((table) => table.name === link.table) : undefined;
  const tab = newTableTab(link.namespace, link.table, known?.kind ?? "table", { filter, origin: "link" });
  openTabInNewPane(tab, nextSplitDirection(workspace.value) ?? "right");
}

function selectTab(tab: PaneTab) {
  if (tab.kind === "query") {
    activeQueryTabId.value = tab.id;
  } else {
    workspace.value = activateTab(workspace.value, tab.id);
    scheduleSaveTableTabs();
  }
}

function splitToast(reason: "max" | "size" | "span") {
  showToast(
    reason === "max"
      ? "The workspace can show 6 tables at once. Close a pane to split again."
      : reason === "span"
        ? `Panes can be up to ${MAX_PANES_ACROSS} across and ${MAX_PANES_DOWN} down.`
        : "This window is too small to add another table pane.",
  );
}

/** Checks a split against the layout as it will be once `tabId` has left its pane. */
function checkTabSplit(tabId: string, paneId: string, edge: DropEdge) {
  const host = splitHost.value;
  return checkSplitPaneAt(
    removeTabFromWorkspace(workspace.value, tabId),
    paneId,
    edge,
    host?.clientWidth ?? 0,
    host?.clientHeight ?? 0,
  );
}

function allowedEdge(tabId: string, paneId: string, edge: DropEdge | null) {
  if (!edge) {
    return null;
  }
  const check = checkTabSplit(tabId, paneId, edge);
  return check.ok || check.reason !== "span" ? edge : null;
}

function ensureCanSplit(direction: SplitDirection): boolean {
  const count = workspace.value.panes.length;
  if (count >= MAX_PANES) {
    splitToast("max");
    return false;
  }
  const width = splitHost.value?.clientWidth ?? 0;
  const height = splitHost.value?.clientHeight ?? 0;
  const check = canSplit(width, height, count, direction);
  if (!check.ok) {
    splitToast(check.reason);
    return false;
  }
  return true;
}

function openTabInNewPane(tab: TableTab, direction: SplitDirection) {
  const focused = findPane(workspace.value, workspace.value.focusedPaneId);
  if (!focused?.tabIds.length) {
    insertTableTab(tab);
    return;
  }
  tabs.value = [...tabs.value, tab];
  const pane = emptyPane([tab.id], tab.id);
  const result = splitFocused(workspace.value, pane, direction);
  if ("error" in result) {
    workspace.value = addTabToPane(workspace.value, workspace.value.focusedPaneId, tab.id);
  } else {
    workspace.value = result;
    flashPane(pane.id);
  }
  scheduleSaveTableTabs();
  flashTab(tab.id);
}

function openTableToTheSide(table: TableInfo) {
  closeMenus();
  const existing = tabsForTable(namespace.value, table.name);
  const focusedId = workspace.value.focusedPaneId;
  const elsewhere = existing.filter((tab) => paneForTab(workspace.value, tab.id)?.id !== focusedId);
  const recentElse = mostRecentTab(elsewhere);
  if (recentElse) {
    workspace.value = activateTab(workspace.value, recentElse.id);
    scheduleSaveTableTabs();
    flashTab(recentElse.id);
    return;
  }
  const direction = nextSplitDirection(workspace.value) ?? "right";
  if (!ensureCanSplit(direction)) {
    return;
  }
  openTabInNewPane(newTableTab(namespace.value, table.name, table.kind), direction);
}

function splitTableTab(tab: TableTab, direction: SplitDirection) {
  closeMenus();
  workspace.value = activateTab(workspace.value, tab.id);
  if (!ensureCanSplit(direction)) {
    return;
  }
  const copy: TableTab = {
    ...tab,
    id: `table:${newId("")}`,
    filter: cloneNode(tab.filter),
    origin: "user",
    title: undefined,
    panelOpen: false,
  };
  openTabInNewPane(copy, direction);
}

function closeWorkspacePane(paneId: string) {
  closeMenus();
  if (!paneId || workspace.value.panes.length <= 1) {
    return;
  }
  workspace.value = removePane(workspace.value, paneId, true);
  scheduleSaveTableTabs();
}

function mergeWorkspacePanes() {
  closeMenus();
  if (workspace.value.panes.length <= 1) {
    return;
  }
  workspace.value = mergeAllPanes(workspace.value, activeTableTabId.value);
  scheduleSaveTableTabs();
}

function moveMenuTabToPane(paneId: string) {
  const tab = menuTableTab.value;
  closeMenus();
  if (!tab) {
    return;
  }
  workspace.value = moveTab(workspace.value, tab.id, paneId);
  scheduleSaveTableTabs();
  flashTab(tab.id);
}

function onFocusPane(paneId: string) {
  if (workspace.value.focusedPaneId === paneId) {
    return;
  }
  workspace.value = focusPane(workspace.value, paneId);
  scheduleSaveTableTabs();
}

function onSplitResize(path: number[], sizes: [number, number]) {
  workspace.value = { ...workspace.value, layout: updateSplitSizes(workspace.value.layout, path, sizes) };
  scheduleSaveTableTabs();
}

function onSplitReset(path: number[]) {
  const node = nodeAtPath(workspace.value.layout, path);
  const sizes = node ? defaultSizesFor(node) : null;
  if (!sizes) {
    return;
  }
  workspace.value = { ...workspace.value, layout: updateSplitSizes(workspace.value.layout, path, sizes) };
  scheduleSaveTableTabs();
}

function tabInfo(tab: TableTab, preview = false): PaneTabInfo {
  const filtered = filteredTabs.value.get(tab.id);
  return {
    id: tab.id,
    title: tabTitle(tab),
    tooltip: preview ? `${tabTitle(tab)} · drop to move here` : filterPopover.value?.tabId === tab.id ? "" : tabTooltip(tab),
    dirty: dirtyTabs.value.has(tab.id),
    filtered: filtered
      ? { count: filtered.count, summary: filtered.summary }
      : hasConditions(tab.filter)
        ? { count: 1, summary: "" }
        : undefined,
    flash: !preview && flashTabId.value === tab.id,
    preview,
  };
}

function paneTabInfos(paneId: string): PaneTabInfo[] {
  const pane = findPane(workspace.value, paneId);
  const infos = (pane?.tabIds ?? []).flatMap((id) => {
    const tab = tabs.value.find((item): item is TableTab => item.id === id && item.kind === "table");
    return tab ? [tabInfo(tab)] : [];
  });
  const place = dropPreviewPlace.value;
  const preview = dropPreviewTab.value;
  if (!place || !preview || place.paneId !== paneId) {
    return infos;
  }
  const ghost: PaneTabInfo = { ...tabInfo(preview, true), id: `${preview.id}:preview` };
  const original = infos.findIndex((item) => item.id === preview.id);
  if (original >= 0) {
    infos.splice(original, 1);
  }
  if (place.atStart) {
    infos.unshift(ghost);
  } else if (place.afterId) {
    const after = infos.findIndex((item) => item.id === place.afterId);
    infos.splice(after >= 0 ? after + 1 : infos.length, 0, ghost);
  } else {
    infos.push(ghost);
  }
  return infos;
}

const dropHoverPaneId = computed(() => {
  const target = dropTarget.value;
  const dragging = draggingTabId.value;
  if (!target || !dragging || target.edge || target.afterId || target.atStart) {
    return "";
  }
  const source = paneForTab(workspace.value, dragging);
  return source && source.id !== target.paneId ? target.paneId : "";
});

const dropPreviewPlace = computed(() => {
  const target = dropTarget.value;
  const dragging = draggingTabId.value;
  if (!target || !dragging || target.edge) {
    return null;
  }
  const source = paneForTab(workspace.value, dragging);
  if (!source || (source.id === target.paneId && !target.afterId && !target.atStart)) {
    return null;
  }
  return target;
});

const dropPreviewTab = computed(() => {
  const id = draggingTabId.value;
  if (!id || !dropPreviewPlace.value) {
    return null;
  }
  const tab = tabs.value.find((item): item is TableTab => item.id === id && item.kind === "table");
  return tab ?? null;
});

const dragTableTab = computed(() => {
  const id = draggingTabId.value;
  if (!id) {
    return null;
  }
  return tabs.value.find((item): item is TableTab => item.id === id && item.kind === "table") ?? null;
});

function tableTabsInPane(paneId: string): TableTab[] {
  const pane = findPane(workspace.value, paneId);
  return (pane?.tabIds ?? []).flatMap((id) => {
    const tab = tabs.value.find((item): item is TableTab => item.id === id && item.kind === "table");
    return tab ? [tab] : [];
  });
}

function otherPanes(exceptPaneId?: string) {
  const skip = exceptPaneId ?? (menuTableTab.value ? paneForTab(workspace.value, menuTableTab.value.id)?.id : "");
  return workspace.value.panes.filter((pane) => pane.id !== skip).map((pane) => {
    const tab = tabs.value.find((item) => item.id === pane.activeTabId);
    return { id: pane.id, label: tab ? tabTitle(tab) : "Pane" };
  });
}

function paneFromPoint(x: number, y: number): { paneId: string; edge?: DropEdge } | null {
  const node = document.elementFromPoint(x, y);
  if (!(node instanceof Element)) {
    return null;
  }
  const preview = node.closest<HTMLElement>("[data-drop-edge]");
  if (preview?.dataset.paneId) {
    return { paneId: preview.dataset.paneId };
  }
  const frame = node.closest<HTMLElement>("[data-pane-id]");
  if (frame?.dataset.paneId) {
    return { paneId: frame.dataset.paneId };
  }
  const sash = node.closest(".split-sash");
  if (!(sash instanceof HTMLElement)) {
    return null;
  }
  const prev = sash.previousElementSibling?.querySelector<HTMLElement>("[data-pane-id]");
  const next = sash.nextElementSibling?.querySelector<HTMLElement>("[data-pane-id]");
  const box = sash.getBoundingClientRect();
  if (sash.classList.contains("axis-x")) {
    return x < box.left + box.width / 2
      ? prev?.dataset.paneId
        ? { paneId: prev.dataset.paneId, edge: "right" }
        : null
      : next?.dataset.paneId
        ? { paneId: next.dataset.paneId, edge: "left" }
        : null;
  }
  return y < box.top + box.height / 2
    ? prev?.dataset.paneId
      ? { paneId: prev.dataset.paneId, edge: "down" }
      : null
    : next?.dataset.paneId
      ? { paneId: next.dataset.paneId, edge: "up" }
      : null;
}

/** Extra room under the tab strip so a slightly low drag still reorders instead of leaving the strip. */
const TAB_STRIP_SLOP = 12;

function tabStripAtPoint(x: number, y: number): { paneId: string; strip: HTMLElement } | null {
  const node = document.elementFromPoint(x, y);
  if (node instanceof Element && node.closest("[data-drop-edge]")) {
    return null;
  }
  if (node instanceof Element) {
    const strip = node.closest(".table-pane-frame .subtab-bar");
    if (strip instanceof HTMLElement) {
      const paneId = strip.closest<HTMLElement>(".table-pane-frame")?.dataset.paneId;
      if (paneId) {
        return { paneId, strip };
      }
    }
  }
  const frames = splitHost.value?.querySelectorAll<HTMLElement>(".table-pane-frame[data-pane-id]") ?? [];
  for (const frame of frames) {
    const paneId = frame.dataset.paneId;
    const strip = frame.querySelector(".subtab-bar");
    if (!paneId || !(strip instanceof HTMLElement)) {
      continue;
    }
    const box = strip.getBoundingClientRect();
    if (x >= box.left && x <= box.right && y >= box.top && y <= box.bottom + TAB_STRIP_SLOP) {
      return { paneId, strip };
    }
  }
  return null;
}

function dropOnTabStrip(paneId: string, strip: HTMLElement, x: number, tabId: string) {
  const pane = findPane(workspace.value, paneId);
  if (!pane) {
    return null;
  }
  const preview = strip.querySelector<HTMLElement>(".subtab.drop-preview");
  const previewBox = preview?.getBoundingClientRect();
  const current = dropTarget.value;
  if (
    previewBox &&
    current?.paneId === paneId &&
    !current.edge &&
    x >= previewBox.left &&
    x <= previewBox.right
  ) {
    return current;
  }
  const ids: string[] = [];
  const midpoints: number[] = [];
  for (const id of pane.tabIds) {
    const tab = strip.querySelector<HTMLElement>(`[data-tab-id="${CSS.escape(id)}"]`);
    if (!tab) {
      continue;
    }
    const box = tab.getBoundingClientRect();
    ids.push(id);
    midpoints.push(box.left + box.width / 2);
  }
  if (!ids.length) {
    return null;
  }
  const place = tabStripDrop(ids, tabId, insertionIndex(midpoints, x));
  if (!place) {
    return null;
  }
  return "atStart" in place ? { paneId, atStart: true } : { paneId, afterId: place.afterId };
}

function canSplitPane(tabId: string, paneId: string) {
  if (workspace.value.panes.length >= MAX_PANES) {
    return false;
  }
  const source = paneForTab(workspace.value, tabId);
  return Boolean(source && (source.id !== paneId || source.tabIds.length > 1));
}

function paneFrame(paneId: string) {
  return splitHost.value?.querySelector<HTMLElement>(`.table-pane-frame[data-pane-id="${CSS.escape(paneId)}"]`) ?? null;
}

/** Full pane box, including a split preview, so the target does not jump once the preview opens. */
function paneOuterRect(frame: HTMLElement): DOMRect {
  const leaf = frame.closest(".split-leaf");
  const split = leaf?.parentElement?.parentElement;
  if (split?.classList.contains("split-node") && split.querySelector(":scope > .split-child > .split-leaf-preview")) {
    return split.getBoundingClientRect();
  }
  return (leaf ?? frame).getBoundingClientRect();
}

/** A drag that has left the workspace but is still beside a pane edge. */
function dropBesidePane(x: number, y: number, tabId: string) {
  const frames = splitHost.value?.querySelectorAll<HTMLElement>(".table-pane-frame[data-pane-id]") ?? [];
  let best: { paneId: string; edge: DropEdge; dist: number } | null = null;
  for (const frame of frames) {
    const paneId = frame.dataset.paneId;
    if (!paneId || !canSplitPane(tabId, paneId)) {
      continue;
    }
    const rect = paneOuterRect(frame);
    const dx = x < rect.left ? rect.left - x : x > rect.right ? x - rect.right : 0;
    const dy = y < rect.top ? rect.top - y : y > rect.bottom ? y - rect.bottom : 0;
    if ((dx === 0 && dy === 0) || dx > DROP_EDGE_ZONE || dy > DROP_EDGE_ZONE) {
      continue;
    }
    if (dx > 0 && dy > 0 && Math.hypot(dx, dy) > DROP_EDGE_ZONE) {
      continue;
    }
    const edge: DropEdge = dx > dy ? (x < rect.left ? "left" : "right") : y < rect.top ? "up" : "down";
    const dist = Math.hypot(dx, dy);
    if (!allowedEdge(tabId, paneId, edge)) {
      continue;
    }
    if (!best || dist < best.dist) {
      best = { paneId, edge, dist };
    }
  }
  return best ? { paneId: best.paneId, edge: best.edge } : null;
}

function onTabPointerDown(event: PointerEvent, tabId: string) {
  if (event.button !== 0 || renamingTabId.value) {
    return;
  }
  const startX = event.clientX;
  const startY = event.clientY;
  let started = false;

  const onMove = (move: PointerEvent) => {
    if (!started) {
      if (Math.hypot(move.clientX - startX, move.clientY - startY) < 6) {
        return;
      }
      started = true;
      draggingTabId.value = tabId;
      document.body.classList.add("dragging-tab");
    }
    const strip = tabStripAtPoint(move.clientX, move.clientY);
    if (strip) {
      dropTarget.value = dropOnTabStrip(strip.paneId, strip.strip, move.clientX, tabId);
      return;
    }
    const hit = paneFromPoint(move.clientX, move.clientY);
    if (!hit) {
      dropTarget.value = dropBesidePane(move.clientX, move.clientY, tabId);
      return;
    }
    const { paneId } = hit;
    const frame = paneFrame(paneId);
    let edge = hit.edge ?? null;
    if (!edge && frame && canSplitPane(tabId, paneId)) {
      // The tab strip fills the pane's top edge, so splitting upward means dragging above the pane.
      edge = dropEdge(paneOuterRect(frame), move.clientX, move.clientY, ["left", "right", "down"]);
    }
    edge = allowedEdge(tabId, paneId, edge);
    if (edge && canSplitPane(tabId, paneId)) {
      dropTarget.value = { paneId, edge };
      return;
    }
    const source = paneForTab(workspace.value, tabId);
    dropTarget.value = source?.id !== paneId ? { paneId } : null;
  };

  const onUp = () => {
    window.removeEventListener("pointermove", onMove);
    window.removeEventListener("pointerup", onUp);
    window.removeEventListener("pointercancel", onUp);
    document.body.classList.remove("dragging-tab");
    const target = dropTarget.value;
    draggingTabId.value = "";
    dropTarget.value = null;
    if (!started || !target) {
      return;
    }
    applyTabDrop(tabId, target);
  };

  window.addEventListener("pointermove", onMove);
  window.addEventListener("pointerup", onUp);
  window.addEventListener("pointercancel", onUp);
}

function applyTabDrop(tabId: string, target: { paneId: string; afterId?: string; atStart?: boolean; edge?: DropEdge }) {
  if (target.edge) {
    const source = paneForTab(workspace.value, tabId);
    if (!source || (source.tabIds.length <= 1 && source.id === target.paneId)) {
      return;
    }
    const check = checkTabSplit(tabId, target.paneId, target.edge);
    if (!check.ok) {
      splitToast(check.reason);
      return;
    }
    const pane = emptyPane([tabId], tabId);
    const result = splitPaneAt(removeTabFromWorkspace(workspace.value, tabId), target.paneId, pane, target.edge);
    if ("error" in result) {
      if (result.error === "max") {
        splitToast("max");
      }
      return;
    }
    workspace.value = result;
    flashPane(pane.id);
    scheduleSaveTableTabs();
    flashTab(tabId);
    return;
  }
  workspace.value = moveTab(workspace.value, tabId, target.paneId, target.afterId, target.atStart);
  scheduleSaveTableTabs();
}

function selectView(next: ConnectionViewTab) {
  view.value = next;
  if (next === "diagram") {
    diagramOpened.value = true;
  }
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

/**
 * Row edits from different tabs on the same table that set one cell to
 * different values. Saves run in tab order, so the rightmost tab's value wins.
 */
function editConflicts(requests: SaveRequest[]) {
  const seen = new Map<string, { value: unknown; tab: number }>();
  const conflicts: string[] = [];
  requests.forEach((request, tab) => {
    for (const update of request.updates) {
      const row = update.key.map((cell) => `${cell.column} = ${cell.value}`).join(", ");
      for (const change of update.changes) {
        const key = JSON.stringify([request.namespace, request.table, update.key, change.column]);
        const earlier = seen.get(key);
        if (earlier && earlier.tab !== tab && earlier.value !== change.value) {
          conflicts.push(`${request.table}.${change.column} where ${row}`);
        }
        seen.set(key, { value: change.value, tab });
      }
    }
  });
  return conflicts;
}

async function saveAll() {
  if (saving.value) {
    return;
  }
  const pending = tabs.value.flatMap((tab) => {
    const view = tab.kind === "table" ? tableViews.get(tab.id) : undefined;
    return view ? [{ view, ...view.pendingChanges() }] : [];
  });
  const conflicts = editConflicts(pending.map((item) => item.request));
  if (conflicts.length) {
    const listed = conflicts.slice(0, 3).join("\n");
    const more = conflicts.length > 3 ? `\n…and ${conflicts.length - 3} more` : "";
    const ok = await confirm(
      `Two tabs change the same cell to different values:\n${listed}${more}\n\nThe value from the tab furthest to the right will be saved.`,
      { title: "Conflicting edits", kind: "warning", okLabel: "Save anyway", cancelLabel: "Cancel" },
    );
    if (!ok) {
      return;
    }
  }
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
  if (!props.active || !(event.metaKey || event.ctrlKey) || event.altKey || event.shiftKey) {
    return;
  }
  const key = event.key.toLowerCase();
  if (key === "b" && tableListView.value && !document.querySelector(".modal-layer")) {
    event.preventDefault();
    toggleTableList();
    return;
  }
  if (key !== "s") {
    return;
  }
  event.preventDefault();
  if (saveDialog.value) {
    void submitSaveDialog();
    return;
  }
  if (view.value === "sql" && activeQueryTabId.value === SAVED_TAB_ID && savedQueriesEl.value?.isEditing) {
    void savedQueriesEl.value.saveEdits();
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
  } else {
    tabActivity.delete(id);
    setFilterIndicator(id, { count: 0, summary: "", preview: null });
    if (filterPopover.value?.tabId === id) {
      filterPopover.value = null;
    }
    if (renamingTabId.value === id) {
      renamingTabId.value = "";
    }
    workspace.value = removeTabFromWorkspace(workspace.value, id);
    scheduleSaveTableTabs();
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

function baseTableTitle(tab: TableTab) {
  return tab.namespace === namespace.value ? tab.table : `${tab.namespace}.${tab.table}`;
}

function tabTitle(tab: PaneTab) {
  if (tab.kind === "query") {
    return savedFor(tab)?.name ?? tab.title;
  }
  return tab.title || `${baseTableTitle(tab)}${tableTitleSuffix.value.get(tab.id) ?? ""}`;
}

function tabTooltip(tab: PaneTab) {
  if (tab.kind === "query") {
    return modifiedQueries.value.has(tab.id) ? `${tabTitle(tab)} (unsaved changes)` : tabTitle(tab);
  }
  return `${tab.namespace}.${tab.table}`;
}

/** Lists a tab's applied filters while the pointer is over its funnel icon. */
function showFilterPopover(event: MouseEvent, tabId: string) {
  window.clearTimeout(filterPopoverTimer);
  filterPopover.value = { tabId, anchor: (event.currentTarget as HTMLElement).getBoundingClientRect() };
}

function hideFilterPopover() {
  window.clearTimeout(filterPopoverTimer);
  filterPopoverTimer = window.setTimeout(() => {
    filterPopover.value = null;
  }, 80);
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
      restoreTableTabs();
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
  tabs.value = tabs.value.map((tab) =>
    tab.kind === "table" && tab.namespace === from ? { ...tab, namespace: to } : tab,
  );
  scheduleSaveTableTabs();
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
    refreshVisibleTables();
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

function selectFromDiagram(names: string[]) {
  selectedTables.value = new Set(names);
  selectionAnchor = names[0] ?? "";
}

function openFromDiagram(name: string) {
  const table = tables.value.find((item) => item.name === name) ?? { name, kind: "table" as const };
  view.value = "tables";
  openTable(table);
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
    const dirty = dirtyTableKeys.value.has(tableKey(namespace.value, table.name));
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

function flushTableTabs() {
  if (tableTabsSaveTimer && !extraTab.value) {
    saveTableTabs();
  }
}

onMounted(() => {
  window.addEventListener("keydown", onWindowKeydown, true);
  window.addEventListener("keydown", onTableTypingKeydown, true);
  document.addEventListener("pointerdown", onTableTypingPointerDown, true);
  window.addEventListener("focus", onWindowFocus);
  window.addEventListener("beforeunload", flushTableTabs);
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
  window.removeEventListener("keydown", onTableTypingKeydown, true);
  document.removeEventListener("pointerdown", onTableTypingPointerDown, true);
  window.removeEventListener("focus", onWindowFocus);
  window.removeEventListener("beforeunload", flushTableTabs);
  stopLost?.();
  stopRestored?.();
  closeMenus();
  unregisterCloser();
  setLiveTitle(props.sessionId, "");
  void api.disconnect(props.sessionId).catch(() => undefined);
  if (extraTab.value) {
    window.clearTimeout(tableTabsSaveTimer);
    for (const tab of tabs.value) {
      if (tab.kind === "query") {
        localStorage.removeItem(`recon.query.${tab.key}`);
      }
    }
    localStorage.removeItem(queryTabsKey.value);
    localStorage.removeItem(tableTabsKey.value);
  } else if (tableTabsSaveTimer) {
    saveTableTabs();
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
            :system-items="systemNamespaces"
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
              @click="openExport(null)"
            >
              <svg viewBox="0 0 16 16" aria-hidden="true">
                <path d="M8 10V2.5M4.8 5.7 8 2.5l3.2 3.2M2.5 11.5v1a1 1 0 0 0 1 1h9a1 1 0 0 0 1-1v-1" />
              </svg>
              Export
            </button>
            <span class="db-toolbar-transfer-divider" aria-hidden="true" />
            <button
              class="ghost tiny"
              type="button"
              :disabled="!canTransfer"
              :title="`Save a full backup of “${namespace}” that Restore can bring back`"
              @click="openBackup"
            >
              <svg viewBox="0 0 16 16" aria-hidden="true">
                <path d="M3.5 2.5h7l2 2v8a1 1 0 0 1-1 1h-8a1 1 0 0 1-1-1v-9a1 1 0 0 1 1-1ZM5.5 2.5v3h5v-3M5 13.5v-4h6v4" />
              </svg>
              Backup
            </button>
            <button
              class="ghost tiny"
              type="button"
              :disabled="!canRestore"
              :title="
                canRestore || !canTransfer
                  ? `Replace everything in “${namespace}” with a Recon backup`
                  : 'SQLite backups can only be restored into the main database'
              "
              @click="startRestore"
            >
              <svg viewBox="0 0 16 16" aria-hidden="true">
                <path d="M2.8 8a5.2 5.2 0 1 0 1.5-3.7M2.5 2.5v2.8h2.8M8 5v3l2 1.5" />
              </svg>
              Restore
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
      <ConnectionViewTabs
        :active="view"
        :table-count="tables.length"
        :list-open="tableListView ? tableListVisible : null"
        @select="selectView"
        @toggle-list="toggleTableList"
      />
      <div class="db-body">
        <aside v-show="tableListVisible" ref="sidebarEl" class="db-sidebar" :style="{ width: `${sidebarWidth}px` }">
          <div class="db-filter">
            <input
              ref="tableFilterInput"
              v-model="filter"
              type="search"
              placeholder="Filter tables"
              spellcheck="false"
            />
          </div>
          <div
            ref="tableListEl"
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
              :aria-selected="selectedTables.has(table.name) || activeTableKey === tableKey(namespace, table.name)"
              :class="{
                active: activeTableKey === tableKey(namespace, table.name),
                selected: selectedTables.has(table.name),
                view: table.kind === 'view',
                dirty: dirtyTableKeys.has(tableKey(namespace, table.name)),
              }"
              :title="
                dirtyTableKeys.has(tableKey(namespace, table.name))
                  ? `${table.name} (unsaved changes)`
                  : table.kind === 'view'
                    ? `${table.name} (view) · double-click to open in a new tab`
                    : `${table.name} · double-click to open in a new tab`
              "
              @mousedown="armTableTyping"
              @focus="armTableTyping"
              @click="onTableClick($event, table)"
              @dblclick="onTableDblclick($event, table)"
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
                v-if="dirtyTableKeys.has(tableKey(namespace, table.name))"
                class="dirty-dot"
                aria-label="Unsaved changes"
              />
            </button>
          </div>
        </aside>
        <div
          v-show="tableListVisible"
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
        <SchemaDiagram
          v-if="diagramOpened"
          ref="diagramEl"
          v-show="view === 'diagram'"
          v-model:scope="diagramScope"
          :connection-id="sessionId"
          :namespace="namespace"
          :selected="selectedTableNames"
          :active="active && view === 'diagram'"
          @open="openFromDiagram"
          @select="selectFromDiagram"
        />
        <section v-show="view !== 'history' && view !== 'diagram'" class="db-main">
          <div
            v-if="view === 'sql' && (viewTabs.length || showSavedTab)"
            ref="subtabBar"
            class="subtab-bar"
            role="tablist"
            @wheel="onSubtabWheel"
          >
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
                'tab-flash': flashTabId === tab.id,
              }"
              :data-tab-id="tab.id"
              role="tab"
              :aria-selected="activeTabId === tab.id"
              :title="filterPopover?.tabId === tab.id ? undefined : tabTooltip(tab)"
              @click="selectTab(tab)"
              @dblclick="startTabRename(tab)"
              @contextmenu="openTabMenu($event, tab)"
              @auxclick.middle="closeTab(tab.id)"
            >
              <svg v-if="savedFor(tab)" class="subtab-icon" viewBox="0 0 16 16" aria-hidden="true">
                <path d="M4 2.5h8a.5.5 0 0 1 .5.5v10.5L8 10.75 3.5 13.5V3a.5.5 0 0 1 .5-.5Z" />
              </svg>
              <svg v-else-if="tab.kind === 'query'" class="subtab-icon" viewBox="0 0 16 16" aria-hidden="true">
                <path d="M5 4 1.5 8 5 12M11 4l3.5 4L11 12" />
              </svg>
              <span
                v-else-if="filteredTabs.has(tab.id)"
                class="subtab-filter"
                :aria-label="`${filteredTabs.get(tab.id)?.count} ${filteredTabs.get(tab.id)?.count === 1 ? 'filter' : 'filters'} applied: ${filteredTabs.get(tab.id)?.summary}`"
                @mouseenter="showFilterPopover($event, tab.id)"
                @mouseleave="hideFilterPopover"
              >
                <svg class="subtab-icon" viewBox="0 0 16 16" aria-hidden="true">
                  <path d="M2.5 3h11L9.2 8.2v4.3l-2.4 1.2V8.2L2.5 3Z" />
                </svg>
                <span class="subtab-filter-dot" aria-hidden="true" />
              </span>
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
          <div
            v-if="view === 'sql' && !viewTabs.length && !(showSavedTab && activeTabId === SAVED_TAB_ID)"
            class="db-empty muted"
          >
            <p>No open queries. Use + to start one.</p>
          </div>
          <SavedQueries
            v-if="connectionSaved.length"
            ref="savedQueriesEl"
            v-show="showSavedTab && activeTabId === SAVED_TAB_ID"
            v-model:selected-id="selectedSavedId"
            :queries="connectionSaved"
            :open-ids="openSavedIds"
            :driver="driver"
            :schema="schema"
            @open="openSavedQuery($event)"
            @run="openSavedQuery($event, true)"
          />
          <div
            v-for="tab in queryTabs"
            v-show="view === 'sql' && activeQueryTabId === tab.id"
            :key="tab.id"
            class="db-main-pane"
          >
            <QueryEditor
              :ref="(instance) => setEditorRef(tab.id, instance)"
              :connection-id="sessionId"
              :driver="driver"
              :schema="schema"
              :storage-key="tab.key"
              :active="active && view === 'sql' && activeQueryTabId === tab.id"
              :saved-sql="savedFor(tab)?.sql ?? null"
              :title="tabTitle(tab)"
              @executed="onExecuted"
              @modified="setQueryModified(tab.id, $event)"
            />
          </div>
          <div v-show="view === 'tables'" ref="splitHost" class="split-workspace-host">
            <SplitWorkspace
              :node="workspace.layout"
              :focused-pane-id="workspace.focusedPaneId"
              :flash-pane-id="flashPaneId"
              :pane-count="workspace.panes.length"
              :drop-pane-id="dropHoverPaneId || (dropTarget?.edge ? dropTarget.paneId : '')"
              :drop-edge="dropTarget?.edge ?? null"
              :drop-preview-title="dropTarget?.edge && dragTableTab ? tabTitle(dragTableTab) : ''"
              :drop-preview-filtered="Boolean(dropTarget?.edge && dragTableTab && filteredTabs.has(dragTableTab.id))"
              @focus="onFocusPane"
              @resize="onSplitResize"
              @reset="onSplitReset"
            >
              <template #default="{ paneId }">
                <TablePaneFrame
                  :pane-id="paneId"
                  :focused="workspace.focusedPaneId === paneId"
                  :tabs="paneTabInfos(paneId)"
                  :active-tab-id="findPane(workspace, paneId)?.activeTabId ?? ''"
                  :renaming-tab-id="renamingTabId"
                  :rename-value="renameValue"
                  :dragging-tab-id="draggingTabId"
                  :drop-hover="dropHoverPaneId === paneId"
                  @focus="onFocusPane(paneId)"
                  @select="(id) => { const tab = tabs.find((item) => item.id === id); if (tab) selectTab(tab); }"
                  @close="closeTab"
                  @menu="(event, id) => { const tab = tabs.find((item) => item.id === id); if (tab) openTabMenu(event, tab); }"
                  @rename-start="(id) => { const tab = tabs.find((item) => item.id === id); if (tab) startTabRename(tab); }"
                  @update:rename-value="renameValue = $event"
                  @rename-commit="commitTabRename"
                  @rename-cancel="cancelTabRename"
                  @filter-enter="showFilterPopover"
                  @filter-leave="hideFilterPopover"
                  @tab-pointer-down="onTabPointerDown"
                >
                  <div
                    v-for="tab in tableTabsInPane(paneId)"
                    v-show="findPane(workspace, paneId)?.activeTabId === tab.id"
                    :key="tab.id"
                    class="db-main-pane"
                  >
                    <TableView
                      :connection-id="sessionId"
                      :driver="driver"
                      :view="tab"
                      :ref="(instance) => setTableViewRef(tab.id, instance)"
                      :active="active && view === 'tables' && workspace.focusedPaneId === paneId && findPane(workspace, paneId)?.activeTabId === tab.id"
                      :visible="active && view === 'tables' && findPane(workspace, paneId)?.activeTabId === tab.id"
                      @changes="setTabChanges(tab.id, $event)"
                      @follow="onTableFollow"
                      @update:view="updateView(tab.id, $event)"
                      @filter-state="setFilterIndicator(tab.id, $event)"
                      @open-sql="openQueryTab($event)"
                    />
                  </div>
                  <div v-if="!tableTabsInPane(paneId).length" class="db-empty muted">
                    <p>Pick a table on the left. Double-click one to open another tab on it.</p>
                  </div>
                </TablePaneFrame>
              </template>
            </SplitWorkspace>
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
            <button class="overflow-menu-item" type="button" role="menuitem" @click="runTableAction('openNew')">
              Open in new tab
            </button>
            <button
              class="overflow-menu-item"
              type="button"
              role="menuitem"
              :disabled="workspace.panes.length >= MAX_PANES && !otherPanes(workspace.focusedPaneId).length"
              @click="runTableAction('openSide')"
            >
              Open to the side
            </button>
            <button class="overflow-menu-item" type="button" role="menuitem" @click="runTableAction('query')">
              Query
            </button>
            <button class="overflow-menu-item" type="button" role="menuitem" @click="runTableAction('copy')">
              Copy name
            </button>
            <div class="overflow-menu-divider" role="separator" />
          </template>
          <button class="overflow-menu-item" type="button" role="menuitem" @click="runTableAction('diagram')">
            Show diagram
          </button>
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
        <div
          v-if="tabMenu && menuTableTab"
          ref="tabMenuEl"
          class="overflow-menu-dropdown table-context-menu"
          role="menu"
          :aria-label="`${tabTitle(menuTableTab)} actions`"
          :style="{ left: `${tabMenu.x}px`, top: `${tabMenu.y}px` }"
          @contextmenu.prevent
        >
          <button class="overflow-menu-item" type="button" role="menuitem" @click="duplicateTableTab(menuTableTab)">
            Duplicate tab
          </button>
          <button
            class="overflow-menu-item"
            type="button"
            role="menuitem"
            :disabled="workspace.panes.length >= MAX_PANES"
            @click="splitTableTab(menuTableTab, 'right')"
          >
            Split right
          </button>
          <button
            class="overflow-menu-item"
            type="button"
            role="menuitem"
            :disabled="workspace.panes.length >= MAX_PANES"
            @click="splitTableTab(menuTableTab, 'down')"
          >
            Split down
          </button>
          <button
            v-if="workspace.panes.length === 2 && otherPanes().length"
            class="overflow-menu-item"
            type="button"
            role="menuitem"
            @click="moveMenuTabToPane(otherPanes()[0].id)"
          >
            Move to other pane
          </button>
          <template v-else-if="workspace.panes.length > 2">
            <button
              v-for="pane in otherPanes()"
              :key="pane.id"
              class="overflow-menu-item"
              type="button"
              role="menuitem"
              @click="moveMenuTabToPane(pane.id)"
            >
              Move to {{ pane.label }}
            </button>
          </template>
          <button class="overflow-menu-item" type="button" role="menuitem" @click="startTabRename(menuTableTab)">
            Rename…
          </button>
          <button
            v-if="menuTableTab.title"
            class="overflow-menu-item"
            type="button"
            role="menuitem"
            @click="resetTableTitle(menuTableTab)"
          >
            Reset name
          </button>
          <div class="overflow-menu-divider" role="separator" />
          <button class="overflow-menu-item" type="button" role="menuitem" @click="closeTableTab(menuTableTab)">
            Close tab
          </button>
          <button
            class="overflow-menu-item"
            type="button"
            role="menuitem"
            :disabled="!tableTabsBeside(menuTableTab, 'others').length"
            @click="closeTableTabs(menuTableTab, 'others')"
          >
            Close other tabs
          </button>
          <button
            class="overflow-menu-item"
            type="button"
            role="menuitem"
            :disabled="!tableTabsBeside(menuTableTab, 'left').length"
            @click="closeTableTabs(menuTableTab, 'left')"
          >
            Close tabs to the left
          </button>
          <button
            class="overflow-menu-item"
            type="button"
            role="menuitem"
            :disabled="!tableTabsBeside(menuTableTab, 'right').length"
            @click="closeTableTabs(menuTableTab, 'right')"
          >
            Close tabs to the right
          </button>
          <template v-if="workspace.panes.length > 1">
            <div class="overflow-menu-divider" role="separator" />
            <button
              class="overflow-menu-item"
              type="button"
              role="menuitem"
              @click="closeWorkspacePane(paneForTab(workspace, menuTableTab.id)?.id ?? '')"
            >
              Close pane
            </button>
            <button class="overflow-menu-item" type="button" role="menuitem" @click="mergeWorkspacePanes">
              Merge all panes
            </button>
          </template>
        </div>
      </Teleport>
      <FilterPopover
        v-if="filterPopover && popoverFilters"
        :preview="popoverFilters.preview"
        :count="popoverFilters.count"
        :anchor="filterPopover.anchor"
      />
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
        :namespace="exportDialog.namespace"
        :namespace-label="namespaceLabel"
        :tables="exportDialog.tables"
        :table-count="exportDialog.tableCount"
        @close="exportDialog = null"
      />
      <ImportDialog
        v-if="importTarget"
        :connection-id="sessionId"
        :namespace="importTarget.namespace"
        :namespace-label="namespaceLabel"
        :path="importTarget.path"
        @imported="onImported"
        @close="importTarget = null"
      />
      <BackupDialog
        v-if="backupTarget"
        :connection-id="sessionId"
        :driver="driver"
        :namespace="backupTarget.namespace"
        :namespace-label="namespaceLabel"
        :table-count="backupTarget.tableCount"
        @close="backupTarget = null"
      />
      <RestoreDialog
        v-if="restoreTarget"
        :connection-id="sessionId"
        :driver="driver"
        :namespace="restoreTarget.namespace"
        :namespace-label="namespaceLabel"
        :path="restoreTarget.path"
        :info="restoreTarget.info"
        @restored="onImported"
        @close="restoreTarget = null"
      />
    </template>
  </div>
</template>
