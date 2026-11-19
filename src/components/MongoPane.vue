<script setup lang="ts">
import { confirm } from "@tauri-apps/plugin-dialog";
import { computed, nextTick, onMounted, onUnmounted, ref, watch } from "vue";
import * as api from "../api";
import { SIDEBAR_MAX, SIDEBAR_MIN, useApp } from "../composables/useApp";
import { useConnectionForm } from "../composables/useConnectionForm";
import { setLiveTitle } from "../composables/useTabs";
import { formatCount } from "../mongo";
import { formatBytesSize } from "../redis";
import { driverLabel, type MongoCollection, type MongoDatabase } from "../types";
import DriverIcon from "./DriverIcon.vue";
import MongoCollectionView from "./MongoCollectionView.vue";
import MongoConsole from "./MongoConsole.vue";

const props = defineProps<{
  connectionId: string;
  sessionId: string;
  initialNamespace?: string;
  active: boolean;
}>();

const { findConnection, sidebarWidth, previewPreferences, savePreferences, showToast } = useApp();
const { openEditConnection } = useConnectionForm();

const match = computed(() => findConnection(props.connectionId));
const entry = computed(() => match.value?.connection ?? null);
const needsPassword = computed(() =>
  Boolean(entry.value && (entry.value.passwordSource ?? "password") === "password" && !entry.value.savePassword),
);
const subtitle = computed(() => {
  const connection = entry.value;
  if (!connection) return "";
  if (connection.uri) return connection.uri.replace(/\?.*$/, "");
  const address = `${connection.user ? `${connection.user}@` : ""}${connection.host}:${connection.port}`;
  return connection.ssh?.enabled ? `${address} via ${connection.ssh.host}` : address;
});

const status = ref<"connecting" | "password" | "connected" | "error">("connecting");
const connectError = ref("");
const password = ref("");
const passwordInput = ref<HTMLInputElement | null>(null);
const serverVersion = ref("");
const lost = ref(false);
const lostError = ref("");
const reconnecting = ref(false);

const db = ref("");
const databases = ref<MongoDatabase[]>([]);
const view = ref<"collections" | "console">("collections");

const collections = ref<MongoCollection[]>([]);
const collectionsLoading = ref(false);
const collectionsError = ref("");
const search = ref("");
const active = ref<MongoCollection | null>(null);
let collectionsToken = 0;

const searchInput = ref<HTMLInputElement | null>(null);
const consoleRef = ref<InstanceType<typeof MongoConsole> | null>(null);
const collectionView = ref<InstanceType<typeof MongoCollectionView> | null>(null);

const visibleCollections = computed(() => {
  const text = search.value.trim().toLowerCase();
  return text ? collections.value.filter((item) => item.name.toLowerCase().includes(text)) : collections.value;
});

watch(
  () => (status.value === "connected" && entry.value ? `${entry.value.name} · ${db.value}` : ""),
  (title) => setLiveTitle(props.sessionId, title),
  { immediate: true },
);

function looksDisconnected(message: string) {
  return /closed|couldn't reach|connection|broken pipe|refused|reset by peer|timed out|ssh tunnel/i.test(message);
}

function onFailure(message: string) {
  if (looksDisconnected(message)) {
    lost.value = true;
    lostError.value = message;
  }
}

function applyInfo(info: { serverVersion: string; database: string; databases: MongoDatabase[] }, keepDb: boolean) {
  serverVersion.value = info.serverVersion;
  databases.value = info.databases;
  if (!keepDb || !db.value) db.value = info.database;
}

async function connect(withPassword: string | null = null) {
  status.value = "connecting";
  connectError.value = "";
  try {
    const info = await api.mongoConnect(props.connectionId, withPassword, props.sessionId);
    applyInfo(info, false);
    if (props.initialNamespace && !db.value) db.value = props.initialNamespace;
    password.value = "";
    lost.value = false;
    status.value = "connected";
    await loadCollections();
  } catch (err) {
    connectError.value = String(err);
    status.value = needsPassword.value && withPassword === null ? "password" : "error";
    if (status.value === "password") {
      void nextTick(() => passwordInput.value?.focus());
    }
  }
}

async function reconnect() {
  reconnecting.value = true;
  try {
    const info = await api.mongoConnect(props.connectionId, null, props.sessionId);
    applyInfo(info, true);
    lost.value = false;
    lostError.value = "";
    await loadCollections();
    collectionView.value?.refresh();
  } catch (err) {
    lostError.value = String(err);
  } finally {
    reconnecting.value = false;
  }
}

async function loadCollections() {
  const token = ++collectionsToken;
  collectionsLoading.value = true;
  collectionsError.value = "";
  try {
    const items = await api.mongoCollections(props.sessionId, db.value);
    if (token !== collectionsToken) return;
    collections.value = items;
    if (active.value) {
      active.value = items.find((item) => item.name === active.value!.name) ?? null;
    }
  } catch (err) {
    if (token !== collectionsToken) return;
    collectionsError.value = String(err);
    onFailure(collectionsError.value);
  } finally {
    if (token === collectionsToken) collectionsLoading.value = false;
  }
}

async function refreshDatabases() {
  try {
    databases.value = await api.mongoDatabases(props.sessionId, db.value);
  } catch (err) {
    onFailure(String(err));
  }
}

async function confirmLeave() {
  if (!collectionView.value?.hasChanges()) return true;
  return confirm("Discard your unsaved changes to this document?", {
    title: "Unsaved changes",
    kind: "warning",
    okLabel: "Discard",
  });
}

async function selectDb(next: string) {
  if (next === db.value) return;
  if (!(await confirmLeave())) return;
  db.value = next;
  active.value = null;
  search.value = "";
  collections.value = [];
  await loadCollections();
}

async function openCollection(item: MongoCollection) {
  if (active.value?.name === item.name) return;
  if (!(await confirmLeave())) return;
  active.value = item;
}

function databaseLabel(item: MongoDatabase) {
  return item.sizeOnDisk ? `${item.name} · ${formatBytesSize(item.sizeOnDisk)}` : item.name;
}

function onCollectionsChanged() {
  void loadCollections();
}

async function onConsoleChanged() {
  await loadCollections();
  void refreshDatabases();
  collectionView.value?.refresh();
}

async function onConsoleUse(next: string) {
  db.value = next;
  active.value = null;
  await loadCollections();
  await refreshDatabases();
}

/* Layout. */

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
      void savePreferences({ sidebarWidth: width }).catch((err) => showToast(String(err), "error"));
    }
  };
  window.addEventListener("pointermove", onMove);
  window.addEventListener("pointerup", onUp);
  window.addEventListener("pointercancel", onUp);
}

function selectView(next: "collections" | "console") {
  view.value = next;
  if (next === "console") void nextTick(() => consoleRef.value?.focus());
}

function onWindowKeydown(event: KeyboardEvent) {
  if (!props.active || status.value !== "connected" || document.querySelector(".modal-layer")) return;
  const mod = event.metaKey || event.ctrlKey;
  const key = event.key.toLowerCase();
  if (mod && !event.shiftKey && key === "f" && view.value === "collections") {
    event.preventDefault();
    if (active.value) {
      collectionView.value?.focusFilter();
    } else {
      searchInput.value?.focus();
      searchInput.value?.select();
    }
  } else if (mod && !event.shiftKey && key === "r") {
    event.preventDefault();
    void loadCollections();
    collectionView.value?.refresh();
  }
}

onMounted(() => {
  window.addEventListener("keydown", onWindowKeydown);
  void connect(null);
});

onUnmounted(() => {
  window.removeEventListener("keydown", onWindowKeydown);
  setLiveTitle(props.sessionId, "");
  void api.mongoDisconnect(props.sessionId).catch(() => undefined);
});
</script>

<template>
  <div class="connection-pane redis-pane mongo-pane">
    <div v-if="status !== 'connected'" class="connect-state">
      <div class="connect-card">
        <div class="connect-title">
          <DriverIcon driver="mongodb" />
          <strong>{{ entry?.name ?? "Connection" }}</strong>
        </div>
        <p class="muted tiny">{{ entry ? `${driverLabel(entry.driver)} · ${subtitle}` : "" }}</p>
        <template v-if="status === 'connecting'">
          <p class="action-progress"><span class="spinner" aria-hidden="true" /> Connecting…</p>
        </template>
        <form v-else-if="status === 'password'" class="connect-password" @submit.prevent="connect(password)">
          <label class="modal-label">
            <span class="muted tiny">Password{{ entry?.user ? ` for ${entry.user}` : "" }}</span>
            <input ref="passwordInput" v-model="password" type="password" autocomplete="off" placeholder="Password" />
          </label>
          <p v-if="connectError" class="settings-error">{{ connectError }}</p>
          <div class="connect-actions">
            <button class="ghost" type="button" @click="editConnection">Edit connection</button>
            <button class="primary" type="submit">Connect</button>
          </div>
        </form>
        <template v-else>
          <p class="settings-error connect-error">{{ connectError }}</p>
          <div class="connect-actions">
            <button class="ghost" type="button" @click="editConnection">Edit connection</button>
            <button
              v-if="entry && (entry.passwordSource ?? 'password') !== 'none'"
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
          <DriverIcon driver="mongodb" />
          <strong class="db-toolbar-name" :title="entry?.name">{{ entry?.name }}</strong>
          <select
            class="redis-db-select"
            :value="db"
            aria-label="Database"
            @focus="refreshDatabases"
            @change="selectDb(($event.target as HTMLSelectElement).value)"
          >
            <option v-for="item in databases" :key="item.name" :value="item.name">{{ databaseLabel(item) }}</option>
            <option v-if="db && !databases.some((item) => item.name === db)" :value="db">{{ db }}</option>
          </select>
          <span class="muted tiny db-toolbar-version" :title="serverVersion">{{ serverVersion }}</span>
          <div class="redis-toolbar-actions">
            <div class="segmented" role="tablist" aria-label="View">
              <button
                type="button"
                role="tab"
                :aria-selected="view === 'collections'"
                :class="{ active: view === 'collections' }"
                @click="selectView('collections')"
              >
                Collections
              </button>
              <button
                type="button"
                role="tab"
                :aria-selected="view === 'console'"
                :class="{ active: view === 'console' }"
                @click="selectView('console')"
              >
                Console
              </button>
            </div>
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

      <div v-show="view === 'collections'" class="db-body">
        <aside class="db-sidebar redis-sidebar" :style="{ width: `${sidebarWidth}px` }">
          <div class="db-filter">
            <input
              ref="searchInput"
              v-model="search"
              type="search"
              placeholder="Filter collections"
              spellcheck="false"
              aria-label="Filter collections"
            />
          </div>
          <div class="db-table-list" role="listbox" :aria-label="`Collections in ${db}`">
            <p v-if="collectionsLoading && !collections.length" class="muted tiny db-list-hint">
              <span class="spinner" aria-hidden="true" /> Loading collections…
            </p>
            <p v-else-if="collectionsError" class="settings-error db-list-hint">{{ collectionsError }}</p>
            <p v-else-if="!collections.length" class="muted tiny db-list-hint">
              No collections yet. Create one in the Console with <code>db.createCollection("name")</code>, or insert into
              one: <code>db.name.insertOne({ … })</code>.
            </p>
            <p v-else-if="!visibleCollections.length" class="muted tiny db-list-hint">No matching collections.</p>
            <button
              v-for="item in visibleCollections"
              :key="item.name"
              class="db-table"
              type="button"
              role="option"
              :aria-selected="active?.name === item.name"
              :class="{ active: active?.name === item.name }"
              :title="item.name"
              @click="openCollection(item)"
            >
              <span v-if="item.kind !== 'collection'" class="redis-kind-tag mongo-kind-tag" :class="`kind-${item.kind}`">
                {{ item.kind === "view" ? "VIEW" : "TS" }}
              </span>
              <span class="db-table-name">{{ item.name }}</span>
              <span v-if="item.count !== null" class="muted tiny redis-folder-count" :title="`About ${item.count.toLocaleString()} documents`">
                {{ formatCount(item.count) }}
              </span>
            </button>
          </div>
          <footer class="redis-list-footer">
            <span class="muted tiny">
              {{ collections.length.toLocaleString() }} {{ collections.length === 1 ? "collection" : "collections" }}
            </span>
            <button class="ghost tiny" type="button" :disabled="collectionsLoading" @click="loadCollections">Refresh</button>
          </footer>
        </aside>
        <div class="db-sidebar-resize" role="separator" aria-orientation="vertical" @pointerdown="startSidebarResize" />
        <main class="redis-main">
          <MongoCollectionView
            v-if="active"
            ref="collectionView"
            :session-id="sessionId"
            :db="db"
            :collection="active"
            @failed="onFailure"
            @changed="onCollectionsChanged"
          />
          <div v-else class="redis-key-empty">
            <p class="muted">Choose a collection to browse its documents.</p>
          </div>
        </main>
      </div>

      <MongoConsole
        v-show="view === 'console'"
        ref="consoleRef"
        :session-id="sessionId"
        :connection-id="connectionId"
        :db="db"
        @changed="onConsoleChanged"
        @use="onConsoleUse"
      />
    </template>
  </div>
</template>
