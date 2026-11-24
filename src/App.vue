<script setup lang="ts">
import { computed, defineAsyncComponent, h, onMounted, onUnmounted, watch } from "vue";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { useRoute } from "vue-router";
import TabBar from "./components/TabBar.vue";
import Toast from "./components/Toast.vue";
import Modal from "./components/Modal.vue";
import ConnectionForm from "./components/ConnectionForm.vue";
import AuthPromptDialog from "./components/AuthPromptDialog.vue";
import ConnectionsView from "./views/ConnectionsView.vue";

const ConnectionPane = defineAsyncComponent(() => import("./components/ConnectionPane.vue"));
const RedisPane = defineAsyncComponent(() => import("./components/RedisPane.vue"));
const MongoPane = defineAsyncComponent(() => import("./components/MongoPane.vue"));
const SettingsView = defineAsyncComponent({
  loader: () => import("./views/SettingsView.vue"),
  errorComponent: {
    setup() {
      return () =>
        h("p", { class: "settings-error settings-pane-error" }, "Could not open Settings.");
    },
  },
});
const ChangelogView = defineAsyncComponent(() => import("./views/ChangelogView.vue"));
import { useApp } from "./composables/useApp";
import { DEFAULT_LIST_FONT, UI_FONT_FALLBACK, resolveFontStack } from "./fonts";
import { watchWindowFocus } from "./notifications";
import { useUpdater } from "./composables/useUpdater";
import { useConnectionForm } from "./composables/useConnectionForm";
import { CHANGELOG_TAB_ID, CONNECTIONS_TAB_ID, SETTINGS_TAB_ID, useTabs } from "./composables/useTabs";

const route = useRoute();
const { formState } = useConnectionForm();
const {
  load,
  error,
  groups,
  standaloneConnections,
  toastMessage,
  toastKind,
  dismissToast,
  pauseToast,
  resumeToast,
  showToast,
  findConnection,
  editorFontFamily,
  editorFontSize,
  gridFontFamily,
  gridFontSize,
  listFontFamily,
  listFontSize,
} = useApp();

const appStyle = computed(() => ({
  "--editor-font-family": resolveFontStack(editorFontFamily.value),
  "--editor-font-size": `${editorFontSize.value}px`,
  "--grid-font-family": resolveFontStack(gridFontFamily.value),
  "--grid-font-size": `${gridFontSize.value}px`,
  "--list-font-family": resolveFontStack(listFontFamily.value, DEFAULT_LIST_FONT, UI_FONT_FALLBACK),
  "--list-font-size": `${listFontSize.value}px`,
}));
const {
  status,
  statusText,
  currentVersion,
  availableVersion,
  busy,
  promptOpen,
  checkForUpdates,
  installUpdate,
  dismissPrompt,
} = useUpdater();

const {
  connectionPanes,
  settingsTabOpen,
  changelogTabOpen,
  activeId,
  syncFromRoute,
  refreshTitles,
  closeActiveTab,
  openSettings,
} = useTabs();

let stopCloseShortcut: (() => void) | undefined;
let stopOpenSettings: (() => void) | undefined;
let stopCheckForUpdates: (() => void) | undefined;

async function closeActiveTabOrWindow() {
  if (closeActiveTab()) {
    return;
  }
  await getCurrentWindow().close();
}

async function onMenuCheckForUpdates() {
  await checkForUpdates({ prompt: true });
  if (status.value === "up-to-date") {
    showToast("You're on the latest version.");
  } else if (status.value === "error") {
    showToast(statusText.value, "error");
  }
}

function onWindowKeydown(event: KeyboardEvent) {
  if (!(event.metaKey || event.ctrlKey) || event.altKey || event.shiftKey) {
    return;
  }
  if (event.key.toLowerCase() !== "w") {
    return;
  }
  if (!closeActiveTab()) {
    return;
  }
  event.preventDefault();
  event.stopPropagation();
}

function syncRoute() {
  const connectionId = typeof route.params.id === "string" ? route.params.id : undefined;
  const section = typeof route.params.section === "string" ? route.params.section : undefined;
  const panel =
    route.name === "settings" ? "settings" : route.name === "changelog" ? "changelog" : undefined;
  syncFromRoute(connectionId, route.name === "home", panel, section);
}

onMounted(() => {
  watchWindowFocus();
  void load().then(syncRoute);
  window.addEventListener("keydown", onWindowKeydown, true);
  void listen("close-tab-or-window", () => {
    void closeActiveTabOrWindow();
  }).then((unlisten) => {
    stopCloseShortcut = unlisten;
  });
  void listen("open-settings", () => {
    openSettings();
  }).then((unlisten) => {
    stopOpenSettings = unlisten;
  });
  void listen("check-for-updates", () => {
    void onMenuCheckForUpdates();
  }).then((unlisten) => {
    stopCheckForUpdates = unlisten;
  });
  if (import.meta.env.PROD) {
    void checkForUpdates({ prompt: true, silent: true });
  }
});

onUnmounted(() => {
  window.removeEventListener("keydown", onWindowKeydown, true);
  stopCloseShortcut?.();
  stopOpenSettings?.();
  stopCheckForUpdates?.();
});

watch(() => [route.name, route.params.id, route.params.section], syncRoute);

watch([groups, standaloneConnections], () => {
  refreshTitles();
});
</script>

<template>
  <div class="app-shell" :style="appStyle">
    <TabBar />
    <main class="main">
      <p v-if="error" class="banner">{{ error }}</p>
      <div class="main-pane" v-show="activeId === CONNECTIONS_TAB_ID">
        <ConnectionsView />
      </div>
      <div
        v-for="tab in connectionPanes"
        :key="tab.id"
        class="main-pane"
        v-show="activeId === tab.id"
      >
        <RedisPane
          v-if="findConnection(tab.connectionId)?.connection.driver === 'redis'"
          :connection-id="tab.connectionId"
          :session-id="tab.id"
          :initial-namespace="tab.namespace"
          :active="activeId === tab.id"
        />
        <MongoPane
          v-else-if="findConnection(tab.connectionId)?.connection.driver === 'mongodb'"
          :connection-id="tab.connectionId"
          :session-id="tab.id"
          :initial-namespace="tab.namespace"
          :active="activeId === tab.id"
        />
        <ConnectionPane
          v-else
          :connection-id="tab.connectionId"
          :session-id="tab.id"
          :initial-namespace="tab.namespace"
          :active="activeId === tab.id"
        />
      </div>
      <div v-if="settingsTabOpen" class="main-pane" v-show="activeId === SETTINGS_TAB_ID">
        <SettingsView />
      </div>
      <div v-if="changelogTabOpen" class="main-pane" v-show="activeId === CHANGELOG_TAB_ID">
        <ChangelogView />
      </div>
    </main>
    <Transition name="toast" :duration="{ enter: 520, leave: 280 }">
      <Toast
        v-if="toastMessage"
        :message="toastMessage"
        :kind="toastKind"
        @dismiss="dismissToast"
        @pause="pauseToast"
        @resume="resumeToast"
      />
    </Transition>
    <ConnectionForm
      v-if="formState"
      :key="formState.connection?.id || (formState.connection ? `copy:${formState.connection.name}` : 'new')"
    />
    <Modal
      v-if="promptOpen"
      title="Update available"
      @close="busy ? undefined : dismissPrompt()"
    >
      <p>
        Recon {{ availableVersion }} is available. You have {{ currentVersion }}.
      </p>
      <p
        v-if="status === 'downloading' || status === 'installing' || status === 'error'"
        class="muted tiny"
        :class="{ 'settings-error': status === 'error' }"
      >
        {{ statusText }}
      </p>
      <template #actions>
        <button class="ghost" type="button" :disabled="busy" @click="dismissPrompt">
          Later
        </button>
        <button class="ghost commit" type="button" :disabled="busy" @click="installUpdate">
          Install and restart
        </button>
      </template>
    </Modal>
    <AuthPromptDialog />
  </div>
</template>
