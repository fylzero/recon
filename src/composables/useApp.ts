import { ref } from "vue";
import * as api from "../api";
import type {
  AppData,
  ConnectionEntry,
  ConnectionGroup,
  PreferencesPatch,
  WindowState,
} from "../types";
import {
  DEFAULT_CODE_FONT,
  DEFAULT_EDITOR_FONT_SIZE,
  DEFAULT_GRID_FONT_SIZE,
  clampFontSize,
  sanitizeFontFamily,
} from "../fonts";

export const DEFAULT_PAGE_SIZE = 300;
export const DEFAULT_QUERY_ROW_LIMIT = 10_000;
export const SIDEBAR_MIN = 180;
export const SIDEBAR_MAX = 560;
export const DEFAULT_MAX_AUTO_COLUMN_WIDTH = 480;

const groups = ref<ConnectionGroup[]>([]);
const standaloneConnections = ref<ConnectionEntry[]>([]);
const error = ref("");
const loaded = ref(false);
const editorFontFamily = ref(DEFAULT_CODE_FONT);
const editorFontSize = ref(DEFAULT_EDITOR_FONT_SIZE);
const gridFontFamily = ref(DEFAULT_CODE_FONT);
const gridFontSize = ref(DEFAULT_GRID_FONT_SIZE);
const pageSize = ref(DEFAULT_PAGE_SIZE);
const queryRowLimit = ref(DEFAULT_QUERY_ROW_LIMIT);
const sidebarWidth = ref(260);
const maxAutoColumnWidth = ref(DEFAULT_MAX_AUTO_COLUMN_WIDTH);
const windowState = ref<WindowState | null>(null);
const toastMessage = ref("");
const toastKind = ref<"success" | "error">("success");
let toastTimer: ReturnType<typeof setTimeout> | null = null;

function clampSidebar(width: number) {
  return Math.round(Math.min(SIDEBAR_MAX, Math.max(SIDEBAR_MIN, width)));
}

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
    pageSize.value = data.pageSize ?? DEFAULT_PAGE_SIZE;
    queryRowLimit.value = data.queryRowLimit ?? DEFAULT_QUERY_ROW_LIMIT;
    sidebarWidth.value = clampSidebar(data.sidebarWidth ?? 260);
    maxAutoColumnWidth.value = data.maxAutoColumnWidth ?? DEFAULT_MAX_AUTO_COLUMN_WIDTH;
    windowState.value = data.window ?? null;
  }

  function dismissToast() {
    if (toastTimer) {
      clearTimeout(toastTimer);
      toastTimer = null;
    }
    toastMessage.value = "";
  }

  function showToast(message: string, kind: "success" | "error" = "success") {
    dismissToast();
    toastKind.value = kind;
    toastMessage.value = message;
    toastTimer = setTimeout(
      () => {
        toastMessage.value = "";
        toastTimer = null;
      },
      kind === "error" ? 5600 : 3200,
    );
  }

  async function savePreferences(patch: PreferencesPatch) {
    const next = await api.updatePreferences(patch);
    applyState(next);
  }

  function previewPreferences(patch: PreferencesPatch) {
    if (patch.editorFontSize !== undefined) {
      editorFontSize.value = clampFontSize(patch.editorFontSize, DEFAULT_EDITOR_FONT_SIZE);
    }
    if (patch.gridFontSize !== undefined) {
      gridFontSize.value = clampFontSize(patch.gridFontSize, DEFAULT_GRID_FONT_SIZE);
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
    return group;
  }

  async function updateGroup(groupId: string, name: string, headerColor?: string) {
    await api.updateGroup(groupId, name, headerColor);
    patchGroup(groupId, headerColor ? { name, headerColor } : { name });
  }

  async function deleteGroup(groupId: string) {
    await api.deleteGroup(groupId);
    groups.value = groups.value.filter((group) => group.id !== groupId);
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

  async function reorderGroups(groupIds: string[]) {
    const byId = new Map(groups.value.map((group) => [group.id, group]));
    if (groupIds.length !== groups.value.length || groupIds.some((id) => !byId.has(id))) {
      throw new Error("Group list does not match saved groups.");
    }
    const previous = groups.value;
    groups.value = groupIds.map((id) => byId.get(id)!);
    try {
      await api.reorderGroups(groupIds);
    } catch (err) {
      groups.value = previous;
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
  ) {
    const saved = await api.saveConnection(groupId, connection, password, sshSecret);
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

  return {
    groups,
    standaloneConnections,
    error,
    loaded,
    editorFontFamily,
    editorFontSize,
    gridFontFamily,
    gridFontSize,
    pageSize,
    queryRowLimit,
    sidebarWidth,
    maxAutoColumnWidth,
    windowState,
    toastMessage,
    toastKind,
    load,
    showToast,
    dismissToast,
    savePreferences,
    previewPreferences,
    replaceSettings,
    saveWindowState,
    createGroup,
    updateGroup,
    deleteGroup,
    toggleGroup,
    setAllGroupsExpanded,
    reorderGroups,
    findConnection,
    saveConnection,
    removeConnection,
    reorderConnections,
  };
}
