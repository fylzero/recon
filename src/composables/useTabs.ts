import { computed, ref } from "vue";
import { useRouter } from "vue-router";
import { useApp } from "./useApp";

export const CONNECTIONS_TAB_ID = "connections";
export const HISTORY_TAB_ID = "history";
export const SETTINGS_TAB_ID = "settings";
export const CHANGELOG_TAB_ID = "changelog";

export const SETTINGS_SECTIONS = ["general", "window", "updates", "json"] as const;
export type SettingsSection = (typeof SETTINGS_SECTIONS)[number];

type UtilityPanel = "settings" | "history" | "changelog";

export interface AppTab {
  id: string;
  title: string;
  closable: boolean;
  accentColor?: string;
}

interface ConnectionTab {
  id: string;
  title: string;
}

const connectionTabs = ref<ConnectionTab[]>([]);
const historyTabOpen = ref(false);
const settingsTabOpen = ref(false);
const changelogTabOpen = ref(false);
const settingsSection = ref<SettingsSection>("general");
const activeId = ref(CONNECTIONS_TAB_ID);

export function isSettingsSection(value: unknown): value is SettingsSection {
  return SETTINGS_SECTIONS.includes(value as SettingsSection);
}

export function settingsPath(section: SettingsSection = settingsSection.value) {
  return section === "general" ? "/settings" : `/settings/${section}`;
}

export function useTabs() {
  const router = useRouter();
  const { findConnection } = useApp();

  const tabs = computed<AppTab[]>(() => [
    { id: CONNECTIONS_TAB_ID, title: "Connections", closable: false },
    ...connectionTabs.value.map((tab) => {
      const match = findConnection(tab.id);
      return {
        ...tab,
        closable: true,
        accentColor: match?.connection.headerColor || match?.group?.headerColor,
      };
    }),
    ...(historyTabOpen.value ? [{ id: HISTORY_TAB_ID, title: "History", closable: true }] : []),
    ...(settingsTabOpen.value ? [{ id: SETTINGS_TAB_ID, title: "Settings", closable: true }] : []),
    ...(changelogTabOpen.value
      ? [{ id: CHANGELOG_TAB_ID, title: "Change Log", closable: true }]
      : []),
  ]);

  function titleFor(id: string) {
    return findConnection(id)?.connection.name ?? id;
  }

  function ensureTab(id: string) {
    if (connectionTabs.value.some((tab) => tab.id === id)) {
      return;
    }
    connectionTabs.value = [...connectionTabs.value, { id, title: titleFor(id) }];
  }

  function routeFor(id: string) {
    if (id === CONNECTIONS_TAB_ID) {
      return "/";
    }
    if (id === HISTORY_TAB_ID) {
      return "/history";
    }
    if (id === SETTINGS_TAB_ID) {
      return settingsPath();
    }
    if (id === CHANGELOG_TAB_ID) {
      return "/changelog";
    }
    return `/connection/${id}`;
  }

  function activate(id: string) {
    activeId.value = id;
    const target = routeFor(id);
    if (router.currentRoute.value.fullPath !== target) {
      void router.push(target);
    }
  }

  function openConnection(id: string) {
    ensureTab(id);
    activate(id);
  }

  function openConnections(ids: string[], focusId = ids[ids.length - 1]) {
    for (const id of ids) {
      ensureTab(id);
    }
    if (focusId) {
      activate(focusId);
    }
  }

  function fallbackUtilityId(closingId: string) {
    if (closingId !== HISTORY_TAB_ID && historyTabOpen.value) {
      return HISTORY_TAB_ID;
    }
    if (closingId !== SETTINGS_TAB_ID && settingsTabOpen.value) {
      return SETTINGS_TAB_ID;
    }
    if (closingId !== CHANGELOG_TAB_ID && changelogTabOpen.value) {
      return CHANGELOG_TAB_ID;
    }
    return connectionTabs.value[connectionTabs.value.length - 1]?.id ?? CONNECTIONS_TAB_ID;
  }

  function closeUtilityTab(id: string, open: { value: boolean }) {
    if (!open.value) {
      return;
    }
    const wasActive = activeId.value === id;
    open.value = false;
    if (wasActive) {
      activate(fallbackUtilityId(id));
    }
  }

  function openHistory() {
    historyTabOpen.value = true;
    activate(HISTORY_TAB_ID);
  }

  function openSettings(section: SettingsSection = "general") {
    settingsTabOpen.value = true;
    settingsSection.value = section;
    activate(SETTINGS_TAB_ID);
  }

  function openSettingsJson() {
    openSettings("json");
  }

  function openChangelog() {
    changelogTabOpen.value = true;
    activate(CHANGELOG_TAB_ID);
  }

  function closeActiveTab() {
    const tab = tabs.value.find((item) => item.id === activeId.value);
    if (!tab?.closable) {
      return false;
    }
    closeTab(tab.id);
    return true;
  }

  function closeTab(id: string) {
    if (id === CONNECTIONS_TAB_ID) {
      return;
    }
    if (id === HISTORY_TAB_ID) {
      closeUtilityTab(HISTORY_TAB_ID, historyTabOpen);
      return;
    }
    if (id === SETTINGS_TAB_ID) {
      closeUtilityTab(SETTINGS_TAB_ID, settingsTabOpen);
      return;
    }
    if (id === CHANGELOG_TAB_ID) {
      closeUtilityTab(CHANGELOG_TAB_ID, changelogTabOpen);
      return;
    }
    closeConnections([id]);
  }

  function closeConnections(ids: string[]) {
    const closing = new Set(ids);
    const index = connectionTabs.value.findIndex((tab) => tab.id === activeId.value);
    connectionTabs.value = connectionTabs.value.filter((tab) => !closing.has(tab.id));
    if (closing.has(activeId.value)) {
      const neighbor = connectionTabs.value[index] ?? connectionTabs.value[index - 1];
      activate(neighbor?.id ?? CONNECTIONS_TAB_ID);
    }
  }

  function hasTab(id: string) {
    if (id === HISTORY_TAB_ID) {
      return historyTabOpen.value;
    }
    if (id === SETTINGS_TAB_ID) {
      return settingsTabOpen.value;
    }
    if (id === CHANGELOG_TAB_ID) {
      return changelogTabOpen.value;
    }
    return connectionTabs.value.some((tab) => tab.id === id);
  }

  function syncFromRoute(
    connectionId: string | undefined,
    isHome: boolean,
    panel?: UtilityPanel,
    section?: string,
  ) {
    if (isHome) {
      activeId.value = CONNECTIONS_TAB_ID;
      return;
    }
    if (panel === "history") {
      historyTabOpen.value = true;
      activeId.value = HISTORY_TAB_ID;
      return;
    }
    if (panel === "settings") {
      settingsTabOpen.value = true;
      settingsSection.value = isSettingsSection(section) ? section : "general";
      activeId.value = SETTINGS_TAB_ID;
      return;
    }
    if (panel === "changelog") {
      changelogTabOpen.value = true;
      activeId.value = CHANGELOG_TAB_ID;
      return;
    }
    if (!connectionId || !findConnection(connectionId)) {
      return;
    }
    ensureTab(connectionId);
    activeId.value = connectionId;
  }

  function refreshTitles() {
    connectionTabs.value = connectionTabs.value
      .filter((tab) => findConnection(tab.id))
      .map((tab) => ({ ...tab, title: titleFor(tab.id) }));
    if (
      !tabs.value.some((tab) => tab.id === activeId.value) &&
      activeId.value !== CONNECTIONS_TAB_ID
    ) {
      activate(CONNECTIONS_TAB_ID);
    }
  }

  return {
    tabs,
    connectionTabs,
    historyTabOpen,
    settingsTabOpen,
    changelogTabOpen,
    settingsSection,
    activeId,
    openConnection,
    openConnections,
    openHistory,
    openSettings,
    openSettingsJson,
    openChangelog,
    activate,
    closeTab,
    closeActiveTab,
    closeConnections,
    hasTab,
    syncFromRoute,
    refreshTitles,
  };
}
