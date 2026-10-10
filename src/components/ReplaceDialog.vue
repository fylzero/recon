<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, shallowRef, watch } from "vue";
import * as api from "../api";
import type { WireNode } from "../filters/compile";
import type { ReplaceMode, ReplacePreview, ReplaceRequest } from "../types";
import Modal from "./Modal.vue";

const props = defineProps<{
  connectionId: string;
  namespace: string;
  table: string;
  column: string;
  /** Enums only take whole values. */
  wholeOnly: boolean;
  filter: WireNode | null;
  filterSummary: string;
}>();

const emit = defineEmits<{
  close: [];
  done: [count: number];
  openSql: [sql: string];
}>();

/** Typing waits this long before the preview reloads. */
const PREVIEW_DELAY = 300;

const find = ref("");
const replacement = ref("");
const mode = ref<ReplaceMode>(props.wholeOnly ? "whole" : "contains");
const onlyFiltered = ref(props.filter !== null);
const preview = shallowRef<ReplacePreview | null>(null);
const previewing = ref(false);
const previewError = ref("");
const running = ref(false);
const error = ref("");
const findInput = ref<HTMLInputElement | null>(null);
let previewTimer = 0;
let previewId = 0;

const unchanged = computed(() => !find.value || find.value === replacement.value);
const request = computed<ReplaceRequest>(() => ({
  namespace: props.namespace,
  table: props.table,
  column: props.column,
  find: find.value,
  replace: replacement.value,
  mode: mode.value,
  filter: onlyFiltered.value ? props.filter : null,
  utcOffset: -new Date().getTimezoneOffset(),
}));
const count = computed(() => preview.value?.count ?? 0);
const rowsLabel = computed(() => `${count.value.toLocaleString()} ${count.value === 1 ? "row" : "rows"}`);
const ready = computed(() => !unchanged.value && !previewing.value && !previewError.value && count.value > 0);

/** Backend filter errors arrive as JSON naming the condition that caused them. */
function messageOf(err: unknown) {
  const message = String(err);
  if (!message.startsWith('{"filterError"')) {
    return message;
  }
  try {
    return (JSON.parse(message) as { filterError?: { message?: string } }).filterError?.message ?? message;
  } catch {
    return message;
  }
}

async function loadPreview() {
  const id = ++previewId;
  if (unchanged.value) {
    preview.value = null;
    previewing.value = false;
    previewError.value = "";
    return;
  }
  previewing.value = true;
  try {
    const next = await api.previewReplace(props.connectionId, request.value);
    if (id === previewId) {
      preview.value = next;
      previewError.value = "";
    }
  } catch (err) {
    if (id === previewId) {
      preview.value = null;
      previewError.value = messageOf(err);
    }
  } finally {
    if (id === previewId) {
      previewing.value = false;
    }
  }
}

watch(request, () => {
  error.value = "";
  window.clearTimeout(previewTimer);
  if (unchanged.value) {
    void loadPreview();
    return;
  }
  previewing.value = true;
  previewTimer = window.setTimeout(() => void loadPreview(), PREVIEW_DELAY);
});

onMounted(() => {
  void nextTick(() => findInput.value?.focus());
});

onBeforeUnmount(() => window.clearTimeout(previewTimer));

function close() {
  if (!running.value) {
    emit("close");
  }
}

async function start() {
  if (!ready.value || running.value) {
    return;
  }
  running.value = true;
  error.value = "";
  try {
    emit("done", await api.replaceValues(props.connectionId, request.value));
  } catch (err) {
    error.value = messageOf(err);
  } finally {
    running.value = false;
  }
}

function openSql() {
  if (preview.value?.sql) {
    emit("openSql", preview.value.sql);
  }
}
</script>

<template>
  <Modal :title="`Find and replace in “${column}”`" medium @close="close">
    <form class="replace-form" @submit.prevent="start">
      <label class="modal-label">
        <span class="muted tiny">Find</span>
        <input ref="findInput" v-model="find" type="text" spellcheck="false" :disabled="running" />
      </label>
      <label class="modal-label">
        <span class="muted tiny">Replace with</span>
        <input v-model="replacement" type="text" spellcheck="false" :disabled="running" />
      </label>
      <button type="submit" hidden />
    </form>
    <div class="transfer-options">
      <label class="checkbox-row" :title="wholeOnly ? 'Enum columns can only take whole values.' : undefined">
        <input v-model="mode" type="radio" value="contains" :disabled="running || wholeOnly" />
        Replace the text wherever it appears in a value
      </label>
      <label class="checkbox-row">
        <input v-model="mode" type="radio" value="whole" :disabled="running" />
        Replace only values that are exactly this text
      </label>
      <p class="muted tiny">Matching is case-sensitive{{ mode === "whole" ? " and includes spaces" : "" }}.</p>
    </div>
    <div v-if="filter" class="transfer-options">
      <label class="checkbox-row">
        <input v-model="onlyFiltered" type="radio" :value="true" :disabled="running" />
        Only rows matching the current filter
      </label>
      <p class="muted tiny table-action-hint replace-filter">{{ filterSummary }}</p>
      <label class="checkbox-row">
        <input v-model="onlyFiltered" type="radio" :value="false" :disabled="running" />
        Every row in “{{ table }}”
      </label>
    </div>
    <div v-if="!unchanged" class="replace-preview" aria-live="polite">
      <p v-if="previewError" class="settings-error transfer-error">{{ previewError }}</p>
      <p v-else-if="previewing && !preview" class="action-progress">
        <span class="spinner" aria-hidden="true" /> Counting matching rows…
      </p>
      <template v-else-if="preview">
        <p class="tiny" :class="{ muted: previewing }">
          <template v-if="count">
            {{ rowsLabel }} will change{{ onlyFiltered || !filter ? "" : " across the whole table" }}. This can't be
            undone.
          </template>
          <template v-else>No rows would change.</template>
        </p>
        <table v-if="preview.samples.length" class="replace-samples" :class="{ muted: previewing }">
          <thead>
            <tr>
              <th>Before</th>
              <th>After</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="(sample, index) in preview.samples" :key="index">
              <td>{{ sample.before }}</td>
              <td>{{ sample.after }}</td>
            </tr>
          </tbody>
        </table>
        <p v-if="count > preview.samples.length" class="muted tiny">
          Showing {{ preview.samples.length }} of {{ rowsLabel }}.
        </p>
      </template>
    </div>
    <p v-if="error" class="settings-error transfer-error">{{ error }}</p>
    <template #actions>
      <button class="ghost" type="button" :disabled="running" @click="close">Cancel</button>
      <button class="ghost" type="button" :disabled="running || !preview?.sql" @click="openSql">Open in SQL tab</button>
      <button class="danger" type="button" :disabled="running || !ready" @click="start">
        <span v-if="running" class="spinner" aria-hidden="true" />
        {{ running ? "Replacing…" : ready ? `Replace in ${rowsLabel}` : "Replace" }}
      </button>
    </template>
  </Modal>
</template>
