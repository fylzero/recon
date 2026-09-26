<script lang="ts">
import { ref } from "vue";

const hideSchema = ref(true);
</script>

<script setup lang="ts">
import { computed, nextTick, onMounted, onUnmounted } from "vue";
import { listen } from "@tauri-apps/api/event";
import { confirm } from "@tauri-apps/plugin-dialog";
import * as api from "../api";
import type { QueryLogEntry } from "../types";

const props = defineProps<{
  connectionId: string;
  connectionName: string;
}>();

const entries = ref<QueryLogEntry[]>([]);
const message = ref("");
const clearing = ref(false);
const paused = ref(false);
const terminal = ref<HTMLDivElement | null>(null);
let stopLog: (() => void) | undefined;
let stopCleared: (() => void) | undefined;
let stickToBottom = true;

const visibleEntries = computed(() =>
  hideSchema.value ? entries.value.filter((entry) => entry.origin !== "schema") : entries.value,
);

function formatStamp(at: number) {
  return new Date(at).toLocaleString(undefined, { hour12: false });
}

function outcomeLabel(entry: QueryLogEntry) {
  if (!entry.success) {
    return "failed";
  }
  if (entry.rows === undefined) {
    return "ok";
  }
  return entry.rows === 1 ? "1 row" : `${entry.rows.toLocaleString()} rows`;
}

function originLabel(entry: QueryLogEntry) {
  if (entry.origin === "editor") {
    return "Query";
  }
  if (entry.origin === "edit") {
    return "Edit";
  }
  return entry.origin === "browse" ? "Browse" : "Schema";
}

function onScroll() {
  const node = terminal.value;
  if (!node) {
    return;
  }
  stickToBottom = node.scrollHeight - node.scrollTop - node.clientHeight < 48;
}

async function scrollIfNeeded() {
  if (!stickToBottom) {
    return;
  }
  await nextTick();
  const node = terminal.value;
  if (node) {
    node.scrollTop = node.scrollHeight;
  }
}

onMounted(async () => {
  void listen<QueryLogEntry>("query-log", (event) => {
    const entry = event.payload;
    if (
      paused.value ||
      entry.connectionId !== props.connectionId ||
      entries.value.some((item) => item.id === entry.id)
    ) {
      return;
    }
    entries.value = [...entries.value, entry];
    void scrollIfNeeded();
  }).then((unlisten) => {
    stopLog = unlisten;
  });
  void listen<string>("query-log-cleared", (event) => {
    if (event.payload === props.connectionId) {
      entries.value = [];
    }
  }).then((unlisten) => {
    stopCleared = unlisten;
  });
  try {
    const [history, isPaused] = await Promise.all([
      api.queryHistory(props.connectionId),
      api.queryHistoryPaused(props.connectionId),
    ]);
    const seen = new Set(history.map((entry) => entry.id));
    entries.value = [...history, ...entries.value.filter((entry) => !seen.has(entry.id))];
    paused.value = isPaused;
    await scrollIfNeeded();
  } catch (err) {
    message.value = String(err);
  }
});

onUnmounted(() => {
  stopLog?.();
  stopCleared?.();
});

async function togglePaused() {
  const next = !paused.value;
  message.value = "";
  try {
    await api.setQueryHistoryPaused(props.connectionId, next);
    paused.value = next;
  } catch (err) {
    message.value = String(err);
  }
}

async function clearLogs() {
  if (!entries.value.length || clearing.value) {
    return;
  }
  const ok = await confirm(
    `Clear the query history for “${props.connectionName}”? This cannot be undone.`,
    {
      title: "Clear history",
      kind: "warning",
      okLabel: "Clear",
      cancelLabel: "Cancel",
    },
  );
  if (!ok) {
    return;
  }
  clearing.value = true;
  message.value = "";
  try {
    await api.clearQueryHistory(props.connectionId);
    entries.value = [];
  } catch (err) {
    message.value = String(err);
  } finally {
    clearing.value = false;
  }
}
</script>

<template>
  <div class="history-pane">
    <div class="history-toolbar">
      <p class="muted tiny history-summary">
        {{
          paused
            ? "Recording paused for this connection."
            : "Every query Recon runs on this connection, including table browsing and schema lookups."
        }}
      </p>
      <div class="history-header-actions">
        <button
          class="history-switch"
          :class="{ on: hideSchema }"
          type="button"
          role="switch"
          :aria-checked="hideSchema"
          title="Hide the queries Recon runs to list databases, tables, and columns"
          @click="hideSchema = !hideSchema"
        >
          <span class="history-switch-track" aria-hidden="true">
            <span class="history-switch-knob" />
          </span>
          Hide schema
        </button>
        <button
          class="ghost tiny"
          type="button"
          :aria-pressed="paused"
          :title="paused ? 'Start recording queries again' : 'Stop recording queries on this connection until you resume'"
          @click="togglePaused"
        >
          <svg v-if="paused" class="button-icon" viewBox="0 0 24 24" aria-hidden="true">
            <path d="M9 7.2v9.6l8.2-4.8Z" stroke-linejoin="round" />
          </svg>
          <svg v-else class="button-icon" viewBox="0 0 24 24" aria-hidden="true">
            <rect x="7" y="6" width="3" height="12" rx="0.75" />
            <rect x="14" y="6" width="3" height="12" rx="0.75" />
          </svg>
          {{ paused ? "Resume" : "Pause" }}
        </button>
        <button
          class="ghost tiny danger"
          type="button"
          :disabled="clearing || !entries.length"
          @click="clearLogs"
        >
          {{ clearing ? "Clearing…" : "Clear" }}
        </button>
      </div>
    </div>
    <p v-if="message" class="settings-error history-message">{{ message }}</p>
    <div ref="terminal" class="history-terminal" @scroll="onScroll">
      <p v-if="paused" class="history-paused-banner">Recording paused</p>
      <p v-if="!entries.length" class="muted tiny history-empty">
        {{ paused ? "Resume to capture queries again." : "No queries yet. Browse a table or run a query to see it here." }}
      </p>
      <p v-else-if="!visibleEntries.length" class="muted tiny history-empty">
        Only schema lookups are in this log. Turn off Hide schema to see them.
      </p>
      <article
        v-for="entry in visibleEntries"
        :key="entry.id"
        class="history-entry"
        :class="{ bad: !entry.success }"
      >
        <header class="history-meta">
          <span class="history-time">{{ formatStamp(entry.at) }}</span>
          <span v-if="entry.database" class="history-cwd" :title="entry.driver">{{ entry.database }}</span>
          <span class="history-origin">{{ originLabel(entry) }}</span>
          <span class="history-status">
            {{ outcomeLabel(entry) }} · {{ entry.durationMs }}ms
          </span>
        </header>
        <pre class="history-command">{{ entry.sql }}</pre>
        <pre v-if="entry.error" class="history-output err">{{ entry.error }}</pre>
      </article>
    </div>
  </div>
</template>
