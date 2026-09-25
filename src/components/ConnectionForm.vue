<script setup lang="ts">
import { computed, nextTick, onMounted, ref, watch } from "vue";
import { homeDir, join } from "@tauri-apps/api/path";
import { open, save } from "@tauri-apps/plugin-dialog";
import * as api from "../api";
import { DEFAULT_HEADER_COLOR } from "../color";
import { useApp } from "../composables/useApp";
import { useConnectionForm } from "../composables/useConnectionForm";
import { useTabs } from "../composables/useTabs";
import {
  DEFAULT_SSH_PORT,
  DRIVER_OPTIONS,
  defaultSshTunnel,
  type ConnectionEntry,
  type Driver,
  type SshAuth,
  type SslMode,
} from "../types";
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
const groupId = ref<string>(formState.value?.groupId ?? "");
const colorPicked = ref(Boolean(initial?.headerColor));
const headerColor = ref(initial?.headerColor || groupColor());
const hasSavedPassword = ref(false);

const initialSsh = initial?.ssh ?? defaultSshTunnel();
const sshEnabled = ref(initialSsh.enabled);
const sshHost = ref(initialSsh.host);
const sshPort = ref<number>(initialSsh.port || DEFAULT_SSH_PORT);
const sshUser = ref(initialSsh.user);
const sshAuth = ref<SshAuth>(initialSsh.auth);
const sshKeyPath = ref(initialSsh.keyPath);
const sshSecret = ref("");
const hasSavedSshSecret = ref(false);
const sshKeys = ref<string[]>([]);

const keyChoice = computed({
  get: () => (sshKeys.value.includes(sshKeyPath.value) ? sshKeyPath.value : ""),
  set: (value: string) => {
    sshKeyPath.value = value;
  },
});

const testing = ref(false);
const saving = ref(false);
const testResult = ref<{ ok: boolean; text: string } | null>(null);
const formError = ref("");
const nameInput = ref<HTMLInputElement | null>(null);

const isSqlite = computed(() => driver.value === "sqlite");
const title = computed(() => (editing ? "Edit connection" : "New connection"));

const useSsh = computed(() => !isSqlite.value && sshEnabled.value);

const sshReady = computed(
  () =>
    !useSsh.value ||
    Boolean(
      sshHost.value.trim() &&
        sshUser.value.trim() &&
        (sshAuth.value !== "key" || sshKeyPath.value.trim()),
    ),
);

const canSubmit = computed(() =>
  isSqlite.value
    ? Boolean(filePath.value.trim())
    : Boolean(host.value.trim() && user.value.trim() && sshReady.value),
);

const sshSecretLabel = computed(() => (sshAuth.value === "key" ? "Key passphrase" : "SSH password"));

const sshSecretPlaceholder = computed(() => {
  if (hasSavedSshSecret.value) {
    return "Saved in Keychain";
  }
  return sshAuth.value === "key" ? "Leave empty if the key has none" : "Password";
});

const passwordPlaceholder = computed(() => {
  if (!savePassword.value) {
    return "Asked for each time you connect";
  }
  return hasSavedPassword.value ? "Saved in Keychain" : "Password";
});

function groupColor() {
  return groups.value.find((group) => group.id === groupId.value)?.headerColor || DEFAULT_HEADER_COLOR;
}

watch(groupId, () => {
  if (!colorPicked.value) {
    headerColor.value = groupColor();
  }
});

watch(driver, (next, previous) => {
  testResult.value = null;
  const previousDefault = DRIVER_OPTIONS.find((option) => option.id === previous)?.defaultPort;
  const nextDefault = DRIVER_OPTIONS.find((option) => option.id === next)?.defaultPort ?? 0;
  if (next !== "sqlite" && (!port.value || port.value === previousDefault)) {
    port.value = nextDefault;
  }
});

watch(
  [
    host,
    port,
    user,
    password,
    database,
    filePath,
    sslMode,
    sshEnabled,
    sshHost,
    sshPort,
    sshUser,
    sshAuth,
    sshKeyPath,
    sshSecret,
  ],
  () => {
    testResult.value = null;
  },
);

watch(sshAuth, (next) => {
  sshSecret.value = "";
  hasSavedSshSecret.value = hasSavedSshSecret.value && next === initialSsh.auth;
  if (next === "key" && !sshKeyPath.value.trim() && sshKeys.value.length) {
    sshKeyPath.value = sshKeys.value[0];
  }
});

async function loadSshKeys() {
  try {
    sshKeys.value = await api.listSshKeys();
  } catch {
    sshKeys.value = [];
  }
  if (!initialSsh.enabled && sshKeys.value.length) {
    sshAuth.value = "key";
  }
}

onMounted(async () => {
  await nextTick();
  nameInput.value?.focus();
  void loadSshKeys();
  if (initial && initial.driver !== "sqlite" && initial.savePassword) {
    try {
      hasSavedPassword.value = await api.hasSavedPassword(initial.id);
    } catch {
      hasSavedPassword.value = false;
    }
  }
  if (initial && initialSsh.enabled && initialSsh.auth !== "agent") {
    try {
      hasSavedSshSecret.value = await api.hasSavedSshSecret(initial.id);
    } catch {
      hasSavedSshSecret.value = false;
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
    headerColor: colorPicked.value ? headerColor.value : "",
    savePassword: isSqlite.value ? false : savePassword.value,
    ssh: isSqlite.value
      ? defaultSshTunnel()
      : {
          enabled: sshEnabled.value,
          host: sshHost.value.trim(),
          port: Number(sshPort.value) || DEFAULT_SSH_PORT,
          user: sshUser.value.trim(),
          auth: sshAuth.value,
          keyPath: sshAuth.value === "key" ? sshKeyPath.value.trim() : "",
        },
  };
}

function passwordArg() {
  if (isSqlite.value) {
    return null;
  }
  return password.value ? password.value : null;
}

function sshSecretArg() {
  if (!useSsh.value || sshAuth.value === "agent") {
    return null;
  }
  return sshSecret.value ? sshSecret.value : null;
}

async function browseKey() {
  const home = await homeDir().catch(() => "");
  const current = sshKeyPath.value.trim().replace(/^~(?=\/)/, home);
  const selected = await open({
    multiple: false,
    directory: false,
    title: "Choose SSH private key",
    defaultPath: current || (home ? await join(home, ".ssh") : undefined),
  });
  if (typeof selected === "string") {
    sshKeyPath.value = home && selected.startsWith(`${home}/`) ? `~${selected.slice(home.length)}` : selected;
  }
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
    const version = await api.testConnection(buildEntry(), passwordArg(), sshSecretArg());
    const via = useSsh.value ? ` · via SSH ${sshHost.value.trim()}` : "";
    testResult.value = { ok: true, text: `Connected · ${version}${via}` };
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
    const saved = await saveConnection(
      groupId.value || null,
      buildEntry(),
      passwordArg(),
      sshSecretArg(),
    );
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

          <label class="checkbox-row span-2 ssh-toggle">
            <input v-model="sshEnabled" type="checkbox" />
            <span>Connect through an SSH tunnel</span>
          </label>
          <template v-if="sshEnabled">
            <p class="muted tiny span-2 ssh-hint">
              Host and port above are resolved from the SSH server, so 127.0.0.1 means the SSH host itself.
            </p>
            <label class="modal-label">
              <span class="muted tiny">SSH host</span>
              <input v-model="sshHost" type="text" spellcheck="false" placeholder="bastion.example.com" />
            </label>
            <label class="modal-label port-field">
              <span class="muted tiny">SSH port</span>
              <input v-model.number="sshPort" type="number" min="1" max="65535" />
            </label>
            <label class="modal-label">
              <span class="muted tiny">SSH user</span>
              <input v-model="sshUser" type="text" spellcheck="false" autocomplete="off" placeholder="deploy" />
            </label>
            <label class="modal-label">
              <span class="muted tiny">Authentication</span>
              <select v-model="sshAuth">
                <option value="password">Password</option>
                <option value="key">Private key</option>
                <option value="agent">SSH agent</option>
              </select>
            </label>
            <label v-if="sshAuth === 'key'" class="modal-label span-2">
              <span class="muted tiny">Private key</span>
              <div class="file-picker">
                <select v-if="sshKeys.length" v-model="keyChoice">
                  <option v-for="key in sshKeys" :key="key" :value="key">{{ key }}</option>
                  <option value="">Other file…</option>
                </select>
                <input
                  v-if="!sshKeys.length || !keyChoice"
                  v-model="sshKeyPath"
                  type="text"
                  spellcheck="false"
                  placeholder="~/.ssh/id_ed25519"
                />
                <button class="ghost" type="button" @click="browseKey">Browse…</button>
              </div>
            </label>
            <label v-if="sshAuth !== 'agent'" class="modal-label span-2">
              <span class="muted tiny">{{ sshSecretLabel }}</span>
              <input
                v-model="sshSecret"
                type="password"
                autocomplete="new-password"
                :placeholder="sshSecretPlaceholder"
              />
            </label>
            <p v-else class="muted tiny span-2 ssh-hint">
              Uses the keys loaded in your SSH agent (ssh-add). Unknown hosts are added to ~/.ssh/known_hosts.
            </p>
          </template>
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
          <span class="muted tiny">Color</span>
          <div class="color-choice">
            <label class="color-picker">
              <span class="color-picker-swatch" aria-hidden="true">
                <input v-model="headerColor" type="color" @input="colorPicked = true" />
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
