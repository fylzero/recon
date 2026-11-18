<script setup lang="ts">
import { confirm } from "@tauri-apps/plugin-dialog";
import { nextTick, onMounted, ref } from "vue";
import * as api from "../api";
import { shortcut } from "../platform";
import { destructiveCommand, formatReply } from "../redis";

const HISTORY_LIMIT = 200;
const OUTPUT_LIMIT = 500;

const props = defineProps<{
  sessionId: string;
  connectionId: string;
  db: number;
}>();

const emit = defineEmits<{
  /** A command ran that may have changed keys. */
  changed: [];
}>();

interface Line {
  id: number;
  db: number;
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
const historyKey = `recon.redisHistory.${props.connectionId}`;
const history = ref<string[]>(loadHistory());
let historyAt = -1;
let draft = "";
let nextId = 1;

/** Read-only commands, so the key list isn't refreshed after them. */
const READS = new Set([
  "GET", "MGET", "STRLEN", "GETRANGE", "EXISTS", "TYPE", "TTL", "PTTL", "KEYS", "SCAN", "DBSIZE", "INFO",
  "HGET", "HMGET", "HGETALL", "HKEYS", "HVALS", "HLEN", "HSCAN", "HEXISTS", "LRANGE", "LLEN", "LINDEX",
  "SMEMBERS", "SCARD", "SISMEMBER", "SSCAN", "ZRANGE", "ZCARD", "ZSCORE", "ZRANK", "ZSCAN", "XRANGE",
  "XREVRANGE", "XLEN", "XINFO", "PING", "ECHO", "TIME", "MEMORY", "OBJECT", "CLIENT", "CONFIG", "SLOWLOG",
  "COMMAND", "ACL", "DUMP", "RANDOMKEY", "LASTSAVE", "ROLE",
]);

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

async function run() {
  const command = input.value.trim();
  if (!command || running.value) return;
  if (["CLEAR", "CLS"].includes(command.toUpperCase())) {
    clear();
    input.value = "";
    return;
  }
  const risky = destructiveCommand(command);
  if (risky) {
    const ok = await confirm(
      risky === "FLUSHALL"
        ? "FLUSHALL deletes every key in every database on this server. This can't be undone."
        : risky === "FLUSHDB"
          ? `FLUSHDB deletes every key in database ${props.db}. This can't be undone.`
          : `Run ${risky}? It can stop the server or change data in ways that can't be undone.`,
      { title: `Run ${risky}?`, kind: "warning", okLabel: `Run ${risky}` },
    );
    if (!ok) return;
  }
  remember(command);
  historyAt = -1;
  input.value = "";
  running.value = true;
  try {
    const result = await api.redisCommand(props.sessionId, command);
    push({
      db: props.db,
      command,
      output: formatReply(result.reply),
      error: result.reply.type === "error",
      durationMs: result.durationMs,
    });
    const name = command.split(/\s+/, 1)[0].toUpperCase();
    if (result.reply.type !== "error" && !READS.has(name)) {
      emit("changed");
    }
  } catch (err) {
    push({ db: props.db, command, output: String(err), error: true, durationMs: null });
  } finally {
    running.value = false;
    void nextTick(() => inputEl.value?.focus());
  }
}

function clear() {
  lines.value = [];
}

function onKeydown(event: KeyboardEvent) {
  if (event.key === "Enter" && !event.shiftKey) {
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
  <section class="redis-console" @click.self="focus">
    <div ref="outputEl" class="redis-console-output" @click.self="focus">
      <p v-if="!lines.length" class="muted tiny redis-console-hint">
        Run any Redis command, such as <code>INFO memory</code> or <code>HGETALL user:1</code>. ↑ and ↓ go
        through history, {{ shortcut("⌘K") }} clears. Quote arguments with spaces: <code>SET greeting "hello world"</code>.
      </p>
      <div v-for="line in lines" :key="line.id" class="redis-console-entry">
        <div class="redis-console-command">
          <span class="redis-console-prompt">db{{ line.db }}&gt;</span>
          <span>{{ line.command }}</span>
          <span v-if="line.durationMs !== null" class="muted tiny redis-console-time">{{ line.durationMs }} ms</span>
        </div>
        <pre class="redis-console-reply" :class="{ error: line.error }">{{ line.output }}</pre>
      </div>
    </div>
    <div class="redis-console-input">
      <span class="redis-console-prompt">db{{ db }}&gt;</span>
      <textarea
        ref="inputEl"
        v-model="input"
        rows="1"
        spellcheck="false"
        autocomplete="off"
        autocapitalize="off"
        placeholder="Type a command and press Return"
        aria-label="Redis command"
        :readonly="running"
        @keydown="onKeydown"
      />
      <span v-if="running" class="spinner" aria-label="Running" />
    </div>
  </section>
</template>
