<script setup lang="ts">
import { computed, nextTick, onMounted, ref, watch } from "vue";
import { open, save } from "@tauri-apps/plugin-dialog";
import * as api from "../api";
import { DEFAULT_HEADER_COLOR } from "../color";
import { useApp } from "../composables/useApp";
import { useConnectionForm } from "../composables/useConnectionForm";
import { useTabs } from "../composables/useTabs";
import { DRIVER_OPTIONS, type ConnectionEntry, type Driver, type SslMode } from "../types";
import Modal from "./Modal.vue";

const SQLITE_FILTERS = [
  { name: "SQLite database", extensions: ["sqlite", "sqlite3", "db", "db3", "s3db", "sl3"] },
  { name: "All files", extensions: ["*"] },
];

const { groups, saveConnection, showToast } = useApp();
const { openConnection } = useTabs();
const { formState, closeConnectionForm } = useConnectionForm();

const initial = formState.value?.connection ?? null;
const editing = Boolean(initial);

const driver = ref<Driver>(initial?.driver ?? "mysql");
const name = ref(initial?.name ?? "");
const host = ref(initial?.host ?? "127.0.0.1");
const port = ref<number>(initial?.port || 3306);
const user = ref(initial?.user ?? "");
const password = ref("");
const savePassword = ref(initial?.savePassword ?? true);
const database = ref(initial?.database ?? "");
const filePath = ref(initial?.filePath ?? "");
const sslMode = ref<SslMode>(initial?.sslMode ?? "prefer");
const useColor = ref(Boolean(initial?.headerColor));
const headerColor = ref(initial?.headerColor || DEFAULT_HEADER_COLOR);
const groupId = ref<string>(formState.value?.groupId ?? "");
const hasSavedPassword = ref(false);

const testing = ref(false);
const saving = ref(false);
const testResult = ref<{ ok: boolean; text: string } | null>(null);
const formError = ref("");
const nameInput = ref<HTMLInputElement | null>(null);

const isSqlite = computed(() => driver.value === "sqlite");
const title = computed(() => (editing ? "Edit connection" : "New connection"));

const canSubmit = computed(() =>
  isSqlite.value ? Boolean(filePath.value.trim()) : Boolean(host.value.trim() && user.value.trim()),
);

const passwordPlaceholder = computed(() => {
  if (!savePassword.value) {
    return "Asked for each time you connect";
  }
  return hasSavedPassword.value ? "Saved in Keychain" : "Password";
});

watch(driver, (next, previous) => {
  testResult.value = null;
  const previousDefault = DRIVER_OPTIONS.find((option) => option.id === previous)?.defaultPort;
  const nextDefault = DRIVER_OPTIONS.find((option) => option.id === next)?.defaultPort ?? 0;
  if (next !== "sqlite" && (!port.value || port.value === previousDefault)) {
    port.value = nextDefault;
  }
});

watch([host, port, user, password, database, filePath, sslMode], () => {
  testResult.value = null;
});

onMounted(async () => {
  await nextTick();
  nameInput.value?.focus();
  if (initial && initial.driver !== "sqlite" && initial.savePassword) {
    try {
      hasSavedPassword.value = await api.hasSavedPassword(initial.id);
    } catch {
      hasSavedPassword.value = false;
    }
  }
});

function fileName(path: string) {
  return path.split("/").filter(Boolean).pop() ?? path;
}

function buildEntry(): ConnectionEntry {
  return {
    id: initial?.id ?? "",
    name: name.value.trim(),
    driver: driver.value,
    host: isSqlite.value ? "" : host.value.trim(),
    port: isSqlite.value ? 0 : Number(port.value) || 0,
    user: isSqlite.value ? "" : user.value.trim(),
    database: isSqlite.value ? "" : database.value.trim(),
    filePath: isSqlite.value ? filePath.value.trim() : "",
    sslMode: sslMode.value,
    headerColor: useColor.value ? headerColor.value : "",
    savePassword: isSqlite.value ? false : savePassword.value,
  };
}

function passwordArg() {
  if (isSqlite.value) {
    return null;
  }
  return password.value ? password.value : null;
}

async function browseFile() {
  const selected = await open({
    multiple: false,
    directory: false,
    title: "Open SQLite database",
    filters: SQLITE_FILTERS,
  });
  if (typeof selected === "string") {
    filePath.value = selected;
    if (!name.value.trim()) {
      name.value = fileName(selected);
    }
  }
}

async function createFile() {
  const selected = await save({
    title: "Create SQLite database",
    defaultPath: "database.sqlite",
    filters: SQLITE_FILTERS,
  });
  if (!selected) {
    return;
  }
  try {
    await api.createSqliteDatabase(selected);
    filePath.value = selected;
    if (!name.value.trim()) {
      name.value = fileName(selected);
    }
    showToast(`Created ${fileName(selected)}`);
  } catch (err) {
    formError.value = String(err);
  }
}

async function testConnection() {
  if (!canSubmit.value || testing.value) {
    return;
  }
  testing.value = true;
  testResult.value = null;
  formError.value = "";
  try {
    const version = await api.testConnection(buildEntry(), passwordArg());
    testResult.value = { ok: true, text: `Connected · ${version}` };
  } catch (err) {
    testResult.value = { ok: false, text: String(err) };
  } finally {
    testing.value = false;
  }
}

async function submit(connectAfter: boolean) {
  if (!canSubmit.value || saving.value) {
    return;
  }
  saving.value = true;
  formError.value = "";
  try {
    const saved = await saveConnection(groupId.value || null, buildEntry(), passwordArg());
    closeConnectionForm();
    if (connectAfter) {
      openConnection(saved.id);
    }
  } catch (err) {
    formError.value = String(err);
  } finally {
    saving.value = false;
  }
}
</script>

<template>
  <Modal :title="title" medium @close="closeConnectionForm">
    <form class="connection-form" @submit.prevent="submit(false)">
      <div class="driver-picker segmented" role="radiogroup" aria-label="Database type">
        <button
          v-for="option in DRIVER_OPTIONS"
          :key="option.id"
          type="button"
          role="radio"
          :aria-checked="driver === option.id"
          :class="{ active: driver === option.id }"
          @click="driver = option.id"
        >
          {{ option.label }}
        </button>
      </div>

      <div class="form-grid">
        <label class="modal-label span-2">
          <span class="muted tiny">Name</span>
          <input
            ref="nameInput"
            v-model="name"
            type="text"
            spellcheck="false"
            :placeholder="isSqlite ? 'Defaults to the file name' : 'Defaults to user@host'"
          />
        </label>

        <template v-if="isSqlite">
          <label class="modal-label span-2">
            <span class="muted tiny">Database file</span>
            <div class="file-picker">
              <input
                v-model="filePath"
                type="text"
                spellcheck="false"
                placeholder="/path/to/database.sqlite"
              />
              <button class="ghost" type="button" @click="browseFile">Browse…</button>
              <button class="ghost" type="button" @click="createFile">New…</button>
            </div>
          </label>
        </template>

        <template v-else>
          <label class="modal-label">
            <span class="muted tiny">Host</span>
            <input v-model="host" type="text" spellcheck="false" placeholder="127.0.0.1" />
          </label>
          <label class="modal-label port-field">
            <span class="muted tiny">Port</span>
            <input v-model.number="port" type="number" min="1" max="65535" />
          </label>
          <label class="modal-label">
            <span class="muted tiny">User</span>
            <input
              v-model="user"
              type="text"
              spellcheck="false"
              autocomplete="off"
              :placeholder="driver === 'postgres' ? 'postgres' : 'root'"
            />
          </label>
          <label class="modal-label">
            <span class="muted tiny">Password</span>
            <input
              v-model="password"
              type="password"
              autocomplete="new-password"
              :placeholder="passwordPlaceholder"
              :disabled="!savePassword"
            />
          </label>
          <label class="checkbox-row span-2">
            <input v-model="savePassword" type="checkbox" />
            <span>Save password in the macOS Keychain</span>
          </label>
          <label class="modal-label">
            <span class="muted tiny">Database</span>
            <input
              v-model="database"
              type="text"
              spellcheck="false"
              placeholder="Optional"
            />
          </label>
          <label class="modal-label">
            <span class="muted tiny">SSL</span>
            <select v-model="sslMode">
              <option value="prefer">Prefer</option>
              <option value="require">Require</option>
              <option value="disable">Disable</option>
            </select>
          </label>
        </template>

        <label class="modal-label">
          <span class="muted tiny">Group</span>
          <select v-model="groupId">
            <option value="">No group</option>
            <option v-for="group in groups" :key="group.id" :value="group.id">
              {{ group.name }}
            </option>
          </select>
        </label>
        <div class="modal-label">
          <span class="muted tiny">Tab color</span>
          <div class="color-choice">
            <label class="checkbox-row">
              <input v-model="useColor" type="checkbox" />
              <span>{{ useColor ? "Custom" : "Use group color" }}</span>
            </label>
            <label v-if="useColor" class="color-picker">
              <span class="color-picker-swatch" aria-hidden="true">
                <input v-model="headerColor" type="color" />
              </span>
            </label>
          </div>
        </div>
      </div>

      <p
        v-if="testResult"
        class="test-result"
        :class="testResult.ok ? 'good' : 'bad'"
        role="status"
      >
        {{ testResult.text }}
      </p>
      <p v-if="formError" class="settings-error">{{ formError }}</p>
      <button type="submit" hidden />
    </form>

    <template #actions>
      <button
        class="ghost test-button"
        type="button"
        :disabled="!canSubmit || testing"
        @click="testConnection"
      >
        <span v-if="testing" class="spinner" aria-hidden="true" />
        {{ testing ? "Testing…" : "Test connection" }}
      </button>
      <button class="ghost" type="button" @click="closeConnectionForm">Cancel</button>
      <button class="ghost" type="button" :disabled="!canSubmit || saving" @click="submit(false)">
        Save
      </button>
      <button class="primary" type="button" :disabled="!canSubmit || saving" @click="submit(true)">
        {{ editing ? "Save & connect" : "Connect" }}
      </button>
    </template>
  </Modal>
</template>
