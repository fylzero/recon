<script setup lang="ts">
import { confirm } from "@tauri-apps/plugin-dialog";
import { useVirtualizer } from "@tanstack/vue-virtual";
import { computed, nextTick, onMounted, onUnmounted, ref, watch } from "vue";
import * as api from "../api";
import { SIDEBAR_MAX, SIDEBAR_MIN, useApp } from "../composables/useApp";
import { useConnectionForm } from "../composables/useConnectionForm";
import { setLiveTitle } from "../composables/useTabs";
import { keyId, keyTree, kindLabel, kindTag, REDIS_KINDS, textBytes, type KeyTreeRow } from "../redis";
import { driverLabel, type RedisBytes, type RedisDatabase, type RedisKeyEntry, type RedisKind } from "../types";
import DriverIcon from "./DriverIcon.vue";
import Modal from "./Modal.vue";
import RedisConsole from "./RedisConsole.vue";
import RedisKeyView from "./RedisKeyView.vue";

const PAGE = 500;
const ROW_HEIGHT = 25;

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

const db = ref(0);
const databases = ref<RedisDatabase[]>([]);
const view = ref<"keys" | "console">("keys");

const search = ref("");
const kindFilter = ref<string>("");
const keys = ref<RedisKeyEntry[]>([]);
const cursor = ref<string | null>(null);
const scanning = ref(false);
const scanError = ref("");
const scanned = ref(false);
let scanToken = 0;

const separatorKey = `recon.redisSeparator.${props.connectionId}`;
const separator = ref(readSetting(separatorKey, ":"));
const treeOpen = ref(new Set<string>());

const activeKey = ref<RedisKeyEntry | null>(null);
const selected = ref(new Set<string>());
let anchorId = "";

const listEl = ref<HTMLElement | null>(null);
const searchInput = ref<HTMLInputElement | null>(null);
const consoleRef = ref<InstanceType<typeof RedisConsole> | null>(null);
const keyView = ref<InstanceType<typeof RedisKeyView> | null>(null);

/** Opens a key, asking first if the open one has unsaved changes. */
async function openKey(next: RedisKeyEntry) {
  const current = activeKey.value;
  if (current && keyId(current.key) !== keyId(next.key) && keyView.value?.hasChanges()) {
    const ok = await confirm(`Discard your unsaved changes to “${current.key.text}”?`, {
      title: "Unsaved changes",
      kind: "warning",
      okLabel: "Discard",
    });
    if (!ok) return false;
  }
  activeKey.value = next;
  return true;
}

function readSetting(key: string, fallback: string) {
  try {
    return localStorage.getItem(key) ?? fallback;
  } catch {
    return fallback;
  }
}

watch(separator, (value) => {
  try {
    localStorage.setItem(separatorKey, value);
  } catch {
    /* a convenience only */
  }
});

watch(
  () => (status.value === "connected" && entry.value ? `${entry.value.name} · db${db.value}` : ""),
  (title) => setLiveTitle(props.sessionId, title),
  { immediate: true },
);

const rows = computed(() => keyTree(keys.value, separator.value, treeOpen.value));
const keyRows = computed(() => rows.value.filter((row): row is Extract<KeyTreeRow, { type: "key" }> => row.type === "key"));
const currentDatabase = computed(() => databases.value.find((item) => item.index === db.value));

const virtualizer = useVirtualizer(
  computed(() => ({
    count: rows.value.length,
    getScrollElement: () => listEl.value,
    estimateSize: () => ROW_HEIGHT,
    overscan: 16,
  })),
);

/** Plain text searches anywhere in the name; text with glob characters is used as typed. */
const pattern = computed(() => {
  const text = search.value.trim();
  if (!text) return "*";
  return /[*?[\]]/.test(text) ? text : `*${text.replace(/\\/g, "\\\\")}*`;
});

function looksDisconnected(message: string) {
  return /closed|connection|broken pipe|refused|reset by peer|didn't answer|timed out/i.test(message);
}

function onFailure(message: string) {
  if (looksDisconnected(message)) {
    lost.value = true;
    lostError.value = message;
  } else {
    showToast(message, "error");
  }
}

async function connect(withPassword: string | null = null) {
  status.value = "connecting";
  connectError.value = "";
  const restore = props.initialNamespace && /^\d+$/.test(props.initialNamespace) ? Number(props.initialNamespace) : null;
  try {
    const info = await api.redisConnect(props.connectionId, withPassword, props.sessionId, restore);
    serverVersion.value = info.serverVersion;
    db.value = info.db;
    databases.value = info.databases;
    password.value = "";
    lost.value = false;
    status.value = "connected";
    await scan(true);
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
    const info = await api.redisConnect(props.connectionId, null, props.sessionId, db.value);
    serverVersion.value = info.serverVersion;
    databases.value = info.databases;
    lost.value = false;
    lostError.value = "";
    await scan(true);
  } catch (err) {
    lostError.value = String(err);
  } finally {
    reconnecting.value = false;
  }
}

async function scan(reset: boolean) {
  const token = ++scanToken;
  scanning.value = true;
  scanError.value = "";
  try {
    const page = await api.redisScan(
      props.sessionId,
      reset ? null : cursor.value,
      pattern.value,
      kindFilter.value || null,
      PAGE,
    );
    if (token !== scanToken) return;
    if (reset) {
      keys.value = page.keys;
      selected.value = new Set();
    } else {
      const seen = new Set(keys.value.map((item) => keyId(item.key)));
      keys.value = [...keys.value, ...page.keys.filter((item) => !seen.has(keyId(item.key)))];
    }
    cursor.value = page.cursor;
    scanned.value = true;
  } catch (err) {
    if (token !== scanToken) return;
    scanError.value = String(err);
    if (looksDisconnected(scanError.value)) onFailure(scanError.value);
  } finally {
    if (token === scanToken) scanning.value = false;
  }
}

let searchTimer: number | undefined;
watch([search, kindFilter], () => {
  window.clearTimeout(searchTimer);
  searchTimer = window.setTimeout(() => void scan(true), 250);
});

async function refreshDatabases() {
  try {
    databases.value = await api.redisDatabases(props.sessionId);
  } catch (err) {
    onFailure(String(err));
  }
}

async function selectDb(next: number) {
  if (next === db.value) return;
  const previous = db.value;
  try {
    await api.redisSelectDb(props.sessionId, next);
    db.value = next;
    activeKey.value = null;
    treeOpen.value = new Set();
    await scan(true);
  } catch (err) {
    db.value = previous;
    onFailure(String(err));
  }
}

function databaseLabel(item: RedisDatabase) {
  return item.keys ? `db${item.index} · ${item.keys.toLocaleString()} keys` : `db${item.index}`;
}

/* Key list: clicks, selection, and keyboard. */

function toggleFolder(id: string) {
  const next = new Set(treeOpen.value);
  if (next.has(id)) next.delete(id);
  else next.add(id);
  treeOpen.value = next;
}

function onRowClick(event: MouseEvent, row: KeyTreeRow) {
  if (row.type === "folder") {
    toggleFolder(row.id);
    return;
  }
  if (event.metaKey || event.ctrlKey) {
    const next = new Set(selected.value);
    if (next.has(row.id)) next.delete(row.id);
    else next.add(row.id);
    selected.value = next;
    anchorId = row.id;
    return;
  }
  if (event.shiftKey && anchorId) {
    const ids = keyRows.value.map((item) => item.id);
    const from = ids.indexOf(anchorId);
    const to = ids.indexOf(row.id);
    if (from >= 0 && to >= 0) {
      selected.value = new Set(ids.slice(Math.min(from, to), Math.max(from, to) + 1));
      return;
    }
  }
  void openKey(row.entry).then((opened) => {
    if (!opened) return;
    anchorId = row.id;
    selected.value = new Set([row.id]);
  });
}

function moveSelection(step: number) {
  const items = keyRows.value;
  if (!items.length) return;
  const current = activeKey.value ? items.findIndex((item) => item.id === keyId(activeKey.value!.key)) : -1;
  const next = items[Math.min(Math.max(current + step, 0), items.length - 1)];
  void openKey(next.entry).then((opened) => {
    if (!opened) return;
    selected.value = new Set([next.id]);
    anchorId = next.id;
    const index = rows.value.findIndex((row) => row.id === next.id);
    if (index >= 0) virtualizer.value.scrollToIndex(index, { align: "auto" });
  });
}

function onListKeydown(event: KeyboardEvent) {
  if (event.key === "ArrowDown" || event.key === "ArrowUp") {
    event.preventDefault();
    moveSelection(event.key === "ArrowDown" ? 1 : -1);
  } else if ((event.key === "Delete" || event.key === "Backspace") && selected.value.size) {
    event.preventDefault();
    void deleteSelected();
  } else if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "a") {
    event.preventDefault();
    selected.value = new Set(keyRows.value.map((row) => row.id));
  }
}

const selectedEntries = computed(() => keys.value.filter((item) => selected.value.has(keyId(item.key))));

async function deleteSelected() {
  const targets = selectedEntries.value;
  if (!targets.length) return;
  const message =
    targets.length === 1
      ? `Delete the key “${targets[0].key.text}”? This can't be undone.`
      : `Delete ${targets.length.toLocaleString()} keys? This can't be undone.`;
  const ok = await confirm(message, { title: "Delete keys", kind: "warning", okLabel: "Delete" });
  if (!ok) return;
  try {
    await api.redisEdit(props.sessionId, { op: "delete", keys: targets.map((item) => item.key) });
    removeKeys(targets.map((item) => item.key));
    showToast(targets.length === 1 ? "Deleted the key." : `Deleted ${targets.length.toLocaleString()} keys.`);
    void refreshDatabases();
  } catch (err) {
    onFailure(String(err));
  }
}

function removeKeys(removed: RedisBytes[]) {
  const ids = new Set(removed.map(keyId));
  keys.value = keys.value.filter((item) => !ids.has(keyId(item.key)));
  selected.value = new Set([...selected.value].filter((id) => !ids.has(id)));
  if (activeKey.value && ids.has(keyId(activeKey.value.key))) {
    activeKey.value = null;
  }
}

function onKeyDeleted(key: RedisBytes) {
  removeKeys([key]);
  void refreshDatabases();
}

function onKeyRenamed(from: RedisBytes, to: RedisBytes) {
  const index = keys.value.findIndex((item) => keyId(item.key) === keyId(from));
  const kind = activeKey.value?.kind ?? keys.value[index]?.kind ?? "string";
  const renamed: RedisKeyEntry = { key: to, kind };
  const next = keys.value.filter((item) => keyId(item.key) !== keyId(to));
  const at = next.findIndex((item) => keyId(item.key) === keyId(from));
  if (at >= 0) next.splice(at, 1, renamed);
  else next.push(renamed);
  keys.value = next;
  activeKey.value = renamed;
  selected.value = new Set([keyId(to)]);
}

function onConsoleChanged() {
  void scan(true);
  void refreshDatabases();
}

/* New key. */

const newKey = ref<{
  name: string;
  kind: RedisKind;
  field: string;
  value: string;
  score: string;
  ttl: string;
  error: string;
  busy: boolean;
} | null>(null);

function openNewKey() {
  const prefix = activeKey.value && separator.value ? activeKey.value.key.text.split(separator.value).slice(0, -1).join(separator.value) : "";
  newKey.value = {
    name: prefix ? `${prefix}${separator.value}` : "",
    kind: "string",
    field: "",
    value: "",
    score: "0",
    ttl: "",
    error: "",
    busy: false,
  };
  void nextTick(() => document.querySelector<HTMLInputElement>(".redis-new-key input[type='text']")?.focus());
}

async function submitNewKey() {
  const form = newKey.value;
  if (!form) return;
  if (!form.name) {
    form.error = "A key needs a name.";
    return;
  }
  const needsField = form.kind === "hash" || form.kind === "stream";
  if (needsField && !form.field) {
    form.error = form.kind === "hash" ? "A hash needs a first field." : "A stream entry needs a field.";
    return;
  }
  const score = Number(form.score);
  if (form.kind === "zset" && (form.score.trim() === "" || !Number.isFinite(score))) {
    form.error = "A score must be a number.";
    return;
  }
  const ttl = form.ttl.trim() ? Number(form.ttl) : null;
  if (ttl !== null && (!Number.isInteger(ttl) || ttl < 1)) {
    form.error = "The TTL is a whole number of seconds, 1 or more.";
    return;
  }
  form.busy = true;
  form.error = "";
  try {
    const key = textBytes(form.name);
    await api.redisCreateKey(props.sessionId, {
      key,
      kind: form.kind,
      field: needsField ? textBytes(form.field) : null,
      value: textBytes(form.value),
      score: form.kind === "zset" ? score : null,
      ttlSeconds: ttl,
    });
    newKey.value = null;
    const created: RedisKeyEntry = { key, kind: form.kind };
    keys.value = [...keys.value.filter((item) => keyId(item.key) !== keyId(key)), created];
    activeKey.value = created;
    selected.value = new Set([keyId(key)]);
    view.value = "keys";
    void refreshDatabases();
  } catch (err) {
    form.error = String(err);
  } finally {
    if (newKey.value) newKey.value.busy = false;
  }
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

function selectView(next: "keys" | "console") {
  view.value = next;
  if (next === "console") void nextTick(() => consoleRef.value?.focus());
}

function onWindowKeydown(event: KeyboardEvent) {
  if (!props.active || status.value !== "connected" || newKey.value) return;
  const mod = event.metaKey || event.ctrlKey;
  if (mod && !event.shiftKey && event.key.toLowerCase() === "f" && view.value === "keys") {
    event.preventDefault();
    searchInput.value?.focus();
    searchInput.value?.select();
  } else if (mod && !event.shiftKey && event.key.toLowerCase() === "r") {
    event.preventDefault();
    void scan(true);
  }
}

onMounted(() => {
  window.addEventListener("keydown", onWindowKeydown);
  void connect(null);
});

onUnmounted(() => {
  window.removeEventListener("keydown", onWindowKeydown);
  window.clearTimeout(searchTimer);
  setLiveTitle(props.sessionId, "");
  void api.redisDisconnect(props.sessionId).catch(() => undefined);
});
</script>

<template>
  <div class="connection-pane redis-pane">
    <div v-if="status !== 'connected'" class="connect-state">
      <div class="connect-card">
        <div class="connect-title">
          <DriverIcon driver="redis" />
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
          <DriverIcon driver="redis" />
          <strong class="db-toolbar-name" :title="entry?.name">{{ entry?.name }}</strong>
          <select
            class="redis-db-select"
            :value="db"
            aria-label="Database"
            @focus="refreshDatabases"
            @change="selectDb(Number(($event.target as HTMLSelectElement).value))"
          >
            <option v-for="item in databases" :key="item.index" :value="item.index">{{ databaseLabel(item) }}</option>
            <option v-if="!databases.some((item) => item.index === db)" :value="db">db{{ db }}</option>
          </select>
          <span class="muted tiny db-toolbar-version" :title="serverVersion">{{ serverVersion }}</span>
          <span class="muted tiny db-toolbar-driver">Redis</span>
          <div class="redis-toolbar-actions">
            <div class="segmented" role="tablist" aria-label="View">
              <button
                type="button"
                role="tab"
                :aria-selected="view === 'keys'"
                :class="{ active: view === 'keys' }"
                @click="selectView('keys')"
              >
                Keys
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
            <button class="ghost tiny redis-new-key-button" type="button" @click="openNewKey">
              <svg viewBox="0 0 24 24" class="button-icon" aria-hidden="true"><path d="M12 4.5v15m7.5-7.5h-15" /></svg>
              New key
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

      <div v-show="view === 'keys'" class="db-body">
        <aside class="db-sidebar redis-sidebar" :style="{ width: `${sidebarWidth}px` }">
          <div class="db-filter">
            <input
              ref="searchInput"
              v-model="search"
              type="search"
              placeholder="Search keys, or a pattern like user:*"
              spellcheck="false"
              aria-label="Search keys"
            />
          </div>
          <div class="redis-list-options">
            <select v-model="kindFilter" aria-label="Key type">
              <option value="">All types</option>
              <option v-for="kind in REDIS_KINDS" :key="kind" :value="kind">{{ kindLabel(kind) }}</option>
            </select>
            <label class="redis-separator" title="Group keys into folders at this character. Leave empty for a flat list.">
              <span class="muted tiny">Group by</span>
              <input v-model="separator" type="text" maxlength="4" spellcheck="false" placeholder="none" />
            </label>
          </div>
          <div
            ref="listEl"
            class="db-table-list redis-key-list"
            role="listbox"
            tabindex="0"
            aria-multiselectable="true"
            :aria-label="`Keys in db${db}`"
            @keydown="onListKeydown"
          >
            <p v-if="scanning && !keys.length" class="muted tiny db-list-hint">
              <span class="spinner" aria-hidden="true" /> Scanning keys…
            </p>
            <p v-else-if="scanError" class="settings-error db-list-hint">{{ scanError }}</p>
            <p v-else-if="scanned && !keys.length" class="muted tiny db-list-hint">
              {{ cursor ? "No matches yet. Keep scanning to look further." : search || kindFilter ? "No matching keys." : "This database is empty." }}
            </p>
            <div v-if="rows.length" class="redis-key-rows" :style="{ height: `${virtualizer.getTotalSize()}px` }">
              <button
                v-for="item in virtualizer.getVirtualItems()"
                :key="rows[item.index].id"
                class="db-table redis-key-row"
                type="button"
                role="option"
                :aria-selected="selected.has(rows[item.index].id)"
                :aria-expanded="rows[item.index].type === 'folder' ? (rows[item.index] as { open: boolean }).open : undefined"
                :class="{
                  folder: rows[item.index].type === 'folder',
                  selected: selected.has(rows[item.index].id),
                  active: activeKey !== null && keyId(activeKey.key) === rows[item.index].id,
                }"
                :style="{
                  transform: `translateY(${item.start}px)`,
                  height: `${ROW_HEIGHT}px`,
                  paddingLeft: `${0.45 + rows[item.index].depth * 0.9}rem`,
                }"
                :title="rows[item.index].type === 'key' ? (rows[item.index] as { entry: RedisKeyEntry }).entry.key.text : undefined"
                tabindex="-1"
                @click="onRowClick($event, rows[item.index])"
              >
                <template v-if="rows[item.index].type === 'folder'">
                  <svg class="redis-chevron" :class="{ open: (rows[item.index] as { open: boolean }).open }" viewBox="0 0 16 16" aria-hidden="true">
                    <path d="m6 4 4 4-4 4" />
                  </svg>
                  <span class="db-table-name">{{ rows[item.index].name }}</span>
                  <span class="muted tiny redis-folder-count">{{ (rows[item.index] as { count: number }).count }}</span>
                </template>
                <template v-else>
                  <span
                    class="redis-kind-tag"
                    :class="`kind-${(rows[item.index] as { entry: RedisKeyEntry }).entry.kind}`"
                  >
                    {{ kindTag((rows[item.index] as { entry: RedisKeyEntry }).entry.kind) }}
                  </span>
                  <span class="db-table-name">{{ rows[item.index].name }}</span>
                </template>
              </button>
            </div>
          </div>
          <footer class="redis-list-footer">
            <span class="muted tiny">
              {{ keys.length.toLocaleString() }}
              {{ cursor ? "loaded" : keys.length === 1 ? "key" : "keys" }}<template
                v-if="currentDatabase && cursor"
              > of ~{{ currentDatabase.keys.toLocaleString() }}</template>
            </span>
            <button v-if="selected.size > 1" class="ghost tiny danger" type="button" @click="deleteSelected">
              Delete {{ selected.size.toLocaleString() }}
            </button>
            <button v-if="cursor" class="ghost tiny" type="button" :disabled="scanning" @click="scan(false)">
              <span v-if="scanning" class="spinner" aria-hidden="true" />
              Load more
            </button>
            <button v-else class="ghost tiny" type="button" title="Scan again" :disabled="scanning" @click="scan(true)">
              Refresh
            </button>
          </footer>
        </aside>
        <div class="db-sidebar-resize" role="separator" aria-orientation="vertical" @pointerdown="startSidebarResize" />
        <main class="redis-main">
          <RedisKeyView
            v-if="activeKey"
            ref="keyView"
            :session-id="sessionId"
            :entry="activeKey"
            @deleted="onKeyDeleted"
            @renamed="onKeyRenamed"
            @failed="(message) => looksDisconnected(message) && onFailure(message)"
          />
          <div v-else class="redis-key-empty">
            <p class="muted">Choose a key to see its value, or create one with New key.</p>
          </div>
        </main>
      </div>

      <RedisConsole
        v-show="view === 'console'"
        ref="consoleRef"
        :session-id="sessionId"
        :connection-id="connectionId"
        :db="db"
        @changed="onConsoleChanged"
      />
    </template>

    <Modal v-if="newKey" title="New key" medium @close="newKey = null">
      <form class="redis-dialog redis-new-key" @submit.prevent="submitNewKey">
        <div class="form-grid">
          <label class="modal-label span-2">
            <span class="muted tiny">Name</span>
            <input v-model="newKey.name" type="text" spellcheck="false" placeholder="user:42" />
          </label>
          <label class="modal-label">
            <span class="muted tiny">Type</span>
            <select v-model="newKey.kind">
              <option v-for="kind in REDIS_KINDS" :key="kind" :value="kind">{{ kindLabel(kind) }}</option>
            </select>
          </label>
          <label class="modal-label">
            <span class="muted tiny">TTL in seconds</span>
            <input v-model="newKey.ttl" type="text" inputmode="numeric" placeholder="No expiry" />
          </label>
          <label v-if="newKey.kind === 'hash' || newKey.kind === 'stream'" class="modal-label span-2">
            <span class="muted tiny">{{ newKey.kind === "hash" ? "First field" : "Entry field" }}</span>
            <input v-model="newKey.field" type="text" spellcheck="false" />
          </label>
          <label v-if="newKey.kind === 'zset'" class="modal-label span-2">
            <span class="muted tiny">Score</span>
            <input v-model="newKey.score" type="text" inputmode="decimal" />
          </label>
          <label class="modal-label span-2">
            <span class="muted tiny">
              {{ newKey.kind === "set" || newKey.kind === "zset" ? "First member" : newKey.kind === "list" ? "First item" : "Value" }}
            </span>
            <textarea v-model="newKey.value" class="redis-value-input" rows="6" spellcheck="false" />
          </label>
        </div>
        <p v-if="newKey.error" class="settings-error">{{ newKey.error }}</p>
        <div class="modal-actions">
          <button class="ghost" type="button" @click="newKey = null">Cancel</button>
          <button class="primary" type="submit" :disabled="newKey.busy">Create</button>
        </div>
      </form>
    </Modal>
  </div>
</template>
