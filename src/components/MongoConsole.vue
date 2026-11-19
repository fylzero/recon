<script setup lang="ts">
import { confirm } from "@tauri-apps/plugin-dialog";
import { nextTick, onMounted, ref } from "vue";
import * as api from "../api";
import { destructiveMongoCommand } from "../mongo";
import { shortcut } from "../platform";

const HISTORY_LIMIT = 200;
const OUTPUT_LIMIT = 300;

const props = defineProps<{
  sessionId: string;
  connectionId: string;
  db: string;
}>();

const emit = defineEmits<{
  /** A command ran that may have changed documents or collections. */
  changed: [];
  /** `use name` switched databases. */
  use: [db: string];
}>();

interface Line {
  id: number;
  db: string;
  command: string;
  output: string;
  error: boolean;
  durationMs: number | null;
}

const lines = ref<Line[]>([]);
const input = ref("");
const running = ref(false);
const inputEl = ref<HTMLTextAreaElement | null>(null);
const outputEl = ref<HTMLElement | null>(null);
const historyKey = `recon.mongoHistory.${props.connectionId}`;
const history = ref<string[]>(loadHistory());
let historyAt = -1;
let draft = "";
let nextId = 1;

function loadHistory(): string[] {
  try {
    const parsed = JSON.parse(localStorage.getItem(historyKey) ?? "[]");
    return Array.isArray(parsed) ? parsed.filter((item): item is string => typeof item === "string") : [];
  } catch {
    return [];
  }
}

function remember(command: string) {
  const next = [command, ...history.value.filter((item) => item !== command)].slice(0, HISTORY_LIMIT);
  history.value = next;
  try {
    localStorage.setItem(historyKey, JSON.stringify(next));
  } catch {
    /* history is a convenience */
  }
}

function scrollToEnd() {
  void nextTick(() => {
    if (outputEl.value) outputEl.value.scrollTop = outputEl.value.scrollHeight;
  });
}

function push(line: Omit<Line, "id">) {
  lines.value.push({ id: nextId++, ...line });
  if (lines.value.length > OUTPUT_LIMIT) lines.value.splice(0, lines.value.length - OUTPUT_LIMIT);
  scrollToEnd();
}

/** Brackets and quotes still open, so Return adds a line instead of running. */
function unfinished(text: string) {
  let depth = 0;
  let quote = "";
  for (let i = 0; i < text.length; i++) {
    const c = text[i];
    if (quote) {
      if (c === "\\") i++;
      else if (c === quote) quote = "";
    } else if (c === '"' || c === "'") quote = c;
    else if ("{[(".includes(c)) depth++;
    else if ("}])".includes(c)) depth--;
  }
  return depth > 0 || quote !== "";
}

async function run() {
  const command = input.value.trim();
  if (!command || running.value) return;
  if (["clear", "cls"].includes(command.toLowerCase())) {
    clear();
    input.value = "";
    return;
  }
  const risky = destructiveMongoCommand(command, props.db);
  if (risky) {
    const ok = await confirm(risky.message, { title: risky.title, kind: "warning", okLabel: risky.action });
    if (!ok) return;
  }
  remember(command);
  historyAt = -1;
  input.value = "";
  running.value = true;
  const db = props.db;
  try {
    const result = await api.mongoCommand(props.sessionId, db, command);
    push({ db, command, output: result.output, error: result.error, durationMs: result.durationMs });
    if (result.database) emit("use", result.database);
    if (result.changed) emit("changed");
  } catch (err) {
    push({ db, command, output: String(err), error: true, durationMs: null });
  } finally {
    running.value = false;
    void nextTick(() => inputEl.value?.focus());
  }
}

function clear() {
  lines.value = [];
}

function onKeydown(event: KeyboardEvent) {
  if (event.key === "Enter" && !event.shiftKey && !unfinished(input.value)) {
    event.preventDefault();
    void run();
    return;
  }
  if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "k") {
    event.preventDefault();
    clear();
    return;
  }
  const el = inputEl.value;
  const singleLine = !input.value.includes("\n");
  if (event.key === "ArrowUp" && singleLine && history.value.length) {
    event.preventDefault();
    if (historyAt === -1) draft = input.value;
    historyAt = Math.min(historyAt + 1, history.value.length - 1);
    input.value = history.value[historyAt];
  } else if (event.key === "ArrowDown" && singleLine && historyAt >= 0) {
    event.preventDefault();
    historyAt -= 1;
    input.value = historyAt === -1 ? draft : history.value[historyAt];
  } else {
    return;
  }
  void nextTick(() => el?.setSelectionRange(input.value.length, input.value.length));
}

function focus() {
  inputEl.value?.focus();
}

onMounted(focus);

defineExpose({ focus });
</script>

<template>
  <section class="redis-console mongo-console" @click.self="focus">
    <div ref="outputEl" class="redis-console-output" @click.self="focus">
      <div v-if="!lines.length" class="muted tiny redis-console-hint">
        <p>
          Run a command document like <code>{ ping: 1 }</code>, or shell commands such as
          <code>db.users.find({ age: { $gt: 30 } }).sort({ age: -1 }).limit(10)</code>,
          <code>db.orders.aggregate([…])</code>, <code>db.users.updateOne(filter, { $set: … })</code>,
          <code>show collections</code>, and <code>use name</code>.
        </p>
        <p>
          It reads shell syntax like <code>ObjectId("…")</code> and <code>ISODate("…")</code>, but isn't JavaScript.
          ↑ and ↓ go through history, {{ shortcut("⌘K") }} clears, and Return in open brackets adds a line.
        </p>
      </div>
      <div v-for="line in lines" :key="line.id" class="redis-console-entry">
        <div class="redis-console-command">
          <span class="redis-console-prompt">{{ line.db }}&gt;</span>
          <span class="mongo-console-text">{{ line.command }}</span>
          <span v-if="line.durationMs !== null" class="muted tiny redis-console-time">{{ line.durationMs }} ms</span>
        </div>
        <pre class="redis-console-reply" :class="{ error: line.error }">{{ line.output }}</pre>
      </div>
    </div>
    <div class="redis-console-input">
      <span class="redis-console-prompt">{{ db }}&gt;</span>
      <textarea
        ref="inputEl"
        v-model="input"
        rows="1"
        spellcheck="false"
        autocomplete="off"
        autocapitalize="off"
        placeholder="Type a command and press Return"
        aria-label="MongoDB command"
        :readonly="running"
        @keydown="onKeydown"
      />
      <span v-if="running" class="spinner" aria-label="Running" />
    </div>
  </section>
</template>
