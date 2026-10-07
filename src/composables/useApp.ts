import { computed, ref, watch } from "vue";
import * as api from "../api";
import { dashboardIds as orderDashboard } from "../dashboard";
import type {
  AppData,
  ConnectionEntry,
  ConnectionGroup,
  NotificationMode,
  PreferencesPatch,
  SavedQuery,
  WindowState,
} from "../types";
import {
  DEFAULT_CODE_FONT,
  DEFAULT_EDITOR_FONT_SIZE,
  DEFAULT_GRID_FONT_SIZE,
  DEFAULT_LIST_FONT,
  DEFAULT_LIST_FONT_SIZE,
  clampFontSize,
  sanitizeFontFamily,
} from "../fonts";
import {
  ensureNotificationPermission,
  prefersSystemNotification,
  sanitizeNotificationMode,
  sendSystemNotification,
  windowFocused,
} from "../notifications";

export const DEFAULT_PAGE_SIZE = 300;
export const DEFAULT_QUERY_ROW_LIMIT = 10_000;
export const SIDEBAR_MIN = 180;
export const SIDEBAR_MAX = 560;
export const DEFAULT_MAX_AUTO_COLUMN_WIDTH = 480;

const groups = ref<ConnectionGroup[]>([]);
const standaloneConnections = ref<ConnectionEntry[]>([]);
const dashboardOrder = ref<string[]>([]);
const dashboardIds = computed(() =>
  orderDashboard(
    dashboardOrder.value,
    standaloneConnections.value.map((entry) => entry.id),
    groups.value.map((group) => group.id),
  ),
);
const savedQueries = ref<SavedQuery[]>([]);
const error = ref("");
const loaded = ref(false);
const editorFontFamily = ref(DEFAULT_CODE_FONT);
const editorFontSize = ref(DEFAULT_EDITOR_FONT_SIZE);
const gridFontFamily = ref(DEFAULT_CODE_FONT);
const gridFontSize = ref(DEFAULT_GRID_FONT_SIZE);
const listFontFamily = ref(DEFAULT_LIST_FONT);
const listFontSize = ref(DEFAULT_LIST_FONT_SIZE);
const pageSize = ref(DEFAULT_PAGE_SIZE);
const queryRowLimit = ref(DEFAULT_QUERY_ROW_LIMIT);
const sidebarWidth = ref(260);
const maxAutoColumnWidth = ref(DEFAULT_MAX_AUTO_COLUMN_WIDTH);
const autoApplyFilters = ref(true);
const notifications = ref<NotificationMode>("background");
const windowState = ref<WindowState | null>(null);
const toastMessage = ref("");
const toastKind = ref<"success" | "error">("success");
let toastTimer: ReturnType<typeof setTimeout> | null = null;
let toastRemaining = 0;
let toastStartedAt = 0;
let toastHovered = false;

const TOAST_RESUME_MIN_MS = 1200;

function clampSidebar(width: number) {
  return Math.round(Math.min(SIDEBAR_MAX, Math.max(SIDEBAR_MIN, width)));
}

function startToastTimer() {
  if (toastTimer || !toastMessage.value || toastHovered || !windowFocused.value) {
    return;
  }
  toastStartedAt = Date.now();
  toastTimer = setTimeout(() => {
    toastMessage.value = "";
    toastTimer = null;
  }, toastRemaining);
}

function stopToastTimer() {
  if (!toastTimer) {
    return;
  }
  clearTimeout(toastTimer);
  toastTimer = null;
  toastRemaining = Math.max(TOAST_RESUME_MIN_MS, toastRemaining - (Date.now() - toastStartedAt));
}

watch(windowFocused, (focused) => {
  if (focused) {
    startToastTimer();
  } else {
    stopToastTimer();
  }
});

export function useApp() {
  async function load() {
    error.value = "";
    try {
      applyState(await api.getState());
      loaded.value = true;
    } catch (err) {
      error.value = String(err);
    }
  }

  function applyState(data: AppData) {
    groups.value = data.groups;
    standaloneConnections.value = data.connections ?? [];
    dashboardOrder.value = data.dashboardOrder ?? [];
    savedQueries.value = data.savedQueries ?? [];
    editorFontFamily.value = sanitizeFontFamily(data.editorFontFamily ?? DEFAULT_CODE_FONT);
    editorFontSize.value = clampFontSize(
      data.editorFontSize ?? DEFAULT_EDITOR_FONT_SIZE,
      DEFAULT_EDITOR_FONT_SIZE,
    );
    gridFontFamily.value = sanitizeFontFamily(data.gridFontFamily ?? DEFAULT_CODE_FONT);
    gridFontSize.value = clampFontSize(
      data.gridFontSize ?? DEFAULT_GRID_FONT_SIZE,
      DEFAULT_GRID_FONT_SIZE,
    );
    listFontFamily.value = sanitizeFontFamily(
      data.listFontFamily ?? DEFAULT_LIST_FONT,
      DEFAULT_LIST_FONT,
    );
    listFontSize.value = clampFontSize(
      data.listFontSize ?? DEFAULT_LIST_FONT_SIZE,
      DEFAULT_LIST_FONT_SIZE,
    );
    pageSize.value = data.pageSize ?? DEFAULT_PAGE_SIZE;
    queryRowLimit.value = data.queryRowLimit ?? DEFAULT_QUERY_ROW_LIMIT;
    sidebarWidth.value = clampSidebar(data.sidebarWidth ?? 260);
    maxAutoColumnWidth.value = data.maxAutoColumnWidth ?? DEFAULT_MAX_AUTO_COLUMN_WIDTH;
    autoApplyFilters.value = data.autoApplyFilters ?? true;
    notifications.value = sanitizeNotificationMode(data.notifications);
    windowState.value = data.window ?? null;
  }

  function dismissToast() {
    if (toastTimer) {
      clearTimeout(toastTimer);
      toastTimer = null;
    }
    toastHovered = false;
    toastMessage.value = "";
  }

  function showInAppToast(message: string, kind: "success" | "error") {
    dismissToast();
    toastKind.value = kind;
    toastMessage.value = message;
    toastRemaining = kind === "error" ? 5600 : 3200;
    startToastTimer();
  }

  /**
   * In the background mode the in-app toast still appears, but its timer waits
   * for the window to regain focus so it is there when you come back.
   */
  function showToast(message: string, kind: "success" | "error" = "success") {
    if (!message || !prefersSystemNotification(notifications.value)) {
      showInAppToast(message, kind);
      return;
    }
    const mode = notifications.value;
    if (mode === "background") {
      showInAppToast(message, kind);
    }
    void sendSystemNotification(message, kind).then((sent) => {
      if (!sent && mode === "always") {
        showInAppToast(message, kind);
      }
    });
  }

  function pauseToast() {
    toastHovered = true;
    stopToastTimer();
  }

  function resumeToast() {
    toastHovered = false;
    startToastTimer();
  }

  async function savePreferences(patch: PreferencesPatch) {
    const next = await api.updatePreferences(patch);
    applyState(next);
    if (patch.notifications && notifications.value !== "off") {
      void ensureNotificationPermission();
    }
  }

  function previewPreferences(patch: PreferencesPatch) {
    if (patch.editorFontSize !== undefined) {
      editorFontSize.value = clampFontSize(patch.editorFontSize, DEFAULT_EDITOR_FONT_SIZE);
    }
    if (patch.gridFontSize !== undefined) {
      gridFontSize.value = clampFontSize(patch.gridFontSize, DEFAULT_GRID_FONT_SIZE);
    }
    if (patch.listFontSize !== undefined) {
      listFontSize.value = clampFontSize(patch.listFontSize, DEFAULT_LIST_FONT_SIZE);
    }
    if (patch.sidebarWidth !== undefined) {
      sidebarWidth.value = clampSidebar(patch.sidebarWidth);
    }
  }

  async function replaceSettings(data: AppData) {
    const next = await api.replaceAppData(data);
    applyState(next);
    return next;
  }

  async function saveWindowState(next: WindowState) {
    windowState.value = await api.updateWindowState(next);
    return windowState.value;
  }

  function patchGroup(groupId: string, patch: Partial<ConnectionGroup>) {
    groups.value = groups.value.map((group) =>
      group.id === groupId ? { ...group, ...patch } : group,
    );
  }

  async function createGroup(name: string, headerColor?: string) {
    const group = await api.createGroup(name, headerColor);
    groups.value = [group, ...groups.value];
    dashboardOrder.value = [group.id, ...dashboardIds.value];
    return group;
  }

  async function updateGroup(groupId: string, name: string, headerColor?: string) {
    await api.updateGroup(groupId, name, headerColor);
    patchGroup(groupId, headerColor ? { name, headerColor } : { name });
  }

  async function deleteGroup(groupId: string) {
    await api.deleteGroup(groupId);
    const removed = new Set(
      groups.value.find((group) => group.id === groupId)?.connections.map((item) => item.id),
    );
    groups.value = groups.value.filter((group) => group.id !== groupId);
    savedQueries.value = savedQueries.value.filter((query) => !removed.has(query.connectionId));
  }

  function toggleGroup(groupId: string) {
    const group = groups.value.find((item) => item.id === groupId);
    if (!group) {
      return;
    }
    const expanded = !group.expanded;
    patchGroup(groupId, { expanded });
    void api.toggleGroup(groupId).catch((err) => {
      patchGroup(groupId, { expanded: !expanded });
      error.value = String(err);
    });
  }

  function setAllGroupsExpanded(expanded: boolean) {
    if (!groups.value.length) {
      return;
    }
    const previous = groups.value.map((group) => group.expanded);
    groups.value = groups.value.map((group) =>
      group.expanded === expanded ? group : { ...group, expanded },
    );
    void api.setAllGroupsExpanded(expanded).catch((err) => {
      groups.value = groups.value.map((group, index) => ({
        ...group,
        expanded: previous[index] ?? group.expanded,
      }));
      error.value = String(err);
    });
  }

  /** Grouped connections listed in `ids` move out of their group. */
  async function reorderDashboard(ids: string[]) {
    const previous = {
      groups: groups.value,
      standalone: standaloneConnections.value,
      order: dashboardOrder.value,
    };
    const groupIds = new Set(groups.value.map((group) => group.id));
    const standalone = new Map(standaloneConnections.value.map((entry) => [entry.id, entry]));
    let nextGroups = groups.value;
    for (const id of ids) {
      if (groupIds.has(id) || standalone.has(id)) {
        continue;
      }
      const found = findConnection(id);
      if (!found?.group) {
        throw new Error("Connection not found.");
      }
      standalone.set(id, found.connection);
      nextGroups = nextGroups.map((group) =>
        group.id === found.group!.id
          ? { ...group, connections: group.connections.filter((item) => item.id !== id) }
          : group,
      );
    }
    const position = new Map(ids.map((id, index) => [id, index]));
    const byPosition = (left: { id: string }, right: { id: string }) =>
      (position.get(left.id) ?? 0) - (position.get(right.id) ?? 0);
    groups.value = [...nextGroups].sort(byPosition);
    standaloneConnections.value = [...standalone.values()].sort(byPosition);
    dashboardOrder.value = ids;
    try {
      await api.reorderDashboard(ids);
    } catch (err) {
      groups.value = previous.groups;
      standaloneConnections.value = previous.standalone;
      dashboardOrder.value = previous.order;
      throw err;
    }
  }

  function findConnection(connectionId: string) {
    const standalone = standaloneConnections.value.find((item) => item.id === connectionId);
    if (standalone) {
      return { group: null, connection: standalone };
    }
    for (const group of groups.value) {
      const connection = group.connections.find((item) => item.id === connectionId);
      if (connection) {
        return { group, connection };
      }
    }
    return null;
  }

  function removeLocally(connectionId: string) {
    standaloneConnections.value = standaloneConnections.value.filter(
      (item) => item.id !== connectionId,
    );
    groups.value = groups.value.map((group) =>
      group.connections.some((item) => item.id === connectionId)
        ? { ...group, connections: group.connections.filter((item) => item.id !== connectionId) }
        : group,
    );
  }

  async function saveConnection(
    groupId: string | null,
    connection: ConnectionEntry,
    password: string | null,
    sshSecret: string | null = null,
    sshPassword: string | null = null,
  ) {
    const saved = await api.saveConnection(groupId, connection, password, sshSecret, sshPassword);
    const current = findConnection(saved.id);
    const sameGroup = current && (current.group?.id ?? null) === groupId;
    if (current && sameGroup) {
      if (groupId) {
        patchGroup(groupId, {
          connections: current.group!.connections.map((item) =>
            item.id === saved.id ? saved : item,
          ),
        });
      } else {
        standaloneConnections.value = standaloneConnections.value.map((item) =>
          item.id === saved.id ? saved : item,
        );
      }
      return saved;
    }
    removeLocally(saved.id);
    if (groupId) {
      const group = groups.value.find((item) => item.id === groupId);
      if (group) {
        patchGroup(groupId, { connections: [...group.connections, saved], expanded: true });
      }
    } else {
      standaloneConnections.value = [...standaloneConnections.value, saved];
    }
    return saved;
  }

  async function removeConnection(connectionId: string) {
    await api.removeConnection(connectionId);
    removeLocally(connectionId);
    savedQueries.value = savedQueries.value.filter((query) => query.connectionId !== connectionId);
  }

  async function saveQuery(query: SavedQuery) {
    const saved = await api.saveQuery(query);
    const exists = savedQueries.value.some((item) => item.id === saved.id);
    savedQueries.value = exists
      ? savedQueries.value.map((item) => (item.id === saved.id ? saved : item))
      : [...savedQueries.value, saved];
    return saved;
  }

  async function deleteSavedQuery(queryId: string) {
    await api.deleteSavedQuery(queryId);
    savedQueries.value = savedQueries.value.filter((query) => query.id !== queryId);
  }

  async function reorderConnections(groupId: string | null, connectionIds: string[]) {
    const list = groupId
      ? (groups.value.find((group) => group.id === groupId)?.connections ?? [])
      : standaloneConnections.value;
    const byId = new Map(list.map((item) => [item.id, item]));
    if (connectionIds.length !== list.length || connectionIds.some((id) => !byId.has(id))) {
      throw new Error("Connection list does not match saved connections.");
    }
    const next = connectionIds.map((id) => byId.get(id)!);
    const apply = (items: ConnectionEntry[]) => {
      if (groupId) {
        patchGroup(groupId, { connections: items });
      } else {
        standaloneConnections.value = items;
      }
    };
    apply(next);
    try {
      await api.reorderConnections(groupId, connectionIds);
    } catch (err) {
      apply(list);
      throw err;
    }
  }

  async function moveConnection(
    connectionId: string,
    groupId: string | null,
    connectionIds: string[],
  ) {
    const current = findConnection(connectionId);
    if (!current) {
      throw new Error("Connection not found.");
    }
    if ((current.group?.id ?? null) === groupId) {
      return reorderConnections(groupId, connectionIds);
    }
    const target = groupId
      ? groups.value.find((group) => group.id === groupId)?.connections
      : standaloneConnections.value;
    if (!target) {
      throw new Error("Group not found.");
    }
    const byId = new Map([...target, current.connection].map((item) => [item.id, item]));
    if (connectionIds.length !== byId.size || connectionIds.some((id) => !byId.has(id))) {
      throw new Error("Connection list does not match saved connections.");
    }
    const previousGroups = groups.value;
    const previousStandalone = standaloneConnections.value;
    removeLocally(connectionId);
    const next = connectionIds.map((id) => byId.get(id)!);
    if (groupId) {
      patchGroup(groupId, { connections: next });
    } else {
      standaloneConnections.value = next;
    }
    try {
      await api.moveConnection(connectionId, groupId, connectionIds);
    } catch (err) {
      groups.value = previousGroups;
      standaloneConnections.value = previousStandalone;
      throw err;
    }
  }

  return {
    groups,
    standaloneConnections,
    dashboardOrder,
    dashboardIds,
    savedQueries,
    error,
    loaded,
    editorFontFamily,
    editorFontSize,
    gridFontFamily,
    gridFontSize,
    listFontFamily,
    listFontSize,
    pageSize,
    queryRowLimit,
    sidebarWidth,
    maxAutoColumnWidth,
    autoApplyFilters,
    notifications,
    windowState,
    toastMessage,
    toastKind,
    load,
    showToast,
    dismissToast,
    pauseToast,
    resumeToast,
    savePreferences,
    previewPreferences,
    replaceSettings,
    saveWindowState,
    createGroup,
    updateGroup,
    deleteGroup,
    toggleGroup,
    setAllGroupsExpanded,
    reorderDashboard,
    findConnection,
    saveConnection,
    removeConnection,
    reorderConnections,
    moveConnection,
    saveQuery,
    deleteSavedQuery,
  };
}
