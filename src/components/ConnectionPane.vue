<script setup lang="ts">
import type { SQLNamespace } from "@codemirror/lang-sql";
import { computed, nextTick, onMounted, onUnmounted, ref, shallowRef } from "vue";
import * as api from "../api";
import { SIDEBAR_MAX, SIDEBAR_MIN, useApp } from "../composables/useApp";
import { useConnectionForm } from "../composables/useConnectionForm";
import { driverLabel, type SessionInfo, type TableInfo } from "../types";
import DriverIcon from "./DriverIcon.vue";
import QueryEditor from "./QueryEditor.vue";
import TableView from "./TableView.vue";

type PaneTab =
  | { id: string; kind: "table"; namespace: string; table: string; tableKind: "table" | "view" }
  | { id: string; kind: "query"; key: string; title: string };

const props = defineProps<{
  connectionId: string;
  active: boolean;
}>();

const { findConnection, sidebarWidth, previewPreferences, savePreferences, showToast } = useApp();
const { openEditConnection } = useConnectionForm();

const status = ref<"connecting" | "password" | "connected" | "error">("connecting");
const connectError = ref("");
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
const activeTabId = ref("");
const editors = new Map<string, InstanceType<typeof QueryEditor>>();

const match = computed(() => findConnection(props.connectionId));
const entry = computed(() => match.value?.connection ?? null);
const driver = computed(() => entry.value?.driver ?? "mysql");
const namespaceLabel = computed(() => session.value?.namespaceLabel ?? "Database");
const needsPassword = computed(
  () => Boolean(entry.value && entry.value.driver !== "sqlite" && !entry.value.savePassword),
);
const queryTabsKey = computed(() => `recon.queryTabs.${props.connectionId}`);

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
  return `${connection.user}@${connection.host}:${connection.port}`;
});

function nextQueryKey() {
  return `${props.connectionId}:${Date.now().toString(36)}${Math.random().toString(36).slice(2, 6)}`;
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
  if (!saved.length) {
    saved = [{ key: nextQueryKey(), title: "Query 1" }];
  }
  tabs.value = saved.map((item) => ({
    id: `query:${item.key}`,
    kind: "query",
    key: item.key,
    title: item.title,
  }));
  activeTabId.value = tabs.value[0]?.id ?? "";
  saveQueryTabs();
}

function openQueryTab(initialSql?: string) {
  const key = nextQueryKey();
  const tab: PaneTab = { id: `query:${key}`, kind: "query", key, title: queryTitle() };
  tabs.value = [...tabs.value, tab];
  activeTabId.value = tab.id;
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
  activeTabId.value = id;
}

function closeTab(id: string) {
  const index = tabs.value.findIndex((tab) => tab.id === id);
  if (index === -1) {
    return;
  }
  const tab = tabs.value[index];
  tabs.value = tabs.value.filter((item) => item.id !== id);
  if (tab.kind === "query") {
    localStorage.removeItem(`recon.query.${tab.key}`);
    editors.delete(id);
    saveQueryTabs();
  }
  if (activeTabId.value === id) {
    activeTabId.value = tabs.value[Math.min(index, tabs.value.length - 1)]?.id ?? "";
  }
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
    const columns = await api.schemaColumns(props.connectionId, namespace.value);
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
    tables.value = await api.listTables(props.connectionId, namespace.value);
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
    const info = await api.connect(props.connectionId, withPassword);
    session.value = info;
    namespaces.value = info.namespaces.items;
    namespace.value = info.namespaces.current;
    password.value = "";
    status.value = "connected";
    if (!tabs.value.length) {
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
    await api.setDatabase(props.connectionId, next);
    await loadTables();
  } catch (err) {
    namespace.value = previous;
    showToast(String(err), "error");
  }
}

async function refreshNamespaces() {
  try {
    const list = await api.listDatabases(props.connectionId);
    namespaces.value = list.items;
    if (list.current) {
      namespace.value = list.current;
    }
  } catch (err) {
    showToast(String(err), "error");
  }
}

async function refreshAll() {
  await refreshNamespaces();
  await loadTables();
}

async function reconnect() {
  await api.disconnect(props.connectionId).catch(() => undefined);
  await connect(null);
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
  const active = tabs.value.find((tab) => tab.id === activeTabId.value);
  const sql = `SELECT * FROM ${name} LIMIT 100;`;
  if (active?.kind === "query") {
    editors.get(active.id)?.insertText(sql);
  } else {
    openQueryTab(sql);
  }
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
    width = Math.min(Math.max(startWidth + move.clientX - startX, SIDEBAR_MIN), SIDEBAR_MAX);
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

onMounted(() => {
  if (needsPassword.value) {
    status.value = "password";
    void nextTick(() => passwordInput.value?.focus());
    return;
  }
  void connect(null);
});

onUnmounted(() => {
  void api.disconnect(props.connectionId).catch(() => undefined);
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
      <aside class="db-sidebar" :style="{ width: `${sidebarWidth}px` }">
        <div class="db-sidebar-header">
          <div class="db-sidebar-title" :title="session?.serverVersion">
            <DriverIcon v-if="entry" :driver="entry.driver" />
            <span class="db-sidebar-name">{{ entry?.name }}</span>
          </div>
          <p class="muted tiny db-sidebar-version" :title="session?.serverVersion">
            {{ session?.serverVersion }}
          </p>
        </div>
        <label v-if="namespaces.length > 1 || driver !== 'sqlite'" class="db-namespace">
          <span class="muted tiny">{{ namespaceLabel }}</span>
          <select :value="namespace" @change="changeNamespace(($event.target as HTMLSelectElement).value)">
            <option v-if="!namespace" value="" disabled>Choose…</option>
            <option v-for="item in namespaces" :key="item" :value="item">{{ item }}</option>
          </select>
        </label>
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
            :aria-selected="activeTabId === `table:${namespace}.${table.name}`"
            :class="{ active: activeTabId === `table:${namespace}.${table.name}`, view: table.kind === 'view' }"
            :title="table.kind === 'view' ? `${table.name} (view)` : table.name"
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
          </button>
        </div>
        <div class="db-sidebar-footer">
          <span class="muted tiny">{{ tables.length.toLocaleString() }} {{ tables.length === 1 ? "table" : "tables" }}</span>
          <button class="ghost tiny" type="button" title="Reload tables" @click="refreshAll">
            <svg class="button-icon" viewBox="0 0 24 24" aria-hidden="true">
              <path
                d="M16.023 9.348h4.992v-.001M2.985 19.644v-4.992m0 0h4.992m-4.993 0 3.181 3.183a8.25 8.25 0 0 0 13.803-3.7M4.031 9.865a8.25 8.25 0 0 1 13.803-3.7l3.181 3.182m0-4.991v4.99"
              />
            </svg>
          </button>
          <button class="ghost tiny" type="button" title="Reconnect" @click="reconnect">
            Reconnect
          </button>
        </div>
      </aside>
      <div class="db-sidebar-resize" role="separator" aria-orientation="vertical" @pointerdown="startSidebarResize" />

      <section class="db-main">
        <div class="subtab-bar" role="tablist">
          <div
            v-for="tab in tabs"
            :key="tab.id"
            class="subtab"
            :class="{ active: activeTabId === tab.id, query: tab.kind === 'query' }"
            role="tab"
            :aria-selected="activeTabId === tab.id"
            :title="tab.kind === 'table' ? `${tab.namespace}.${tab.table}` : tab.title"
            @click="activeTabId = tab.id"
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
              ×
            </button>
          </div>
          <button class="subtab-add ghost tiny" type="button" title="New query" @click="openQueryTab()">
            <svg class="button-icon" viewBox="0 0 24 24" aria-hidden="true">
              <path d="M12 5v14M5 12h14" />
            </svg>
            Query
          </button>
        </div>
        <div class="subtab-panes">
          <div v-if="!tabs.length" class="db-empty muted">
            <p>Pick a table on the left, or start a new query.</p>
            <button class="primary" type="button" @click="openQueryTab()">New query</button>
          </div>
          <div
            v-for="tab in tabs"
            v-show="activeTabId === tab.id"
            :key="tab.id"
            class="subtab-pane"
          >
            <TableView
              v-if="tab.kind === 'table'"
              :connection-id="connectionId"
              :namespace="tab.namespace"
              :table="tab.table"
              :kind="tab.tableKind"
            />
            <QueryEditor
              v-else
              :ref="(instance) => setEditorRef(tab.id, instance)"
              :connection-id="connectionId"
              :driver="driver"
              :schema="schema"
              :storage-key="tab.key"
              :active="active && activeTabId === tab.id"
              @executed="onExecuted"
            />
          </div>
        </div>
      </section>
    </template>
  </div>
</template>
