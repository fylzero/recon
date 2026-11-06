<script setup lang="ts">
import { computed, reactive, ref } from "vue";
import * as api from "../api";
import type { Driver, TruncateOptions } from "../types";
import Modal from "./Modal.vue";

const props = defineProps<{
  action: "truncate" | "drop";
  connectionId: string;
  driver: Driver;
  namespace: string;
  tables: string[];
}>();

const emit = defineEmits<{
  close: [];
  done: [tables: string[]];
}>();

const options = reactive<TruncateOptions>({ disableForeignKeys: false, cascade: false, restartIdentity: true });
const running = ref(false);
const error = ref("");

const single = computed(() => props.tables.length === 1);
const verb = computed(() => (props.action === "drop" ? "Drop" : "Truncate"));
const title = computed(() =>
  single.value ? `${verb.value} “${props.tables[0]}”` : `${verb.value} ${props.tables.length} tables`,
);
const warning = computed(() => {
  if (props.action === "truncate") {
    const subject = single.value ? `“${props.tables[0]}”` : `these ${props.tables.length} tables`;
    return `Every row in ${subject} will be permanently deleted`;
  }
  return single.value
    ? `“${props.tables[0]}” and all of its rows will be permanently deleted`
    : `These ${props.tables.length} tables and all of their rows will be permanently deleted`;
});
const it = computed(() => (single.value ? "this one" : "these"));

function close() {
  if (!running.value) {
    emit("close");
  }
}

async function start() {
  if (running.value) {
    return;
  }
  running.value = true;
  error.value = "";
  try {
    if (props.action === "drop") {
      const { disableForeignKeys, cascade } = options;
      await api.dropTables(props.connectionId, props.namespace, props.tables, { disableForeignKeys, cascade });
    } else {
      await api.truncateTables(props.connectionId, props.namespace, props.tables, { ...options });
    }
    emit("done", props.tables);
  } catch (err) {
    error.value = String(err);
  } finally {
    running.value = false;
  }
}
</script>

<template>
  <Modal :title="title" @close="close">
    <div class="transfer-warning" role="alert">
      <strong>{{ warning }}</strong>
      <ul v-if="!single">
        <li v-for="table in tables" :key="table">{{ table }}</li>
      </ul>
      <p v-if="action === 'truncate'">The table structure is kept. This can't be undone.</p>
      <p v-else>This can't be undone.</p>
      <p v-if="driver === 'sqlite' && action === 'truncate'">
        SQLite has no TRUNCATE, so this deletes every row. Delete triggers still run, and so do ON DELETE actions on
        other tables unless foreign key checks are off.
      </p>
      <p v-if="driver === 'sqlite' && action === 'drop'">
        SQLite deletes the rows before dropping, so ON DELETE actions on other tables run unless foreign key checks
        are off.
      </p>
    </div>
    <div class="transfer-options">
      <template v-if="driver === 'postgres'">
        <label class="checkbox-row">
          <input v-model="options.cascade" type="checkbox" :disabled="running" />
          <template v-if="action === 'truncate'">
            Also truncate tables that reference {{ single ? "it" : "them" }} (CASCADE)
          </template>
          <template v-else>Also drop objects that depend on {{ single ? "it" : "them" }} (CASCADE)</template>
        </label>
        <p class="muted tiny table-action-hint">
          <template v-if="action === 'truncate'">
            Postgres can't skip foreign key checks when truncating. This empties every table with a foreign key to
            {{ it }} as well, and any that reference those.
          </template>
          <template v-else>
            Postgres can't skip foreign key checks. This also drops views built on {{ it }} and the foreign keys in
            other tables that point here. Those tables and their rows are kept.
          </template>
        </p>
      </template>
      <template v-else>
        <label class="checkbox-row">
          <input v-model="options.disableForeignKeys" type="checkbox" :disabled="running" />
          Disable foreign key checks
        </label>
        <p class="muted tiny table-action-hint">
          Needed when other tables have foreign keys to {{ it }}.
          <template v-if="action === 'truncate'">Rows that point here are left in place.</template>
          <template v-else>Those foreign keys are left pointing at a table that no longer exists.</template>
        </p>
      </template>
      <p v-if="action === 'drop' && driver !== 'postgres'" class="muted tiny">
        {{ driver === "mysql" ? "MySQL" : "SQLite" }} has no cascading drop, so other tables and views are never
        removed with {{ single ? "it" : "them" }}.
      </p>
      <template v-if="action === 'truncate'">
        <p v-if="driver === 'mysql'" class="muted tiny">MySQL always restarts AUTO_INCREMENT at 1 when truncating.</p>
        <template v-else>
          <label class="checkbox-row">
            <input v-model="options.restartIdentity" type="checkbox" :disabled="running" />
            Restart identity
          </label>
          <p class="muted tiny table-action-hint">
            <template v-if="driver === 'postgres'">
              Resets the sequences behind serial and identity columns so new rows are numbered from 1 again.
            </template>
            <template v-else>
              Resets AUTOINCREMENT counters so new rows are numbered from 1 again. Other integer keys start over once
              the table is empty either way.
            </template>
          </p>
        </template>
      </template>
    </div>
    <p v-if="error" class="settings-error transfer-error">{{ error }}</p>
    <template #actions>
      <button class="ghost" type="button" :disabled="running" @click="close">Cancel</button>
      <button class="danger" type="button" :disabled="running" @click="start">
        <span v-if="running" class="spinner" aria-hidden="true" />
        {{ running ? (action === "drop" ? "Dropping…" : "Truncating…") : verb }}
      </button>
    </template>
  </Modal>
</template>
