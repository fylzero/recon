<script setup lang="ts">
import { computed, nextTick, onMounted, ref, watch } from "vue";
import { homeDir, join } from "@tauri-apps/api/path";
import { open, save } from "@tauri-apps/plugin-dialog";
import * as api from "../api";
import { DEFAULT_HEADER_COLOR } from "../color";
import { useApp } from "../composables/useApp";
import { useConnectionForm } from "../composables/useConnectionForm";
import { useTabs } from "../composables/useTabs";
import { baseName, passwordCommandShell, secretStoreFullName, secretStoreName } from "../platform";
import {
  DEFAULT_SSH_PORT,
  DRIVER_OPTIONS,
  defaultSshTunnel,
  type ConnectionEntry,
  type Driver,
  type PasswordSource,
  type ResolvedSshHost,
  type SshAuth,
  type SslMode,
} from "../types";
import AwsProfileField from "./AwsProfileField.vue";
import DriverIcon from "./DriverIcon.vue";
import Modal from "./Modal.vue";

const SQLITE_FILTERS = [
  { name: "SQLite database", extensions: ["sqlite", "sqlite3", "db", "db3", "s3db", "sl3"] },
  { name: "All files", extensions: ["*"] },
];

const CERT_FILTERS = [
  { name: "Certificate or key", extensions: ["pem", "crt", "cer", "key"] },
  { name: "All files", extensions: ["*"] },
];

const RDS_BUNDLE_URL = "https://truststore.pki.rds.amazonaws.com/global/global-bundle.pem";
const RDS_HOST_REGION = /\.([a-z]{2}(?:-[a-z]+)+-\d+)\.rds\.amazonaws\.com$/i;

const { saveConnection, showToast } = useApp();
const { openConnection } = useTabs();
const { formState, closeConnectionForm } = useConnectionForm();

const initial = formState.value?.connection ?? null;
const editing = Boolean(initial?.id);

function defaultUser(id: Driver) {
  if (id === "postgres") return "postgres";
  if (id === "mssql") return "sa";
  if (id === "mysql") return "root";
  return "";
}

const driver = ref<Driver>(initial?.driver ?? "mysql");
const name = ref(initial?.name ?? "");
const host = ref(initial?.host ?? "127.0.0.1");
const port = ref<number>(initial?.port || 3306);
const user = ref(initial?.user ?? defaultUser(driver.value));
const password = ref("");
const savePassword = ref(initial?.savePassword ?? true);
const database = ref(initial?.database ?? "");
const filePath = ref(initial?.filePath ?? "");
const sslMode = ref<SslMode>(initial?.sslMode ?? (initial?.driver === "redis" ? "disable" : "prefer"));
const cleartextAuth = ref(initial?.cleartextAuth ?? false);
const passwordSource = ref<PasswordSource>(initial?.passwordSource ?? "password");
const awsRegion = ref(initial?.awsRegion ?? "");
const awsProfile = ref(initial?.awsProfile ?? "");
const passwordCommand = ref(initial?.passwordCommand ?? "");
const sslCaPath = ref(initial?.sslCaPath ?? "");
const sslCertPath = ref(initial?.sslCertPath ?? "");
const sslKeyPath = ref(initial?.sslKeyPath ?? "");
const useClientCert = ref(Boolean(initial?.sslCertPath));
// Set when the form opened from a group, or editing a grouped connection. Connections move
// between groups by dragging on the connections page, so the form only keeps this.
const groupId = formState.value?.groupId ?? "";
const colorPicked = ref(Boolean(initial?.headerColor));
const headerColor = ref(initial?.headerColor || DEFAULT_HEADER_COLOR);
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
const sshAlsoPassword = ref(initialSsh.alsoPassword ?? false);
const sshPassword = ref("");
const hasSavedSshPassword = ref(false);
const sshKeys = ref<string[]>([]);
const sshHosts = ref<string[]>([]);
const awsProfiles = ref<string[]>([]);
const awsProfilesLoaded = ref(false);
const sshResolved = ref<ResolvedSshHost | null>(null);
let resolveToken = 0;

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
const testResultEl = ref<HTMLElement | null>(null);
const formErrorEl = ref<HTMLElement | null>(null);

watch(
  [testResult, formError],
  ([result, error], [prevResult, prevError]) => {
    const errorAppeared = error && error !== prevError;
    const resultAppeared = result && result !== prevResult;
    const el = errorAppeared ? formErrorEl.value : resultAppeared ? testResultEl.value : null;
    el?.scrollIntoView({ block: "nearest", behavior: "smooth" });
  },
  { flush: "post" },
);

const isSqlite = computed(() => driver.value === "sqlite");
const isRedis = computed(() => driver.value === "redis");
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

const usesIam = computed(() => !isSqlite.value && passwordSource.value === "aws-iam");
const usesCommand = computed(() => !isSqlite.value && passwordSource.value === "command");
const typesPassword = computed(() => !isSqlite.value && passwordSource.value === "password");

const usesCleartext = computed(
  () => driver.value === "mysql" && passwordSource.value !== "none" && (cleartextAuth.value || usesIam.value),
);

const requiresTls = computed(() => ["require", "verify-ca", "verify-full"].includes(sslMode.value));
const verifiesCertificate = computed(() => sslMode.value === "verify-ca" || sslMode.value === "verify-full");

const cleartextBlocked = computed(
  () => usesCleartext.value && !usesIam.value && !useSsh.value && !requiresTls.value,
);

const iamBlocked = computed(() => usesIam.value && !requiresTls.value);

const hostCheckBlocked = computed(() => useSsh.value && sslMode.value === "verify-full");

const certLocations = computed(() =>
  [
    verifiesCertificate.value ? sslCaPath.value : "",
    requiresTls.value && useClientCert.value ? sslCertPath.value : "",
    requiresTls.value && useClientCert.value ? sslKeyPath.value : "",
  ].map((location) => location.trim().toLowerCase()),
);

const readsFromS3 = computed(
  () => !isSqlite.value && certLocations.value.some((location) => location.startsWith("s3://")),
);

const httpBlocked = computed(
  () => !isSqlite.value && certLocations.value.some((location) => location.startsWith("http://")),
);

const usesAwsSettings = computed(() => usesIam.value || readsFromS3.value);

const clientCertIncomplete = computed(
  () => requiresTls.value && useClientCert.value && !(sslCertPath.value.trim() && sslKeyPath.value.trim()),
);

const sourceReady = computed(() => {
  if (usesIam.value) {
    return Boolean(awsRegion.value.trim());
  }
  if (usesCommand.value) {
    return Boolean(passwordCommand.value.trim());
  }
  return true;
});

const redisDatabaseValid = computed(() => !isRedis.value || /^\d*$/.test(database.value.trim()));

const canSubmit = computed(() =>
  isSqlite.value
    ? Boolean(filePath.value.trim())
    : Boolean(
        host.value.trim() &&
          (user.value.trim() || isRedis.value) &&
          redisDatabaseValid.value &&
          sshReady.value &&
          sourceReady.value &&
          !cleartextBlocked.value &&
          !iamBlocked.value &&
          !hostCheckBlocked.value &&
          !clientCertIncomplete.value &&
          !httpBlocked.value,
      ),
);

const usesSshPassword = computed(
  () => useSsh.value && sshAuth.value !== "password" && sshAlsoPassword.value,
);

const sshConfigHint = computed(() => {
  const resolved = sshResolved.value;
  if (!resolved || !useSsh.value) {
    return "";
  }
  const renamed = resolved.hostname && resolved.hostname !== sshHost.value.trim() ? resolved.hostname : "";
  const proxy = resolved.proxyCommand ? "its ProxyCommand" : resolved.proxyJump;
  if (renamed && proxy) {
    return `From ~/.ssh/config, connects to ${renamed} through ${proxy}.`;
  }
  if (renamed) {
    return `From ~/.ssh/config, connects to ${renamed}.`;
  }
  return proxy ? `From ~/.ssh/config, connects through ${proxy}.` : "";
});

const sshSecretLabel = computed(() => (sshAuth.value === "key" ? "Key passphrase" : "SSH password"));

const sshSecretPlaceholder = computed(() => {
  if (hasSavedSshSecret.value) {
    return `Saved in ${secretStoreName}`;
  }
  return sshAuth.value === "key" ? "Leave empty if the key has none" : "Password";
});

const passwordPlaceholder = computed(() => {
  if (!savePassword.value) {
    return "Asked for each time you connect";
  }
  return hasSavedPassword.value ? `Saved in ${secretStoreName}` : "Password";
});

watch(driver, (next, previous) => {
  testResult.value = null;
  const previousOption = DRIVER_OPTIONS.find((option) => option.id === previous);
  const nextOption = DRIVER_OPTIONS.find((option) => option.id === next);
  if (next !== "sqlite" && (!port.value || port.value === previousOption?.defaultPort)) {
    port.value = nextOption?.defaultPort ?? 0;
  }
  const previousUser = defaultUser(previous);
  if (next !== "sqlite" && (!user.value.trim() || user.value === previousUser)) {
    user.value = defaultUser(next);
  }
  if ((next === "mssql" || next === "redis") && passwordSource.value === "aws-iam") {
    passwordSource.value = "password";
  }
  // Redis TLS is all or nothing, and always checks the host name when it verifies.
  if (next === "redis") {
    if (sslMode.value === "prefer") sslMode.value = "disable";
    if (sslMode.value === "verify-ca") sslMode.value = "verify-full";
    if (/^\d*$/.test(database.value.trim()) === false) database.value = "";
  } else if (previous === "redis") {
    if (sslMode.value === "disable") sslMode.value = "prefer";
    if (/^\d+$/.test(database.value.trim())) database.value = "";
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
    cleartextAuth,
    passwordSource,
    awsRegion,
    awsProfile,
    passwordCommand,
    sslCaPath,
    sslCertPath,
    sslKeyPath,
    useClientCert,
    sshEnabled,
    sshHost,
    sshPort,
    sshUser,
    sshAuth,
    sshKeyPath,
    sshSecret,
    sshAlsoPassword,
    sshPassword,
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

watch(cleartextAuth, (enabled) => {
  if (enabled && !useSsh.value && !requiresTls.value) {
    sslMode.value = "require";
  }
});

function regionFromHost() {
  return RDS_HOST_REGION.exec(host.value.trim())?.[1]?.toLowerCase() ?? "";
}

function useRdsBundle() {
  sslCaPath.value = RDS_BUNDLE_URL;
}

watch(sslMode, (next) => {
  if ((next === "verify-ca" || next === "verify-full") && !sslCaPath.value.trim() && regionFromHost()) {
    useRdsBundle();
  }
});

watch(passwordSource, (next) => {
  if (next !== "aws-iam") {
    return;
  }
  if (!requiresTls.value) {
    sslMode.value = "require";
  }
  if (!awsRegion.value.trim()) {
    awsRegion.value = regionFromHost();
  }
});

watch(host, (_next, previous) => {
  if (!usesIam.value) {
    return;
  }
  const previousRegion = RDS_HOST_REGION.exec(previous.trim())?.[1]?.toLowerCase() ?? "";
  if (!awsRegion.value.trim() || awsRegion.value.trim() === previousRegion) {
    awsRegion.value = regionFromHost();
  }
});

watch(sshHost, () => {
  void resolveSshHost(true);
});

/**
 * Looks up a ~/.ssh/config alias so the form shows where it really connects.
 * With `fill`, the alias's port, user, and key are copied into the form.
 */
async function resolveSshHost(fill: boolean) {
  const token = ++resolveToken;
  const alias = sshHost.value.trim();
  if (!sshHosts.value.includes(alias)) {
    sshResolved.value = null;
    return;
  }
  const resolved = await api.resolveSshHost(alias).catch(() => null);
  if (token !== resolveToken) {
    return;
  }
  sshResolved.value = resolved;
  if (!resolved || !fill) {
    return;
  }
  if (resolved.port) {
    sshPort.value = resolved.port;
  }
  if (resolved.user && !sshUser.value.trim()) {
    sshUser.value = resolved.user;
  }
  if (resolved.identityFiles.length) {
    sshAuth.value = "key";
    sshKeyPath.value = resolved.identityFiles[0];
  } else if (resolved.identityAgent) {
    sshAuth.value = "agent";
  }
}

async function loadSshHosts() {
  sshHosts.value = await api.listSshHosts().catch(() => []);
  void resolveSshHost(false);
}

async function loadAwsProfiles() {
  awsProfiles.value = await api.listAwsProfiles().catch(() => []);
  awsProfilesLoaded.value = true;
}

watch(
  usesAwsSettings,
  (shown) => {
    if (shown) {
      void loadAwsProfiles();
    }
  },
  { immediate: true },
);

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
  void loadSshHosts();
  if (
    initial?.id &&
    initial.driver !== "sqlite" &&
    (initial.passwordSource ?? "password") === "password" &&
    initial.savePassword
  ) {
    try {
      hasSavedPassword.value = await api.hasSavedPassword(initial.id);
    } catch {
      hasSavedPassword.value = false;
    }
  }
  if (initial?.id && initialSsh.enabled && initialSsh.auth !== "agent") {
    try {
      hasSavedSshSecret.value = await api.hasSavedSshSecret(initial.id);
    } catch {
      hasSavedSshSecret.value = false;
    }
  }
  if (initial?.id && initialSsh.enabled && initialSsh.alsoPassword) {
    hasSavedSshPassword.value = await api.hasSavedSshPassword(initial.id).catch(() => false);
  }
});

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
    headerColor: !groupId && colorPicked.value ? headerColor.value : "",
    savePassword: typesPassword.value && savePassword.value,
    cleartextAuth: usesCleartext.value,
    passwordSource: isSqlite.value ? "password" : passwordSource.value,
    awsRegion: usesAwsSettings.value ? awsRegion.value.trim() : "",
    awsProfile: usesAwsSettings.value ? awsProfile.value.trim() : "",
    passwordCommand: usesCommand.value ? passwordCommand.value.trim() : "",
    sslCaPath: verifiesCertificate.value ? sslCaPath.value.trim() : "",
    sslCertPath: requiresTls.value && useClientCert.value ? sslCertPath.value.trim() : "",
    sslKeyPath: requiresTls.value && useClientCert.value ? sslKeyPath.value.trim() : "",
    ssh: isSqlite.value
      ? defaultSshTunnel()
      : {
          enabled: sshEnabled.value,
          host: sshHost.value.trim(),
          port: Number(sshPort.value) || DEFAULT_SSH_PORT,
          user: sshUser.value.trim(),
          auth: sshAuth.value,
          keyPath: sshAuth.value === "key" ? sshKeyPath.value.trim() : "",
          alsoPassword: sshAuth.value !== "password" && sshAlsoPassword.value,
        },
  };
}

function passwordArg() {
  if (!typesPassword.value) {
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

function sshPasswordArg() {
  return usesSshPassword.value && sshPassword.value ? sshPassword.value : null;
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

async function browseCertificate(field: "ca" | "cert" | "key", title: string) {
  const target = { ca: sslCaPath, cert: sslCertPath, key: sslKeyPath }[field];
  const home = await homeDir().catch(() => "");
  const selected = await open({
    multiple: false,
    directory: false,
    title,
    filters: CERT_FILTERS,
    defaultPath: target.value.trim().replace(/^~(?=\/)/, home) || undefined,
  });
  if (typeof selected === "string") {
    target.value = home && selected.startsWith(`${home}/`) ? `~${selected.slice(home.length)}` : selected;
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
      name.value = baseName(selected);
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
      name.value = baseName(selected);
    }
    showToast(`Created ${baseName(selected)}`);
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
    const version = await api.testConnection(
      buildEntry(),
      passwordArg(),
      sshSecretArg(),
      sshPasswordArg(),
    );
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
      groupId || null,
      buildEntry(),
      passwordArg(),
      sshSecretArg(),
      sshPasswordArg(),
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
          <DriverIcon :driver="option.id" />
          {{ option.label }}
        </button>
      </div>

      <div class="form-grid">
        <div class="modal-label span-2">
          <label class="muted tiny" for="connection-name">Name</label>
          <div class="name-row">
            <input
              id="connection-name"
              ref="nameInput"
              v-model="name"
              type="text"
              spellcheck="false"
              :placeholder="isSqlite ? 'Defaults to the file name' : 'Defaults to user@host'"
            />
            <!-- A grouped connection takes its group's color. -->
            <label v-if="!groupId" class="color-picker" title="Color">
              <span class="color-picker-swatch" aria-hidden="true">
                <input
                  v-model="headerColor"
                  type="color"
                  aria-label="Color"
                  @input="colorPicked = true"
                />
              </span>
            </label>
          </div>
        </div>

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
            <span class="muted tiny">{{ isRedis ? "User (ACL)" : "User" }}</span>
            <input
              v-model="user"
              type="text"
              spellcheck="false"
              autocomplete="off"
              :placeholder="isRedis ? 'default' : defaultUser(driver)"
            />
          </label>
          <label class="modal-label">
            <span class="muted tiny">Authentication</span>
            <select v-model="passwordSource">
              <option value="password">Password</option>
              <option value="none">No password</option>
              <option v-if="driver !== 'mssql' && driver !== 'redis'" value="aws-iam">AWS IAM</option>
              <option value="command">Password command</option>
            </select>
          </label>
          <template v-if="passwordSource === 'password'">
            <label class="modal-label span-2">
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
              <span>Save password in the {{ secretStoreFullName }}</span>
            </label>
          </template>
          <template v-else-if="passwordSource === 'aws-iam'">
            <label class="modal-label">
              <span class="muted tiny">AWS region</span>
              <input v-model="awsRegion" type="text" spellcheck="false" autocomplete="off" placeholder="us-east-1" />
            </label>
            <AwsProfileField v-model="awsProfile" :profiles="awsProfiles" :loading="!awsProfilesLoaded" />
            <p class="muted tiny span-2 ssh-hint">
              Signs in with a token from the AWS CLI (aws rds generate-db-auth-token), fetched again each time
              Recon connects. For SSO profiles, run aws sso login first. IAM logins need SSL.
            </p>
          </template>
          <template v-else-if="passwordSource === 'none'">
            <p class="muted tiny span-2 ssh-hint">
              Connects without a password, and doesn't store or ask for one.
            </p>
          </template>
          <template v-else-if="passwordSource === 'command'">
            <label class="modal-label span-2">
              <span class="muted tiny">Command</span>
              <input
                v-model="passwordCommand"
                class="code-input"
                type="text"
                spellcheck="false"
                autocomplete="off"
                placeholder="op read op://Private/Database/password"
              />
            </label>
            <p class="muted tiny span-2 ssh-hint">{{ passwordCommandShell }}</p>
          </template>
          <label class="modal-label">
            <span class="muted tiny">{{ isRedis ? "Database number" : "Database" }}</span>
            <input
              v-model="database"
              type="text"
              :inputmode="isRedis ? 'numeric' : undefined"
              spellcheck="false"
              :placeholder="isRedis ? '0' : 'Optional'"
            />
          </label>
          <label v-if="isRedis" class="modal-label">
            <span class="muted tiny">TLS</span>
            <select v-model="sslMode">
              <option value="disable">Off</option>
              <option value="require">On, without verifying the certificate</option>
              <option value="verify-full">On, verify the certificate and host name</option>
            </select>
          </label>
          <label v-else class="modal-label">
            <span class="muted tiny">SSL</span>
            <select v-model="sslMode">
              <option value="prefer">Prefer</option>
              <option value="require">Require</option>
              <option value="verify-ca">Verify CA</option>
              <option value="verify-full">Verify CA and host name</option>
              <option value="disable">Disable</option>
            </select>
          </label>
          <p v-if="!redisDatabaseValid" class="settings-error span-2">The Redis database is a number, like 0.</p>
          <p v-if="iamBlocked" class="settings-error span-2">AWS IAM logins need SSL set to Require or Verify.</p>
          <p v-if="hostCheckBlocked" class="settings-error span-2">
            {{
              isRedis
                ? "The host name can't be checked through an SSH tunnel. Turn TLS on without verifying instead."
                : "The host name can't be checked through an SSH tunnel. Set SSL to Verify CA instead."
            }}
          </p>
          <template v-if="verifiesCertificate">
            <label class="modal-label span-2">
              <span class="muted tiny">CA certificate</span>
              <div class="file-picker">
                <input
                  v-model="sslCaPath"
                  type="text"
                  spellcheck="false"
                  placeholder="File, https:// URL, or s3:// URI"
                />
                <button class="ghost" type="button" @click="browseCertificate('ca', 'Choose CA certificate')">
                  Browse…
                </button>
                <button v-if="!isRedis" class="ghost" type="button" @click="useRdsBundle">AWS RDS bundle</button>
              </div>
            </label>
            <p class="muted tiny span-2 ssh-hint">
              Leave empty to use the public certificate authorities. AWS RDS bundle uses AWS's global bundle for
              RDS and Aurora, downloaded each time Recon connects so it stays current.
            </p>
          </template>
          <template v-if="requiresTls">
            <label class="checkbox-row span-2">
              <input v-model="useClientCert" type="checkbox" />
              <span>Sign in with a client certificate</span>
            </label>
            <template v-if="useClientCert">
              <label class="modal-label span-2">
                <span class="muted tiny">Client certificate</span>
                <div class="file-picker">
                  <input
                    v-model="sslCertPath"
                    type="text"
                    spellcheck="false"
                    placeholder="File, https:// URL, or s3:// URI"
                  />
                  <button
                    class="ghost"
                    type="button"
                    @click="browseCertificate('cert', 'Choose client certificate')"
                  >
                    Browse…
                  </button>
                </div>
              </label>
              <label class="modal-label span-2">
                <span class="muted tiny">Client key</span>
                <div class="file-picker">
                  <input
                    v-model="sslKeyPath"
                    type="text"
                    spellcheck="false"
                    placeholder="File, https:// URL, or s3:// URI"
                  />
                  <button class="ghost" type="button" @click="browseCertificate('key', 'Choose client key')">
                    Browse…
                  </button>
                </div>
              </label>
            </template>
          </template>
          <p v-if="httpBlocked" class="settings-error span-2">
            Certificates can't be downloaded over http://, since anyone on the network could swap them. Use
            https://.
          </p>
          <template v-if="readsFromS3 && !usesIam">
            <label class="modal-label">
              <span class="muted tiny">AWS region</span>
              <input v-model="awsRegion" type="text" spellcheck="false" autocomplete="off" placeholder="Optional" />
            </label>
            <AwsProfileField v-model="awsProfile" :profiles="awsProfiles" :loading="!awsProfilesLoaded" />
            <p class="muted tiny span-2 ssh-hint">
              The AWS CLI (aws s3 cp) reads files from S3 each time Recon connects, and keeps keys in memory
              only. For SSO profiles, run aws sso login first.
            </p>
          </template>
          <template v-if="driver === 'mysql' && passwordSource !== 'aws-iam' && passwordSource !== 'none'">
            <label class="checkbox-row span-2">
              <input v-model="cleartextAuth" type="checkbox" />
              <span>Send password in clear text</span>
            </label>
            <p v-if="cleartextBlocked" class="settings-error span-2">
              Set SSL to Require or Verify, or connect through an SSH tunnel, to send the password in clear text.
            </p>
            <p v-else-if="cleartextAuth" class="muted tiny span-2 ssh-hint">
              For servers that check passwords with PAM, LDAP, or AWS IAM (mysql_clear_password).
            </p>
          </template>

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
              <input
                v-model="sshHost"
                type="text"
                spellcheck="false"
                autocomplete="off"
                list="ssh-config-hosts"
                :placeholder="sshHosts.length ? 'Host or ~/.ssh/config alias' : 'bastion.example.com'"
              />
              <datalist id="ssh-config-hosts">
                <option v-for="alias in sshHosts" :key="alias" :value="alias" />
              </datalist>
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
            <p v-if="sshConfigHint" class="muted tiny span-2 ssh-hint">{{ sshConfigHint }}</p>
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
            <template v-if="sshAuth !== 'password'">
              <label class="checkbox-row span-2">
                <input v-model="sshAlsoPassword" type="checkbox" />
                <span>Server also asks for a password</span>
              </label>
              <label v-if="sshAlsoPassword" class="modal-label span-2">
                <span class="muted tiny">SSH password</span>
                <input
                  v-model="sshPassword"
                  type="password"
                  autocomplete="new-password"
                  :placeholder="hasSavedSshPassword ? `Saved in ${secretStoreName}` : 'Password'"
                />
              </label>
            </template>
          </template>
        </template>
      </div>

      <p
        v-if="testResult"
        ref="testResultEl"
        class="test-result"
        :class="testResult.ok ? 'good' : 'bad'"
        role="status"
      >
        {{ testResult.text }}
      </p>
      <p v-if="formError" ref="formErrorEl" class="settings-error">{{ formError }}</p>
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
